use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{AcquiredVia, RepositoryError};

// ── ForemanRecord (brick 7a-1) ───────────────────────────────────────────────

/// A Mining Foreman's row in `foreman_records` (migration 0022). The roster columns hold
/// `sim_engine::roster` display strings, pinned by `0022`'s CHECKs. The career columns
/// (rank, xp, stress, is_shattered, is_kia) mirror `commander_records`. Their one writer is
/// `settle_foreman_dig` (BP-19a, demo-only on `breakpoint`, dropped at the port), which writes
/// stress, xp, rank and is_shattered; nothing writes is_kia. Phase 2 §13 owns the keeper writer.
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

/// `AppState` holds it since BP-2b-2 (the roster route reads it); the join writes the
/// starting Foreman in its own transaction.
#[async_trait]
pub trait ForemanRepository: Send + Sync {
    /// Stores `foreman_grade` as given; it never recomputes or validates the Grade.
    async fn create_foreman(&self, record: ForemanRecord) -> Result<(), RepositoryError>;
    async fn get_foreman(&self, foreman_id: Uuid) -> Result<Option<ForemanRecord>, RepositoryError>;
    async fn list_foremen_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<ForemanRecord>, RepositoryError>;
    /// One member's Foremen in `campaign_id`, oldest first (`created_at`, then `foreman_id`).
    /// The roster route's read (BP-2b-2).
    async fn list_foremen_for_member(&self, campaign_id: Uuid, player_wallet: &[u8]) -> Result<Vec<ForemanRecord>, RepositoryError>;
    /// **Demo-only (BP-19a, `breakpoint`; dropped at the port).** Adds what finished digs owe
    /// the Foreman, computed from the row as it stands under its lock: stress capped at 100
    /// (Shattered at 100, never cleared here), xp added, rank `max(rank, rank_for_xp(xp))`.
    /// `Ok(false)` and nothing written for an unknown or KIA Foreman.
    async fn settle_foreman_dig(&self, foreman_id: Uuid, stress: u32, xp: u32) -> Result<bool, RepositoryError>;
    /// **Demo-only (BP-19b, `breakpoint`; dropped at the port).** The bench: one member's
    /// Foremen that are not KIA and above 0 shed `shed` stress, floor 0, in one statement, and a
    /// Shattered one's flag clears once the new stress is `clear_at` or below (ODQ-73, a demo
    /// exemption; the caller binds `STRESS_STRAINED_MAX`). Member-scoped (F4). Returns the rows
    /// touched.
    async fn bench_foremen(&self, campaign_id: Uuid, player_wallet: &[u8], shed: i16, clear_at: i16) -> Result<u64, RepositoryError>;
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

    async fn list_foremen_for_member(&self, campaign_id: Uuid, player_wallet: &[u8]) -> Result<Vec<ForemanRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                foreman_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, foreman_grade,
                is_shattered, is_kia, prng_seed_state, created_at, updated_at
               FROM foreman_records WHERE campaign_id = $1 AND player_wallet = $2
               ORDER BY created_at, foreman_id"#,
            campaign_id,
            player_wallet
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

    async fn settle_foreman_dig(&self, foreman_id: Uuid, stress: u32, xp: u32) -> Result<bool, RepositoryError> {
        // Dropping `tx` without commit rolls back. The row lock is held to commit, so the
        // effects are computed from what any other writer committed before us (§4c).
        let mut tx = self.pool.begin().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let row = sqlx::query!(
            "SELECT stress, xp, rank, is_kia FROM foreman_records WHERE foreman_id = $1 FOR NO KEY UPDATE",
            foreman_id,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let Some(row) = row.filter(|r| !r.is_kia) else {
            tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
            return Ok(false);
        };
        let due_stress = i32::try_from(stress).unwrap_or(i32::MAX);
        let new_stress = i32::from(row.stress).saturating_add(due_stress).min(100);
        let new_xp = row.xp.saturating_add(i32::try_from(xp).unwrap_or(i32::MAX));
        let new_rank = row.rank.max(crate::commander_gen::rank_for_xp(new_xp));
        let new_stress = i16::try_from(new_stress).map_err(|e| RepositoryError::Internal(e.to_string()))?;
        sqlx::query!(
            "UPDATE foreman_records SET stress = $2, xp = $3, rank = $4, is_shattered = is_shattered OR $2 >= 100::SMALLINT, updated_at = now() WHERE foreman_id = $1",
            foreman_id,
            new_stress,
            new_xp,
            new_rank,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(true)
    }

    async fn bench_foremen(&self, campaign_id: Uuid, player_wallet: &[u8], shed: i16, clear_at: i16) -> Result<u64, RepositoryError> {
        let result = sqlx::query!(
            "UPDATE foreman_records SET stress = GREATEST(stress - $3, 0),
       is_shattered = is_shattered AND GREATEST(stress - $3, 0) > $4, updated_at = now()
 WHERE campaign_id = $1 AND player_wallet = $2 AND NOT is_kia AND stress > 0",
            campaign_id,
            player_wallet,
            shed,
            // `GREATEST(smallint, 0)` is `integer`, so Postgres types `$4` as `integer`.
            i32::from(clear_at),
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected())
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

    async fn list_foremen_for_member(&self, campaign_id: Uuid, player_wallet: &[u8]) -> Result<Vec<ForemanRecord>, RepositoryError> {
        let mut rows: Vec<ForemanRecord> = self.store.iter()
            .filter(|r| r.campaign_id == campaign_id && r.player_wallet == player_wallet)
            .map(|r| r.clone())
            .collect();
        rows.sort_by_key(|r| (r.created_at, r.foreman_id));
        Ok(rows)
    }

    async fn settle_foreman_dig(&self, foreman_id: Uuid, stress: u32, xp: u32) -> Result<bool, RepositoryError> {
        let Some(mut r) = self.store.get_mut(&foreman_id) else {
            return Ok(false);
        };
        if r.is_kia {
            return Ok(false);
        }
        let due_stress = i32::try_from(stress).unwrap_or(i32::MAX);
        r.stress = i32::from(r.stress).saturating_add(due_stress).min(100) as i16;
        r.xp = r.xp.saturating_add(i32::try_from(xp).unwrap_or(i32::MAX));
        r.rank = r.rank.max(crate::commander_gen::rank_for_xp(r.xp));
        // `create_foreman`'s invariant: a record at 100 is Shattered.
        r.is_shattered = r.is_shattered || r.stress >= 100;
        r.updated_at = Utc::now();
        Ok(true)
    }

    async fn bench_foremen(&self, campaign_id: Uuid, player_wallet: &[u8], shed: i16, clear_at: i16) -> Result<u64, RepositoryError> {
        let mut touched = 0;
        for mut r in self.store.iter_mut() {
            if r.campaign_id == campaign_id && r.player_wallet == player_wallet && !r.is_kia && r.stress > 0 {
                // The clear reads the new stress, never the assigned field (Postgres `SET` reads
                // the old row, H6).
                let new_stress = (r.stress - shed).max(0);
                r.is_shattered = r.is_shattered && new_stress > clear_at;
                r.stress = new_stress;
                r.updated_at = Utc::now();
                touched += 1;
            }
        }
        Ok(touched)
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

    /// M1 (BP-19b): the bench mirror at `clear_at` 70. Shed 5: Shattered 75 → 70, cleared;
    /// Shattered 76 → 71, still Shattered; plain 70 → 65, the flag still false; KIA 40 and plain
    /// 0 untouched; 3 rows. Then a Shattered 100: shed 25 → 75, still Shattered; shed 5 → 70,
    /// cleared.
    #[tokio::test]
    async fn in_memory_bench_foremen_clears_at_70() {
        let repo = InMemoryForemanRepository::new();
        let campaign_id = Uuid::new_v4();
        let wallet = vec![7u8; 32];
        let mut ids = Vec::new();
        for (seed, stress, shattered, kia) in [(1, 75, true, false), (2, 76, true, false), (3, 70, false, false), (4, 40, false, true), (5, 0, false, false)] {
            let mut f = crate::foreman_gen::generate_foreman(wallet.clone(), campaign_id, seed);
            f.stress = stress;
            f.is_shattered = shattered;
            f.is_kia = kia;
            ids.push(f.foreman_id);
            repo.create_foreman(f).await.unwrap();
        }
        assert_eq!(repo.bench_foremen(campaign_id, &wallet, 5, 70).await.unwrap(), 3);
        let mut got = Vec::new();
        for id in &ids {
            let r = repo.get_foreman(*id).await.unwrap().unwrap();
            got.push((r.stress, r.is_shattered));
        }
        assert_eq!(got, vec![(70, false), (71, true), (65, false), (40, false), (0, false)]);

        let mut f = crate::foreman_gen::generate_foreman(wallet.clone(), campaign_id, 6);
        f.stress = 100;
        f.is_shattered = true;
        let id = f.foreman_id;
        repo.create_foreman(f).await.unwrap();
        repo.bench_foremen(campaign_id, &wallet, 25, 70).await.unwrap();
        let r = repo.get_foreman(id).await.unwrap().unwrap();
        assert_eq!((r.stress, r.is_shattered), (75, true), "five benched minutes");
        repo.bench_foremen(campaign_id, &wallet, 5, 70).await.unwrap();
        let r = repo.get_foreman(id).await.unwrap().unwrap();
        assert_eq!((r.stress, r.is_shattered), (70, false), "the sixth clears");
    }
}
