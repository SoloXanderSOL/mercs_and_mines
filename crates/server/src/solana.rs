// NOTE: Founding Courtesy dispatch targets devnet. The deployed binary predates
// the declare_id!() program ID fix — testing is pending redeployment.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::read_keypair_file,
    signer::Signer,
    transaction::Transaction,
};

// System program pubkey (11111111111111111111111111111111); using direct const
// avoids the deprecated solana_sdk::system_program module in SDK 2.x.
const SYSTEM_PROGRAM_ID: Pubkey = solana_sdk::pubkey!("11111111111111111111111111111111");

use crate::config::Config;

const PROGRAM_ID: Pubkey = solana_sdk::pubkey!("2f4NRu39hkNgDMvNhuLFk4E7aCgDRVExGrfc3zNW6m7G");

pub async fn dispatch_founding_courtesy(wallet_pubkey: Pubkey, config: &Config) -> Result<()> {
    let treasury = read_keypair_file(&config.server.treasury_keypair_path)
        .map_err(|e| anyhow!("treasury keypair load failed: {e}"))?;
    let treasury_pubkey = treasury.pubkey();

    let (player_account, _) =
        Pubkey::find_program_address(&[b"player", wallet_pubkey.as_ref()], &PROGRAM_ID);
    let (player_inventory, _) =
        Pubkey::find_program_address(&[b"inventory", wallet_pubkey.as_ref()], &PROGRAM_ID);

    let mut disc = [0u8; 8];
    disc.copy_from_slice(&Sha256::digest(b"global:founding_courtesy")[..8]);

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(wallet_pubkey, false),
            AccountMeta::new(player_account, false),
            AccountMeta::new(player_inventory, false),
            AccountMeta::new(treasury_pubkey, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: disc.to_vec(),
    };

    let client = reqwest::Client::new();
    let rpc = &config.server.rpc_url;

    // Fetch the latest blockhash required for transaction construction.
    let bh_resp: serde_json::Value = client
        .post(rpc)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}]
        }))
        .send()
        .await
        .context("getLatestBlockhash request failed")?
        .json()
        .await
        .context("getLatestBlockhash parse failed")?;

    let blockhash_str = bh_resp["result"]["value"]["blockhash"]
        .as_str()
        .ok_or_else(|| anyhow!("missing blockhash in RPC response: {bh_resp}"))?;

    let recent_blockhash: solana_sdk::hash::Hash = blockhash_str
        .parse()
        .context("blockhash parse failed")?;

    let message = Message::new_with_blockhash(&[ix], Some(&treasury_pubkey), &recent_blockhash);
    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[&treasury], recent_blockhash);

    let tx_bytes = bincode::serialize(&tx).context("transaction serialization failed")?;
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);

    let send_resp: serde_json::Value = client
        .post(rpc)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [tx_b64, {"encoding": "base64", "skipPreflight": false}]
        }))
        .send()
        .await
        .context("sendTransaction request failed")?
        .json()
        .await
        .context("sendTransaction parse failed")?;

    if let Some(err) = send_resp.get("error") {
        let msg = err.to_string();
        if msg.contains("AccountAlreadyInitialized") || msg.contains("already in use") {
            eprintln!("[solana] founding_courtesy: accounts already exist for {wallet_pubkey}");
            return Ok(());
        }
        return Err(anyhow!("founding_courtesy sendTransaction error: {msg}"));
    }

    let sig = send_resp["result"].as_str().unwrap_or("<unknown>");
    eprintln!("[solana] founding_courtesy submitted: {sig}");
    Ok(())
}
