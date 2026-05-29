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
            "SELECT wallet_address, trust_standing, gcn_balance \
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
            gcn_balance: row.gcn_balance,
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
        wallet: &WalletAddress,
        entry: GcnLedgerEntry,
    ) -> Result<(), RepositoryError> {
        let wallet_bytes = wallet.0.to_vec();
        let mut tx = self.pool.begin().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;

        let row = sqlx::query!(
            "UPDATE player_accounts \
             SET gcn_balance = gcn_balance + $1, updated_at = now() \
             WHERE wallet_address = $2 \
             RETURNING gcn_balance",
            entry.delta,
            wallet_bytes
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        let balance_after = row.gcn_balance;

        sqlx::query!(
            "INSERT INTO gcn_transaction_ledger \
             (wallet_address, delta, balance_after, entry_type, session_id, memo) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            wallet_bytes,
            entry.delta,
            balance_after,
            entry.entry_type,
            entry.session_id,
            entry.memo
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_gcn_ledger(&self, wallet: &WalletAddress) -> Vec<GcnLedgerEntry> {
        let wallet_bytes = wallet.0.to_vec();
        let rows = sqlx::query!(
            "SELECT entry_id, wallet_address, delta, balance_after, entry_type, session_id, memo, recorded_at \
             FROM gcn_transaction_ledger \
             WHERE wallet_address = $1 \
             ORDER BY recorded_at ASC",
            wallet_bytes
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        rows.into_iter()
            .filter_map(|row| {
                let addr: [u8; 32] = row.wallet_address.try_into().ok()?;
                Some(GcnLedgerEntry {
                    entry_id: row.entry_id,
                    wallet: WalletAddress(addr),
                    delta: row.delta,
                    balance_after: row.balance_after,
                    entry_type: row.entry_type,
                    session_id: row.session_id,
                    memo: row.memo,
                    recorded_at: row.recorded_at,
                })
            })
            .collect()
    }
}
