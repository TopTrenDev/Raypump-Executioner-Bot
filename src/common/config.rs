use dotenv::dotenv;
use solana_sdk::signer::Signer;

use crate::common::constants::INIT_MSG;
use crate::common::logger::Logger;
use crate::common::utils::{
    create_coingecko_proxy, create_nonblocking_rpc_client, create_rpc_client, import_env_var,
    import_wallet, AppState, SwapConfig,
};
use crate::engine::swap::{SwapDirection, SwapInType};

pub struct Config {
    pub rpc_wss: String,
    pub app_state: AppState,
    pub swap_config: SwapConfig,
    pub time_exceed: u64,
    pub solana_price: f64,
}

impl Config {
    pub async fn new() -> Self {
        let init_msg = INIT_MSG;
        println!("{}", init_msg);

        dotenv().ok(); // Load .env file

        let logger = Logger::new("[INIT] => ".to_string());

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

        logger.log(format!(
            "Sniper Environment: \n\t\t\t\t [Web Socket RPC]: {},
            \n\t\t\t\t [Wallet]: {:?}, [Balance]: {} Sol, 
            \n\t\t\t\t [Slippage]: {}, [Solana]: {},
            \n\t\t\t\t [Time Exceed]: {}, [Amount]: {}\n",
            rpc_wss,
            wallet_cloned.pubkey(),
            balance as f64 / 1_000_000_000_f64,
            slippage,
            solana_price,
            time_exceed,
            amount_in,
        ));

        Config {
            rpc_wss,
            app_state,
            swap_config,
            time_exceed,
            solana_price,
        }
    }
}
