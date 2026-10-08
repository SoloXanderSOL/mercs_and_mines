use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sim_engine::config::SimConfig;
use sim_engine::game_types::OutcomeType;
use sim_engine::resolver::{stress_after_casualties, stress_after_deployment};
use sqlx::PgPool;
use uuid::Uuid;

use super::RepositoryError;

/// The `stress` column is SMALLINT with no CHECK constraint, so a stored value may sit
/// outside 0..=100 (`create_commander` inserts unclamped). Clamp before handing it to
/// the sim's u8 stress functions. The way back is a plain `i16::from(level)`.
fn stress_to_level(stress: i16) -> u8 {
    stress.clamp(0, 100) as u8
}

// ── CommanderRecord ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CommanderRecord {
    pub commander_id:    Uuid,
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
    pub veteran_trait:   Option<String>,
    pub is_shattered:    bool,
    pub is_kia:          bool,
    pub is_nft:          bool,
    pub prng_seed_state: i64,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

// ── CommanderRepository trait ────────────────────────────────────────────────

#[async_trait]
pub trait CommanderRepository: Send + Sync {
    async fn create_commander(&self, record: CommanderRecord) -> Result<(), RepositoryError>;
    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError>;
    /// Clamps to 0..=100 and sets `is_shattered` in the same write when the clamped
    /// value reaches 100. The flag is sticky: a lower value never clears it.
    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError>;
    async fn update_rank_and_xp(&self, commander_id: Uuid, rank: i16, xp: i32) -> Result<(), RepositoryError>;
    async fn set_veteran_trait(&self, commander_id: Uuid, veteran_trait: String) -> Result<(), RepositoryError>;
    /// `update_stress` is authoritative on the stress path: it sets Shattered itself when
    /// stress reaches 100. This mutator remains for non-stress causes.
    async fn set_shattered(&self, commander_id: Uuid) -> Result<(), RepositoryError>;
    async fn set_kia(&self, commander_id: Uuid) -> Result<(), RepositoryError>;
    async fn list_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CommanderRecord>, RepositoryError>;
    async fn delete_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError>;

    /// Orchestration wrapper — Phase_1_Section_4 §4b. The arithmetic lives in
    /// commander_gen; this method only sequences one read and one existing write.
    async fn apply_mission_xp(
        &self,
        commander_id: Uuid,
        tier: u8,
        outcome: OutcomeType,
    ) -> Result<(), RepositoryError> {
        let rec = self
            .get_commander(commander_id)
            .await?
            .ok_or(RepositoryError::NotFound)?;
        if rec.is_kia {
            return Ok(()); // "the XP record dies with them" — no write
        }
        if matches!(outcome, OutcomeType::Wipeout) {
            return Ok(()); // 0 XP, no write
        }
        // Shattered still earns: the mission that broke them still happened.
        let new_xp = rec.xp + crate::commander_gen::xp_award(tier, &outcome);
        let new_rank = crate::commander_gen::rank_for_xp(new_xp);
        if new_rank > rec.rank {
            tracing::info!(commander_id = %commander_id, from = rec.rank, to = new_rank, "commander rank-up");
            // TODO(phase-2): ranks 2-4 — implement Field Citations shortlist generation and
            // citation_gate_pending resolution — see Commander_Field_Citations.md and Phase 2 §10.
            if new_rank == 5 {
                // TODO(phase-2): offer Retirement Option — Commander_Field_Citations.md §4.
                tracing::info!(commander_id = %commander_id, "rank 5 reached");
            }
        }
        self.update_rank_and_xp(commander_id, new_rank, new_xp).await
    }

    /// Orchestration wrapper — Phase_1_Section_4 §4c. The formula lives in
    /// sim_engine::resolver; this method only sequences one read and one write.
    /// Applies regardless of mission outcome.
    async fn apply_deployment_stress(
        &self,
        commander_id: Uuid,
        cfg: &SimConfig,
    ) -> Result<(), RepositoryError> {
        let rec = self
            .get_commander(commander_id)
            .await?
            .ok_or(RepositoryError::NotFound)?;
        if rec.is_kia {
            return Ok(());
        }
        let next = stress_after_deployment(stress_to_level(rec.stress), cfg);
        self.update_stress(commander_id, i16::from(next)).await
    }

    /// Orchestration wrapper — Phase_1_Section_4 §4c. Applies regardless of mission outcome.
    async fn apply_casualty_stress(
        &self,
        commander_id: Uuid,
        casualties: u32,
        cfg: &SimConfig,
    ) -> Result<(), RepositoryError> {
        let rec = self
            .get_commander(commander_id)
            .await?
            .ok_or(RepositoryError::NotFound)?;
        if rec.is_kia {
            return Ok(());
        }
        let next = stress_after_casualties(stress_to_level(rec.stress), casualties, cfg);
        self.update_stress(commander_id, i16::from(next)).await
    }
}

// ── PostgresCommanderRepository ──────────────────────────────────────────────

pub struct PostgresCommanderRepository {
    pool: PgPool,
}

impl PostgresCommanderRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CommanderRepository for PostgresCommanderRepository {
    async fn create_commander(&self, r: CommanderRecord) -> Result<(), RepositoryError> {
        sqlx::query!(
            r#"INSERT INTO commander_records (
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, is_nft, prng_seed_state
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)"#,
            r.commander_id,
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
            r.veteran_trait,
            r.is_shattered,
            r.is_kia,
            r.is_nft,
            r.prng_seed_state,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, is_nft, prng_seed_state, created_at, updated_at
               FROM commander_records WHERE commander_id = $1"#,
            commander_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| CommanderRecord {
            commander_id:    r.commander_id,
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
            veteran_trait:   r.veteran_trait,
            is_shattered:    r.is_shattered,
            is_kia:          r.is_kia,
            is_nft:          r.is_nft,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }))
    }

    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError> {
        let clamped = i16::max(0, i16::min(100, stress));
        // One statement: no path can leave stress at 100 without Shattered. The formula
        // stays in Rust; SQL only compares the already-clamped value. The literal is typed
        // SMALLINT so Postgres deduces one type for $1 (an int4 literal makes it ambiguous).
        let result = sqlx::query!(
            "UPDATE commander_records SET stress = $1, is_shattered = is_shattered OR $1 >= 100::SMALLINT, updated_at = now() WHERE commander_id = $2",
            clamped,
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn update_rank_and_xp(&self, commander_id: Uuid, rank: i16, xp: i32) -> Result<(), RepositoryError> {
        let result = sqlx::query!(
            "UPDATE commander_records SET rank = $1, xp = $2, updated_at = now() WHERE commander_id = $3",
            rank,
            xp,
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn set_veteran_trait(&self, commander_id: Uuid, veteran_trait: String) -> Result<(), RepositoryError> {
        let result = sqlx::query!(
            "UPDATE commander_records SET veteran_trait = $1, updated_at = now() WHERE commander_id = $2",
            veteran_trait,
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn set_shattered(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        let result = sqlx::query!(
            "UPDATE commander_records SET is_shattered = TRUE, updated_at = now() WHERE commander_id = $1",
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn set_kia(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        let result = sqlx::query!(
            "UPDATE commander_records SET is_kia = TRUE, updated_at = now() WHERE commander_id = $1",
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn list_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CommanderRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, is_nft, prng_seed_state, created_at, updated_at
               FROM commander_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| CommanderRecord {
            commander_id:    r.commander_id,
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
            veteran_trait:   r.veteran_trait,
            is_shattered:    r.is_shattered,
            is_kia:          r.is_kia,
            is_nft:          r.is_nft,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }).collect())
    }

    async fn delete_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError> {
        let result = sqlx::query!(
            "DELETE FROM commander_records WHERE campaign_id = $1",
            campaign_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected())
    }
}

// ── InMemoryCommanderRepository (unit-test stub) ─────────────────────────────

pub struct InMemoryCommanderRepository {
    store: DashMap<Uuid, CommanderRecord>,
}

impl InMemoryCommanderRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryCommanderRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CommanderRepository for InMemoryCommanderRepository {
    async fn create_commander(&self, record: CommanderRecord) -> Result<(), RepositoryError> {
        self.store.insert(record.commander_id, record);
        Ok(())
    }

    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError> {
        Ok(self.store.get(&commander_id).map(|r| r.clone()))
    }

    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError> {
        let mut r = self.store.get_mut(&commander_id).ok_or(RepositoryError::NotFound)?;
        let clamped = i16::max(0, i16::min(100, stress));
        r.stress = clamped;
        r.is_shattered = r.is_shattered || clamped >= 100;
        Ok(())
    }

    async fn update_rank_and_xp(&self, commander_id: Uuid, rank: i16, xp: i32) -> Result<(), RepositoryError> {
        let mut r = self.store.get_mut(&commander_id).ok_or(RepositoryError::NotFound)?;
        r.rank = rank;
        r.xp = xp;
        Ok(())
    }

    async fn set_veteran_trait(&self, commander_id: Uuid, veteran_trait: String) -> Result<(), RepositoryError> {
        let mut r = self.store.get_mut(&commander_id).ok_or(RepositoryError::NotFound)?;
        r.veteran_trait = Some(veteran_trait);
        Ok(())
    }

    async fn set_shattered(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        let mut r = self.store.get_mut(&commander_id).ok_or(RepositoryError::NotFound)?;
        r.is_shattered = true;
        Ok(())
    }

    async fn set_kia(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        let mut r = self.store.get_mut(&commander_id).ok_or(RepositoryError::NotFound)?;
        r.is_kia = true;
        Ok(())
    }

    async fn list_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CommanderRecord>, RepositoryError> {
        Ok(self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.clone())
            .collect())
    }

    async fn delete_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError> {
        let keys: Vec<Uuid> = self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.commander_id)
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

    fn stub_commander(commander_id: Uuid) -> CommanderRecord {
        CommanderRecord {
            commander_id,
            campaign_id:     Uuid::new_v4(),
            player_wallet:   vec![0u8; 32],
            name:            "Test".into(),
            rank:            1,
            xp:              0,
            stress:          50,
            origin:          "unknown".into(),
            faction_bias:    "none".into(),
            specialization:  "none".into(),
            fatal_flaw:      "none".into(),
            veteran_trait:   None,
            is_shattered:    false,
            is_kia:          false,
            is_nft:          false,
            prng_seed_state: 0,
            created_at:      chrono::Utc::now(),
            updated_at:      chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn update_stress_clamps_to_valid_range() {
        let repo = InMemoryCommanderRepository::new();
        let id = Uuid::new_v4();
        repo.create_commander(stub_commander(id)).await.unwrap();

        // over-max clamps to 100
        repo.update_stress(id, 150).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 100, "stress 150 must clamp to 100");

        // under-min clamps to 0
        repo.update_stress(id, -5).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 0, "stress -5 must clamp to 0");

        // in-range value stored as-is
        repo.update_stress(id, 73).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 73, "stress 73 must be stored unchanged");
    }

    // ── apply_mission_xp (brick 4b-1) ────────────────────────────────────────

    /// Build a generated commander with the given xp/rank, persist it in an
    /// in-memory repo, and return (repo, id). Flags are applied by the caller
    /// through the closure before insertion.
    async fn seeded_repo(
        xp: i32,
        rank: i16,
        mutate: impl FnOnce(&mut CommanderRecord),
    ) -> (InMemoryCommanderRepository, Uuid) {
        let mut rec = crate::commander_gen::generate_commander(vec![7u8; 32], Uuid::nil(), 42);
        rec.xp = xp;
        rec.rank = rank;
        mutate(&mut rec);
        let id = rec.commander_id;
        let repo = InMemoryCommanderRepository::new();
        repo.create_commander(rec).await.unwrap();
        (repo, id)
    }

    #[tokio::test]
    async fn apply_mission_xp_adds_xp_below_threshold() {
        let (repo, id) = seeded_repo(0, 1, |_| {}).await;
        repo.apply_mission_xp(id, 1, OutcomeType::Success).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.xp, 12);
        assert_eq!(r.rank, 1);
    }

    #[tokio::test]
    async fn apply_mission_xp_levels_up_at_threshold() {
        let (repo, id) = seeded_repo(88, 1, |_| {}).await;
        repo.apply_mission_xp(id, 1, OutcomeType::Success).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.xp, 100);
        assert_eq!(r.rank, 2);
    }

    #[tokio::test]
    async fn apply_mission_xp_rank_caps_at_5() {
        let (repo, id) = seeded_repo(2600, 5, |_| {}).await;
        repo.apply_mission_xp(id, 5, OutcomeType::Success).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.xp, 2750);
        assert_eq!(r.rank, 5);
    }

    #[tokio::test]
    async fn apply_mission_xp_wipeout_writes_nothing() {
        let (repo, id) = seeded_repo(88, 1, |_| {}).await;
        repo.apply_mission_xp(id, 5, OutcomeType::Wipeout).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.xp, 88);
        assert_eq!(r.rank, 1);
    }

    #[tokio::test]
    async fn apply_mission_xp_kia_writes_nothing() {
        let (repo, id) = seeded_repo(88, 1, |r| r.is_kia = true).await;
        repo.apply_mission_xp(id, 1, OutcomeType::Success).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.xp, 88);
        assert_eq!(r.rank, 1);
    }

    #[tokio::test]
    async fn apply_mission_xp_shattered_still_earns() {
        let (repo, id) = seeded_repo(88, 1, |r| r.is_shattered = true).await;
        repo.apply_mission_xp(id, 1, OutcomeType::Success).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.xp, 100);
        assert_eq!(r.rank, 2);
    }

    /// Fetch happens before the Wipeout short-circuit: an unknown commander is
    /// NotFound even when the outcome would otherwise write nothing.
    #[tokio::test]
    async fn apply_mission_xp_unknown_commander_is_not_found() {
        let repo = InMemoryCommanderRepository::new();
        let r = repo.apply_mission_xp(Uuid::new_v4(), 1, OutcomeType::Wipeout).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "got {:?}", r);
    }

    // ── stress wrappers + F3 (brick 4c-1) ────────────────────────────────────

    #[test]
    fn stress_to_level_clamps() {
        assert_eq!(stress_to_level(-5), 0);
        assert_eq!(stress_to_level(0), 0);
        assert_eq!(stress_to_level(55), 55);
        assert_eq!(stress_to_level(100), 100);
        assert_eq!(stress_to_level(150), 100);
        assert_eq!(stress_to_level(i16::MIN), 0);
        assert_eq!(stress_to_level(i16::MAX), 100);
    }

    #[tokio::test]
    async fn apply_deployment_stress_adds_penalty() {
        let (repo, id) = seeded_repo(0, 1, |r| r.stress = 30).await;
        repo.apply_deployment_stress(id, &SimConfig::default()).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 40);
        assert!(!r.is_shattered);
    }

    /// G3 (in-memory): the single update_stress write sets Shattered; no set_shattered call.
    #[tokio::test]
    async fn apply_casualty_stress_to_100_shatters() {
        let (repo, id) = seeded_repo(0, 1, |r| r.stress = 90).await;
        repo.apply_casualty_stress(id, 4, &SimConfig::default()).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 100);
        assert!(r.is_shattered);
    }

    #[tokio::test]
    async fn update_stress_never_clears_shattered() {
        let (repo, id) = seeded_repo(0, 1, |_| {}).await;
        repo.update_stress(id, 100).await.unwrap();
        assert!(repo.get_commander(id).await.unwrap().unwrap().is_shattered);
        repo.update_stress(id, 40).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 40);
        assert!(r.is_shattered, "Shattered is sticky");
    }

    #[tokio::test]
    async fn stress_wrappers_skip_kia() {
        let (repo, id) = seeded_repo(0, 1, |r| { r.is_kia = true; r.stress = 30; }).await;
        let cfg = SimConfig::default();
        repo.apply_deployment_stress(id, &cfg).await.unwrap();
        repo.apply_casualty_stress(id, 4, &cfg).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 30);
    }

    #[tokio::test]
    async fn stress_wrappers_unknown_id_return_not_found() {
        let repo = InMemoryCommanderRepository::new();
        let cfg = SimConfig::default();
        let r = repo.apply_deployment_stress(Uuid::new_v4(), &cfg).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "got {:?}", r);
        let r = repo.apply_casualty_stress(Uuid::new_v4(), 1, &cfg).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "got {:?}", r);
    }

    /// G4 (in-memory): F3 across all five single-row mutators.
    #[tokio::test]
    async fn mutators_unknown_id_return_not_found() {
        let repo = InMemoryCommanderRepository::new();
        let id = Uuid::new_v4();
        let r = repo.update_stress(id, 10).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "update_stress: {:?}", r);
        let r = repo.update_rank_and_xp(id, 2, 100).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "update_rank_and_xp: {:?}", r);
        let r = repo.set_veteran_trait(id, "Grizzled".into()).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "set_veteran_trait: {:?}", r);
        let r = repo.set_shattered(id).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "set_shattered: {:?}", r);
        let r = repo.set_kia(id).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "set_kia: {:?}", r);
    }
}
