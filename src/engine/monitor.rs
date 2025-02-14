use std::sync::{Arc, Mutex};
use std::{collections::HashSet, time::Duration};

use crate::common::blacklist::Blacklist;
use crate::common::utils::{import_env_var, LOG_INSTRUCTION, PUMP_LOG_INSTRUCTION};
use crate::dex::pump_fun::Pump;
use crate::dex::raydium::RAYDIUM_AUTHORITY_V4;
use crate::{
    common::{
        logger::Logger,
        utils::{AppState, LiquidityPool, Status, SwapConfig},
    },
    dex::{
        pump_fun::PUMP_PROGRAM,
        raydium::{Raydium, AMM_PROGRAM},
    },
};
use futures_util::SinkExt;
use serde_json::Value;
use spl_token::ui_amount_to_amount;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use super::swap::{SwapDirection, SwapInType};
use chrono::Utc;
use futures_util::stream::StreamExt;
use tokio::time::Instant;

pub async fn pumpfun_autosell_monitor(
    existing_liquidity_pools: Arc<Mutex<HashSet<LiquidityPool>>> ,
    app_state: AppState,
    swap_config: SwapConfig,
    time_exceed: u64,
) {
    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
    let logger = Logger::new(format!(
        "[AUTOSELL MONITOR])({}) => ",
        existing_pools.len()
    ));
    for existing_pool in existing_pools.clone().iter() {
        let timeout = Duration::from_secs(time_exceed);
        let start_time = Instant::now();
        if existing_pool.status == Status::Bought {
            if let Some(timestamp) = existing_pool.timestamp {
                if timestamp.elapsed() > timeout {
                    // Now Auto-Sell
                    logger.log(format!(
                        "[Auto-Sell Pool]({}): Reached at selling time, Selling at {} ({:?}).",
                        existing_pool.clone().mint.clone(),
                        Utc::now(),
                        start_time.elapsed()
                    ));
                    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
                    let rpc_client = app_state.clone().rpc_client;
                    let wallet = app_state.clone().wallet;
                    let swapx = Pump::new(rpc_nonblocking_client, rpc_client, wallet);
                    let sell_config = SwapConfig {
                        swap_direction: SwapDirection::Sell,
                        in_type: SwapInType::Pct,
                        amount_in: 1_f64,
                        slippage: 100_u64,
                        use_jito: swap_config.clone().use_jito,
                    };

                    // Update status into ING status..
                    let selling_pool = LiquidityPool {
                        mint: existing_pool.clone().mint,
                        in_amount: existing_pool.in_amount,
                        out_amount: existing_pool.out_amount,
                        status: Status::Selling,
                        timestamp: Some(Instant::now()),
                    };
                    existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
                    existing_pools.insert(selling_pool.clone());

                    let swapx_clone = swapx.clone();
                    let existing_pool_clone = existing_pool.clone();
                    let logger_clone = logger.clone();
                    let sell_config_clone = sell_config.clone();
                    let existing_pools_clone = Arc::clone(&existing_liquidity_pools);

                    let _ = tokio::spawn(async move {
                        match swapx_clone
                            .swap_by_mint(
                                &existing_pool_clone.clone().mint,
                                sell_config_clone,
                                start_time,
                            )
                            .await
                        {
                            Ok(res) => {
                                // Update Status::New with Status::Bought
                                let sold_pool = LiquidityPool {
                                    mint: existing_pool_clone.clone().mint,
                                    in_amount: existing_pool_clone.in_amount,
                                    out_amount: existing_pool_clone.out_amount,
                                    status: Status::Sold,
                                    timestamp: Some(Instant::now()),
                                };

                                let mut update_pools = existing_pools_clone.lock().unwrap();
                                update_pools
                                    .retain(|pool| pool.mint != existing_pool_clone.clone().mint);
                                update_pools.insert(sold_pool.clone());
                                logger_clone.log(format!("[Sell Result]: {}\n{:#?}", res[0], sold_pool));
                            }
                            Err(e) => {
                                // Update Status::Selling with Status::Bought
                                let bought_pool = LiquidityPool {
                                    mint: existing_pool_clone.clone().mint,
                                    in_amount: existing_pool_clone.in_amount,
                                    out_amount: existing_pool_clone.out_amount,
                                    status: Status::Bought,
                                    timestamp: existing_pool_clone.timestamp,
                                };
                                
                                let mut update_pools = existing_pools_clone.lock().unwrap();
                                update_pools
                                    .retain(|pool| pool.mint != existing_pool_clone.clone().mint);
                                update_pools.insert(bought_pool.clone());
                                logger_clone.log(format!(
                                    "[Sell Result]: Sell issue -> {:?}, will try to sell in the next time.",
                                    e,
                                ));
                            }
                        };
                    });
                }
            }
        }
    }
}

pub async fn autosell_monitor(
    mut existing_liquidity_pools: HashSet<LiquidityPool>,
    app_state: AppState,
    swap_config: SwapConfig,
) -> HashSet<LiquidityPool> {
    let logger = Logger::new(format!(
        "[AUTOSELL MONITOR])({}) => ",
        existing_liquidity_pools.len()
    ));
    for existing_pool in existing_liquidity_pools.clone().iter() {
        let timeout = Duration::from_secs(60);
        let start_time = Instant::now();
        if existing_pool.status == Status::Bought {
            if let Some(timestamp) = existing_pool.timestamp {
                if timestamp.elapsed() > timeout {
                    // Now Auto-Sell
                    logger.log(format!(
                        "[Auto-Sell Pool]({}): Reached at selling time, Selling at {}.",
                        existing_pool.clone().mint.clone(),
                        Utc::now(),
                    ));
                    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
                    let rpc_client = app_state.clone().rpc_client;
                    let wallet = app_state.clone().wallet;
                    let swapx = Raydium::new(rpc_nonblocking_client, rpc_client, wallet);
                    let sell_config = SwapConfig {
                        swap_direction: SwapDirection::Sell,
                        in_type: SwapInType::Pct,
                        amount_in: 1_f64,
                        slippage: swap_config.clone().slippage,
                        use_jito: swap_config.clone().use_jito,
                    };
                    match swapx
                        .swap_by_mint(&existing_pool.clone().mint, sell_config.clone(), start_time)
                        .await
                    {
                        Ok(res) => {
                            // Update Status::New with Status::Bought
                            let sold_pool = LiquidityPool {
                                mint: existing_pool.clone().mint,
                                in_amount: existing_pool.clone().in_amount,
                                out_amount: existing_pool.clone().out_amount,
                                status: Status::Sold,
                                timestamp: Some(Instant::now()),
                            };
                            existing_liquidity_pools
                                .retain(|pool| pool.mint != existing_pool.clone().mint);
                            existing_liquidity_pools.insert(sold_pool.clone());
                            logger.log(format!("[Sell Result]: {}\n{:#?}", res[0], sold_pool));
                        }
                        Err(e) => {
                            let failed_pool = LiquidityPool {
                                mint: existing_pool.clone().mint,
                                in_amount: existing_pool.clone().in_amount,
                                out_amount: existing_pool.clone().out_amount,
                                status: Status::Failure,
                                timestamp: Some(Instant::now()),
                            };
                            existing_liquidity_pools
                                .retain(|pool| pool.mint != existing_pool.clone().mint);
                            existing_liquidity_pools.insert(failed_pool.clone());
                            logger.log(format!("[Sell Result]: Sell issue -> {:?}", e));
                            continue;
                        }
                    }
                }
            }
        }
    }
    existing_liquidity_pools
}

fn process_token_balances(balances: &Vec<Value>, target: &str) -> (u128, u128) {
    let mut mint_amount = 0;
    let mut sol_amount = 0;

    for token_balance in balances {
        // Extract owner and mint for easier access
        let owner = token_balance["owner"].as_str().unwrap();
        let mint = token_balance["mint"].as_str().unwrap();
        let amount_str = token_balance["uiTokenAmount"]["amount"].as_str().unwrap();

        let amount = amount_str.parse::<u128>().unwrap();

        if owner == target && mint != spl_token::native_mint::ID.to_string() {
            mint_amount = amount;
        }
        if owner == RAYDIUM_AUTHORITY_V4 && mint == spl_token::native_mint::ID.to_string() {
            sol_amount = amount;
        }
    }

    (mint_amount, sol_amount)
}

pub async fn new_token_trader_pumpfun(
    rpc_wss: &str,
    app_state: AppState,
    swap_config: SwapConfig,
    blacklist: Blacklist,
    time_exceed: u64,
    solana_price: f64,
) {
    let (ws_stream, _) = connect_async(rpc_wss)
        .await
        .expect("Failed to connect to WebSocket server");
    let (mut write, mut read) = ws_stream.split();
    // Subscribe to logs
    let subscription_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "transactionSubscribe",
        "params": [

            {
                "failed": false,
                "accountInclude": [PUMP_PROGRAM],
                "accountExclude": [],
                // Optionally specify accounts of interest
            },
            {
                "commitment": "processed",
                "encoding": "jsonParsed",
                "transactionDetails": "full",
                "maxSupportedTransactionVersion": 0
            }
        ]
    });
    write
        .send(subscription_message.to_string().into())
        .await
        .expect("Failed to send subscription message");

    let existing_liquidity_pools = Arc::new(Mutex::new(HashSet::<LiquidityPool>::new()));

    let mut buy_ui_amount: f64 = import_env_var("BUY_THRESHOLD")
        .parse()
        .expect("Failed to parse string into f64");
    buy_ui_amount /= solana_price;
    let buy_thredshold = ui_amount_to_amount(buy_ui_amount, spl_token::native_mint::DECIMALS);

    let mut sell_ui_amount: f64 = import_env_var("SELL_THRESHOLD")
        .parse()
        .expect("Failed to parse string into f64");
    sell_ui_amount /= solana_price;
    let sell_thredshold: u128 =
        ui_amount_to_amount(sell_ui_amount, spl_token::native_mint::DECIMALS).into();

    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
    let rpc_client = app_state.clone().rpc_client;
    let wallet = app_state.clone().wallet;
    let swapx = Pump::new(
        rpc_nonblocking_client.clone(),
        rpc_client.clone(),
        wallet.clone(),
    );

    // Listen for messages
    let logger = Logger::new("[PUMPFUN MONITOR] => ".to_string());
    logger.log("Started. monitoring...".to_string());
    // let mut counter = 0;
    while let Some(Ok(msg)) = read.next().await {
        if let WsMessage::Text(text) = msg {
            let start_time = Instant::now();
            let json: Value = serde_json::from_str(&text).unwrap();
            // logger.log(format!("{:#?}", json));
            pumpfun_autosell_monitor(
                Arc::clone(&existing_liquidity_pools),
                app_state.clone(),
                swap_config.clone(),
                time_exceed,
            )
            .await;
            
            if let Some(log_messages) =
                json["params"]["result"]["transaction"]["meta"]["logMessages"].as_array()
            {
                // Check if this is mint tx
                let mut mint_flag = false; //mint_tx?true:false;
                let mut skip_flag = false; //skip_flag?true:false;
                let slot = json["params"]["result"]["slot"].as_number().unwrap();
                let signature = json["params"]["result"]["signature"].clone().to_string();
                let mut target = "";
                let mut mint = "".to_string();
                let mut bonding_curve = "".to_string();
                let mut bonding_curve_index = 0;
                let mut sol_post_amount = 0_u64;
                let mut sol_pre_amount = 0_u64;

                // Retrieve Target Wallet Pubkey
                let account_keys = json["params"]["result"]["transaction"]["transaction"]
                    ["message"]["accountKeys"]
                    .as_array()
                    .expect("Failed to get account keys");
                if let Some(account_key) = account_keys
                    .iter()
                    .find(|account_key| account_key["signer"].as_bool().unwrap())
                {
                    target = account_key["pubkey"].as_str().unwrap();
                }

                if let Some(post_token_balances) =
                    json["params"]["result"]["transaction"]["meta"]["postTokenBalances"].as_array()
                {
                    for post_token_balance in post_token_balances.iter() {
                        let owner = post_token_balance["owner"].as_str().unwrap();

                        if owner != target {
                            bonding_curve = owner.to_string();
                        }

                        if owner == target || owner == bonding_curve {
                            mint = post_token_balance["mint"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                }

                if let Some(index) = account_keys.iter().position(|account_key| {
                    account_key["pubkey"].as_str().unwrap() == bonding_curve
                }) {
                    bonding_curve_index = index
                }

                if let Some(post_balances) =
                    json["params"]["result"]["transaction"]["meta"]["postBalances"].as_array()
                {
                    sol_post_amount = post_balances[bonding_curve_index].as_u64().unwrap();
                }
                if let Some(pre_balances) =
                    json["params"]["result"]["transaction"]["meta"]["preBalances"].as_array()
                {
                    sol_pre_amount = pre_balances[bonding_curve_index].as_u64().unwrap();
                }
                // logger.log(format!("{}:{}:{}:{}:{}:{}", target, mint, bonding_curve, bonding_curve_index, sol_post_amount, sol_pre_amount));
                for log_message in log_messages.iter() {
                    if let Some(log_msg) = log_message.as_str() {
                        // Mint tx log
                        if log_msg.contains(PUMP_LOG_INSTRUCTION) {
                            // Filter the token: initial LP

                            // Add existing liquidity pools
                            let new_pool = LiquidityPool {
                                mint: mint.to_string(),
                                in_amount: 0,
                                out_amount: 0,
                                status: Status::New,
                                timestamp: None,
                            };
                            
                            let mut update_pools = existing_liquidity_pools.lock().unwrap();
                            update_pools.insert(new_pool.clone());
                            logger.log(format!(
                                "[New Pool]({}-{}): Detected at {} ({:?}).",
                                signature,
                                slot,
                                Utc::now(),
                                start_time.elapsed()
                            ));
                            mint_flag = true;
                            logger.log(format!(
                                "[New Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                                new_pool.mint,
                                new_pool.in_amount,
                                new_pool.out_amount,
                                new_pool.status,
                                new_pool.timestamp
                            ));
                            // logger.log(format!(
                            //     "[Existing Pools]: \n {:#?}",
                            //     existing_liquidity_pools
                            // ));
                            break;
                        }
                    }
                }
                if !mint_flag && !skip_flag {
                    // If this is not mint tx, then check if it is existing in existing_liquidity_pool
                    // Check if mint is existing in existing_liquidity_pool
                    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                    if let Some(existing_pool) = existing_pools
                        .clone()
                        .iter()
                        .find(|pool| pool.mint == mint)
                    {
                        if existing_pool.status == Status::New {
                            // if counter > 8 {
                            //     continue;
                            // }
                            // Check Blacklist
                            if blacklist.is_blacklisted(target) {
                                let failed_pool = LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount,
                                    status: Status::Failure,
                                    timestamp: Some(Instant::now()),
                                };
                                existing_pools
                                    .retain(|pool| pool.mint != existing_pool.clone().mint);
                                existing_pools.insert(failed_pool.clone());
                                logger.log(format!(
                                    "Notification: {} bought this token, then we don't buy!",
                                    target
                                ));
                                continue;
                            }

                            // Check Buy Condition
                            let mut buy_amount = 0_u64;
                            let check_pool = if sol_pre_amount < sol_post_amount {
                                // buy
                                buy_amount = sol_post_amount - sol_pre_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount + buy_amount as u128,
                                    out_amount: existing_pool.out_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: None,
                                }
                            } else {
                                // sell
                                let sell_amount = sol_pre_amount - sol_post_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount + sell_amount as u128,
                                    status: existing_pool.clone().status,
                                    timestamp: None,
                                }
                            };
                            existing_pools
                                .retain(|pool| pool.mint != existing_pool.clone().mint);
                            existing_pools.insert(check_pool.clone());
                            // logger.log(format!(
                            //     "[Sync Pool]({}): Syncing at {} ({:?}).",
                            //     signature.clone(),
                            //     Utc::now(),
                            //     start_time.elapsed()
                            // ));
                            // logger.log(format!(
                            //     "[Sync Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                            //     check_pool.mint,
                            //     check_pool.in_amount,
                            //     check_pool.out_amount,
                            //     check_pool.status,
                            //     check_pool.timestamp
                            // ));

                            if existing_pool.in_amount + buy_amount as u128 > buy_thredshold as u128
                            {
                                // Now Buy!
                                logger.log(format!(
                                    "[Buy Pool]({}): Reached at buying condition, Buying at {}({}) ({:?}).",
                                    signature.clone(),
                                    Utc::now(),
                                    Utc::now().timestamp(),
                                    start_time.elapsed()
                                ));

                                // Update status into ING status..
                                let buying_pool = LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount + buy_amount as u128,
                                    out_amount: 0_u128,
                                    status: Status::Buying,
                                    timestamp: Some(Instant::now()),
                                };
                                existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
                                existing_pools.insert(buying_pool.clone());
                                // counter += 1;

                                // Buy through the thread
                                let swapx_clone = swapx.clone();
                                let existing_pool_clone = existing_pool.clone();
                                let swap_config_clone = swap_config.clone();
                                let logger_clone = logger.clone();
                                let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
                                let _ = tokio::spawn(async move {
                                    match swapx_clone
                                        .swap_by_mint(
                                            &existing_pool_clone.mint,
                                            swap_config_clone,
                                            start_time,
                                        )
                                        .await
                                    {
                                        Ok(res) => {
                                            // Update Status::New with Status::Bought
                                            let bought_pool = LiquidityPool {
                                                mint: existing_pool_clone.mint.clone(),
                                                in_amount: existing_pool_clone.in_amount + buy_amount as u128,
                                                out_amount: 0,
                                                status: Status::Bought,
                                                timestamp: Some(Instant::now()),
                                            };
                                            let mut update_pools = existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools
                                                .retain(|pool| pool.mint != existing_pool_clone.mint);
                                            update_pools.insert(bought_pool.clone());
                                            logger_clone.log(format!(
                                                "[Buy Result]({:?} :: {} :: {:?}):{} \n {:#?}",
                                                Utc::now(),
                                                Utc::now().timestamp(),
                                                start_time.elapsed(),
                                                res[0],
                                                bought_pool
                                            ));
                                            // logger.log(format!(
                                            //     "[Existing Pools]: \n {:#?}",
                                            //     existing_liquidity_pools
                                            // ));
                                            // counter += 1;
                                        }
                                        Err(e) => {
                                            let failed_pool = LiquidityPool {
                                                mint: existing_pool_clone.mint.clone(),
                                                in_amount: existing_pool_clone.in_amount,
                                                out_amount: existing_pool_clone.out_amount,
                                                status: Status::Failure,
                                                timestamp: Some(Instant::now()),
                                            };
                                            let mut update_pools = existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools
                                                .retain(|pool| pool.mint != existing_pool_clone.mint);
                                            update_pools.insert(failed_pool.clone());
                                            logger_clone.log(format!("[Buy Result]: Buy issue -> {:?}", e));
                                            // continue;
                                        }
                                    };
                                });
                            } else {
                                // logger.log(format!(
                                //     "[Skip Pool]({}): Not reached at buying coindition, Skipping at {}.",
                                //     signature.clone(),
                                //     Utc::now(),
                                // ));
                            }
                        } else if existing_pool.status == Status::Bought {
                            // Check Sell Condition
                            let mut sell_amount = 0_u64;
                            let check_pool = if sol_pre_amount < sol_post_amount {
                                // buy
                                let buy_amount = sol_post_amount - sol_pre_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount + buy_amount as u128,
                                    out_amount: existing_pool.out_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: existing_pool.timestamp,
                                }
                            } else {
                                // sell
                                sell_amount = sol_pre_amount - sol_post_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount + sell_amount as u128,
                                    status: existing_pool.clone().status,
                                    timestamp: existing_pool.timestamp,
                                }
                            };
                            existing_pools
                                .retain(|pool| pool.mint != existing_pool.clone().mint);
                            existing_pools.insert(check_pool.clone());
                            // logger.log(format!(
                            //     "[Sync Pool]({}): Syncing at {} ({:?}).",
                            //     signature.clone(),
                            //     Utc::now(),
                            //     start_time.elapsed()
                            // ));

                            // logger.log(format!(
                            //     "[Sync Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                            //     check_pool.mint,
                            //     check_pool.in_amount,
                            //     check_pool.out_amount,
                            //     check_pool.status,
                            //     check_pool.timestamp
                            // ));
                            // logger.log(format!(
                            //     "[Existing Pools]: \n {:#?}",
                            //     existing_liquidity_pools
                            // ));

                            if existing_pool.out_amount + sell_amount as u128 > sell_thredshold {
                                // Now Sell!
                                logger.log(format!(
                                    "[Sell Pool]({}): Reached at selling condition, Selling at {} ({:?}).",
                                    signature.clone(),
                                    Utc::now(),
                                    start_time.elapsed()
                                ));
                                logger.log(format!(
                                    "[[Sell Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                                    existing_pool.clone().mint,
                                    existing_pool.in_amount,
                                    existing_pool.out_amount,
                                    existing_pool.status,
                                    existing_pool.timestamp
                                ));

                                // Update status into ING status..
                                let selling_pool = LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount,
                                    status: Status::Selling,
                                    timestamp: Some(Instant::now()),
                                };
                                existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
                                existing_pools.insert(selling_pool.clone());
                                
                                // Sell through the thread
                                let sell_config = SwapConfig {
                                    swap_direction: SwapDirection::Sell,
                                    in_type: SwapInType::Pct,
                                    amount_in: 1_f64,
                                    slippage: 100_u64,
                                    use_jito: swap_config.clone().use_jito,
                                };
                                let swapx_clone = swapx.clone();
                                let existing_pool_clone = existing_pool.clone();
                                let logger_clone = logger.clone();
                                let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
                                let _ = tokio::spawn(async move {
                                    let max_retries = 5;
                                    let mut retry_count = 0;
                                    let mut sell_successful = false;

                                    while retry_count < max_retries && !sell_successful {
                                        match swapx_clone
                                            .swap_by_mint(
                                                &existing_pool_clone.clone().mint,
                                                sell_config.clone(),
                                                start_time,
                                            )
                                            .await
                                        {
                                            Ok(res) => {
                                                // Update Status::Bought with Status::Sold
                                                let sold_pool = LiquidityPool {
                                                    mint: existing_pool_clone.clone().mint,
                                                    in_amount: existing_pool_clone.in_amount,
                                                    out_amount: existing_pool_clone.out_amount,
                                                    status: Status::Sold,
                                                    timestamp: Some(Instant::now()),
                                                };
                                                
                                                let mut update_pools = existing_liquidity_pools_clone.lock().unwrap();
                                                update_pools
                                                    .retain(|pool| pool.mint != existing_pool_clone.clone().mint);
                                                update_pools.insert(sold_pool.clone());
                                                logger_clone.log(format!(
                                                    "[Sell Result]: {}\n{:#?}",
                                                    res[0], sold_pool
                                                ));
                                                sell_successful = true; // Mark the sell as successful
                                            }
                                            Err(e) => {
                                                retry_count += 1; // Increment retry count
                                                // Update Status::Selling with Status::Bought
                                                let bought_pool = LiquidityPool {
                                                    mint: existing_pool_clone.clone().mint,
                                                    in_amount: existing_pool_clone.in_amount,
                                                    out_amount: existing_pool_clone.out_amount,
                                                    status: Status::Bought,
                                                    timestamp: existing_pool_clone.timestamp,
                                                };
                                                
                                                let mut update_pools = existing_liquidity_pools_clone.lock().unwrap();
                                                update_pools
                                                    .retain(|pool| pool.mint != existing_pool_clone.clone().mint);
                                                update_pools.insert(bought_pool.clone());
                                                logger_clone.log(format!(
                                                    "[Sell Result]: Sell issue -> {:?}, will try to sell in the next time",
                                                    e, 
                                                ));
                                            }
                                        };
                                    }
                                });
                            } else {
                                // logger.log(format!(
                                //     "[Skip Pool]({}): Not reached at selling condition, Skipping at {}.",
                                //     signature.clone(),
                                //     Utc::now(),
                                // ));
                            }
                        } else {
                            // logger.log(format!(
                            //     "[Skip Pool]({}): Already Sold or Failed Pool, Skipping at {}.",
                            //     existing_pool.mint.clone(),
                            //     Utc::now(),
                            // ));
                        }
                    }
                } else if mint_flag && !skip_flag {
                    //TODO: Write the logic once client wants to buy once it detects mint tx
                    // Check Buy Condition
                    // if counter > 8 {
                    //     continue;
                    // }
                    if sol_post_amount < sol_pre_amount {
                        continue;
                    }
                    let buy_amount = sol_post_amount - sol_pre_amount;
                    if buy_amount > buy_thredshold {
                        // Now Buy!
                        logger.log(format!(
                            "[Buy Pool]({}): \n Reached at buying condition, Buying at {} ({:?}).",
                            signature.clone(),
                            Utc::now(),
                            start_time.elapsed(),
                        ));
                        
                        // Update status into ING status..
                        let buying_pool = LiquidityPool {
                            mint: mint.clone(),
                            in_amount: buy_amount as u128,
                            out_amount: 0_u128,
                            status: Status::Buying,
                            timestamp: Some(Instant::now()),
                        };
                        let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                        existing_pools.retain(|pool| pool.mint != mint.clone());
                        existing_pools.insert(buying_pool.clone());

                        // counter += 1;
                        // Buy through the thread
                        let swapx_clone = swapx.clone();
                        let mint_clone = mint.clone();
                        let swap_config_clone = swap_config.clone();
                        let logger_clone = logger.clone();
                        let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
                        let _ = tokio::spawn(async move {
                            match swapx_clone
                                .swap_by_mint(&mint_clone, swap_config_clone, start_time)
                                .await
                            {
                                Ok(res) => {
                                    // Update Status::New with Status::Bought
                                    let bought_pool = LiquidityPool {
                                        mint: mint_clone.clone(),
                                        in_amount: buy_amount as u128,
                                        out_amount: 0_u128,
                                        status: Status::Bought,
                                        timestamp: Some(Instant::now()),
                                    };
                                    
                                    let mut update_pools = existing_liquidity_pools_clone.lock().unwrap();
                                    update_pools.retain(|pool| pool.mint != mint_clone.clone());
                                    update_pools.insert(bought_pool.clone());
                                    logger_clone
                                        .log(format!("[Buy Result]:{} \n {:#?}", res[0], bought_pool));
                                    // logger.log(format!(
                                    //     "[Existing Pools]: \n {:#?}",
                                    //     existing_liquidity_pools
                                    // ));
                                    // counter += 1;
                                }
                                Err(e) => {
                                    let failed_pool = LiquidityPool {
                                        mint: mint_clone.clone(),
                                        in_amount: buy_amount as u128,
                                        out_amount: 0_u128,
                                        status: Status::Failure,
                                        timestamp: Some(Instant::now()),
                                    };
                                    
                                    let mut update_pools = existing_liquidity_pools_clone.lock().unwrap();
                                    update_pools.retain(|pool| pool.mint != mint_clone.clone());
                                    update_pools.insert(failed_pool.clone());
                                    
                                    logger_clone.log(format!("[Buy Result]: Buy issue -> {:?}", e));
                                }
                            };
                        });
                    } else {
                        // logger.log(format!(
                        //     "[Initial Pool]({}): \n Not reached at buying condition, Updating at {} ({:?}).",
                        //     signature.clone(),
                        //     Utc::now(),
                        //     start_time.elapsed(),
                        // ));
                        let initial_pool = LiquidityPool {
                            mint: mint.clone(),
                            in_amount: buy_amount as u128,
                            out_amount: 0_u128,
                            status: Status::New,
                            timestamp: Some(Instant::now()),
                        };
                        let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                        existing_pools.retain(|pool| pool.mint != mint.clone());
                        existing_pools.insert(initial_pool.clone());
                        // logger.log(format!("[Initial Result]: \n {:#?}", initial_pool));
                    }
                }
            }
        }
    }
}

pub async fn new_token_trader_raydium(
    rpc_wss: &str,
    app_state: AppState,
    swap_config: SwapConfig,
    solana_price: f64,
) {
    let (ws_stream, _) = connect_async(rpc_wss)
        .await
        .expect("Failed to connect to WebSocket server");
    let (mut write, mut read) = ws_stream.split();
    // Subscribe to logs
    let subscription_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "transactionSubscribe",
        "params": [

            {
                "failed": false,
                "accountInclude": [AMM_PROGRAM],
                "accountExclude": [],
                // Optionally specify accounts of interest
            },
            {
                "commitment": "processed",
                "encoding": "jsonParsed",
                "transactionDetails": "full",
                "maxSupportedTransactionVersion": 0
            }
        ]
    });
    write
        .send(subscription_message.to_string().into())
        .await
        .expect("Failed to send subscription message");

    let mut existing_liquidity_pools = HashSet::<LiquidityPool>::new();
    // Listen for messages
    let logger = Logger::new("[RAYDIUM MONITOR] => ".to_string());
    logger.log("Started. monitoring...".to_string());

    while let Some(Ok(msg)) = read.next().await {
        if let WsMessage::Text(text) = msg {
            let json: Value = serde_json::from_str(&text).unwrap();
            existing_liquidity_pools = autosell_monitor(
                existing_liquidity_pools.clone(),
                app_state.clone(),
                swap_config.clone(),
            )
            .await;
            if let Some(log_messages) =
                json["params"]["result"]["transaction"]["meta"]["logMessages"].as_array()
            {
                let start_time = Instant::now();
                // Check if this is mint tx
                let mut mint_flag = false; //mint_tx?true:false;
                let mut skip_flag = false; //skip_flag?true:false;
                let slot = json["params"]["result"]["slot"].as_number().unwrap();
                let signature = json["params"]["result"]["signature"].clone().to_string();
                for log_message in log_messages.iter() {
                    if let Some(log_msg) = log_message.as_str() {
                        // Mint tx log
                        if log_msg.contains(LOG_INSTRUCTION) {
                            let instructions = &json["params"]["result"]["transaction"]["meta"]
                                ["innerInstructions"]
                                .as_array()
                                .unwrap()[0]["instructions"]
                                .as_array()
                                .unwrap();
                            let mint = if instructions[12]["parsed"]["info"]["mint"]
                                == spl_token::native_mint::ID.to_string()
                            {
                                match instructions[16]["parsed"]["info"]["mint"].as_str() {
                                    Some(mint) => mint,
                                    None => break,
                                }
                            } else {
                                match instructions[12]["parsed"]["info"]["mint"].as_str() {
                                    Some(mint) => mint,
                                    None => break,
                                }
                            };
                            let mut mint_post_amount = 0_u128;
                            let mut sol_post_amount = 0_u128;
                            let mut target = "";

                            // Retrieve Target Wallet Pubkey
                            let account_keys = json["params"]["result"]["transaction"]
                                ["transaction"]["message"]["accountKeys"]
                                .as_array()
                                .expect("Failed to get account keys");
                            if let Some(account_key) = account_keys
                                .iter()
                                .find(|account_key| account_key["signer"].as_bool().unwrap())
                            {
                                target = account_key["pubkey"].as_str().unwrap();
                            }
                            if let Some(post_token_balances) = json["params"]["result"]
                                ["transaction"]["meta"]["postTokenBalances"]
                                .as_array()
                            {
                                (mint_post_amount, sol_post_amount) =
                                    process_token_balances(post_token_balances, target);
                            }

                            // Filter the token: initial LP
                            let threshold = 50_000_000_000_u128;
                            if sol_post_amount < threshold {
                                logger.log(format!(
                                    "This token has low liquidity: {} - {}. Trade carefully!",
                                    mint, sol_post_amount
                                ));
                                skip_flag = true;
                                break;
                            }
                            // Add existing liquidity pools
                            let new_pool = LiquidityPool {
                                mint: mint.to_string(),
                                in_amount: 0,
                                out_amount: 0,
                                status: Status::New,
                                timestamp: None,
                            };
                            existing_liquidity_pools.insert(new_pool.clone());
                            logger.log(format!(
                                "[New Pool]({}-{}): Detected at {}.",
                                signature,
                                slot,
                                Utc::now()
                            ));
                            mint_flag = true;
                            logger.log(format!(
                                "[New Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                                new_pool.mint,
                                new_pool.in_amount,
                                new_pool.out_amount,
                                new_pool.status,
                                new_pool.timestamp
                            ));
                            logger.log(format!(
                                "[Existing Pools]: \n {:#?}",
                                existing_liquidity_pools
                            ));
                            break;
                        }
                    }
                }
                if !mint_flag && !skip_flag {
                    // If this is not mint tx, then check if it is existing in existing_liquidity_pool
                    let mut mint = "".to_string();
                    let mut mint_post_amount = 0_u128;
                    let mut mint_pre_amount = 0_u128;
                    let mut sol_post_amount = 0_u128;
                    let mut sol_pre_amount = 0_u128;
                    let mut target = "";

                    // Retrieve Target Wallet Pubkey
                    let account_keys = json["params"]["result"]["transaction"]["transaction"]
                        ["message"]["accountKeys"]
                        .as_array()
                        .expect("Failed to get account keys");
                    if let Some(account_key) = account_keys
                        .iter()
                        .find(|account_key| account_key["signer"].as_bool().unwrap())
                    {
                        target = account_key["pubkey"].as_str().unwrap();
                    }

                    // Retrive Mint to check if it is in existing_pool_list
                    if let Some(post_token_balances) = json["params"]["result"]["transaction"]
                        ["meta"]["postTokenBalances"]
                        .as_array()
                    {
                        if let Some(post_token_balance) =
                            post_token_balances.iter().find(|balance| {
                                balance["owner"] == *target
                                    && balance["mint"] != spl_token::native_mint::ID.to_string()
                            })
                        {
                            mint = post_token_balance["mint"].as_str().unwrap().to_string();
                        }
                    }

                    // Check if mint is existing in existing_liquidity_pool
                    if let Some(existing_pool) = existing_liquidity_pools
                        .clone()
                        .iter()
                        .find(|pool| pool.mint == mint)
                    {
                        // If this is existing in existing_liquidity_pools, check the status of this pool: New, Bought, Sold, Failure
                        // logger.log(format!(
                        //     "[Check Pool]({}): Checking at {}.",
                        //     mint,
                        //     Utc::now()
                        // ));
                        // Retrieve Swap info
                        if let Some(post_token_balances) = json["params"]["result"]["transaction"]
                            ["meta"]["postTokenBalances"]
                            .as_array()
                        {
                            (mint_post_amount, sol_post_amount) =
                                process_token_balances(post_token_balances, target);
                        }

                        if let Some(pre_token_balances) = json["params"]["result"]["transaction"]
                            ["meta"]["preTokenBalances"]
                            .as_array()
                        {
                            (mint_pre_amount, sol_pre_amount) =
                                process_token_balances(pre_token_balances, target);
                        }
                        if existing_pool.status == Status::New {
                            // Check Buy Condition
                            let mut buy_amount = 0_u128;
                            let check_pool = if mint_pre_amount < mint_post_amount {
                                // buy
                                buy_amount = sol_post_amount - sol_pre_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount + buy_amount,
                                    out_amount: existing_pool.out_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: None,
                                }
                            } else {
                                // sell
                                let mut sell_amount = 0_u128;
                                if sol_pre_amount > sol_post_amount {
                                    sell_amount = sol_pre_amount - sol_post_amount;
                                } else {
                                    sell_amount = sol_post_amount - sol_pre_amount;
                                }
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount + sell_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: None,
                                }
                            };
                            existing_liquidity_pools
                                .retain(|pool| pool.mint != existing_pool.clone().mint);
                            existing_liquidity_pools.insert(check_pool.clone());
                            logger.log(format!(
                                "[Sync Pool]({}): Syncing at {}.",
                                signature.clone(),
                                Utc::now(),
                            ));
                            logger.log(format!(
                                "[Sync Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                                check_pool.mint,
                                check_pool.in_amount,
                                check_pool.out_amount,
                                check_pool.status,
                                check_pool.timestamp
                            ));

                            let mut buy_ui_amount: f64 = import_env_var("BUY_THRESHOLD")
                                .parse()
                                .expect("Failed to parse string into f64");
                            buy_ui_amount /= solana_price;
                            let buy_thredshold: u128 = ui_amount_to_amount(
                                buy_ui_amount,
                                spl_token::native_mint::DECIMALS,
                            )
                            .into();
                            if existing_pool.in_amount + buy_amount > buy_thredshold {
                                // Now Buy!
                                logger.log(format!(
                                    "[Buy Pool]({}): Reached at buying condition, Buying at {}.",
                                    signature.clone(),
                                    Utc::now(),
                                ));

                                let rpc_nonblocking_client =
                                    app_state.clone().rpc_nonblocking_client;
                                let rpc_client = app_state.clone().rpc_client;
                                let wallet = app_state.clone().wallet;
                                let swapx =
                                    Raydium::new(rpc_nonblocking_client, rpc_client, wallet);

                                match swapx
                                    .swap_by_mint(
                                        &existing_pool.clone().mint,
                                        swap_config.clone(),
                                        start_time,
                                    )
                                    .await
                                {
                                    Ok(res) => {
                                        // Update Status::New with Status::Bought
                                        let bought_pool = LiquidityPool {
                                            mint: existing_pool.clone().mint,
                                            in_amount: existing_pool.in_amount + buy_amount,
                                            out_amount: 0,
                                            status: Status::Bought,
                                            timestamp: Some(Instant::now()),
                                        };
                                        existing_liquidity_pools
                                            .retain(|pool| pool.mint != existing_pool.clone().mint);
                                        existing_liquidity_pools.insert(bought_pool.clone());
                                        logger.log(format!(
                                            "[Buy Result]:{} \n {:#?}",
                                            res[0], bought_pool
                                        ));
                                        logger.log(format!(
                                            "[Existing Pools]: \n {:#?}",
                                            existing_liquidity_pools
                                        ));
                                    }
                                    Err(e) => {
                                        let failed_pool = LiquidityPool {
                                            mint: existing_pool.clone().mint,
                                            in_amount: existing_pool.in_amount,
                                            out_amount: existing_pool.out_amount,
                                            status: Status::Failure,
                                            timestamp: Some(Instant::now()),
                                        };
                                        existing_liquidity_pools
                                            .retain(|pool| pool.mint != existing_pool.clone().mint);
                                        existing_liquidity_pools.insert(failed_pool.clone());
                                        logger.log(format!("[Buy Result]: Buy issue -> {:?}", e));
                                        continue;
                                    }
                                }
                            } else {
                                logger.log(format!(
                                    "[Skip Pool]({}): Not reached at buying coindition, Skipping at {}.",
                                    signature.clone(),
                                    Utc::now(),
                                ));
                            }
                        } else if existing_pool.status == Status::Bought {
                            // Check Sell Condition
                            let check_pool = if mint_pre_amount < mint_post_amount {
                                // buy
                                let buy_amount = sol_post_amount - sol_pre_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount + buy_amount,
                                    out_amount: existing_pool.out_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: existing_pool.timestamp,
                                }
                            } else {
                                // sell
                                let mut sell_amount = 0_u128;
                                if sol_pre_amount > sol_post_amount {
                                    sell_amount = sol_pre_amount - sol_post_amount;
                                } else {
                                    sell_amount = sol_post_amount - sol_pre_amount;
                                }
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount + sell_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: existing_pool.timestamp,
                                }
                            };
                            existing_liquidity_pools
                                .retain(|pool| pool.mint != existing_pool.clone().mint);
                            existing_liquidity_pools.insert(check_pool.clone());
                            logger.log(format!(
                                "[Sync Pool]({}): Syncing at {}.",
                                signature.clone(),
                                Utc::now(),
                            ));

                            logger.log(format!(
                                "[Sync Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                                check_pool.mint,
                                check_pool.in_amount,
                                check_pool.out_amount,
                                check_pool.status,
                                check_pool.timestamp
                            ));
                            logger.log(format!(
                                "[Existing Pools]: \n {:#?}",
                                existing_liquidity_pools
                            ));
                            let mut sell_ui_amount: f64 = import_env_var("SELL_THRESHOLD")
                                .parse()
                                .expect("Failed to parse string into f64");
                            sell_ui_amount /= solana_price;
                            let sell_thredshold: u128 = ui_amount_to_amount(
                                sell_ui_amount,
                                spl_token::native_mint::DECIMALS,
                            )
                            .into();
                            if existing_pool.out_amount > sell_thredshold {
                                // Now Sell!
                                logger.log(format!(
                                    "[Sell Pool]({}): Reached at selling condition, Selling at {}.",
                                    signature.clone(),
                                    Utc::now(),
                                ));
                                logger.log(format!(
                                    "[[Sell Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                                    existing_pool.clone().mint,
                                    existing_pool.in_amount,
                                    existing_pool.out_amount,
                                    existing_pool.status,
                                    existing_pool.timestamp
                                ));

                                let rpc_nonblocking_client =
                                    app_state.clone().rpc_nonblocking_client;
                                let rpc_client = app_state.clone().rpc_client;
                                let wallet = app_state.clone().wallet;
                                let swapx =
                                    Raydium::new(rpc_nonblocking_client, rpc_client, wallet);
                                let sell_config = SwapConfig {
                                    swap_direction: SwapDirection::Sell,
                                    in_type: SwapInType::Pct,
                                    amount_in: 1_f64,
                                    slippage: swap_config.clone().slippage,
                                    use_jito: swap_config.clone().use_jito,
                                };
                                let max_retries = 5;
                                let mut retry_count = 0;
                                let mut sell_successful = false;

                                while retry_count < max_retries && !sell_successful {
                                    match swapx
                                        .swap_by_mint(
                                            &existing_pool.clone().mint,
                                            sell_config.clone(),
                                            start_time,
                                        )
                                        .await
                                    {
                                        Ok(res) => {
                                            // Update Status::New with Status::Bought
                                            let sold_pool = LiquidityPool {
                                                mint: existing_pool.clone().mint,
                                                in_amount: existing_pool.in_amount,
                                                out_amount: existing_pool.out_amount,
                                                status: Status::Sold,
                                                timestamp: Some(Instant::now()),
                                            };
                                            existing_liquidity_pools
                                                .retain(|pool| pool.mint != mint.clone());
                                            existing_liquidity_pools.insert(sold_pool.clone());
                                            logger.log(format!(
                                                "[Sell Result]: {}\n{:#?}",
                                                res[0], sold_pool
                                            ));
                                            sell_successful = true; // Mark the sell as successful
                                        }
                                        Err(e) => {
                                            retry_count += 1; // Increment retry count
                                            logger.log(format!(
                                                "[Sell Result]: Sell issue -> {:?}, Attempt {}/{}",
                                                e, retry_count, max_retries
                                            ));

                                            // If it's the last attempt, mark as failed
                                            if retry_count == max_retries {
                                                let failed_pool = LiquidityPool {
                                                    mint: existing_pool.clone().mint,
                                                    in_amount: existing_pool.in_amount,
                                                    out_amount: existing_pool.out_amount,
                                                    status: Status::Failure,
                                                    timestamp: Some(Instant::now()),
                                                };
                                                existing_liquidity_pools
                                                    .retain(|pool| pool.mint != mint.clone());
                                                existing_liquidity_pools
                                                    .insert(failed_pool.clone());
                                                logger.log(format!(
                                                    "[Sell Result]: Maximum retries reached. Marking as failed. Error: {:?}",
                                                    e
                                                ));
                                            }
                                        }
                                    }
                                }
                            } else {
                                logger.log(format!(
                                    "[Skip Pool]({}): Not reached at selling condition, Skipping at {}.",
                                    signature.clone(),
                                    Utc::now(),
                                ));
                            }
                        } else {
                            // logger.log(format!(
                            //     "[Skip Pool]({}): Already Sold or Failed Pool, Skipping at {}.",
                            //     existing_pool.mint.clone(),
                            //     Utc::now(),
                            // ));
                        }
                    }
                } else {
                    //TODO: Write the logic once client wants to buy once it detects mint tx
                }
            }
        }
    }
}

pub async fn test_buy(app_state: AppState, swap_config: SwapConfig, mint: &str) {
    let logger = Logger::new("[RAYDIUM BUY] => ".to_string());
    logger.log("Started. Buying...".to_string());
    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
    let rpc_client = app_state.clone().rpc_client;
    let wallet = app_state.clone().wallet;
    let swapx = Pump::new(rpc_nonblocking_client, rpc_client, wallet);
    let swap_config = SwapConfig {
        swap_direction: SwapDirection::Buy,
        in_type: SwapInType::Qty,
        amount_in: 0.0000001_f64,
        slippage: swap_config.clone().slippage,
        use_jito: swap_config.clone().use_jito,
    };
    let start_time = Instant::now();
    match swapx
        .swap_by_mint(mint, swap_config.clone(), start_time)
        .await
    {
        Ok(res) => {
            logger.log(format!("{:?}", res));
        }
        Err(e) => {
            logger.log(format!("Error:{:?}", e));
        }
    };
    // match swapx
    //     .swap_jupiter(swap_config.clone(), mint.to_string(), start_time)
    //     .await
    // {
    //     Ok(res) => {
    //         logger.log(format!("{:?}", res));
    //     }
    //     Err(e) => {
    //         logger.log(format!("Error:{:?}", e));
    //     }
    // };
}

pub async fn test_sell(app_state: AppState, swap_config: SwapConfig, mint: &str) {
    let logger = Logger::new("[RAYDIUM SELL] => ".to_string());
    logger.log("Started. selling...".to_string());
    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
    let rpc_client = app_state.clone().rpc_client;
    let wallet = app_state.clone().wallet;
    let swapx = Pump::new(rpc_nonblocking_client, rpc_client, wallet);
    let sell_config = SwapConfig {
        swap_direction: SwapDirection::Sell,
        in_type: SwapInType::Pct,
        amount_in: 1_f64,
        slippage: swap_config.clone().slippage,
        use_jito: swap_config.clone().use_jito,
    };
    let start_time = Instant::now();
    match swapx
        .swap_by_mint(mint, sell_config.clone(), start_time)
        .await
    {
        Ok(res) => {
            logger.log(format!("{:?}", res));
        }
        Err(e) => {
            logger.log(format!("Error:{:?}", e));
        }
    };
    // match swapx
    //     .swap_jupiter(sell_config.clone(), mint.to_string(), start_time)
    //     .await
    // {
    //     Ok(res) => {
    //         logger.log(format!("{:?}", res));
    //     }
    //     Err(e) => {
    //         logger.log(format!("Error:{:?}", e));
    //     }
    // };
}
