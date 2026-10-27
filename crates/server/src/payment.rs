//! The payment seam (BP-5, keeper). Solana_Platform_Integration §9 is binding: all payment
//! logic sits behind `PaymentRail`. `SolanaPayRail` is the launch rail; a future
//! `GooglePlayBillingRail` adds a `PayInstructions` variant and implements the same two
//! methods, so no route, store or game rule changes. Nothing in this file names a chain.

use async_trait::async_trait;
use serde::Serialize;

/// The campaign price, $3.99, in USDC minor units (6 decimal places). Canon: "USDC exact".
pub const CHECKOUT_PRICE_USDC_MINOR: u64 = 3_990_000;
pub const USDC_DECIMALS: u8 = 6;

/// What is being paid for, in minor units of the rail's currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub amount_minor: u64,
}

/// What the client needs to pay one intent. The rail mints it, fresh each time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayInstructions {
    /// A plain SPL transfer of exactly `amount_minor` of `mint` to `recipient`, carrying
    /// `reference` as a read-only account (Solana Pay).
    SolanaPay { reference: String, recipient: String, mint: String, amount_minor: u64, decimals: u8, cluster: &'static str },
}

impl PayInstructions {
    /// The intent's id: the key the store and the confirm route use.
    pub fn intent_id(&self) -> &str {
        match self {
            PayInstructions::SolanaPay { reference, .. } => reference,
        }
    }
}

/// Proof of one payment. It unlocks nothing yet (BP-5: a receipt only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub intent_id: String,
    /// The rail's transaction id (a Solana signature).
    pub rail_tx: String,
    pub payer: String,
    pub payer_is_session_wallet: bool,
    pub amount_minor: u64,
    pub slot: u64,
    pub block_time: Option<i64>,
    /// The commitment the payment was seen at.
    pub commitment: String,
}

/// One look for an intent's payment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Nothing has reached the recipient for this intent yet.
    Pending,
    /// A payment arrived for another amount. Terminal: no receipt.
    Mismatch { paid: u64, rail_tx: String },
    Paid(Receipt),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RailError {
    /// The upstream refused for rate; answer `503` and let the client retry later.
    Busy,
    /// Any other upstream failure; the text holds no URL.
    Upstream(String),
}

#[async_trait]
pub trait PaymentRail: Send + Sync {
    /// Fresh payment instructions for one price. No I/O.
    fn instructions(&self, price: Price) -> PayInstructions;
    /// Looks for the payment of the intent `instructions` describes. `wallet` is the
    /// intent's owner; any payer satisfies the intent, and the receipt records which.
    async fn check(&self, instructions: &PayInstructions, wallet: &str) -> Result<Check, RailError>;
}
