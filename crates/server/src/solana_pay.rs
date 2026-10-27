//! The Solana Pay rail (BP-5, keeper). A payment is a plain SPL transfer that carries the
//! intent's `reference` as a read-only account (Solana_Platform_Integration §1.2, §3). The
//! rail finds it with `getSignaturesForAddress(reference)`, fetches it with
//! `getTransaction` (`jsonParsed`), and reads what the recipient received from the token
//! balances, never from the instruction: with the reference as an extra account, the
//! parsed `transferChecked` shows the payer as `multisigAuthority` and the reference as a
//! signer (BP-5 spec check, measured on devnet), and a payment can take other shapes.
//! Which cluster, recipient and mint is the caller's choice: nothing here names one.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::json_rpc::{JsonRpc, RpcError};
use crate::payment::{Check, PayInstructions, PaymentRail, Price, RailError, Receipt, USDC_DECIMALS};

/// Signatures read per check (the RPC's default is 1,000).
pub const MAX_SIGNATURES: usize = 10;
/// Transactions fetched per check, oldest first.
pub const MAX_TX_FETCHES: usize = 2;
/// The commitment a receipt needs. A receipt unlocks nothing yet; a rail that unlocks a
/// paid campaign should use `finalized`, or re-check there before unlocking.
pub const RECEIPT_COMMITMENT: &str = "confirmed";

pub struct SolanaPayRail {
    rpc: JsonRpc,
    recipient: String,
    mint: String,
    cluster: &'static str,
}

impl SolanaPayRail {
    pub fn new(rpc: JsonRpc, recipient: &str, mint: &str, cluster: &'static str) -> Self {
        Self { rpc, recipient: recipient.to_string(), mint: mint.to_string(), cluster }
    }
}

/// A fresh reference: 32 bytes from OS entropy, base58. It is never derived from a stored
/// or displayed seed (the standing seed rule, Reviewer BP-2b-2 F3).
pub fn fresh_reference() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS RNG unavailable");
    bs58::encode(bytes).into_string()
}

fn rail_error(e: RpcError) -> RailError {
    match e {
        RpcError::RateLimited(_) => RailError::Busy,
        RpcError::Failed(text) => RailError::Upstream(text),
    }
}

/// What one transaction paid `recipient` in `mint`, net. `None` when it doesn't count: the
/// reference isn't among its accounts, it failed, or nothing reached the recipient.
pub(crate) fn paid_to(tx: &Value, reference: &str, mint: &str, recipient: &str) -> Option<u64> {
    let keys = tx["transaction"]["message"]["accountKeys"].as_array()?;
    if !keys.iter().any(|k| k["pubkey"].as_str() == Some(reference)) {
        return None;
    }
    if !tx["meta"]["err"].is_null() {
        return None;
    }
    let sum = |list: &Value| -> i128 {
        list.as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["mint"].as_str() == Some(mint))
            .filter(|b| b["owner"].as_str() == Some(recipient))
            .filter_map(|b| b["uiTokenAmount"]["amount"].as_str()?.parse::<u64>().ok())
            .map(i128::from)
            .sum()
    };
    let paid = sum(&tx["meta"]["postTokenBalances"]) - sum(&tx["meta"]["preTokenBalances"]);
    if paid <= 0 {
        return None;
    }
    u64::try_from(paid).ok()
}

/// The verdict of one transaction on one intent, or `None` when it doesn't count. Exact
/// amount → a receipt; any other amount → a terminal mismatch (canon: "USDC exact").
pub(crate) fn decide(tx: &Value, pay: &PayInstructions, signature: &str, wallet: &str, commitment: &str) -> Option<Check> {
    let PayInstructions::SolanaPay { reference, recipient, mint, amount_minor, .. } = pay;
    let paid = paid_to(tx, reference, mint, recipient)?;
    if paid == *amount_minor {
        let payer = tx["transaction"]["message"]["accountKeys"][0]["pubkey"].as_str().unwrap_or_default().to_string();
        Some(Check::Paid(Receipt {
            intent_id: reference.clone(),
            rail_tx: signature.to_string(),
            payer_is_session_wallet: payer == wallet,
            payer,
            amount_minor: paid,
            slot: tx["slot"].as_u64().unwrap_or(0),
            block_time: tx["blockTime"].as_i64(),
            commitment: commitment.to_string(),
        }))
    } else {
        Some(Check::Mismatch { paid, rail_tx: signature.to_string() })
    }
}

#[async_trait]
impl PaymentRail for SolanaPayRail {
    fn instructions(&self, price: Price) -> PayInstructions {
        PayInstructions::SolanaPay {
            reference: fresh_reference(),
            recipient: self.recipient.clone(),
            mint: self.mint.clone(),
            amount_minor: price.amount_minor,
            decimals: USDC_DECIMALS,
            cluster: self.cluster,
        }
    }

    /// At most `1 + MAX_TX_FETCHES` calls. Failed transactions are skipped unfetched; the
    /// oldest transaction that counts decides. A second successful transaction on one
    /// reference (a double payment after a dropped wallet session) is journalled.
    async fn check(&self, pay: &PayInstructions, wallet: &str) -> Result<Check, RailError> {
        let reference = pay.intent_id();
        let sigs = self
            .rpc
            .call("getSignaturesForAddress", json!([reference, {"limit": MAX_SIGNATURES, "commitment": RECEIPT_COMMITMENT}]))
            .await
            .map_err(rail_error)?;
        let list = sigs.as_array().ok_or_else(|| RailError::Upstream("getSignaturesForAddress: result is not an array".into()))?;
        let mut landed: Vec<(&str, &str)> = list
            .iter()
            .filter(|s| s["err"].is_null())
            .filter_map(|s| Some((s["signature"].as_str()?, s["confirmationStatus"].as_str().unwrap_or(RECEIPT_COMMITMENT))))
            .collect();
        landed.reverse();
        if landed.len() > 1 {
            tracing::warn!(reference, transactions = landed.len(), "checkout: more than one transaction carries this reference; the oldest that counts decides");
        }
        for (signature, commitment) in landed.into_iter().take(MAX_TX_FETCHES) {
            let tx = self
                .rpc
                .call("getTransaction", json!([signature, {"encoding": "jsonParsed", "maxSupportedTransactionVersion": 0, "commitment": RECEIPT_COMMITMENT}]))
                .await
                .map_err(rail_error)?;
            if let Some(verdict) = decide(&tx, pay, signature, wallet, commitment) {
                return Ok(verdict);
            }
        }
        Ok(Check::Pending)
    }
}

/// Synthetic transactions shaped like the devnet ones the spec check captured (legacy
/// message, `accountKeys` objects, the payer first, the payer's own balance entries, and
/// the `multisigAuthority`/`signers` form of `transferChecked`). Never real chain data.
#[cfg(test)]
pub(crate) mod fixtures {
    use serde_json::{json, Value};

    pub(crate) const MINT: &str = "MintUsdcTest";
    pub(crate) const OTHER_MINT: &str = "MintOtherTest";
    pub(crate) const RECIPIENT: &str = "RecipientTest";
    pub(crate) const OTHER_OWNER: &str = "OwnerOtherTest";
    pub(crate) const PAYER: &str = "PayerOne";
    pub(crate) const PRICE: u64 = 3_990_000;

    /// One balance entry: `(account index, mint, owner, amount)`.
    pub(crate) type Bal = (u64, &'static str, &'static str, u64);

    fn balances(list: &[Bal]) -> Value {
        Value::Array(
            list.iter()
                .map(|(i, mint, owner, amount)| {
                    json!({"accountIndex": i, "mint": mint, "owner": owner,
                           "programId": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                           "uiTokenAmount": {"amount": amount.to_string(), "decimals": 6}})
                })
                .collect(),
        )
    }

    /// A transaction carrying `reference` (unless `None`), with `err` and these balances.
    pub(crate) fn tx(reference: Option<&str>, err: Value, pre: &[Bal], post: &[Bal]) -> Value {
        let mut keys = vec![
            json!({"pubkey": PAYER, "signer": true, "writable": true, "source": "transaction"}),
            json!({"pubkey": "RecipientAtaTest", "signer": false, "writable": true, "source": "transaction"}),
            json!({"pubkey": "PayerAtaTest", "signer": false, "writable": true, "source": "transaction"}),
        ];
        if let Some(r) = reference {
            keys.push(json!({"pubkey": r, "signer": false, "writable": false, "source": "transaction"}));
        }
        json!({
            "slot": 515758038u64,
            "blockTime": 1793119670i64,
            "version": "legacy",
            "meta": {"err": err, "fee": 45000, "preTokenBalances": balances(pre), "postTokenBalances": balances(post)},
            "transaction": {"message": {"accountKeys": keys, "instructions": [
                {"program": "spl-token", "parsed": {"type": "transferChecked", "info": {
                    "destination": "RecipientAtaTest", "mint": MINT, "multisigAuthority": PAYER,
                    "signers": [reference.unwrap_or("")], "source": "PayerAtaTest",
                    "tokenAmount": {"amount": PRICE.to_string(), "decimals": 6}}}},
            ]}},
        })
    }

    /// `amount` of `mint` moved from the payer to `owner`'s new account: the payer's entry
    /// on both sides, the receiver's on the post side only (as for a just-created account).
    pub(crate) fn transfer(reference: &str, mint: &'static str, owner: &'static str, amount: u64) -> Value {
        tx(Some(reference), Value::Null, &[(2, mint, PAYER, 20_000_000)], &[(1, mint, owner, amount), (2, mint, PAYER, 20_000_000 - amount)])
    }

    /// The intent's exact payment.
    pub(crate) fn exact(reference: &str) -> Value {
        transfer(reference, MINT, RECIPIENT, PRICE)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{exact, transfer, tx, MINT, OTHER_MINT, OTHER_OWNER, PAYER, PRICE, RECIPIENT};
    use super::{decide, fresh_reference};
    use crate::payment::{Check, PayInstructions};
    use serde_json::{json, Value};

    const REF: &str = "RefOne";

    fn pay() -> PayInstructions {
        PayInstructions::SolanaPay {
            reference: REF.into(),
            recipient: RECIPIENT.into(),
            mint: MINT.into(),
            amount_minor: PRICE,
            decimals: 6,
            cluster: "devnet",
        }
    }

    fn verdict(t: &Value) -> Option<Check> {
        decide(t, &pay(), "SigOne", PAYER, "confirmed")
    }

    #[test]
    fn exact_payment_is_paid() {
        let Some(Check::Paid(r)) = verdict(&exact(REF)) else { panic!("a receipt") };
        assert_eq!((r.intent_id.as_str(), r.rail_tx.as_str(), r.payer.as_str()), (REF, "SigOne", PAYER));
        assert!(r.payer_is_session_wallet);
        assert_eq!((r.amount_minor, r.slot, r.block_time, r.commitment.as_str()), (PRICE, 515758038, Some(1793119670), "confirmed"));
    }

    /// The recipient receives the price in another mint; nothing of the intent's mint moves.
    #[test]
    fn wrong_mint_is_not_paid() {
        assert_eq!(verdict(&transfer(REF, OTHER_MINT, RECIPIENT, PRICE)), None);
    }

    /// Another owner receives the price in the intent's mint.
    #[test]
    fn wrong_recipient_is_not_paid() {
        assert_eq!(verdict(&transfer(REF, MINT, OTHER_OWNER, PRICE)), None);
    }

    #[test]
    fn underpayment_is_a_mismatch() {
        let got = verdict(&transfer(REF, MINT, RECIPIENT, PRICE - 1));
        assert_eq!(got, Some(Check::Mismatch { paid: PRICE - 1, rail_tx: "SigOne".into() }));
    }

    #[test]
    fn overpayment_is_a_mismatch() {
        let got = verdict(&transfer(REF, MINT, RECIPIENT, PRICE + 1));
        assert_eq!(got, Some(Check::Mismatch { paid: PRICE + 1, rail_tx: "SigOne".into() }));
    }

    /// A failed transaction moves nothing on chain; the synthetic delta makes `meta.err` the
    /// only thing between it and a receipt.
    #[test]
    fn failed_transaction_is_not_paid() {
        let mut t = exact(REF);
        t["meta"]["err"] = json!({"InstructionError": [1, {"Custom": 1}]});
        assert_eq!(verdict(&t), None);
    }

    /// The exact payment, but the transaction doesn't carry the intent's reference.
    #[test]
    fn transaction_without_the_reference_is_not_paid() {
        let t = tx(None, Value::Null, &[(2, MINT, PAYER, 20_000_000)], &[(1, MINT, RECIPIENT, PRICE), (2, MINT, PAYER, 20_000_000 - PRICE)]);
        assert_eq!(verdict(&t), None);
    }

    #[test]
    fn references_are_fresh_32_byte_keys() {
        let (a, b) = (fresh_reference(), fresh_reference());
        assert_ne!(a, b);
        assert_eq!(bs58::decode(&a).into_vec().expect("base58").len(), 32);
    }
}
