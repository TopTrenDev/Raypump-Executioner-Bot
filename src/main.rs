use raypump_executioner_bot::{
    common::{config::Config, constants::RUN_MSG},
    engine::monitor::new_token_trader_pumpfun,
};

#[tokio::main]
async fn main() {
    /* Initial Settings */
    let config = Config::new().await;

    /* Running Bot */
    let run_msg = RUN_MSG;
    println!("{}", run_msg);

    new_token_trader_pumpfun(
        &config.rpc_wss,
        config.app_state,
        config.swap_config,
        config.time_exceed,
        config.take_profit,
        config.stop_loss,
        config.blacklist,
    )
    .await;
}
