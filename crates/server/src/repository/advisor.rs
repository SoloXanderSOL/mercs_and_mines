use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

use super::RepositoryError;

// ── AdvisorRecord (brick 7a-2) ───────────────────────────────────────────────

/// An Advisor's row in `advisor_records` (migration 0023). The roster columns hold
/// `sim_engine::roster` display strings, pinned by `0023`'s CHECKs. Advisors are ungraded and
/// have no Stress, flaw or XP (ODQ-39). `rank` is Skill Level, inert in Phase 1. The Board is
/// derived from `specialization` (`AdvisorSpecialization::board`), never stored.
#[derive(Debug, Clone)]
pub struct AdvisorRecord {
    pub advisor_id:      Uuid,
    pub campaign_id:     Uuid,
    pub player_wallet:   Vec<u8>,
    pub name:            String,
    pub rank:            i16,
    pub origin:          String,
    pub faction_bias:    String,
    pub specialization:  String,
    pub quirk:           String,
    pub prng_seed_state: i64,
    pub created_at:      DateTime<Utc>,
}

// ── AdvisorRepository trait ──────────────────────────────────────────────────

/// No production caller yet, and no `AppState` field.
#[async_trait]
pub trait AdvisorRepository: Send + Sync {
    async fn create_advisor(&self, record: AdvisorRecord) -> Result<(), RepositoryError>;
    async fn get_advisor(&self, advisor_id: Uuid) -> Result<Option<AdvisorRecord>, RepositoryError>;
    /// Unordered (Reviewer 7a-1 F3): the first caller that shows or indexes the list adds an
    /// explicit order to both impls.
    async fn list_advisors_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<AdvisorRecord>, RepositoryError>;
}

// ── PostgresAdvisorRepository ────────────────────────────────────────────────

pub struct PostgresAdvisorRepository {
    pool: PgPool,
}

impl PostgresAdvisorRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AdvisorRepository for PostgresAdvisorRepository {
    async fn create_advisor(&self, r: AdvisorRecord) -> Result<(), RepositoryError> {
        sqlx::query!(
            r#"INSERT INTO advisor_records (
                advisor_id, campaign_id, player_wallet, name, rank,
                origin, faction_bias, specialization, quirk, prng_seed_state
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"#,
            r.advisor_id,
            r.campaign_id,
            r.player_wallet,
            r.name,
            r.rank,
            r.origin,
            r.faction_bias,
            r.specialization,
            r.quirk,
            r.prng_seed_state,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_advisor(&self, advisor_id: Uuid) -> Result<Option<AdvisorRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                advisor_id, campaign_id, player_wallet, name, rank,
                origin, faction_bias, specialization, quirk, prng_seed_state, created_at
               FROM advisor_records WHERE advisor_id = $1"#,
            advisor_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| AdvisorRecord {
            advisor_id:      r.advisor_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            name:            r.name,
            rank:            r.rank,
            origin:          r.origin,
            faction_bias:    r.faction_bias,
            specialization:  r.specialization,
            quirk:           r.quirk,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
        }))
    }

    async fn list_advisors_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<AdvisorRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                advisor_id, campaign_id, player_wallet, name, rank,
                origin, faction_bias, specialization, quirk, prng_seed_state, created_at
               FROM advisor_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| AdvisorRecord {
            advisor_id:      r.advisor_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            name:            r.name,
            rank:            r.rank,
            origin:          r.origin,
            faction_bias:    r.faction_bias,
            specialization:  r.specialization,
            quirk:           r.quirk,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
        }).collect())
    }
}

// ── InMemoryAdvisorRepository ────────────────────────────────────────────────

/// A duplicate `advisor_id` overwrites here, while Postgres refuses it (PK, as `Internal`):
/// parity with the Commander and Foreman in-memory repos (Reviewer 7a-1 F2). Do not assert
/// duplicate refusal against this impl.
pub struct InMemoryAdvisorRepository {
    store: DashMap<Uuid, AdvisorRecord>,
}

impl InMemoryAdvisorRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryAdvisorRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AdvisorRepository for InMemoryAdvisorRepository {
    async fn create_advisor(&self, record: AdvisorRecord) -> Result<(), RepositoryError> {
        self.store.insert(record.advisor_id, record);
        Ok(())
    }

    async fn get_advisor(&self, advisor_id: Uuid) -> Result<Option<AdvisorRecord>, RepositoryError> {
        Ok(self.store.get(&advisor_id).map(|r| r.clone()))
    }

    async fn list_advisors_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<AdvisorRecord>, RepositoryError> {
        Ok(self
            .store
            .iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_engine::roster::GenericBoard;

    #[tokio::test]
    async fn in_memory_advisor_repository_round_trips() {
        use crate::advisor_gen::generate_advisor;
        let repo = InMemoryAdvisorRepository::new();
        let campaign_id = Uuid::new_v4();
        let a = generate_advisor(vec![7u8; 32], campaign_id, GenericBoard::Mercenary, 42);
        let b = generate_advisor(vec![7u8; 32], campaign_id, GenericBoard::Mining, 43);
        let other = generate_advisor(vec![7u8; 32], Uuid::new_v4(), GenericBoard::Mining, 44);
        repo.create_advisor(a.clone()).await.unwrap();
        repo.create_advisor(b.clone()).await.unwrap();
        repo.create_advisor(other).await.unwrap();

        let got = repo.get_advisor(a.advisor_id).await.unwrap().expect("stored Advisor");
        assert_eq!(got.name, a.name);
        assert_eq!(got.rank, 1);
        assert_eq!(got.origin, a.origin);
        assert_eq!(got.faction_bias, a.faction_bias);
        assert_eq!(got.specialization, a.specialization);
        assert_eq!(got.quirk, a.quirk);
        assert_eq!(got.prng_seed_state, 42);

        assert!(repo.get_advisor(Uuid::new_v4()).await.unwrap().is_none());

        let mut listed: Vec<Uuid> = repo.list_advisors_by_campaign(campaign_id).await.unwrap().iter().map(|r| r.advisor_id).collect();
        listed.sort();
        let mut want = vec![a.advisor_id, b.advisor_id];
        want.sort();
        assert_eq!(listed, want, "list returns only this campaign's Advisors");
    }
}
