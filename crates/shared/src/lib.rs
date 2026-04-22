use serde::{Deserialize, Serialize};

pub mod hex;

// ── Hex map primitives ─────────────────────────────────────────────────────────

/// Axial hex grid coordinate. Both axes can be negative.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coordinates {
    pub q: i32,
    pub r: i32,
}

/// Any entity (vehicle, building, unit) that actively clears Fog of War.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityVision {
    pub vision_radius: u32,
}

/// Sufficient to initialize and replay a game session deterministically.
/// Stored at session creation; all input log entries reference session_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub session_id: String,
    pub build_version: String,
    /// Seed that initializes the session GameRng. Must be stored and replayed exactly.
    pub seed: u64,
    pub sector_id: String,
    pub campaign_id: String,
    pub sector_tier: String,
    pub ruleset: String,
}

/// One entry in the append-only input log. The log is the financial audit trail —
/// treat it as immutable once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputLogEntry {
    pub tick: u64,
    pub seq: u32,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_id: Option<String>,
    /// Event-specific data; shape is determined by event_type.
    pub payload: serde_json::Value,
    /// Presentational string streamed to the Live Tactical Dashboard.
    /// Never read by Step(); omitted in serialization when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrative_event: Option<String>,
}

/// Persistent player account. Stored in the relational DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub account_id: String,
    pub player_name: String,
    /// Solana wallet pubkey (base58).
    pub wallet_address: String,
    /// Unix timestamp (seconds).
    pub created_at: i64,
    /// Initialized at 0 (Neutral) on account creation.
    pub trust_standing: i32,
}

/// Session authorization issued by the Seed Vault after TEEPIN authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToken {
    pub token_id: String,
    pub account_id: String,
    pub wallet_address: String,
    pub issued_at: i64,
    /// issued_at + 7200 — the 2-hour TEEPIN session window.
    pub expires_at: i64,
}

/// Snapshot of a player's economy balances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomyBalance {
    pub account_id: String,
    /// He3 units. Starts at 500 after the Trust Founding Courtesy delivery.
    pub he3_balance: u64,
    /// $ASH SPL token balance (smallest token unit).
    pub ash_balance: u64,
    /// Soft credits (off-chain only).
    pub credits: u64,
    pub trust_standing: i32,
}

/// One entry in the transaction ledger ($ASH / He3 / Credits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub transaction_id: String,
    /// None for Trust-initiated deliveries (no sender account on Founding Courtesy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_account: Option<String>,
    pub to_account: String,
    pub amount: u64,
    /// "HE3" | "ASH" | "CREDITS"
    pub currency: String,
    pub created_at: i64,
    /// Solana transaction signature; present only for on-chain transactions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solana_signature: Option<String>,
}
