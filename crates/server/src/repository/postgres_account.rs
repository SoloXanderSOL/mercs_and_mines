use async_trait::async_trait;
use sqlx::PgPool;

use super::{
    account::{AccountRepository, GcnLedgerEntry, PlayerAccount, PlayerProfile},
    RepositoryError, WalletAddress,
};

pub struct PostgresAccountRepository {
    pool: PgPool,
}

impl PostgresAccountRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AccountRepository for PostgresAccountRepository {
    async fn get_account(&self, wallet: &WalletAddress) -> Option<PlayerAccount> {
        let wallet_bytes = wallet.0.to_vec();
        let row = sqlx::query!(
            "SELECT wallet_address, trust_standing \
             FROM player_accounts \
             WHERE wallet_address = $1",
            wallet_bytes
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;

        let addr: [u8; 32] = row.wallet_address.try_into().ok()?;
        Some(PlayerAccount {
            wallet: WalletAddress(addr),
            trust_standing: row.trust_standing as i32,
            profile: PlayerProfile { display_name: None, sector_id: None },
            gcn_ledger: vec![],
        })
    }

    async fn upsert_account(&self, account: PlayerAccount) -> Result<(), RepositoryError> {
        let wallet_bytes = account.wallet.0.to_vec();
        let trust = account.trust_standing as i64;
        sqlx::query!(
            r#"INSERT INTO player_accounts (wallet_address, trust_standing)
               VALUES ($1, $2)
               ON CONFLICT (wallet_address) DO UPDATE SET updated_at = now()"#,
            wallet_bytes,
            trust
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn append_gcn_entry(
        &self,
        _wallet: &WalletAddress,
        _entry: GcnLedgerEntry,
    ) -> Result<(), RepositoryError> {
        // gcn_transaction_ledger table is built in Brick 1a-4; no-op stub until then.
        Ok(())
    }

    async fn get_gcn_ledger(&self, _wallet: &WalletAddress) -> Vec<GcnLedgerEntry> {
        // gcn_transaction_ledger table is built in Brick 1a-4; stub returns empty.
        vec![]
    }
}
