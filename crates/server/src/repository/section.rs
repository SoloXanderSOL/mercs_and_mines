#![allow(unused)]
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value as JsonValue;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{AcquiredVia, RepositoryError};

// ── SectionRecord ────────────────────────────────────────────────────────────

/// A Section. Commanders lead missions, not Sections: the Section ↔ Commander link is
/// per mission, on `deployment_sections` (7b-2). The legacy Commander column on
/// `section_records` (`0005`) is no longer read or written; a later release drops it.
#[derive(Debug, Clone)]
pub struct SectionRecord {
    pub section_id:    Uuid,
    pub campaign_id:   Uuid,
    /// The owning wallet (GAP-63, `0025`; `NOT NULL` since `0026`).
    pub player_wallet: Vec<u8>,
    pub name:          String,
    pub headcount:     i16,
    pub loadout:       JsonValue,
    pub xp:            i32,
    pub created_at:    DateTime<Utc>,
    pub updated_at:    DateTime<Utc>,
}

// ── SectionRepository trait ──────────────────────────────────────────────────

#[async_trait]
pub trait SectionRepository: Send + Sync {
    async fn create_section(&self, record: SectionRecord) -> Result<(), RepositoryError>;
    async fn get_section(&self, section_id: Uuid) -> Result<Option<SectionRecord>, RepositoryError>;
    async fn update_headcount(&self, section_id: Uuid, headcount: i16) -> Result<(), RepositoryError>;
    async fn update_xp(&self, section_id: Uuid, xp: i32) -> Result<(), RepositoryError>;
    async fn list_sections_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError>;
    async fn delete_sections_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError>;
    /// The subset of `ids` that exist, belong to `player_wallet` and are in `campaign_id`.
    /// Unordered. The mission route's ownership check (7b-2): its only production call.
    async fn owned_section_ids(&self, ids: &[Uuid], player_wallet: &[u8], campaign_id: Uuid) -> Result<Vec<Uuid>, RepositoryError>;
}

// ── PostgresSectionRepository ────────────────────────────────────────────────

pub struct PostgresSectionRepository {
    pool: PgPool,
}

impl PostgresSectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The Section INSERT on a caller's connection (the join, 5a-2). No clamp: the CHECKs guard
/// the row. `via` is the row's provenance (`acquired_via`, 0030): `create_section` passes
/// `Hired`.
pub(crate) async fn insert_section(
    conn: &mut PgConnection,
    r: SectionRecord,
    via: AcquiredVia,
) -> Result<(), RepositoryError> {
    sqlx::query!(
            r#"INSERT INTO section_records (
                section_id, campaign_id, player_wallet, name, headcount, loadout, xp, acquired_via
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"#,
        r.section_id,
        r.campaign_id,
        r.player_wallet,
        r.name,
        r.headcount,
        r.loadout,
        r.xp,
        via.as_str(),
    )
    .execute(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(())
}

#[async_trait]
impl SectionRepository for PostgresSectionRepository {
    async fn create_section(&self, r: SectionRecord) -> Result<(), RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        insert_section(&mut conn, r, AcquiredVia::Hired).await
    }

    async fn get_section(&self, section_id: Uuid) -> Result<Option<SectionRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                section_id, campaign_id, player_wallet, name, headcount,
                loadout AS "loadout: JsonValue", xp, created_at, updated_at
               FROM section_records WHERE section_id = $1"#,
            section_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| SectionRecord {
            section_id:    r.section_id,
            campaign_id:   r.campaign_id,
            player_wallet: r.player_wallet,
            name:          r.name,
            headcount:     r.headcount,
            loadout:       r.loadout,
            xp:            r.xp,
            created_at:    r.created_at,
            updated_at:    r.updated_at,
        }))
    }

    async fn update_headcount(&self, section_id: Uuid, headcount: i16) -> Result<(), RepositoryError> {
        let clamped = i16::max(0, headcount);
        sqlx::query!(
            "UPDATE section_records SET headcount = $1, updated_at = now() WHERE section_id = $2",
            clamped,
            section_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn update_xp(&self, section_id: Uuid, xp: i32) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE section_records SET xp = $1, updated_at = now() WHERE section_id = $2",
            xp,
            section_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn list_sections_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                section_id, campaign_id, player_wallet, name, headcount,
                loadout AS "loadout: JsonValue", xp, created_at, updated_at
               FROM section_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| SectionRecord {
            section_id:    r.section_id,
            campaign_id:   r.campaign_id,
            player_wallet: r.player_wallet,
            name:          r.name,
            headcount:     r.headcount,
            loadout:       r.loadout,
            xp:            r.xp,
            created_at:    r.created_at,
            updated_at:    r.updated_at,
        }).collect())
    }

    async fn delete_sections_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError> {
        let result = sqlx::query!(
            "DELETE FROM section_records WHERE campaign_id = $1",
            campaign_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected())
    }

    async fn owned_section_ids(&self, ids: &[Uuid], player_wallet: &[u8], campaign_id: Uuid) -> Result<Vec<Uuid>, RepositoryError> {
        sqlx::query_scalar!(
            "SELECT section_id FROM section_records WHERE section_id = ANY($1) AND player_wallet = $2 AND campaign_id = $3",
            ids,
            player_wallet,
            campaign_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}

// ── InMemorySectionRepository (unit-test stub) ───────────────────────────────

/// In-memory stub. It stores an out-of-range headcount as-is (7c-1 C2 parity): only
/// Postgres refuses it, through `section_records_headcount_range`. A duplicate
/// `section_id` overwrites.
pub struct InMemorySectionRepository {
    store: DashMap<Uuid, SectionRecord>,
}

impl InMemorySectionRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemorySectionRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SectionRepository for InMemorySectionRepository {
    async fn create_section(&self, record: SectionRecord) -> Result<(), RepositoryError> {
        self.store.insert(record.section_id, record);
        Ok(())
    }

    async fn get_section(&self, section_id: Uuid) -> Result<Option<SectionRecord>, RepositoryError> {
        Ok(self.store.get(&section_id).map(|r| r.clone()))
    }

    async fn update_headcount(&self, section_id: Uuid, headcount: i16) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&section_id) {
            r.headcount = i16::max(0, headcount);
        }
        Ok(())
    }

    async fn update_xp(&self, section_id: Uuid, xp: i32) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&section_id) {
            r.xp = xp;
        }
        Ok(())
    }

    async fn list_sections_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError> {
        Ok(self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.clone())
            .collect())
    }

    async fn delete_sections_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError> {
        let keys: Vec<Uuid> = self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.section_id)
            .collect();
        let count = keys.len() as u64;
        for k in keys {
            self.store.remove(&k);
        }
        Ok(count)
    }

    async fn owned_section_ids(&self, ids: &[Uuid], player_wallet: &[u8], campaign_id: Uuid) -> Result<Vec<Uuid>, RepositoryError> {
        Ok(self.store.iter()
            .filter(|r| ids.contains(&r.section_id) && r.player_wallet == player_wallet && r.campaign_id == campaign_id)
            .map(|r| r.section_id)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(player_wallet: Vec<u8>) -> SectionRecord {
        SectionRecord {
            section_id:    Uuid::new_v4(),
            campaign_id:   Uuid::new_v4(),
            player_wallet,
            name:          "Alpha Section".into(),
            headcount:     8,
            loadout:       serde_json::json!({}),
            xp:            0,
            created_at:    Utc::now(),
            updated_at:    Utc::now(),
        }
    }

    /// 7b-1 — the in-memory owner round-trips through both reads.
    #[tokio::test]
    async fn in_memory_section_repository_round_trips_the_owner() {
        let repo = InMemorySectionRepository::new();
        let owned = record(vec![33u8; 32]);
        repo.create_section(owned.clone()).await.expect("create_section failed");

        let got = repo.get_section(owned.section_id).await.unwrap().expect("stored section");
        assert_eq!(got.player_wallet, vec![33u8; 32]);
        let by_campaign = repo.list_sections_by_campaign(owned.campaign_id).await.unwrap();
        assert_eq!(by_campaign.len(), 1);
        assert_eq!(by_campaign[0].player_wallet, vec![33u8; 32]);
    }
}
