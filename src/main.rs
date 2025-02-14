use dotenv::dotenv;
use raypump_executioner_bot::{
    common::{
        blacklist::Blacklist,
        logger::Logger,
        utils::{
            create_coingecko_proxy, create_nonblocking_rpc_client, create_rpc_client,
            import_env_var, import_wallet, AppState, SwapConfig,
        },
    },
    engine::{
        monitor::{new_token_trader_pumpfun, new_token_trader_raydium, test_buy, test_sell},
        swap::{SwapDirection, SwapInType},
    },
};
use solana_sdk::signer::Signer;

#[tokio::main]
async fn main() {
    /* Initial Settings */
    let init_msg = "                                                                       
    @@@  @@@  @@@  @@@  @@@@@@@  @@@   @@@@@@   @@@                           
    @@@  @@@@ @@@  @@@  @@@@@@@  @@@  @@@@@@@@  @@@                           
    @@!  @@!@!@@@  @@!    @@!    @@!  @@!  @@@  @@!                           
    !@!  !@!!@!@!  !@!    !@!    !@!  !@!  @!@  !@!                           
    !!@  @!@ !!@!  !!@    @!!    !!@  @!@!@!@!  @!!                           
    !!!  !@!  !!!  !!!    !!!    !!!  !!!@!!!!  !!!                           
    !!:  !!:  !!!  !!:    !!:    !!:  !!:  !!!  !!:                           
    :!:  :!:  !:!  :!:    :!:    :!:  :!:  !:!   :!:                          
    ::   ::   ::   ::     ::     ::  ::   :::   :: ::::                      
    :    ::    :   :       :     :     :   : :  : :: : :                      
                                                                            
                                                                            
    @@@@@@   @@@@@@@@  @@@@@@@  @@@@@@@  @@@  @@@  @@@   @@@@@@@@   @@@@@@   
    @@@@@@@   @@@@@@@@  @@@@@@@  @@@@@@@  @@@  @@@@ @@@  @@@@@@@@@  @@@@@@@   
    !@@       @@!         @@!      @@!    @@!  @@!@!@@@  !@@        !@@       
    !@!       !@!         !@!      !@!    !@!  !@!!@!@!  !@!        !@!       
    !!@@!!    @!!!:!      @!!      @!!    !!@  @!@ !!@!  !@! @!@!@  !!@@!!    
    !!@!!!   !!!!!:      !!!      !!!    !!!  !@!  !!!  !!! !!@!!   !!@!!!   
        !:!  !!:         !!:      !!:    !!:  !!:  !!!  :!!   !!:       !:!  
        !:!   :!:         :!:      :!:    :!:  :!:  !:!  :!:   !::      !:!   
    :::: ::    :: ::::     ::       ::     ::   ::   ::   ::: ::::  :::: ::   
    :: : :    : :: ::      :        :     :    ::    :    :: :: :   :: : :                                                             
    ";
    println!("{}", init_msg);

    dotenv().ok();
    let logger = Logger::new("[INIT] => ".to_string());
    let rpc_wss = import_env_var("RPC_WSS");
    let slippage = import_env_var("SLIPPAGE").parse::<u64>().unwrap_or(5);
    let rpc_client = create_rpc_client().unwrap();
    let solana_price = create_coingecko_proxy().await.unwrap_or(200_f64);
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

    let state = AppState {
        rpc_client,
        rpc_nonblocking_client,
        wallet,
    };

    let blacklist = match Blacklist::new("blacklist.txt") {
        Ok(blacklist) => blacklist,
        Err(_) => Blacklist::empty(),
    };
    let time_exceed: u64 = import_env_var("TIME_EXCEED")
        .parse()
        .expect("Failed to parse string into u64");

    logger.log(format!(
        "Sniper Environment: \n\t\t\t\t [Web Socket RPC]: {},
        \n\t\t\t\t [Wallet]: {:?}, [Balance]: {} Sol, 
        \n\t\t\t\t [Slippage]: {}, [Solana]: {},
        \n\t\t\t\t [Time Exceed]: {}, [Amount]: {},
        \n\t\t\t\t [Blacklist]: {} \n",
        rpc_wss,
        wallet_cloned.pubkey(),
        balance as f64 / 1_000_000_000_f64,
        slippage,
        solana_price,
        time_exceed,
        amount_in,
        blacklist.clone().length(),
    ));

    /* Running Bot */
    let run_msg = "
        @@@@@@@   @@@  @@@  @@@  @@@                                                           
        @@@@@@@@  @@@  @@@  @@@@ @@@                                                           
        @@!  @@@  @@!  @@@  @@!@!@@@                                                           
        !@!  @!@  !@!  @!@  !@!!@!@!                                                           
        @!@!!@!   @!@  !@!  @!@ !!@!                                                           
        !!@!@!    !@!  !!!  !@!  !!!                                                           
        !!: :!!   !!:  !!!  !!:  !!!                                                           
        :!:  !:!  :!:  !:!  :!:  !:!                                                           
        ::   :::  ::::: ::   ::   ::                                                           
        :   : :   : :  :   ::    :                                                            
                                                                                            
                                                                                            
        @@@@@@   @@@  @@@  @@@  @@@@@@@   @@@@@@@@  @@@@@@@      @@@@@@@    @@@@@@   @@@@@@@  
        @@@@@@@   @@@@ @@@  @@@  @@@@@@@@  @@@@@@@@  @@@@@@@@     @@@@@@@@  @@@@@@@@  @@@@@@@  
        !@@       @@!@!@@@  @@!  @@!  @@@  @@!       @@!  @@@     @@!  @@@  @@!  @@@    @@!    
        !@!       !@!!@!@!  !@!  !@!  @!@  !@!       !@!  @!@     !@   @!@  !@!  @!@    !@!    
        !!@@!!    @!@ !!@!  !!@  @!@@!@!   @!!!:!    @!@!!@!      @!@!@!@   @!@  !@!    @!!    
        !!@!!!   !@!  !!!  !!!  !!@!!!    !!!!!:    !!@!@!       !!!@!!!!  !@!  !!!    !!!    
            !:!  !!:  !!!  !!:  !!:       !!:       !!: :!!      !!:  !!!  !!:  !!!    !!:    
            !:!   :!:  !:!  :!:  :!:       :!:       :!:  !:!     :!:  !:!  :!:  !:!    :!:    
        :::: ::    ::   ::   ::   ::        :: ::::  ::   :::      :: ::::  ::::: ::     ::    
        :: : :    ::    :   :     :        : :: ::    :   : :     :: : ::    : :  :      :     
    ";
    println!("{}", run_msg);

    // new_token_trader_raydium(&rpc_wss, state, swap_config, solana_price).await;
    new_token_trader_pumpfun(
        &rpc_wss,
        state,
        swap_config,
        blacklist,
        time_exceed,
        solana_price,
    )
    .await;
    // let mint = "HiSj5QPzJb7pt9PoxS8ftF6pEFikZAatHJEkLBr8pump";
    // 2KmzaM31TABRepdCA74UkVKP4F37dahfe9JzummWCGHt
    // test_buy(state, swap_config, mint).await;
    // test_sell(state, swap_config, mint).await;
}
