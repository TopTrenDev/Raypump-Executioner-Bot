use std::sync::{Arc, Mutex};
use std::{collections::HashSet, time::Duration};

use crate::common::blacklist::Blacklist;
use crate::common::{
    config::{AppState, LiquidityPool, Status, SwapConfig, PUMP_LOG_INSTRUCTION, SUBSCRIPTION_MSG},
    logger::Logger,
};
use crate::core::tx;
use crate::dex::pump_fun::Pump;
use anyhow::Result;
use colored::Colorize;
use futures_util::SinkExt;
use serde_json::Value;
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
    pub volume_change: i64,
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
        volume_change: i64,
    ) -> Self {
        Self {
            slot,
            signature,
            target,
            mint,
            bonding_curve,
            bonding_curve_index,
            volume_change,
        }
    }

    pub fn from_json(json: Value) -> Result<Self> {
        let slot = json["params"]["result"]["slot"].as_u64().unwrap();
        let signature = json["params"]["result"]["signature"].clone().to_string();
        let mut target = String::new();
        let mut mint = String::new();
        let mut bonding_curve = String::new();

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

        let bonding_curve_index = account_keys
            .iter()
            .position(|account_key| account_key["pubkey"].as_str().unwrap() == bonding_curve)
            .unwrap_or(0);

        let sol_post_amount = json["params"]["result"]["transaction"]["meta"]["postBalances"]
            .as_array()
            .and_then(|post_balances| post_balances.get(bonding_curve_index))
            .and_then(|post_balance| post_balance.as_u64())
            .unwrap_or(0_u64);

        let sol_pre_amount = json["params"]["result"]["transaction"]["meta"]["preBalances"]
            .as_array()
            .and_then(|pre_balances| pre_balances.get(bonding_curve_index))
            .and_then(|pre_balance| pre_balance.as_u64())
            .unwrap_or(0_u64);

        let volume_change = (sol_post_amount - sol_pre_amount) as i64;

        Ok(Self {
            slot,
            signature,
            target,
            mint,
            bonding_curve,
            bonding_curve_index,
            volume_change,
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
    let logger = Logger::new(
        format!("[AUTO-SELL])({}) => ", existing_pools.len())
            .yellow()
            .to_string(),
    );
    for existing_pool in existing_pools.clone().iter() {
        let timeout = Duration::from_secs(time_exceed);
        let start_time = Instant::now();
        if existing_pool.status == Status::Bought {
            if let Some(timestamp) = existing_pool.timestamp {
                if timestamp.elapsed() > timeout {
                    // Now Auto-Sell
                    // -------------
                    logger.log(
                        format!(
                            "[DETECT-POOL]({}): Reached at selling time, Selling at {} ({:?}).",
                            existing_pool.clone().mint.clone(),
                            Utc::now(),
                            start_time.elapsed()
                        )
                        .yellow()
                        .to_string(),
                    );
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
                        buy_price: existing_pool.buy_price,
                        sell_price: existing_pool.sell_price,
                        status: Status::Selling,
                        timestamp: Some(Instant::now()),
                    };
                    existing_pools.retain(|pool| pool.mint != existing_pool.clone().mint);
                    existing_pools.insert(selling_pool.clone());

                    let swapx_clone = swapx.clone();
                    let logger_clone = logger.clone();
                    let existing_pool_clone = existing_pool.clone();
                    let sell_config_clone = sell_config.clone();
                    let mint_str = existing_pool.clone().mint;
                    let existing_pools_clone = Arc::clone(&existing_liquidity_pools);
                    let task = tokio::spawn(async move {
                        // Build ixn 'n Calc the Cost of token buying
                        // -------------------
                        match swapx_clone
                            .build_swap_ixn_by_mint(&mint_str, sell_config_clone, start_time)
                            .await
                        {
                            Ok(result) => {
                                // Send Instructions and Confirm
                                // -------------------
                                let (client, keypair, instructions, token_price) =
                                    (result.0, result.1, result.2, result.3);
                                match tx::new_signed_and_send(
                                    &client,
                                    &keypair,
                                    instructions,
                                    &logger_clone,
                                )
                                .await
                                {
                                    Ok(res) => {
                                        let sold_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: existing_pool_clone.buy_price,
                                            sell_price: token_price,
                                            status: Status::Sold,
                                            timestamp: Some(Instant::now()),
                                        };
                                        let mut update_pools = existing_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(sold_pool.clone());
                                        logger_clone.log(
                                            format!(
                                                "[SUCCESSFUL-SELL]({}):{} \n {:#?}",
                                                mint_str, res[0], sold_pool
                                            )
                                            .green()
                                            .to_string(),
                                        );
                                    }
                                    Err(e) => {
                                        logger_clone.log(
                                            format!("Skip {}: {}", mint_str.clone(), e)
                                                .red()
                                                .italic()
                                                .to_string(),
                                        );
                                        let bought_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: existing_pool_clone.buy_price,
                                            sell_price: existing_pool_clone.sell_price,
                                            status: Status::Bought,
                                            timestamp: None,
                                        };
                                        let mut update_pools = existing_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(bought_pool.clone());
                                    }
                                };
                            }
                            Err(error) => {
                                logger_clone.log(
                                    format!("Skip {} by {}", mint_str.clone(), error)
                                        .red()
                                        .italic()
                                        .to_string(),
                                );
                                let bought_pool = LiquidityPool {
                                    mint: mint_str.clone(),
                                    buy_price: existing_pool_clone.buy_price,
                                    sell_price: existing_pool_clone.sell_price,
                                    status: Status::Bought,
                                    timestamp: None,
                                };
                                let mut update_pools = existing_pools_clone.lock().unwrap();
                                update_pools.retain(|pool| pool.mint != mint_str);
                                update_pools.insert(bought_pool.clone());
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
    time_exceed: u64,
    take_profit: f64,
    stop_loss: f64,
    _blacklist: Blacklist,
) {
    // INITIAL SETTING FOR SUBSCIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    //
    let (ws_stream, _) = connect_async(rpc_wss)
        .await
        .expect("Failed to connect to WebSocket server");
    let (mut write, mut read) = ws_stream.split();

    write
        .send(SUBSCRIPTION_MSG.to_string().into())
        .await
        .expect("Failed to send subscription message");

    let existing_liquidity_pools = Arc::new(Mutex::new(HashSet::<LiquidityPool>::new()));

    let rpc_nonblocking_client = app_state.clone().rpc_nonblocking_client;
    let rpc_client = app_state.clone().rpc_client;
    let wallet = app_state.clone().wallet;
    let swapx = Pump::new(
        rpc_nonblocking_client.clone(),
        rpc_client.clone(),
        wallet.clone(),
    );

    let logger = Logger::new("[PUMPFUN-MONITOR] => ".blue().bold().to_string());
    logger.log("[STARTED. MONITORING]...".blue().bold().to_string());
    let mut counter = 0;
    // NOW SUBSCRIBE
    // -----------------------------------------------------------------------------------------------------------------------------
    //
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
            // logger.log(
            //     format!("[AUTO-SELL MONITORING]: {:?}", start_time.elapsed())
            //         .yellow()
            //         .to_string(),
            // );

            if let Some(log_messages) =
                json["params"]["result"]["transaction"]["meta"]["logMessages"].as_array()
            {
                let mut mint_flag = false;
                let trade_info = match TradeInfoFromToken::from_json(json.clone()) {
                    Ok(info) => info,
                    Err(e) => {
                        logger.log(
                            format!("Error in parsing txn: {}", e)
                                .red()
                                .italic()
                                .to_string(),
                        );
                        continue;
                    }
                };

                for log_message in log_messages.iter() {
                    if let Some(log_msg) = log_message.as_str() {
                        if log_msg.contains(PUMP_LOG_INSTRUCTION) {
                            //TODO: Add the condition that filters the token.
                            // ---------------

                            // NEW POOL DETECT!
                            // ---------------
                            logger.log(format!(
                                "\n\t * [NEW POOL|BUY] => ({}) - SLOT:({}) \n\t * [DETECT] => ({}) \n\t * [BUYING] => {} :: ({:?}).",
                                trade_info.signature,
                                trade_info.slot,
                                trade_info.mint,
                                Utc::now(),
                                start_time.elapsed(),
                            ).yellow().to_string());
                            mint_flag = true;
                            break;
                        }
                    }
                }
                if mint_flag {
                    // NOW BUY!
                    // Update into ...ing status
                    // ---------------
                    if counter > 50 {
                        continue;
                    };
                    let buying_pool = LiquidityPool {
                        mint: trade_info.clone().mint,
                        buy_price: 0_f64,
                        sell_price: 0_f64,
                        status: Status::Buying,
                        timestamp: None,
                    };
                    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                    existing_pools.insert(buying_pool.clone());

                    // Buy token 'n Calc the Cost of token buying
                    // -------------------
                    let swapx_clone = swapx.clone();
                    let logger_clone = logger.clone();
                    let swap_config_clone = swap_config.clone();
                    let mint_str = trade_info.mint.clone();
                    let existing_liquidity_pools_clone = Arc::clone(&existing_liquidity_pools);
                    logger_clone.log(
                        format!(
                            "[BUILD-IXN]({}) - {} :: {:?}",
                            mint_str.clone(),
                            Utc::now(),
                            start_time.elapsed()
                        )
                        .yellow()
                        .to_string(),
                    );

                    counter += 1;
                    let task = tokio::spawn(async move {
                        // Build ixn 'n Calc the Cost of token buying
                        // -------------------
                        match swapx_clone
                            .build_swap_ixn_by_mint(&mint_str, swap_config_clone, start_time)
                            .await
                        {
                            Ok(result) => {
                                // Send Instructions and Confirm
                                // -------------------
                                let (client, keypair, instructions, token_price) =
                                    (result.0, result.1, result.2, result.3);
                                logger_clone.log(
                                    format!(
                                        "[CONFIRM-TXN]({}) - {} :: {:?}",
                                        mint_str.clone(),
                                        Utc::now(),
                                        start_time.elapsed()
                                    )
                                    .yellow()
                                    .to_string(),
                                );

                                match tx::new_signed_and_send(
                                    &client,
                                    &keypair,
                                    instructions,
                                    &logger_clone,
                                )
                                .await
                                {
                                    Ok(res) => {
                                        let bought_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: token_price,
                                            sell_price: 0_f64,
                                            status: Status::Bought,
                                            timestamp: Some(Instant::now()),
                                        };
                                        let mut existing_pools =
                                            existing_liquidity_pools_clone.lock().unwrap();
                                        existing_pools.retain(|pool| pool.mint != mint_str);
                                        existing_pools.insert(bought_pool.clone());
                                        logger_clone.log(format!(
                                            "\n\t * [SUCCESSFUL-BUY] => TX_HASH: ({:?}) \n\t * [POOL] => ({}) \n\t * [BOUGHT] => {} :: ({:?}).",
                                            res[0], mint_str, Utc::now(), start_time.elapsed()
                                        ).green().to_string());
                                    }
                                    Err(e) => {
                                        logger_clone.log(
                                            format!("Skip {}: {}", mint_str.clone(), e)
                                                .red()
                                                .italic()
                                                .to_string(),
                                        );
                                        let failed_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: 0_f64,
                                            sell_price: 0_f64,
                                            status: Status::Failure,
                                            timestamp: None,
                                        };
                                        let mut update_pools =
                                            existing_liquidity_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(failed_pool.clone());
                                    }
                                };
                            }
                            Err(error) => {
                                logger_clone.log(
                                    format!("Skip {} by {}", mint_str.clone(), error)
                                        .red()
                                        .italic()
                                        .to_string(),
                                );
                                let failed_pool = LiquidityPool {
                                    mint: mint_str.clone(),
                                    buy_price: 0_f64,
                                    sell_price: 0_f64,
                                    status: Status::Failure,
                                    timestamp: None,
                                };
                                let mut update_pools =
                                    existing_liquidity_pools_clone.lock().unwrap();
                                update_pools.retain(|pool| pool.mint != mint_str);
                                update_pools.insert(failed_pool.clone());
                            }
                        };
                    });
                    drop(task);
                } else {
                    // CHECK IF `trade_info.mint` EXISTS IN 'existing_liquidity_pool'.
                    // --------------------------------------------------
                    let mut existing_pools = existing_liquidity_pools.lock().unwrap();
                    if let Some(existing_pool) = existing_pools
                        .clone()
                        .iter()
                        .find(|pool| pool.mint == trade_info.clone().mint)
                    {
                        if existing_pool.status == Status::Bought {
                            // Sync `volume` 'n `txn_num` | Update `timestamp`
                            // Check `volume_change`
                            // --------------------
                            // logger.log(format!(
                            //     "[VOLUME-CHECK]({}) => {}",
                            //     trade_info.mint, trade_info.volume_change
                            // ));
                            // Update into ...ing status
                            // --------------------
                            if trade_info.volume_change <= 0 {
                                let check_pool = LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    buy_price: existing_pool.buy_price,
                                    sell_price: 0_f64,
                                    status: Status::Checking,
                                    timestamp: existing_pool.timestamp,
                                };
                                existing_pools
                                    .retain(|pool| pool.mint != existing_pool.clone().mint);
                                existing_pools.insert(check_pool.clone());
                            } else {
                                let check_pool = LiquidityPool {
                                    mint: existing_pool.clone().mint,
                                    buy_price: existing_pool.buy_price,
                                    sell_price: 0_f64,
                                    status: Status::Checking,
                                    timestamp: Some(Instant::now()),
                                };
                                existing_pools
                                    .retain(|pool| pool.mint != existing_pool.clone().mint);
                                existing_pools.insert(check_pool.clone());
                            }

                            // CHECK|SELL TOKEN: BY PT/SL
                            // -------------------
                            let swapx_clone = swapx.clone();
                            let logger_clone = logger.clone();
                            let swap_config_clone = swap_config.clone();
                            let mint_str = existing_pool.clone().mint;
                            let existing_pool_clone = existing_pool.clone();
                            let existing_liquidity_pools_clone =
                                Arc::clone(&existing_liquidity_pools);
                            let task = tokio::spawn(async move {
                                // Build Ixn 'n Calc the Cost of token buying
                                // -------------------
                                match swapx_clone
                                    .build_swap_ixn_by_mint(
                                        &mint_str,
                                        swap_config_clone,
                                        start_time,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        // Check TP/SL
                                        // -------------------
                                        let (client, keypair, instructions, token_price) =
                                            (result.0, result.1, result.2, result.3);
                                        let profit_rate =
                                            token_price / existing_pool_clone.buy_price;
                                        logger_clone.log(format!(
                                            "[TP/SL-CHECK]:{} => {} :: {}",
                                            profit_rate, token_price, existing_pool_clone.buy_price
                                        ));
                                        if profit_rate >= take_profit || profit_rate <= stop_loss {
                                            // Send Instructions and Confirm
                                            // -------------------
                                            match tx::new_signed_and_send(
                                                &client,
                                                &keypair,
                                                instructions,
                                                &logger_clone,
                                            )
                                            .await
                                            {
                                                Ok(res) => {
                                                    let sold_pool = LiquidityPool {
                                                        mint: mint_str.clone(),
                                                        buy_price: existing_pool_clone.buy_price,
                                                        sell_price: token_price,
                                                        status: Status::Sold,
                                                        timestamp: Some(Instant::now()),
                                                    };
                                                    let mut update_pools =
                                                        existing_liquidity_pools_clone
                                                            .lock()
                                                            .unwrap();
                                                    update_pools
                                                        .retain(|pool| pool.mint != mint_str);
                                                    update_pools.insert(sold_pool.clone());
                                                    logger_clone.log(
                                                        format!(
                                                            "[SUCCESSFUL-SELL]({}):{} \n {:#?}",
                                                            mint_str, res[0], sold_pool
                                                        )
                                                        .green()
                                                        .to_string(),
                                                    );
                                                }
                                                Err(e) => {
                                                    logger_clone.log(
                                                        format!("Skip {}: {}", mint_str.clone(), e)
                                                            .red()
                                                            .italic()
                                                            .to_string(),
                                                    );
                                                    let bought_pool = LiquidityPool {
                                                        mint: mint_str.clone(),
                                                        buy_price: existing_pool_clone.buy_price,
                                                        sell_price: existing_pool_clone.sell_price,
                                                        status: Status::Bought,
                                                        timestamp: existing_pool_clone.timestamp,
                                                    };
                                                    let mut update_pools =
                                                        existing_liquidity_pools_clone
                                                            .lock()
                                                            .unwrap();
                                                    update_pools
                                                        .retain(|pool| pool.mint != mint_str);
                                                    update_pools.insert(bought_pool.clone());
                                                }
                                            };
                                        } else {
                                            let bought_pool = LiquidityPool {
                                                mint: mint_str.clone(),
                                                buy_price: existing_pool_clone.buy_price,
                                                sell_price: existing_pool_clone.sell_price,
                                                status: Status::Bought,
                                                timestamp: existing_pool_clone.timestamp,
                                            };
                                            let mut update_pools =
                                                existing_liquidity_pools_clone.lock().unwrap();
                                            update_pools.retain(|pool| pool.mint != mint_str);
                                            update_pools.insert(bought_pool.clone());
                                        }
                                    }
                                    Err(_) => {
                                        // Skip checking TP/SL
                                        // -------------------
                                        let bought_pool = LiquidityPool {
                                            mint: mint_str.clone(),
                                            buy_price: existing_pool_clone.buy_price,
                                            sell_price: existing_pool_clone.sell_price,
                                            status: Status::Bought,
                                            timestamp: existing_pool_clone.timestamp,
                                        };
                                        let mut update_pools =
                                            existing_liquidity_pools_clone.lock().unwrap();
                                        update_pools.retain(|pool| pool.mint != mint_str);
                                        update_pools.insert(bought_pool.clone());
                                    }
                                };
                            });
                            drop(task);
                        } else {
                            // Already Sold or Failed Pool, Skipping at {existing_pool.mint}.
                            // ---------------------------------------
                        }
                    }
                }
            }
        }
    }
}
