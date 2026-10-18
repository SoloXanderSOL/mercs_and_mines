use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{AcquiredVia, RepositoryError};

// ── ForemanRecord (brick 7a-1) ───────────────────────────────────────────────

/// A Mining Foreman's row in `foreman_records` (migration 0022). The roster columns hold
/// `sim_engine::roster` display strings, pinned by `0022`'s CHECKs. The career columns
/// (rank, xp, stress, is_shattered, is_kia) mirror `commander_records` and have no writer
/// until Phase 2 §13.
#[derive(Debug, Clone)]
pub struct ForemanRecord {
    pub foreman_id:      Uuid,
    pub campaign_id:     Uuid,
    pub player_wallet:   Vec<u8>,
    pub name:            String,
    pub rank:            i16,
    pub xp:              i32,
    pub stress:          i16,
    pub origin:          String,
    pub faction_bias:    String,
    pub specialization:  String,
    pub fatal_flaw:      String,
    /// The spawn Grade's display string (`roster::CommanderGrade::as_str`; Foremen use the
    /// same Grade enum). Stored as given, never recomputed from the traits.
    pub foreman_grade:   String,
    pub is_shattered:    bool,
    pub is_kia:          bool,
    pub prng_seed_state: i64,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

/// `create_foreman` stores stress in 0..=100 and keeps the Commander invariant: a record
/// that arrives at 100 is Shattered.
pub(super) fn clamp_new_record(mut r: ForemanRecord) -> ForemanRecord {
    r.stress = r.stress.clamp(0, 100);
    r.is_shattered = r.is_shattered || r.stress >= 100;
    r
}

// ── ForemanRepository trait ──────────────────────────────────────────────────

/// No production caller yet: §5a's join writes the starting Foreman and adds `foreman_repo`
/// to `AppState`.
#[async_trait]
pub trait ForemanRepository: Send + Sync {
    /// Stores `foreman_grade` as given; it never recomputes or validates the Grade.
    async fn create_foreman(&self, record: ForemanRecord) -> Result<(), RepositoryError>;
    async fn get_foreman(&self, foreman_id: Uuid) -> Result<Option<ForemanRecord>, RepositoryError>;
    async fn list_foremen_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<ForemanRecord>, RepositoryError>;
}

// ── PostgresForemanRepository ────────────────────────────────────────────────

pub struct PostgresForemanRepository {
    pool: PgPool,
}

impl PostgresForemanRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The Foreman INSERT on a caller's connection (the join, 5a-2), with the clamp inside it.
/// `via` is the row's provenance (`acquired_via`, 0030): `create_foreman` passes `Hired`.
pub(crate) async fn insert_foreman(
    conn: &mut PgConnection,
    r: ForemanRecord,
    via: AcquiredVia,
) -> Result<(), RepositoryError> {
    let r = clamp_new_record(r);
    sqlx::query!(
            r#"INSERT INTO foreman_records (
                foreman_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, foreman_grade,
                is_shattered, is_kia, prng_seed_state, acquired_via
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)"#,
        r.foreman_id,
        r.campaign_id,
        r.player_wallet,
        r.name,
        r.rank,
        r.xp,
        r.stress,
        r.origin,
        r.faction_bias,
        r.specialization,
        r.fatal_flaw,
        r.foreman_grade,
        r.is_shattered,
        r.is_kia,
        r.prng_seed_state,
        via.as_str(),
    )
    .execute(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(())
}

#[async_trait]
impl ForemanRepository for PostgresForemanRepository {
    async fn create_foreman(&self, r: ForemanRecord) -> Result<(), RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        insert_foreman(&mut conn, r, AcquiredVia::Hired).await
    }

    async fn get_foreman(&self, foreman_id: Uuid) -> Result<Option<ForemanRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                foreman_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, foreman_grade,
                is_shattered, is_kia, prng_seed_state, created_at, updated_at
               FROM foreman_records WHERE foreman_id = $1"#,
            foreman_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| ForemanRecord {
            foreman_id:      r.foreman_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            name:            r.name,
            rank:            r.rank,
            xp:              r.xp,
            stress:          r.stress,
            origin:          r.origin,
            faction_bias:    r.faction_bias,
            specialization:  r.specialization,
            fatal_flaw:      r.fatal_flaw,
            foreman_grade:   r.foreman_grade,
            is_shattered:    r.is_shattered,
            is_kia:          r.is_kia,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }))
    }

    async fn list_foremen_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<ForemanRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                foreman_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, foreman_grade,
                is_shattered, is_kia, prng_seed_state, created_at, updated_at
               FROM foreman_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| ForemanRecord {
            foreman_id:      r.foreman_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            name:            r.name,
            rank:            r.rank,
            xp:              r.xp,
            stress:          r.stress,
            origin:          r.origin,
            faction_bias:    r.faction_bias,
            specialization:  r.specialization,
            fatal_flaw:      r.fatal_flaw,
            foreman_grade:   r.foreman_grade,
            is_shattered:    r.is_shattered,
            is_kia:          r.is_kia,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }).collect())
    }
}

// ── InMemoryForemanRepository ────────────────────────────────────────────────

pub struct InMemoryForemanRepository {
    store: DashMap<Uuid, ForemanRecord>,
}

impl InMemoryForemanRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryForemanRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ForemanRepository for InMemoryForemanRepository {
    async fn create_foreman(&self, record: ForemanRecord) -> Result<(), RepositoryError> {
        let record = clamp_new_record(record);
        self.store.insert(record.foreman_id, record);
        Ok(())
    }

    async fn get_foreman(&self, foreman_id: Uuid) -> Result<Option<ForemanRecord>, RepositoryError> {
        Ok(self.store.get(&foreman_id).map(|r| r.clone()))
    }

    async fn list_foremen_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<ForemanRecord>, RepositoryError> {
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

    #[tokio::test]
    async fn in_memory_foreman_repository_round_trips() {
        let repo = InMemoryForemanRepository::new();
        let campaign_id = Uuid::new_v4();
        let mut a = crate::foreman_gen::generate_foreman(vec![7u8; 32], campaign_id, 42);
        a.stress = 140;
        let b = crate::foreman_gen::generate_foreman(vec![7u8; 32], campaign_id, 43);
        let other = crate::foreman_gen::generate_foreman(vec![7u8; 32], Uuid::new_v4(), 44);
        repo.create_foreman(a.clone()).await.unwrap();
        repo.create_foreman(b.clone()).await.unwrap();
        repo.create_foreman(other).await.unwrap();

        let got = repo.get_foreman(a.foreman_id).await.unwrap().expect("stored Foreman");
        assert_eq!(got.name, a.name);
        assert_eq!(got.origin, a.origin);
        assert_eq!(got.faction_bias, a.faction_bias);
        assert_eq!(got.specialization, a.specialization);
        assert_eq!(got.fatal_flaw, a.fatal_flaw);
        assert_eq!(got.foreman_grade, a.foreman_grade);
        assert_eq!(got.prng_seed_state, 42);
        assert_eq!(got.stress, 100, "create clamps stress to 100");
        assert!(got.is_shattered, "a record that arrives at 100 is Shattered");

        assert!(repo.get_foreman(Uuid::new_v4()).await.unwrap().is_none());

        let mut listed: Vec<Uuid> = repo.list_foremen_by_campaign(campaign_id).await.unwrap().iter().map(|r| r.foreman_id).collect();
        listed.sort();
        let mut want = vec![a.foreman_id, b.foreman_id];
        want.sort();
        assert_eq!(listed, want, "list returns only this campaign's Foremen");
    }
}
