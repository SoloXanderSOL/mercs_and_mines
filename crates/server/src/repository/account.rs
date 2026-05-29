#![allow(unused)]
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use crate::repository::WalletAddress as Pubkey;
use uuid::Uuid;

use super::RepositoryError;

#[derive(Clone)]
pub struct PlayerAccount {
    pub wallet: Pubkey,
    pub trust_standing: i32,
    pub gcn_balance: i64,
    pub profile: PlayerProfile,
    pub gcn_ledger: Vec<GcnLedgerEntry>,
}

#[derive(Clone)]
pub struct PlayerProfile {
    pub display_name: Option<String>,
    pub sector_id: Option<Uuid>,
}

/// Append-only audit entry for every server-side $GCN movement.
/// `balance_after` is a snapshot for audit; `player_accounts.gcn_balance` is authoritative.
#[derive(Clone)]
pub struct GcnLedgerEntry {
    pub entry_id: Uuid,
    pub wallet: Pubkey,
    pub delta: i64,
    pub balance_after: i64,
    pub entry_type: String,
    pub session_id: Option<Uuid>,
    pub memo: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[async_trait]
pub trait AccountRepository: Send + Sync {
    async fn get_account(&self, wallet: &Pubkey) -> Option<PlayerAccount>;
    async fn upsert_account(&self, account: PlayerAccount) -> Result<(), RepositoryError>;
    async fn append_gcn_entry(
        &self,
        wallet: &Pubkey,
        entry: GcnLedgerEntry,
    ) -> Result<(), RepositoryError>;
    async fn get_gcn_ledger(&self, wallet: &Pubkey) -> Vec<GcnLedgerEntry>;
}

pub struct InMemoryAccountRepository(pub Arc<DashMap<Pubkey, PlayerAccount>>);

impl InMemoryAccountRepository {
    pub fn new() -> Self {
        Self(Arc::new(DashMap::new()))
    }
}

impl Default for InMemoryAccountRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AccountRepository for InMemoryAccountRepository {
    async fn get_account(&self, wallet: &Pubkey) -> Option<PlayerAccount> {
        self.0.get(wallet).map(|entry| entry.value().clone())
    }

    async fn upsert_account(&self, account: PlayerAccount) -> Result<(), RepositoryError> {
        let wallet = account.wallet;
        let display_name = account.profile.display_name.clone();
        let sector_id = account.profile.sector_id;
        // Insert if absent; update only profile fields if present (preserves ledger and trust_standing).
        self.0
            .entry(wallet)
            .and_modify(move |existing| {
                existing.profile.display_name = display_name;
                existing.profile.sector_id = sector_id;
            })
            .or_insert(account);
        Ok(())
    }

    async fn append_gcn_entry(
        &self,
        wallet: &Pubkey,
        entry: GcnLedgerEntry,
    ) -> Result<(), RepositoryError> {
        let mut account = self.0.get_mut(wallet).ok_or(RepositoryError::NotFound)?;
        account.gcn_balance += entry.delta;
        let new_balance = account.gcn_balance;
        let mut entry = entry;
        entry.balance_after = new_balance;
        account.gcn_ledger.push(entry);
        Ok(())
    }

    async fn get_gcn_ledger(&self, wallet: &Pubkey) -> Vec<GcnLedgerEntry> {
        self.0
            .get(wallet)
            .map(|entry| entry.gcn_ledger.clone())
            .unwrap_or_default()
    }
}
