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
    pub name:         String,
    pub headcount:    i16,
    pub loadout:      JsonValue,
    pub xp:           i32,
    pub created_at:   DateTime<Utc>,
    pub updated_at:   DateTime<Utc>,
}

// ── SectionRepository trait ──────────────────────────────────────────────────

#[async_trait]
pub trait SectionRepository: Send + Sync {
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
        sqlx::query!(
            r#"INSERT INTO section_records (
                section_id, campaign_id, commander_id, name, headcount, loadout, xp
            ) VALUES ($1,$2,$3,$4,$5,$6,$7)"#,
            r.section_id,
            r.campaign_id,
            r.commander_id,
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
                section_id, campaign_id, commander_id, name, headcount,
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
                section_id, campaign_id, commander_id, name, headcount,
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
                section_id, campaign_id, commander_id, name, headcount,
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
