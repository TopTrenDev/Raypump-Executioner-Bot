use crate::{
    common::{logger::Logger, utils::SwapConfig},
    core::{
        token::{get_account_info, get_associated_token_address, get_mint_info},
        tx,
    },
    engine::swap::{SwapDirection, SwapInType},
};
use amm_cli::AmmSwapInfoResult;
use anyhow::{anyhow, Context, Result};
use bincode::Options;
use bytemuck;
use raydium_amm::state::{AmmInfo, Loadable};
use serde::Deserialize;
use serde::Serialize;
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_sdk::{
    instruction::Instruction, message::VersionedMessage, program_pack::Pack, pubkey::Pubkey,
    signature::Keypair, signer::Signer, system_instruction, transaction::VersionedTransaction,
};
use spl_associated_token_account::instruction::create_associated_token_account;
use spl_token::{amount_to_ui_amount, state::Account, ui_amount_to_amount};
use spl_token_client::token::TokenError;
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::time::Instant;

pub const AMM_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_AUTHORITY_V4: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";

#[derive(Serialize)]
struct SwapRequest {
    quoteResponse: serde_json::Value, // You may deserialize it into a specific struct if known
    userPublicKey: String,
    wrapAndUnwrapSol: bool,
    dynamicComputeUnitLimit: bool,
    prioritizationFeeLamports: u64,
}

#[derive(Debug, Deserialize)]
pub struct PoolInfo {
    pub success: bool,
    pub data: PoolData,
}

#[derive(Debug, Deserialize)]
pub struct PoolData {
    // pub count: u32,
    pub data: Vec<Pool>,
}

impl PoolData {
    pub fn get_pool(&self) -> Option<Pool> {
        self.data.first().cloned()
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Pool {
    pub id: String,
    #[serde(rename = "programId")]
    pub program_id: String,
    #[serde(rename = "mintA")]
    pub mint_a: Mint,
    #[serde(rename = "mintB")]
    pub mint_b: Mint,
    #[serde(rename = "marketId")]
    pub market_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Mint {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
}

pub struct Raydium {
    pub rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    pub rpc_client: Option<Arc<solana_client::rpc_client::RpcClient>>,
    pub keypair: Arc<Keypair>,
    pub pool_id: Option<String>,
}

impl Raydium {
    pub fn new(
        rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        rpc_client: Arc<solana_client::rpc_client::RpcClient>,
        keypair: Arc<Keypair>,
    ) -> Self {
        Self {
            rpc_nonblocking_client,
            keypair,
            rpc_client: Some(rpc_client),
            pool_id: None,
        }
    }

    pub async fn swap_jupiter(
        &self,
        swap_config: SwapConfig,
        mint: String,
        start_time: Instant,
    ) -> Result<Vec<String>> {
        let logger = Logger::new(format!(
            "[SWAP IN JUPITER]({}:{:?}) => ",
            chrono::Utc::now().timestamp(),
            start_time.elapsed()
        ));

        let mut url = "".to_string();
        let input_mint = "So11111111111111111111111111111111111111112";
        let base_mint = &mint;
        let wallet = self.keypair.as_ref();
        if swap_config.swap_direction == SwapDirection::Buy {
            let buy_amount = swap_config.amount_in * 1_000_000_000_f64;
            url = format!(
                "https://ultra-api.jup.ag/order?inputMint={}&outputMint={}&amount={}&slippageBps=10000",
                input_mint,
                base_mint,
                buy_amount
            );
        } else {
            let mint_pubkey = Pubkey::from_str(base_mint).unwrap();
            let token_account = spl_associated_token_account::get_associated_token_address(
                &wallet.pubkey(),
                &mint_pubkey,
            );
            if let Ok(token_account_info) = get_account_info(
                self.rpc_nonblocking_client.clone(),
                &mint_pubkey,
                &token_account,
                &logger,
            )
            .await
            {
                let in_mint = get_mint_info(
                    self.rpc_nonblocking_client.clone(),
                    self.keypair.clone(),
                    &mint_pubkey,
                )
                .await?;
                let amount = match swap_config.in_type {
                    SwapInType::Qty => {
                        ui_amount_to_amount(swap_config.amount_in, in_mint.base.decimals)
                    }
                    SwapInType::Pct => {
                        let amount_in_pct = swap_config.amount_in.min(1.0);
                        if amount_in_pct == 1.0 {
                            logger.log(format!(
                                "Sell all. will be close ATA for mint {}",
                                mint_pubkey
                            ));
                            token_account_info.base.amount
                        } else {
                            (((amount_in_pct * 100.0) as u64) * token_account_info.base.amount)
                                / 100
                        }
                    }
                };
                url = format!(
                    "https://ultra-api.jup.ag/order?inputMint={}&outputMint={}&amount={}&slippageBps=10000",
                    base_mint,
                    input_mint,
                    amount
                );
            };
        }
        logger.log(format!("Requesting the URL: {}", url));
        let client = reqwest::Client::new();
        // Send the GET request
        if let Ok(response) = reqwest::get(&url).await {
            if response.status().is_success() {
                logger.log("Response status is success!".to_string());
                let json: serde_json::Value = response.json().await.unwrap_or_default();
                let swap_request = SwapRequest {
                    quoteResponse: json,
                    userPublicKey: wallet.pubkey().to_string(),
                    wrapAndUnwrapSol: true,
                    dynamicComputeUnitLimit: true,
                    prioritizationFeeLamports: 52000,
                };
                if let Ok(response) = client
                    .post("https://quote-api.jup.ag/v6/swap")
                    .header("Content-Type", "application/json")
                    .json(&swap_request)
                    .send()
                    .await
                {
                    let res: serde_json::Value = response.json().await.unwrap();
                    let tx = base64::decode(
                        res["swapTransaction"]
                            .as_str()
                            .unwrap_or("default")
                            .to_string(),
                    )
                    .unwrap_or_default();
                    if let Ok(transaction) = bincode::options()
                        .with_fixint_encoding()
                        .reject_trailing_bytes()
                        .deserialize::<VersionedTransaction>(&tx)
                    {
                        let signed_tx =
                            VersionedTransaction::try_new(transaction.message.clone(), &[wallet])
                                .unwrap_or_default();
                        let recent_blockhash =
                            VersionedMessage::recent_blockhash(&transaction.message);
                        let txn = tx::jito_confirm(
                            &self.rpc_client.clone().unwrap(),
                            wallet,
                            signed_tx,
                            recent_blockhash,
                            &logger,
                        )
                        .await;
                        return txn;
                    };
                }
            }
        }
        Err(anyhow::format_err!("Not tradable token."))
    }

    pub async fn swap_by_mint(
        &self,
        mint_str: &str,
        swap_config: SwapConfig,
        start_time: Instant,
    ) -> Result<Vec<String>> {
        let logger = Logger::new(format!(
            "[SWAP IN RAYDIUM BY MINT]({}:{:?}) => ",
            chrono::Utc::now().timestamp(),
            start_time.elapsed()
        ));
        let slippage_bps = swap_config.slippage * 100;
        let owner = self.keypair.pubkey();
        let program_id = spl_token::ID;
        let native_mint = spl_token::native_mint::ID;
        let (amm_pool_id, pool_state) = get_pool_state(
            self.rpc_client.clone().unwrap(),
            self.pool_id.as_deref(),
            Some(mint_str),
            &logger,
        )
        .await?;
        let mint = Pubkey::from_str(mint_str)
            .map_err(|e| logger.log(format!("Failed to parse mint pubkey: {}", e)))
            .unwrap();

        let (token_in, token_out, user_input_token, swap_base_in) = match (
            swap_config.swap_direction.clone(),
            pool_state.coin_vault_mint == native_mint,
        ) {
            (SwapDirection::Buy, true) => (native_mint, mint, pool_state.coin_vault, true),
            (SwapDirection::Buy, false) => (native_mint, mint, pool_state.pc_vault, true),
            (SwapDirection::Sell, true) => (mint, native_mint, pool_state.pc_vault, true),
            (SwapDirection::Sell, false) => (mint, native_mint, pool_state.coin_vault, true),
        };

        // logger.log(format!(
        //     "({}:{:?}): token_in:{}, token_out:{}, user_input_token:{}, swap_base_in:{}",
        //     chrono::Utc::now().timestamp(),
        //     start_time.elapsed(),
        //     token_in,
        //     token_out,
        //     user_input_token,
        //     swap_base_in
        // ));

        let in_ata = get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_in,
            &owner,
        );
        let out_ata = get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_out,
            &owner,
        );

        let mut create_instruction = None;
        let mut close_instruction = None;

        let (amount_specified, _amount_ui_pretty) = match swap_config.swap_direction {
            SwapDirection::Buy => {
                // Create base ATA if it doesn't exist.
                match get_account_info(
                    self.rpc_nonblocking_client.clone(),
                    &token_out,
                    &out_ata,
                    &logger,
                )
                .await
                {
                    Ok(_) => {
                        // logger.log("Base ata exists. skipping creation..".to_string());
                    }
                    Err(TokenError::AccountNotFound) | Err(TokenError::AccountInvalidOwner) => {
                        // logger.log(format!(
                        //     "({}:{:?}): Base ATA for mint {} does not exist. will be create",
                        //     chrono::Utc::now().timestamp(),
                        //     start_time.elapsed(),
                        //     token_out
                        // ));
                        create_instruction = Some(create_associated_token_account(
                            &owner,
                            &owner,
                            &token_out,
                            &program_id,
                        ));
                    }
                    Err(_) => {
                        // logger.log(format!("Error retrieving out ATA: {}", error));
                    }
                }

                (
                    ui_amount_to_amount(swap_config.amount_in, spl_token::native_mint::DECIMALS),
                    (swap_config.amount_in, spl_token::native_mint::DECIMALS),
                )
            }
            SwapDirection::Sell => {
                let in_account = get_account_info(
                    self.rpc_nonblocking_client.clone(),
                    &token_in,
                    &in_ata,
                    &logger,
                )
                .await?;
                let in_mint = get_mint_info(
                    self.rpc_nonblocking_client.clone(),
                    self.keypair.clone(),
                    &token_in,
                )
                .await?;

                let amount = match swap_config.in_type {
                    SwapInType::Qty => {
                        ui_amount_to_amount(swap_config.amount_in, in_mint.base.decimals)
                    }
                    SwapInType::Pct => {
                        let amount_in_pct = swap_config.amount_in.min(1.0);
                        if amount_in_pct == 1.0 {
                            // logger
                            //     .log(format!("Sell all. will be close ATA for mint {}", token_in));
                            close_instruction = Some(spl_token::instruction::close_account(
                                &program_id,
                                &in_ata,
                                &owner,
                                &owner,
                                &[&owner],
                            )?);
                            in_account.base.amount
                        } else {
                            (((amount_in_pct * 100.0) as u64) * in_account.base.amount) / 100
                        }
                    }
                };
                (
                    amount,
                    (
                        amount_to_ui_amount(amount, in_mint.base.decimals),
                        in_mint.base.decimals,
                    ),
                )
            }
        };

        let amm_program = Pubkey::from_str(AMM_PROGRAM)?;
        let client = self.rpc_client.clone().unwrap();
        let swap_info_result = amm_cli::calculate_swap_info(
            &client,
            amm_program,
            amm_pool_id,
            user_input_token,
            amount_specified,
            slippage_bps,
            swap_base_in,
        )?;
        let other_amount_threshold = swap_info_result.other_amount_threshold;

        // logger.log(format!(
        //     "[swap_info_result]({}): {:#?}",
        //     chrono::Utc::now().timestamp(),
        //     swap_info_result
        // ));
        // logger.log(format!(
        //     "swap({}:{:?}): {}, value: {:?} -> {}",
        //     chrono::Utc::now().timestamp(),
        //     start_time.elapsed(),
        //     token_in,
        //     _amount_ui_pretty,
        //     token_out
        // ));

        // build instructions
        let mut instructions = vec![];
        // sol <-> wsol support
        let mut wsol_account = None;
        if token_in == native_mint || token_out == native_mint {
            // create wsol account
            let seed = &format!("{}", Keypair::new().pubkey())[..32];
            let wsol_pubkey = Pubkey::create_with_seed(&owner, seed, &spl_token::id())?;
            wsol_account = Some(wsol_pubkey);

            // LAMPORTS_PER_SOL / 100 // 0.01 SOL as rent
            // get rent
            let rent = self
                .rpc_nonblocking_client
                .get_minimum_balance_for_rent_exemption(Account::LEN)
                .await?;
            // if buy add amount_specified
            let total_amount = if token_in == native_mint {
                rent + amount_specified
            } else {
                rent
            };
            // create tmp wsol account
            instructions.push(system_instruction::create_account_with_seed(
                &owner,
                &wsol_pubkey,
                &owner,
                seed,
                total_amount,
                Account::LEN as u64, // 165, // Token account size
                &spl_token::id(),
            ));

            // initialize account
            instructions.push(spl_token::instruction::initialize_account(
                &spl_token::id(),
                &wsol_pubkey,
                &native_mint,
                &owner,
            )?);
        }

        if let Some(create_instruction) = create_instruction {
            instructions.push(create_instruction);
        }
        if amount_specified > 0 {
            let mut close_wsol_account_instruction = None;
            // replace native mint with tmp wsol account
            let mut final_in_ata = in_ata;
            let mut final_out_ata = out_ata;

            if let Some(wsol_account) = wsol_account {
                match swap_config.swap_direction {
                    SwapDirection::Buy => {
                        final_in_ata = wsol_account;
                    }
                    SwapDirection::Sell => {
                        final_out_ata = wsol_account;
                    }
                }
                close_wsol_account_instruction = Some(spl_token::instruction::close_account(
                    &program_id,
                    &wsol_account,
                    &owner,
                    &owner,
                    &[&owner],
                )?);
            }

            // build swap instruction
            let build_swap_instruction = amm_swap(
                &amm_program,
                swap_info_result,
                &owner,
                &final_in_ata,
                &final_out_ata,
                amount_specified,
                other_amount_threshold,
                swap_base_in,
            )?;
            // logger.log({
            //     format!(
            //         "({}:{:?}) => amount_specified: {}, other_amount_threshold: {}, wsol_account: {:?}",
            //         chrono::Utc::now().timestamp(), start_time.elapsed(), amount_specified, other_amount_threshold, wsol_account
            //     )
            // });
            if swap_config.swap_direction == SwapDirection::Buy
                && start_time.elapsed() > Duration::from_millis(700)
            {
                return Err(anyhow!("Long RPC Connection with Pool State."));
            }
            instructions.push(build_swap_instruction);
            // close wsol account
            if let Some(close_wsol_account_instruction) = close_wsol_account_instruction {
                instructions.push(close_wsol_account_instruction);
            }
        }
        if let Some(close_instruction) = close_instruction {
            instructions.push(close_instruction);
        }
        if instructions.is_empty() {
            return Err(anyhow!("instructions is empty, no tx required"));
        }
        logger.log(format!("sending tx: {:?}", start_time.elapsed()));
        tx::new_signed_and_send(
            &client,
            &self.keypair,
            instructions,
            swap_config.use_jito,
            &logger,
        )
        .await
    }

    pub async fn swap(
        &self,
        swap_config: SwapConfig,
        amm_pool_id: Pubkey,
        pool_state: AmmInfo,
        start_time: Instant,
    ) -> Result<Vec<String>> {
        let logger = Logger::new(format!(
            "[SWAP IN RAYDIUM]({}:{:?}) => ",
            chrono::Utc::now().timestamp(),
            start_time.elapsed()
        ));
        let slippage_bps = swap_config.slippage * 100;
        let owner = self.keypair.pubkey();
        let program_id = spl_token::ID;
        let native_mint = spl_token::native_mint::ID;
        let mint = if native_mint != pool_state.coin_vault_mint {
            pool_state.coin_vault_mint
        } else {
            pool_state.pc_vault_mint
        };

        let (token_in, token_out, user_input_token, swap_base_in) = match (
            swap_config.swap_direction.clone(),
            pool_state.coin_vault_mint == native_mint,
        ) {
            (SwapDirection::Buy, true) => (native_mint, mint, pool_state.coin_vault, true),
            (SwapDirection::Buy, false) => (native_mint, mint, pool_state.pc_vault, true),
            (SwapDirection::Sell, true) => (mint, native_mint, pool_state.pc_vault, true),
            (SwapDirection::Sell, false) => (mint, native_mint, pool_state.coin_vault, true),
        };

        logger.log(format!(
            "({}:{:?}): token_in:{}, token_out:{}, user_input_token:{}, swap_base_in:{}",
            chrono::Utc::now().timestamp(),
            start_time.elapsed(),
            token_in,
            token_out,
            user_input_token,
            swap_base_in
        ));

        let in_ata = get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_in,
            &owner,
        );
        let out_ata = get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_out,
            &owner,
        );

        let mut create_instruction = None;
        let mut close_instruction = None;

        let (amount_specified, _amount_ui_pretty) = match swap_config.swap_direction {
            SwapDirection::Buy => {
                // Create base ATA if it doesn't exist.
                match get_account_info(
                    self.rpc_nonblocking_client.clone(),
                    &token_out,
                    &out_ata,
                    &logger,
                )
                .await
                {
                    Ok(_) => {
                        logger.log("Base ata exists. skipping creation..".to_string());
                    }
                    Err(TokenError::AccountNotFound) | Err(TokenError::AccountInvalidOwner) => {
                        logger.log(format!(
                            "({}:{:?}): Base ATA for mint {} does not exist. will be create",
                            chrono::Utc::now().timestamp(),
                            start_time.elapsed(),
                            token_out
                        ));
                        create_instruction = Some(create_associated_token_account(
                            &owner,
                            &owner,
                            &token_out,
                            &program_id,
                        ));
                    }
                    Err(error) => {
                        logger.log(format!("Error retrieving out ATA: {}", error));
                    }
                }

                (
                    ui_amount_to_amount(swap_config.amount_in, spl_token::native_mint::DECIMALS),
                    (swap_config.amount_in, spl_token::native_mint::DECIMALS),
                )
            }
            SwapDirection::Sell => {
                let in_account = get_account_info(
                    self.rpc_nonblocking_client.clone(),
                    &token_in,
                    &in_ata,
                    &logger,
                )
                .await?;
                let in_mint = get_mint_info(
                    self.rpc_nonblocking_client.clone(),
                    self.keypair.clone(),
                    &token_in,
                )
                .await?;

                let amount = match swap_config.in_type {
                    SwapInType::Qty => {
                        ui_amount_to_amount(swap_config.amount_in, in_mint.base.decimals)
                    }
                    SwapInType::Pct => {
                        let amount_in_pct = swap_config.amount_in.min(1.0);
                        if amount_in_pct == 1.0 {
                            logger
                                .log(format!("Sell all. will be close ATA for mint {}", token_in));
                            close_instruction = Some(spl_token::instruction::close_account(
                                &program_id,
                                &in_ata,
                                &owner,
                                &owner,
                                &[&owner],
                            )?);
                            in_account.base.amount
                        } else {
                            (((amount_in_pct * 100.0) as u64) * in_account.base.amount) / 100
                        }
                    }
                };
                (
                    amount,
                    (
                        amount_to_ui_amount(amount, in_mint.base.decimals),
                        in_mint.base.decimals,
                    ),
                )
            }
        };

        let amm_program = Pubkey::from_str(AMM_PROGRAM)?;
        let client = self.rpc_client.clone().unwrap();
        let swap_info_result = amm_cli::calculate_swap_info(
            &client,
            amm_program,
            amm_pool_id,
            user_input_token,
            amount_specified,
            slippage_bps,
            swap_base_in,
        )?;
        let other_amount_threshold = swap_info_result.other_amount_threshold;

        // logger.log(format!(
        //     "[swap_info_result]({}): {:#?}",
        //     chrono::Utc::now().timestamp(),
        //     swap_info_result
        // ));
        logger.log(format!(
            "swap({}:{:?}): {}, value: {:?} -> {}",
            chrono::Utc::now().timestamp(),
            start_time.elapsed(),
            token_in,
            _amount_ui_pretty,
            token_out
        ));

        // build instructions
        let mut instructions = vec![];
        // sol <-> wsol support
        let mut wsol_account = None;
        if token_in == native_mint || token_out == native_mint {
            // create wsol account
            let seed = &format!("{}", Keypair::new().pubkey())[..32];
            let wsol_pubkey = Pubkey::create_with_seed(&owner, seed, &spl_token::id())?;
            wsol_account = Some(wsol_pubkey);

            // LAMPORTS_PER_SOL / 100 // 0.01 SOL as rent
            // get rent
            let rent = self
                .rpc_nonblocking_client
                .get_minimum_balance_for_rent_exemption(Account::LEN)
                .await?;
            // if buy add amount_specified
            let total_amount = if token_in == native_mint {
                rent + amount_specified
            } else {
                rent
            };
            // create tmp wsol account
            instructions.push(system_instruction::create_account_with_seed(
                &owner,
                &wsol_pubkey,
                &owner,
                seed,
                total_amount,
                Account::LEN as u64, // 165, // Token account size
                &spl_token::id(),
            ));

            // initialize account
            instructions.push(spl_token::instruction::initialize_account(
                &spl_token::id(),
                &wsol_pubkey,
                &native_mint,
                &owner,
            )?);
        }

        if let Some(create_instruction) = create_instruction {
            instructions.push(create_instruction);
        }
        if amount_specified > 0 {
            let mut close_wsol_account_instruction = None;
            // replace native mint with tmp wsol account
            let mut final_in_ata = in_ata;
            let mut final_out_ata = out_ata;

            if let Some(wsol_account) = wsol_account {
                match swap_config.swap_direction {
                    SwapDirection::Buy => {
                        final_in_ata = wsol_account;
                    }
                    SwapDirection::Sell => {
                        final_out_ata = wsol_account;
                    }
                }
                close_wsol_account_instruction = Some(spl_token::instruction::close_account(
                    &program_id,
                    &wsol_account,
                    &owner,
                    &owner,
                    &[&owner],
                )?);
            }

            // build swap instruction
            let build_swap_instruction = amm_swap(
                &amm_program,
                swap_info_result,
                &owner,
                &final_in_ata,
                &final_out_ata,
                amount_specified,
                other_amount_threshold,
                swap_base_in,
            )?;
            logger.log({
                format!(
                    "({}:{:?}) => amount_specified: {}, other_amount_threshold: {}, wsol_account: {:?}",
                    chrono::Utc::now().timestamp(), start_time.elapsed(), amount_specified, other_amount_threshold, wsol_account
                )
            });
            instructions.push(build_swap_instruction);
            // close wsol account
            if let Some(close_wsol_account_instruction) = close_wsol_account_instruction {
                instructions.push(close_wsol_account_instruction);
            }
        }
        if let Some(close_instruction) = close_instruction {
            instructions.push(close_instruction);
        }
        if instructions.is_empty() {
            return Err(anyhow!("instructions is empty, no tx required"));
        }
        tx::new_signed_and_send(
            &client,
            &self.keypair,
            instructions,
            swap_config.use_jito,
            &logger,
        )
        .await
    }

    pub async fn swap_test(&self, mint_str: &str, swap_config: SwapConfig) -> Result<Vec<String>> {
        let logger = Logger::new(format!(
            "[SWAP IN RAYDIUM]({}) => ",
            chrono::Utc::now().timestamp(),
        ));
        let start_time = Instant::now();
        let slippage_bps = swap_config.slippage * 100;
        let owner = self.keypair.pubkey();
        let program_id = spl_token::ID;
        let native_mint = spl_token::native_mint::ID;
        let mint = Pubkey::from_str(mint_str)
            .map_err(|e| anyhow!("failed to parse mint pubkey: {}", e))?;
        let (amm_pool_id, pool_state) = get_pool_state(
            self.rpc_client.clone().unwrap(),
            self.pool_id.as_deref(),
            Some(mint_str),
            &logger,
        )
        .await?;

        let (token_in, token_out, user_input_token, swap_base_in) = match (
            swap_config.swap_direction.clone(),
            pool_state.coin_vault_mint == native_mint,
        ) {
            (SwapDirection::Buy, true) => (native_mint, mint, pool_state.coin_vault, true),
            (SwapDirection::Buy, false) => (native_mint, mint, pool_state.pc_vault, true),
            (SwapDirection::Sell, true) => (mint, native_mint, pool_state.pc_vault, true),
            (SwapDirection::Sell, false) => (mint, native_mint, pool_state.coin_vault, true),
        };

        logger.log(format!(
            "({}:{:?}): token_in:{}, token_out:{}, user_input_token:{}, swap_base_in:{}",
            chrono::Utc::now().timestamp(),
            start_time.elapsed(),
            token_in,
            token_out,
            user_input_token,
            swap_base_in
        ));

        let in_ata = get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_in,
            &owner,
        );
        let out_ata = get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_out,
            &owner,
        );

        let mut create_instruction = None;
        let mut close_instruction = None;

        let (amount_specified, _amount_ui_pretty) = match swap_config.swap_direction {
            SwapDirection::Buy => {
                // Create base ATA if it doesn't exist.
                match get_account_info(
                    self.rpc_nonblocking_client.clone(),
                    &token_out,
                    &out_ata,
                    &logger,
                )
                .await
                {
                    Ok(_) => {
                        logger.log("Base ata exists. skipping creation..".to_string());
                    }
                    Err(TokenError::AccountNotFound) | Err(TokenError::AccountInvalidOwner) => {
                        logger.log(format!(
                            "({}:{:?}): Base ATA for mint {} does not exist. will be create",
                            chrono::Utc::now().timestamp(),
                            start_time.elapsed(),
                            token_out
                        ));
                        create_instruction = Some(create_associated_token_account(
                            &owner,
                            &owner,
                            &token_out,
                            &program_id,
                        ));
                    }
                    Err(error) => {
                        logger.log(format!("Error retrieving out ATA: {}", error));
                    }
                }

                (
                    ui_amount_to_amount(swap_config.amount_in, spl_token::native_mint::DECIMALS),
                    (swap_config.amount_in, spl_token::native_mint::DECIMALS),
                )
            }
            SwapDirection::Sell => {
                let in_account = get_account_info(
                    self.rpc_nonblocking_client.clone(),
                    &token_in,
                    &in_ata,
                    &logger,
                )
                .await?;
                let in_mint = get_mint_info(
                    self.rpc_nonblocking_client.clone(),
                    self.keypair.clone(),
                    &token_in,
                )
                .await?;

                let amount = match swap_config.in_type {
                    SwapInType::Qty => {
                        ui_amount_to_amount(swap_config.amount_in, in_mint.base.decimals)
                    }
                    SwapInType::Pct => {
                        let amount_in_pct = swap_config.amount_in.min(1.0);
                        if amount_in_pct == 1.0 {
                            logger
                                .log(format!("Sell all. will be close ATA for mint {}", token_in));
                            close_instruction = Some(spl_token::instruction::close_account(
                                &program_id,
                                &in_ata,
                                &owner,
                                &owner,
                                &[&owner],
                            )?);
                            in_account.base.amount
                        } else {
                            (((amount_in_pct * 100.0) as u64) * in_account.base.amount) / 100
                        }
                    }
                };
                (
                    amount,
                    (
                        amount_to_ui_amount(amount, in_mint.base.decimals),
                        in_mint.base.decimals,
                    ),
                )
            }
        };

        let amm_program = Pubkey::from_str(AMM_PROGRAM)?;
        let client = self.rpc_client.clone().unwrap();
        let swap_info_result = amm_cli::calculate_swap_info(
            &client,
            amm_program,
            amm_pool_id,
            user_input_token,
            amount_specified,
            slippage_bps,
            swap_base_in,
        )?;
        let other_amount_threshold = swap_info_result.other_amount_threshold;

        // logger.log(format!(
        //     "[swap_info_result]({}): {:#?}",
        //     chrono::Utc::now().timestamp(),
        //     swap_info_result
        // ));
        logger.log(format!(
            "swap({}:{:?}): {}, value: {:?} -> {}",
            chrono::Utc::now().timestamp(),
            start_time.elapsed(),
            token_in,
            _amount_ui_pretty,
            token_out
        ));

        // build instructions
        let mut instructions = vec![];
        // sol <-> wsol support
        let mut wsol_account = None;
        if token_in == native_mint || token_out == native_mint {
            // create wsol account
            let seed = &format!("{}", Keypair::new().pubkey())[..32];
            let wsol_pubkey = Pubkey::create_with_seed(&owner, seed, &spl_token::id())?;
            wsol_account = Some(wsol_pubkey);

            // LAMPORTS_PER_SOL / 100 // 0.01 SOL as rent
            // get rent
            let rent = self
                .rpc_nonblocking_client
                .get_minimum_balance_for_rent_exemption(Account::LEN)
                .await?;
            // if buy add amount_specified
            let total_amount = if token_in == native_mint {
                rent + amount_specified
            } else {
                rent
            };
            // create tmp wsol account
            instructions.push(system_instruction::create_account_with_seed(
                &owner,
                &wsol_pubkey,
                &owner,
                seed,
                total_amount,
                Account::LEN as u64, // 165, // Token account size
                &spl_token::id(),
            ));

            // initialize account
            instructions.push(spl_token::instruction::initialize_account(
                &spl_token::id(),
                &wsol_pubkey,
                &native_mint,
                &owner,
            )?);
        }

        if let Some(create_instruction) = create_instruction {
            instructions.push(create_instruction);
        }
        if amount_specified > 0 {
            let mut close_wsol_account_instruction = None;
            // replace native mint with tmp wsol account
            let mut final_in_ata = in_ata;
            let mut final_out_ata = out_ata;

            if let Some(wsol_account) = wsol_account {
                match swap_config.swap_direction {
                    SwapDirection::Buy => {
                        final_in_ata = wsol_account;
                    }
                    SwapDirection::Sell => {
                        final_out_ata = wsol_account;
                    }
                }
                close_wsol_account_instruction = Some(spl_token::instruction::close_account(
                    &program_id,
                    &wsol_account,
                    &owner,
                    &owner,
                    &[&owner],
                )?);
            }

            // build swap instruction
            let build_swap_instruction = amm_swap(
                &amm_program,
                swap_info_result,
                &owner,
                &final_in_ata,
                &final_out_ata,
                amount_specified,
                other_amount_threshold,
                swap_base_in,
            )?;
            logger.log({
                format!(
                    "({}:{:?}) => amount_specified: {}, other_amount_threshold: {}, wsol_account: {:?}",
                    chrono::Utc::now().timestamp(), start_time.elapsed(), amount_specified, other_amount_threshold, wsol_account
                )
            });
            instructions.push(build_swap_instruction);
            // close wsol account
            if let Some(close_wsol_account_instruction) = close_wsol_account_instruction {
                instructions.push(close_wsol_account_instruction);
            }
        }
        if let Some(close_instruction) = close_instruction {
            instructions.push(close_instruction);
        }
        if instructions.is_empty() {
            return Err(anyhow!("instructions is empty, no tx required"));
        }
        tx::new_signed_and_send(
            &client,
            &self.keypair,
            instructions,
            swap_config.use_jito,
            &logger,
        )
        .await
    }
}

pub fn amm_swap(
    amm_program: &Pubkey,
    result: AmmSwapInfoResult,
    user_owner: &Pubkey,
    user_source: &Pubkey,
    user_destination: &Pubkey,
    amount_specified: u64,
    other_amount_threshold: u64,
    swap_base_in: bool,
) -> Result<Instruction> {
    let swap_instruction = if swap_base_in {
        raydium_amm::instruction::swap_base_in(
            amm_program,
            &result.pool_id,
            &result.amm_authority,
            &result.amm_open_orders,
            &result.amm_coin_vault,
            &result.amm_pc_vault,
            &result.market_program,
            &result.market,
            &result.market_bids,
            &result.market_asks,
            &result.market_event_queue,
            &result.market_coin_vault,
            &result.market_pc_vault,
            &result.market_vault_signer,
            user_source,
            user_destination,
            user_owner,
            amount_specified,
            other_amount_threshold,
        )?
    } else {
        raydium_amm::instruction::swap_base_out(
            amm_program,
            &result.pool_id,
            &result.amm_authority,
            &result.amm_open_orders,
            &result.amm_coin_vault,
            &result.amm_pc_vault,
            &result.market_program,
            &result.market,
            &result.market_bids,
            &result.market_asks,
            &result.market_event_queue,
            &result.market_coin_vault,
            &result.market_pc_vault,
            &result.market_vault_signer,
            user_source,
            user_destination,
            user_owner,
            other_amount_threshold,
            amount_specified,
        )?
    };

    Ok(swap_instruction)
}

pub async fn get_pool_state(
    rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    pool_id: Option<&str>,
    mint: Option<&str>,
    logger: &Logger,
) -> Result<(Pubkey, AmmInfo)> {
    if let Some(pool_id) = pool_id {
        // logger.log(format!("[FIND POOL STATE BY pool_id]: {}", pool_id));
        let amm_pool_id = Pubkey::from_str(pool_id)?;
        let pool_data = common::rpc::get_account(&rpc_client, &amm_pool_id)?
            .ok_or(anyhow!("NotFoundPool: pool state not found"))?;
        let pool_state: &AmmInfo =
            bytemuck::from_bytes(&pool_data[0..core::mem::size_of::<AmmInfo>()]);
        Ok((amm_pool_id, *pool_state))
    } else if let Some(mint) = mint {
        // find pool by mint via rpc
        if let Ok(pool_state) = get_pool_state_by_mint(rpc_client.clone(), mint, logger).await {
            return Ok(pool_state);
        }
        // find pool by mint via raydium api
        let pool_data = get_pool_info(&spl_token::native_mint::ID.to_string(), mint).await;
        if let Ok(pool_data) = pool_data {
            let pool = pool_data
                .get_pool()
                .ok_or(anyhow!("NotFoundPool: pool not found in raydium api"))?;
            let amm_pool_id = Pubkey::from_str(&pool.id)?;
            // logger.log(format!("[FIND POOL STATE BY raydium api]: {}", amm_pool_id));
            let pool_data = common::rpc::get_account(&rpc_client, &amm_pool_id)?
                .ok_or(anyhow!("NotFoundPool: pool state not found"))?;
            let pool_state: &AmmInfo =
                bytemuck::from_bytes(&pool_data[0..core::mem::size_of::<AmmInfo>()]);

            return Ok((amm_pool_id, *pool_state));
        }
        Err(anyhow!("NotFoundPool: pool state not found"))
    } else {
        Err(anyhow!("NotFoundPool: pool state not found"))
    }
}

pub async fn get_pool_state_by_mint(
    rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    mint: &str,
    _logger: &Logger,
) -> Result<(Pubkey, AmmInfo)> {
    // logger.log(format!("[FIND POOL STATE BY mint]: {}", mint));
    let pairs = vec![
        // pump pool
        (
            Some(spl_token::native_mint::ID),
            Pubkey::from_str(mint).ok(),
        ),
        // general pool
        (
            Pubkey::from_str(mint).ok(),
            Some(spl_token::native_mint::ID),
        ),
    ];

    let pool_len = core::mem::size_of::<AmmInfo>() as u64;
    let amm_program = Pubkey::from_str(AMM_PROGRAM)?;
    // Find matching AMM pool from mint pairs by filter
    let mut found_pools = None;
    for (coin_mint, pc_mint) in pairs {
        // logger.log(format!(
        //     "get_pool_state_by_mint filter: coin_mint: {:?}, pc_mint: {:?}",
        //     coin_mint, pc_mint
        // ));
        let filters = match (coin_mint, pc_mint) {
            (None, None) => Some(vec![RpcFilterType::DataSize(pool_len)]),
            (Some(coin_mint), None) => Some(vec![
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(400, &coin_mint.to_bytes())),
                RpcFilterType::DataSize(pool_len),
            ]),
            (None, Some(pc_mint)) => Some(vec![
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(432, &pc_mint.to_bytes())),
                RpcFilterType::DataSize(pool_len),
            ]),
            (Some(coin_mint), Some(pc_mint)) => Some(vec![
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(400, &coin_mint.to_bytes())),
                RpcFilterType::Memcmp(Memcmp::new_base58_encoded(432, &pc_mint.to_bytes())),
                RpcFilterType::DataSize(pool_len),
            ]),
        };
        let pools =
            common::rpc::get_program_accounts_with_filters(&rpc_client, amm_program, filters)
                .unwrap();
        if !pools.is_empty() {
            found_pools = Some(pools);
            break;
        }
    }

    match found_pools {
        Some(pools) => {
            let pool = &pools[0];
            let pool_state = AmmInfo::load_from_bytes(&pools[0].1.data)?;
            Ok((pool.0, *pool_state))
        }
        None => Err(anyhow!("NotFoundPool: pool state not found")),
    }
}

// get pool info
// https://api-v3.raydium.io/pools/info/mint?mint1=So11111111111111111111111111111111111111112&mint2=EzM2d8JVpzfhV7km3tUsR1U1S4xwkrPnWkM4QFeTpump&poolType=standard&poolSortField=default&sortType=desc&pageSize=10&page=1
pub async fn get_pool_info(mint1: &str, mint2: &str) -> Result<PoolData> {
    let client = reqwest::Client::new();

    let result = client
        .get("https://api-v3.raydium.io/pools/info/mint")
        .query(&[
            ("mint1", mint1),
            ("mint2", mint2),
            ("poolType", "standard"),
            ("poolSortField", "default"),
            ("sortType", "desc"),
            ("pageSize", "1"),
            ("page", "1"),
        ])
        .send()
        .await?
        .json::<PoolInfo>()
        .await
        .context("Failed to parse pool info JSON")?;
    Ok(result.data)
}
