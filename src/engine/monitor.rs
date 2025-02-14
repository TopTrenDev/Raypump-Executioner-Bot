use std::sync::{Arc, Mutex};
use std::{collections::HashSet, time::Duration};

use crate::common::blacklist::Blacklist;
use crate::common::utils::{import_env_var, PUMP_LOG_INSTRUCTION};
use crate::dex::pump_fun::Pump;
use crate::{
    common::{
        logger::Logger,
        utils::{AppState, LiquidityPool, Status, SwapConfig},
    },
    dex::pump_fun::PUMP_PROGRAM,
};
use anyhow::Result;
use futures_util::SinkExt;
use serde_json::Value;
use spl_token::ui_amount_to_amount;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use super::swap::{SwapDirection, SwapInType};
use chrono::Utc;
use futures_util::stream::StreamExt;
use tokio::time::Instant;

#[derive(Clone)]
pub struct TradeInfoFromToken {
    pub slot: u64,
    pub signature: String,
    pub target: String,
    pub mint: String,
    pub bonding_curve: String,
    pub bonding_curve_index: usize,
    pub sol_post_amount: u64,
    pub sol_pre_amount: u64,
}

impl TradeInfoFromToken {
    pub fn new(
        &self,
        slot: u64,
        signature: String,
        target: String,
        mint: String,
        bonding_curve: String,
        bonding_curve_index: usize,
        sol_post_amount: u64,
        sol_pre_amount: u64,
    ) -> Self {
        Self {
            slot,
            signature,
            target,
            mint,
            bonding_curve,
            bonding_curve_index,
            sol_post_amount,
            sol_pre_amount,
        }
    }

    pub fn from_json(json: Value) -> Result<Self> {
        let slot = json["params"]["result"]["slot"].as_u64().unwrap();
        let signature = json["params"]["result"]["signature"].clone().to_string();
        let mut target = String::new();
        let mut mint = String::new();
        let mut bonding_curve = String::new();
        let mut bonding_curve_index = 0;
        let mut sol_post_amount = 0_u64;
        let mut sol_pre_amount = 0_u64;

        // Retrieve Target Wallet Pubkey
        let account_keys = json["params"]["result"]["transaction"]["transaction"]["message"]
            ["accountKeys"]
            .as_array()
            .expect("Failed to get account keys");
        if let Some(account_key) = account_keys
            .iter()
            .find(|account_key| account_key["signer"].as_bool().unwrap())
        {
            target = account_key["pubkey"].as_str().unwrap().to_string();
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

        if let Some(index) = account_keys
            .iter()
            .position(|account_key| account_key["pubkey"].as_str().unwrap() == bonding_curve)
        {
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
        Ok(Self {
            slot,
            signature,
            target,
            mint,
            bonding_curve,
            bonding_curve_index,
            sol_post_amount,
            sol_pre_amount,
        })
    }
}

pub async fn pumpfun_autosell_monitor(
    existing_liquidity_pools: Arc<Mutex<HashSet<LiquidityPool>>>,
    app_state: AppState,
    swap_config: SwapConfig,
    time_exceed: u64,
) {
    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
    let logger = Logger::new(format!("[AUTOSELL MONITOR])({}) => ", existing_pools.len()));
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

                    let task = tokio::spawn(async move {
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
                                logger_clone
                                    .log(format!("[Sell Result]: {}\n{:#?}", res[0], sold_pool));
                            }
                            Err(_) => {
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
                                // logger_clone.log(format!(
                                //     "[Sell Result]: Sell issue -> {:?}, will try to sell in the next time.",
                                //     e
                                // ));
                            }
                        };
                    });
                    drop(task);
                }
            }
        }
    }
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
                let mut _skip_flag = false; //_skip_flag?true:false;
                let trade_info = match TradeInfoFromToken::from_json(json.clone()) {
                    Ok(info) => info,
                    Err(e) => {
                        logger.log(format!("Error in parsing txn: {}", e));
                        continue;
                    }
                };

                for log_message in log_messages.iter() {
                    if let Some(log_msg) = log_message.as_str() {
                        // Mint tx log
                        if log_msg.contains(PUMP_LOG_INSTRUCTION) {
                            // Filter the token: initial LP

                            // Add existing liquidity pools
                            let new_pool = LiquidityPool {
                                mint: trade_info.clone().mint,
                                in_amount: 0,
                                out_amount: 0,
                                status: Status::New,
                                timestamp: None,
                            };

                            let mut update_pools = existing_liquidity_pools.lock().unwrap();
                            update_pools.insert(new_pool.clone());
                            // logger.log(format!(
                            //     "[New Pool]({}-{}): Detected at {} ({:?}).",
                            //     signature,
                            //     slot,
                            //     Utc::now(),
                            //     start_time.elapsed()
                            // ));
                            mint_flag = true;
                            // logger.log(format!(
                            //     "[New Pool Detail]: {:?}, {}, {}, {:?}, {:?}",
                            //     new_pool.mint,
                            //     new_pool.in_amount,
                            //     new_pool.out_amount,
                            //     new_pool.status,
                            //     new_pool.timestamp
                            // ));
                            // logger.log(format!(
                            //     "[Existing Pools]: \n {:#?}",
                            //     existing_liquidity_pools
                            // ));
                            break;
                        }
                    }
                }
                if !mint_flag && !_skip_flag {
                    // If this is not mint tx, then check if it is existing in existing_liquidity_pool
                    // Check if mint is existing in existing_liquidity_pool
                    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                    if let Some(existing_pool) = existing_pools
                        .clone()
                        .iter()
                        .find(|pool| pool.mint == trade_info.clone().mint)
                    {
                        if existing_pool.status == Status::New {
                            // if counter > 8 {
                            //     continue;
                            // }
                            // Check Blacklist
                            if blacklist.is_blacklisted(trade_info.clone().target) {
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
                                    trade_info.clone().target
                                ));
                                continue;
                            }

                            // Check Buy Condition
                            let mut buy_amount = 0_u64;
                            let check_pool = if trade_info.sol_pre_amount
                                < trade_info.sol_post_amount
                            {
                                // buy
                                buy_amount = trade_info.sol_post_amount - trade_info.sol_pre_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount + buy_amount as u128,
                                    out_amount: existing_pool.out_amount,
                                    status: existing_pool.clone().status,
                                    timestamp: None,
                                }
                            } else {
                                // sell
                                let sell_amount =
                                    trade_info.sol_pre_amount - trade_info.sol_post_amount;
                                LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    in_amount: existing_pool.in_amount,
                                    out_amount: existing_pool.out_amount + sell_amount as u128,
                                    status: existing_pool.clone().status,
                                    timestamp: None,
                                }
                            };
                            existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
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
                                    trade_info.signature,
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
                                existing_pools
                                    .retain(|pool| pool.mint != existing_pool.clone().mint);
                                existing_pools.insert(buying_pool.clone());
                                // counter += 1;

                                // Buy through the thread
                                let swapx_clone = swapx.clone();
                                let existing_pool_clone = existing_pool.clone();
                                let swap_config_clone = swap_config.clone();
                                let logger_clone = logger.clone();
                                let existing_liquidity_pools_clone =
                                    Arc::clone(&existing_liquidity_pools);
                                let task = tokio::spawn(async move {
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
                                                in_amount: existing_pool_clone.in_amount
                                                    + buy_amount as u128,
                                                out_amount: 0,
                                                status: Status::Bought,
                                                timestamp: Some(Instant::now()),
                                            };
                                            let mut update_pools =
                                                existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools.retain(|pool| {
                                                pool.mint != existing_pool_clone.mint
                                            });
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
                                            let mut update_pools =
                                                existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools.retain(|pool| {
                                                pool.mint != existing_pool_clone.mint
                                            });
                                            update_pools.insert(failed_pool.clone());
                                            logger_clone
                                                .log(format!("[Buy Result]: Buy issue -> {:?}", e));
                                            // continue;
                                        }
                                    };
                                });
                                drop(task);
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
                            let check_pool =
                                if trade_info.sol_pre_amount < trade_info.sol_post_amount {
                                    // buy
                                    let buy_amount =
                                        trade_info.sol_post_amount - trade_info.sol_pre_amount;
                                    LiquidityPool {
                                        mint: existing_pool.clone().mint,
                                        in_amount: existing_pool.in_amount + buy_amount as u128,
                                        out_amount: existing_pool.out_amount,
                                        status: existing_pool.clone().status,
                                        timestamp: existing_pool.timestamp,
                                    }
                                } else {
                                    // sell
                                    sell_amount =
                                        trade_info.sol_pre_amount - trade_info.sol_post_amount;
                                    LiquidityPool {
                                        mint: existing_pool.clone().mint,
                                        in_amount: existing_pool.in_amount,
                                        out_amount: existing_pool.out_amount + sell_amount as u128,
                                        status: existing_pool.clone().status,
                                        timestamp: existing_pool.timestamp,
                                    }
                                };
                            existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
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
                                    trade_info.signature,
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
                                existing_pools
                                    .retain(|pool| pool.mint != existing_pool.clone().mint);
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
                                let existing_liquidity_pools_clone =
                                    Arc::clone(&existing_liquidity_pools);
                                let task = tokio::spawn(async move {
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

                                            let mut update_pools =
                                                existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools.retain(|pool| {
                                                pool.mint != existing_pool_clone.clone().mint
                                            });
                                            update_pools.insert(sold_pool.clone());
                                            logger_clone.log(format!(
                                                "[Sell Result]: {}\n{:#?}",
                                                res[0], sold_pool
                                            ));
                                        }
                                        Err(_) => {
                                            // Update Status::Selling with Status::Bought
                                            let bought_pool = LiquidityPool {
                                                mint: existing_pool_clone.clone().mint,
                                                in_amount: existing_pool_clone.in_amount,
                                                out_amount: existing_pool_clone.out_amount,
                                                status: Status::Bought,
                                                timestamp: existing_pool_clone.timestamp,
                                            };

                                            let mut update_pools =
                                                existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools.retain(|pool| {
                                                pool.mint != existing_pool_clone.clone().mint
                                            });
                                            update_pools.insert(bought_pool.clone());
                                            // logger_clone.log(format!(
                                            //     "[Sell Result]: Sell issue -> {:?}, will try to sell in the next time",
                                            //     e
                                            // ));
                                        }
                                    };
                                });
                                drop(task);
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
                } else if mint_flag && !_skip_flag {
                    //TODO: Write the logic once client wants to buy once it detects mint tx
                    // Check Buy Condition
                    // if counter > 8 {
                    //     continue;
                    // }
                    if trade_info.sol_post_amount < trade_info.sol_pre_amount {
                        continue;
                    }
                    let buy_amount = trade_info.sol_post_amount - trade_info.sol_pre_amount;
                    if buy_amount > buy_thredshold {
                        // Now Buy!
                        logger.log(format!(
                            "[Buy Pool]({}): \n Reached at buying condition, Buying at {} ({:?}).",
                            trade_info.signature,
                            Utc::now(),
                            start_time.elapsed(),
                        ));

                        // Update status into ING status..
                        let buying_pool = LiquidityPool {
                            mint: trade_info.clone().mint,
                            in_amount: buy_amount as u128,
                            out_amount: 0_u128,
                            status: Status::Buying,
                            timestamp: Some(Instant::now()),
                        };
                        let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                        existing_pools.retain(|pool| pool.mint != trade_info.clone().mint);
                        existing_pools.insert(buying_pool.clone());

                        // counter += 1;
                        // Buy through the thread
                        let swapx_clone = swapx.clone();
                        let mint_clone = trade_info.clone().mint;
                        let swap_config_clone = swap_config.clone();
                        let logger_clone = logger.clone();
                        let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
                        let task = tokio::spawn(async move {
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

                                    let mut update_pools =
                                        existing_liquidity_pools_clone.lock().unwrap();
                                    update_pools.retain(|pool| pool.mint != mint_clone.clone());
                                    update_pools.insert(bought_pool.clone());
                                    logger_clone.log(format!(
                                        "[Buy Result]:{} \n {:#?}",
                                        res[0], bought_pool
                                    ));
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

                                    let mut update_pools =
                                        existing_liquidity_pools_clone.lock().unwrap();
                                    update_pools.retain(|pool| pool.mint != mint_clone.clone());
                                    update_pools.insert(failed_pool.clone());

                                    logger_clone.log(format!("[Buy Result]: Buy issue -> {:?}", e));
                                }
                            };
                        });
                        drop(task);
                    } else {
                        // logger.log(format!(
                        //     "[Initial Pool]({}): \n Not reached at buying condition, Updating at {} ({:?}).",
                        //     signature.clone(),
                        //     Utc::now(),
                        //     start_time.elapsed(),
                        // ));
                        let initial_pool = LiquidityPool {
                            mint: trade_info.clone().mint,
                            in_amount: buy_amount as u128,
                            out_amount: 0_u128,
                            status: Status::New,
                            timestamp: Some(Instant::now()),
                        };
                        let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                        existing_pools.retain(|pool| pool.mint != trade_info.clone().mint.clone());
                        existing_pools.insert(initial_pool.clone());
                        // logger.log(format!("[Initial Result]: \n {:#?}", initial_pool));
                    }
                }
            }
        }
    }
}
