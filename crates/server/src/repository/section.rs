#![allow(unused)]
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use super::RepositoryError;

// ── SectionRecord ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SectionRecord {
    pub section_id:   Uuid,
    pub campaign_id:  Uuid,
    pub commander_id: Option<Uuid>,
    /// The owning wallet (GAP-63, `0025`). Nullable in the schema until a contract
    /// migration: a row written by an older binary has none. `create_section` never
    /// writes a null.
    pub player_wallet: Option<Vec<u8>>,
    pub name:         String,
    pub headcount:    i16,
    pub loadout:      JsonValue,
    pub xp:           i32,
    pub created_at:   DateTime<Utc>,
    pub updated_at:   DateTime<Utc>,
}

/// `create_section`'s refusal of a record with no owner, before any write.
fn require_owner(record: &SectionRecord) -> Result<(), RepositoryError> {
    if record.player_wallet.is_none() {
        return Err(RepositoryError::Internal("create_section: player_wallet is required".into()));
    }
    Ok(())
}

// ── SectionRepository trait ──────────────────────────────────────────────────

#[async_trait]
pub trait SectionRepository: Send + Sync {
    /// Refuses a record whose `player_wallet` is `None`, before any write.
    async fn create_section(&self, record: SectionRecord) -> Result<(), RepositoryError>;
    async fn get_section(&self, section_id: Uuid) -> Result<Option<SectionRecord>, RepositoryError>;
    async fn update_headcount(&self, section_id: Uuid, headcount: i16) -> Result<(), RepositoryError>;
    async fn assign_commander(&self, section_id: Uuid, commander_id: Option<Uuid>) -> Result<(), RepositoryError>;
    async fn update_xp(&self, section_id: Uuid, xp: i32) -> Result<(), RepositoryError>;
    async fn list_sections_by_commander(&self, commander_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError>;
    async fn list_sections_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError>;
    async fn delete_sections_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError>;
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

#[async_trait]
impl SectionRepository for PostgresSectionRepository {
    async fn create_section(&self, r: SectionRecord) -> Result<(), RepositoryError> {
        require_owner(&r)?;
        sqlx::query!(
            r#"INSERT INTO section_records (
                section_id, campaign_id, commander_id, player_wallet, name, headcount, loadout, xp
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"#,
            r.section_id,
            r.campaign_id,
            r.commander_id,
            r.player_wallet,
            r.name,
            r.headcount,
            r.loadout,
            r.xp,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_section(&self, section_id: Uuid) -> Result<Option<SectionRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                section_id, campaign_id, commander_id, player_wallet, name, headcount,
                loadout AS "loadout: JsonValue", xp, created_at, updated_at
               FROM section_records WHERE section_id = $1"#,
            section_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| SectionRecord {
            section_id:   r.section_id,
            campaign_id:  r.campaign_id,
            commander_id: r.commander_id,
            player_wallet: r.player_wallet,
            name:         r.name,
            headcount:    r.headcount,
            loadout:      r.loadout,
            xp:           r.xp,
            created_at:   r.created_at,
            updated_at:   r.updated_at,
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

    async fn assign_commander(&self, section_id: Uuid, commander_id: Option<Uuid>) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE section_records SET commander_id = $1, updated_at = now() WHERE section_id = $2",
            commander_id,
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

    async fn list_sections_by_commander(&self, commander_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                section_id, campaign_id, commander_id, player_wallet, name, headcount,
                loadout AS "loadout: JsonValue", xp, created_at, updated_at
               FROM section_records WHERE commander_id = $1"#,
            commander_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| SectionRecord {
            section_id:   r.section_id,
            campaign_id:  r.campaign_id,
            commander_id: r.commander_id,
            player_wallet: r.player_wallet,
            name:         r.name,
            headcount:    r.headcount,
            loadout:      r.loadout,
            xp:           r.xp,
            created_at:   r.created_at,
            updated_at:   r.updated_at,
        }).collect())
    }

    async fn list_sections_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                section_id, campaign_id, commander_id, player_wallet, name, headcount,
                loadout AS "loadout: JsonValue", xp, created_at, updated_at
               FROM section_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| SectionRecord {
            section_id:   r.section_id,
            campaign_id:  r.campaign_id,
            commander_id: r.commander_id,
            player_wallet: r.player_wallet,
            name:         r.name,
            headcount:    r.headcount,
            loadout:      r.loadout,
            xp:           r.xp,
            created_at:   r.created_at,
            updated_at:   r.updated_at,
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
}

// ── InMemorySectionRepository (unit-test stub) ───────────────────────────────

/// In-memory stub. `create_section` refuses a `None` owner like the Postgres impl, but it
/// stores an out-of-range headcount as-is (7c-1 C2 parity): only Postgres refuses it,
/// through `section_records_headcount_range`. A duplicate `section_id` overwrites.
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
        require_owner(&record)?;
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

    async fn assign_commander(&self, section_id: Uuid, commander_id: Option<Uuid>) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&section_id) {
            r.commander_id = commander_id;
        }
        Ok(())
    }

    async fn update_xp(&self, section_id: Uuid, xp: i32) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&section_id) {
            r.xp = xp;
        }
        Ok(())
    }

    async fn list_sections_by_commander(&self, commander_id: Uuid) -> Result<Vec<SectionRecord>, RepositoryError> {
        Ok(self.store.iter()
            .filter(|r| r.commander_id == Some(commander_id))
            .map(|r| r.clone())
            .collect())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(player_wallet: Option<Vec<u8>>) -> SectionRecord {
        SectionRecord {
            section_id:    Uuid::new_v4(),
            campaign_id:   Uuid::new_v4(),
            commander_id:  None,
            player_wallet,
            name:          "Alpha Section".into(),
            headcount:     8,
            loadout:       serde_json::json!({}),
            xp:            0,
            created_at:    Utc::now(),
            updated_at:    Utc::now(),
        }
    }

    /// 7b-1 — the in-memory owner round-trips through every read, and a `None` owner is
    /// refused with nothing stored.
    #[tokio::test]
    async fn in_memory_section_repository_round_trips_the_owner_and_refuses_none() {
        let repo = InMemorySectionRepository::new();
        let mut owned = record(Some(vec![33u8; 32]));
        let commander_id = Uuid::new_v4();
        owned.commander_id = Some(commander_id);
        repo.create_section(owned.clone()).await.expect("create_section failed");

        let got = repo.get_section(owned.section_id).await.unwrap().expect("stored section");
        assert_eq!(got.player_wallet, Some(vec![33u8; 32]));
        let by_campaign = repo.list_sections_by_campaign(owned.campaign_id).await.unwrap();
        assert_eq!(by_campaign.len(), 1);
        assert_eq!(by_campaign[0].player_wallet, Some(vec![33u8; 32]));
        let by_commander = repo.list_sections_by_commander(commander_id).await.unwrap();
        assert_eq!(by_commander.len(), 1);
        assert_eq!(by_commander[0].player_wallet, Some(vec![33u8; 32]));

        let unowned = record(None);
        let err = repo.create_section(unowned.clone()).await.expect_err("a None owner must be refused");
        assert!(matches!(err, RepositoryError::Internal(_)), "expected Internal, got {err:?}");
        assert!(repo.get_section(unowned.section_id).await.unwrap().is_none(), "a refused section must not be stored");
        assert!(repo.list_sections_by_campaign(unowned.campaign_id).await.unwrap().is_empty());
    }
}
