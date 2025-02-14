use std::{env, sync::Arc, time::Duration};

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    signature::Keypair,
    signer::Signer,
    system_instruction, system_transaction,
    transaction::{Transaction, VersionedTransaction},
};
use spl_token::ui_amount_to_amount;

use jito_json_rpc_client::jsonrpc_client::rpc_client::RpcClient as JitoRpcClient;
use std::str::FromStr;
use tokio::time::Instant;

use crate::{
    common::logger::Logger,
    services::jito::{
        self, get_tip_account, get_tip_value, wait_for_bundle_confirmation, JitoClient,
    },
};

// prioritization fee = 5000 + (UNIT_PRICE * 10^-6 ) * UNIT_LIMIT
fn get_unit_price() -> u64 {
    env::var("UNIT_PRICE")
        .ok()
        .and_then(|v| u64::from_str(&v).ok())
        .unwrap_or(1)
}

fn get_unit_limit() -> u32 {
    env::var("UNIT_LIMIT")
        .ok()
        .and_then(|v| u32::from_str(&v).ok())
        .unwrap_or(300_000)
}

pub async fn jito_confirm(
    client: &RpcClient,
    keypair: &Keypair,
    version_tx: VersionedTransaction,
    recent_block_hash: &Hash,
    logger: &Logger,
) -> Result<Vec<String>> {
    let (tip_account, tip1_account) = get_tip_account()?;
    let jito_client = Arc::new(JitoRpcClient::new(format!(
        "{}/api/v1/bundles",
        *jito::BLOCK_ENGINE_URL
    )));
    // jito tip, the upper limit is 0.1
    let mut tip_value = get_tip_value().await.unwrap();
    let tip = 0.0004_f64;
    tip_value -= tip;
    let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);
    let tip_value_lamports = ui_amount_to_amount(tip_value, spl_token::native_mint::DECIMALS); // tip tx

    let simulate_result = client.simulate_transaction(&version_tx)?;
    // logger.log("Tx Stimulate".to_string());
    // if let Some(logs) = simulate_result.value.logs {
    //     for log in logs {
    //         logger.log(log.to_string());
    //     }
    // }
    if let Some(err) = simulate_result.value.err {
        return Err(anyhow::anyhow!("{}", err));
    };
    let bundle: Vec<VersionedTransaction> = if tip_value > 0_f64 {
        vec![
            version_tx,
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip_account,
                tip_lamports,
                *recent_block_hash,
            )),
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip1_account,
                tip_value_lamports,
                *recent_block_hash,
            )),
        ]
    } else {
        vec![
            version_tx,
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip_account,
                tip_lamports,
                *recent_block_hash,
            )),
        ]
    };
    let start_time = Instant::now();
    let bundle_id = jito_client.send_bundle(&bundle).await.unwrap();
    logger.log(format!(
        "tx ellapsed({}): {:?}",
        bundle_id,
        start_time.elapsed()
    ));
    wait_for_bundle_confirmation(
        move |id: String| {
            let client = Arc::clone(&jito_client);
            async move {
                let response = client.get_bundle_statuses(&[id]).await;
                let statuses = response.inspect_err(|err| {
                    println!("Error fetching bundle status: {:?}", err);
                })?;
                Ok(statuses.value)
            }
        },
        bundle_id,
        Duration::from_millis(1000),
        Duration::from_secs(10),
    )
    .await
}

pub async fn new_signed_and_send(
    client: &RpcClient,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    use_jito: bool,
    logger: &Logger,
) -> Result<Vec<String>> {
    let start_time = Instant::now();
    let micro_lamports = get_unit_price();
    let units = get_unit_limit();
    // If not using Jito, manually set the compute unit price and limit
    if !use_jito {
        let modify_compute_units =
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(
                micro_lamports,
            );
        let add_priority_fee =
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(units);

        instructions.insert(0, modify_compute_units);
        instructions.insert(1, add_priority_fee);
    }

    let mut txs = vec![];
    if use_jito {
        let (tip_account, tip1_account) = get_tip_account()?;

        // jito tip, the upper limit is 0.1
        let mut tip_value = get_tip_value().await?;
        let tip = 0.0004_f64;
        tip_value -= tip;
        let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);
        let tip_value_lamports = ui_amount_to_amount(tip_value, spl_token::native_mint::DECIMALS);

        let jito_tip_instruction =
            system_instruction::transfer(&keypair.pubkey(), &tip_account, tip_lamports);
        instructions.push(jito_tip_instruction);
        if tip_value > 0_f64 {
            let jito_tip2_instruction =
                system_instruction::transfer(&keypair.pubkey(), &tip1_account, tip_value_lamports);
            instructions.push(jito_tip2_instruction);
        }

        // send init tx
        let recent_blockhash = client.get_latest_blockhash()?;
        let txn = Transaction::new_signed_with_payer(
            &instructions,
            Some(&keypair.pubkey()),
            &vec![keypair],
            recent_blockhash,
        );

        // let simulate_result = client.simulate_transaction(&txn)?;
        // logger.log("Tx Stimulate".to_string());
        // if let Some(logs) = simulate_result.value.logs {
        //     for log in logs {
        //         logger.log(log.to_string());
        //     }
        // }
        // if let Some(err) = simulate_result.value.err {
        //     return Err(anyhow::anyhow!("{}", err));
        // };

        let jito_client = Arc::new(JitoClient::new(
            format!("{}/api/v1/transactions", *jito::BLOCK_ENGINE_URL).as_str(),
        ));
        let sig = match jito_client.send_transaction(&txn).await {
            Ok(signature) => signature,
            Err(_) => {
                // logger.log(format!("{}", e));
                return Err(anyhow::anyhow!("Bundle status get timeout"));
            }
        };
        txs.push(sig.clone().to_string());
        logger.log(format!("tx ellapsed: {:?}", start_time.elapsed()));
    } else {
        // send init tx
        let recent_blockhash = client.get_latest_blockhash()?;
        let txn = Transaction::new_signed_with_payer(
            &instructions,
            Some(&keypair.pubkey()),
            &vec![keypair],
            recent_blockhash,
        );
        let sig = common::rpc::send_txn(client, &txn, true)?;
        logger.log(format!(
            "signature({}): {:#?}",
            chrono::Utc::now().timestamp(),
            sig
        ));
        txs.push(sig.to_string());
    }

    Ok(txs)
}
