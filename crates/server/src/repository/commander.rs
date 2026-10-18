use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sim_engine::config::SimConfig;
use sim_engine::game_types::OutcomeType;
use sim_engine::resolver::{stress_after_casualties, stress_after_deployment};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::commander_gen::{is_glorious_death, GloriousDeathCriteria};
use uuid::Uuid;

use super::{AcquiredVia, RepositoryError};

/// Converts the SMALLINT `stress` column to the sim's u8 stress level. Migration 0016's
/// `CHECK (stress BETWEEN 0 AND 100)` and `create_commander`'s clamp keep stored values in
/// range; the clamp here is defence in depth. The way back is a plain `i16::from(level)`.
pub fn stress_to_level(stress: i16) -> u8 {
    stress.clamp(0, 100) as u8
}

/// A one-shot mission's `active` row lives for milliseconds, so one older than this is a
/// crash leftover. `start_deployment` aborts it before inserting (§4g item 10). Revisit
/// when Phase 2 timed deployments make a long-lived `active` row legitimate.
pub const STALE_DEPLOYMENT_AFTER: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The `deployment_records.outcome` text. Exhaustive with no wildcard, so a new variant
/// is a compile error here and in the migration's CHECK, never a silent `{:?}` string.
pub fn outcome_db_str(o: &OutcomeType) -> &'static str {
    match o {
        OutcomeType::Success         => "Success",
        OutcomeType::Defeat          => "Defeat",
        OutcomeType::TacticalRetreat => "TacticalRetreat",
        OutcomeType::Wipeout         => "Wipeout",
    }
}

/// Rank-up side effects shared by `apply_mission_xp` and the mission route. Logs only;
/// nothing here is written to the input log (a rank-up is recomputable).
pub(crate) fn log_rank_up(commander_id: Uuid, from: i16, to: i16) {
    if to > from {
        tracing::info!(commander_id = %commander_id, from = from, to = to, "commander rank-up");
        // TODO(phase-2): ranks 2-4 — implement Field Citations shortlist generation and
        // citation_gate_pending resolution — see Commander_Field_Citations.md and Phase 2 §10.
        if to == 5 {
            // TODO(phase-2): offer Retirement Option — Commander_Field_Citations.md §4.
            tracing::info!(commander_id = %commander_id, "rank 5 reached");
        }
    }
}

/// The values `resolve_deployment` committed: the locked row's rank before the write,
/// the rank, stress and XP it wrote, and whether the mission killed the Commander
/// (a `Wipeout`, §4e) and the death was Glorious (its card is foil).
#[derive(Debug, Clone, Copy)]
pub struct AppliedEffects {
    pub old_rank: i16,
    pub new_rank: i16,
    pub stress:   i16,
    pub xp:       i32,
    pub is_kia:   bool,
    pub glorious: bool,
}

/// The mission's effects on a Commander, computed from the values `resolve_deployment`
/// read under its lock. Both backends call this, so the formula has one home.
fn locked_row_effects(
    stress: i16,
    xp: i32,
    rank: i16,
    kia: u32,
    tier: u8,
    outcome: &OutcomeType,
    cfg: &SimConfig,
) -> AppliedEffects {
    let stress = stress_after_casualties(stress_after_deployment(stress_to_level(stress), cfg), kia, cfg);
    let xp = xp + crate::commander_gen::xp_award(tier, outcome);
    AppliedEffects {
        old_rank: rank,
        new_rank: crate::commander_gen::rank_for_xp(xp),
        stress:   i16::from(stress),
        xp,
        is_kia:   false,
        glorious: false,
    }
}

/// Permadeath reads one signal: `Wipeout` (§4e). The Glorious check reads the locked
/// rank; a Wipeout pays 0 XP, so it equals the new rank. Returns `(died, glorious)`.
fn death_effects(rank: i16, tier: u8, ticker_contribution_pct: f64, outcome: &OutcomeType) -> (bool, bool) {
    let died = matches!(outcome, OutcomeType::Wipeout);
    let glorious = died && is_glorious_death(&GloriousDeathCriteria { rank, mission_tier: tier, ticker_contribution_pct });
    (died, glorious)
}

/// One queued death card in the in-memory backend; mirrors a `card_mint_pending` row.
#[derive(Debug, Clone)]
pub struct CardMintPending {
    pub card_mint_id:   Uuid,
    pub player_wallet:  Vec<u8>,
    pub campaign_id:    Uuid,
    pub commander_id:   Uuid,
    pub deployment_id:  Uuid,
    pub session_id:     Uuid,
    pub commander_name: String,
    pub rank:           i16,
    pub foil:           bool,
    /// Copied from the Commander row, as Postgres copies `c.commander_grade` (Reviewer 4a-2 F2).
    pub commander_grade: Option<String>,
}

/// `create_commander` stores stress in 0..=100 and keeps `update_stress`'s invariant:
/// a record that arrives at 100 is Shattered.
pub(super) fn clamp_new_record(mut r: CommanderRecord) -> CommanderRecord {
    r.stress = r.stress.clamp(0, 100);
    r.is_shattered = r.is_shattered || r.stress >= 100;
    r
}

/// GAP-61 step 1: a new Commander must carry its Grade, so no binary that is the rollback
/// target once §5b's `SET NOT NULL` lands can write a NULL. Both impls call it first,
/// before the clamp and any write.
pub(super) fn require_grade(r: &CommanderRecord) -> Result<(), RepositoryError> {
    if r.commander_grade.is_none() {
        return Err(RepositoryError::Internal("commander_grade is required".into()));
    }
    Ok(())
}

// ── Deployment records (brick 4g-1) ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub deployment_id: Uuid,
    pub commander_id:  Uuid,
    pub session_id:    Uuid,
    pub mission_id:    String,
    /// Supplied by the caller so the stale rule is testable without sleeping.
    pub started_at:    DateTime<Utc>,
    /// The Commander's owner and campaign, carried onto every `deployment_sections` row
    /// (7b-2). The composite FKs hold the Commander and each Section to these values.
    pub player_wallet: Vec<u8>,
    pub campaign_id:   Uuid,
    /// The Sections this deployment sends (7b-2). Empty is accepted here; the route
    /// refuses it.
    pub section_ids:   Vec<Uuid>,
}

/// What `start_deployment` did. A return value, not an error (7b-2 R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentStart {
    Started,
    /// The Commander already has an `active` deployment; nothing was written.
    CommanderBusy,
    /// A requested Section is in another `active` deployment; nothing was written.
    SectionBusy,
}

/// Mirrors `deployment_records`. `status` is `active` / `resolved` / `aborted`.
#[derive(Debug, Clone)]
pub struct DeploymentRecord {
    pub deployment_id: Uuid,
    pub commander_id:  Uuid,
    pub session_id:    Uuid,
    pub mission_id:    String,
    pub status:        String,
    pub outcome:       Option<String>,
    pub started_at:    DateTime<Utc>,
    pub closed_at:     Option<DateTime<Utc>>,
}

fn stale_cutoff(started_at: DateTime<Utc>) -> DateTime<Utc> {
    started_at - chrono::Duration::seconds(STALE_DEPLOYMENT_AFTER.as_secs() as i64)
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
    pub prng_seed_state: i64,
    /// The spawn Grade's display string (`roster::CommanderGrade::as_str`). Nullable only
    /// for the expand step: every Field Leader carries one. Not parsed here (CF3): the
    /// first reader that needs the enum parses it, and a `None` there is an integrity `500`.
    pub commander_grade: Option<String>,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

// ── CommanderRepository trait ────────────────────────────────────────────────

#[async_trait]
pub trait CommanderRepository: Send + Sync {
    /// Stores `commander_grade` as given. It never recomputes or validates the Grade against
    /// the traits: Story Event characters are hand-graded with any combination (Grade §6).
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

    /// Records an `active` deployment and its Sections. In one transaction it first aborts
    /// every `active` deployment older than `STALE_DEPLOYMENT_AFTER` that is this
    /// Commander's or holds a requested Section (7b-2 R4), then inserts the deployment,
    /// then its `deployment_sections` rows. `CommanderBusy` when the Commander already has
    /// an `active` deployment (`deployment_records_one_active`); `SectionBusy` when a
    /// requested Section is in one (`deployment_sections_one_active`). Either refusal
    /// writes nothing. Closing a deployment releases its Sections: in Postgres through the
    /// trigger `deployment_records_release_sections` (`0027`), so every close path does.
    async fn start_deployment(&self, d: &NewDeployment) -> Result<DeploymentStart, RepositoryError>;
    /// Marks an `active` deployment `aborted`. `NotFound` if it is not `active`.
    async fn abort_deployment(&self, deployment_id: Uuid) -> Result<(), RepositoryError>;
    /// The mission's atomic write (§4g item 6.5). Locks the Commander row, refuses a KIA
    /// Commander (`Internal`), and computes stress, XP and rank from the locked values, so
    /// a write committed since the caller's read is built on, never overwritten (4g-2).
    /// A Shattered Commander is not refused: the mission still happened. Writes stress
    /// (with `update_stress`'s Shattered invariant), then rank and XP, then — last — the
    /// close of the `active` deployment. On a `Wipeout` it also marks the Commander KIA
    /// and queues a `card_mint_pending` card (foil if Glorious) before the close (§4e).
    /// Any statement touching 0 rows is `NotFound` and nothing is applied. Returns what
    /// was committed.
    async fn resolve_deployment(
        &self,
        deployment_id: Uuid,
        commander_id: Uuid,
        kia: u32,
        tier: u8,
        ticker_contribution_pct: f64,
        outcome: &OutcomeType,
        cfg: &SimConfig,
    ) -> Result<AppliedEffects, RepositoryError>;
    /// Ordered by `started_at`.
    async fn list_deployments_by_commander(&self, commander_id: Uuid) -> Result<Vec<DeploymentRecord>, RepositoryError>;

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
        log_rank_up(commander_id, rec.rank, new_rank);
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

/// The permadeath writes (§4e, R1), run only inside `resolve_deployment`'s locked
/// transaction so §4c's writer rule holds: KIA, then the death card. The card is an
/// account-scoped snapshot of the record and the fatal deployment, so it outlives the
/// §4f campaign wipe. Not on the trait: the mission Wipeout is its only caller.
async fn mark_commander_kia(
    tx: &mut Transaction<'_, Postgres>,
    commander_id: Uuid,
    deployment_id: Uuid,
    glorious: bool,
    rank: i16,
) -> Result<(), RepositoryError> {
    // `set_kia`'s statement, byte for byte, so the two share one `.sqlx/` cache file.
    let r = sqlx::query!(
        "UPDATE commander_records SET is_kia = TRUE, updated_at = now() WHERE commander_id = $1",
        commander_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    if r.rows_affected() == 0 {
        return Err(RepositoryError::NotFound);
    }
    // No `status = 'active'` guard: the close checks that, last.
    // The rank is bound, not read from `c.rank`: inside this transaction that column
    // already holds the recomputed rank, while the card (and Glorious) is the rank the
    // Commander died at, the locked one (Reviewer 4e-1 F1).
    let r = sqlx::query!(
        r#"INSERT INTO card_mint_pending
            (card_mint_id, player_wallet, campaign_id, commander_id, deployment_id, session_id, commander_name, rank, foil, commander_grade)
        SELECT $1, c.player_wallet, c.campaign_id, c.commander_id, d.deployment_id, d.session_id, c.name, $5, $2, c.commander_grade
        FROM commander_records c
        JOIN deployment_records d ON d.commander_id = c.commander_id
        WHERE c.commander_id = $3 AND d.deployment_id = $4"#,
        Uuid::new_v4(),
        glorious,
        commander_id,
        deployment_id,
        rank,
    )
    .execute(&mut **tx)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    if r.rows_affected() == 0 {
        return Err(RepositoryError::NotFound);
    }
    // TODO(phase-2): apply_roster_grief_spike(player_id, deceased_commander_id, mission_understrength_factor) — §4c; Commander_Stress_System §1.
    Ok(())
}

/// The Commander INSERT on a caller's connection, so the join (5a-2) can run it inside its
/// own transaction. The Grade refusal and the clamp live here, so no caller can skip them.
/// `via` is the row's provenance (`acquired_via`, 0030): `create_commander` passes `Hired`.
pub(crate) async fn insert_commander(
    conn: &mut PgConnection,
    r: CommanderRecord,
    via: AcquiredVia,
) -> Result<(), RepositoryError> {
    require_grade(&r)?;
    let r = clamp_new_record(r);
    sqlx::query!(
            r#"INSERT INTO commander_records (
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, prng_seed_state, commander_grade, acquired_via
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)"#,
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
        r.prng_seed_state,
        r.commander_grade,
        via.as_str(),
    )
    .execute(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(())
}

#[async_trait]
impl CommanderRepository for PostgresCommanderRepository {
    async fn create_commander(&self, r: CommanderRecord) -> Result<(), RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        insert_commander(&mut conn, r, AcquiredVia::Hired).await
    }

    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, prng_seed_state, commander_grade, created_at, updated_at
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
            prng_seed_state: r.prng_seed_state,
            commander_grade: r.commander_grade,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }))
    }

    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError> {
        let clamped = i16::max(0, i16::min(100, stress));
        // One statement: no update_stress call can leave stress at 100 without Shattered. The formula
        // stays in Rust; SQL only compares the already-clamped value. The literal is typed
        // SMALLINT so Postgres deduces one type for $1 (an int4 literal makes it ambiguous).
        // resolve_deployment runs this exact statement text inside its transaction.
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
                is_shattered, is_kia, prng_seed_state, commander_grade, created_at, updated_at
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
            prng_seed_state: r.prng_seed_state,
            commander_grade: r.commander_grade,
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

    async fn start_deployment(&self, d: &NewDeployment) -> Result<DeploymentStart, RepositoryError> {
        // Dropping `tx` without commit rolls back the stale abort and the deployment insert,
        // so a refusal writes nothing.
        let mut tx = self.pool.begin().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        // R4: a crashed deployment that still holds a requested Section is stale too,
        // whoever its Commander is. The trigger releases the Sections of what this aborts.
        sqlx::query!(
            "UPDATE deployment_records SET status = 'aborted', closed_at = $2 WHERE status = 'active' AND started_at < $3 AND (commander_id = $1 OR deployment_id IN (SELECT deployment_id FROM deployment_sections WHERE section_id = ANY($4) AND released_at IS NULL))",
            d.commander_id,
            d.started_at,
            stale_cutoff(d.started_at),
            d.section_ids.as_slice(),
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let inserted = sqlx::query!(
            "INSERT INTO deployment_records (deployment_id, commander_id, session_id, mission_id, status, started_at) VALUES ($1, $2, $3, $4, 'active', $5) ON CONFLICT (commander_id) WHERE status = 'active' DO NOTHING",
            d.deployment_id,
            d.commander_id,
            d.session_id,
            d.mission_id.clone(),
            d.started_at,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if inserted.rows_affected() != 1 {
            return Ok(DeploymentStart::CommanderBusy);
        }
        // R5: sorted, so two starts that share Sections lock them in the same order and
        // cannot deadlock. The input log keeps the request's order.
        let mut sorted = d.section_ids.clone();
        sorted.sort();
        let joined = sqlx::query!(
            "INSERT INTO deployment_sections (deployment_id, section_id, commander_id, player_wallet, campaign_id) SELECT $1, unnest($2::uuid[]), $3, $4, $5 ON CONFLICT (section_id) WHERE released_at IS NULL DO NOTHING",
            d.deployment_id,
            sorted.as_slice(),
            d.commander_id,
            d.player_wallet.as_slice(),
            d.campaign_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if joined.rows_affected() < sorted.len() as u64 {
            return Ok(DeploymentStart::SectionBusy);
        }
        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(DeploymentStart::Started)
    }

    async fn abort_deployment(&self, deployment_id: Uuid) -> Result<(), RepositoryError> {
        let result = sqlx::query!(
            "UPDATE deployment_records SET status = 'aborted', closed_at = now() WHERE deployment_id = $1 AND status = 'active'",
            deployment_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn resolve_deployment(
        &self,
        deployment_id: Uuid,
        commander_id: Uuid,
        kia: u32,
        tier: u8,
        ticker_contribution_pct: f64,
        outcome: &OutcomeType,
        cfg: &SimConfig,
    ) -> Result<AppliedEffects, RepositoryError> {
        // Dropping `tx` without commit rolls everything back, so every early return below
        // leaves stress, XP, rank, KIA, the card queue and the deployment exactly as they were.
        let mut tx = self.pool.begin().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        // The row lock is held to commit: any other writer of this Commander waits, and
        // the effects below are computed from what it committed before us.
        let row = sqlx::query!(
            "SELECT stress, xp, rank, is_kia FROM commander_records WHERE commander_id = $1 FOR NO KEY UPDATE",
            commander_id,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?
        .ok_or(RepositoryError::NotFound)?;
        if row.is_kia {
            return Err(RepositoryError::Internal("commander is KIA".into()));
        }
        let mut eff = locked_row_effects(row.stress, row.xp, row.rank, kia, tier, outcome, cfg);
        // The two UPDATE texts are `update_stress`'s and `update_rank_and_xp`'s, unchanged.
        let r = sqlx::query!(
            "UPDATE commander_records SET stress = $1, is_shattered = is_shattered OR $1 >= 100::SMALLINT, updated_at = now() WHERE commander_id = $2",
            eff.stress,
            commander_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if r.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        let r = sqlx::query!(
            "UPDATE commander_records SET rank = $1, xp = $2, updated_at = now() WHERE commander_id = $3",
            eff.new_rank,
            eff.xp,
            commander_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if r.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        let (died, glorious) = death_effects(row.rank, tier, ticker_contribution_pct, outcome);
        if died {
            mark_commander_kia(&mut tx, commander_id, deployment_id, glorious, row.rank).await?;
        }
        eff.is_kia = died;
        eff.glorious = glorious;
        // Last, and guarded on `active`: a closed or foreign deployment rolls back the above.
        let r = sqlx::query!(
            "UPDATE deployment_records SET status = 'resolved', outcome = $1, closed_at = now() WHERE deployment_id = $2 AND commander_id = $3 AND status = 'active'",
            outcome_db_str(outcome),
            deployment_id,
            commander_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if r.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(eff)
    }

    async fn list_deployments_by_commander(&self, commander_id: Uuid) -> Result<Vec<DeploymentRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT deployment_id, commander_id, session_id, mission_id, status, outcome, started_at, closed_at
               FROM deployment_records WHERE commander_id = $1 ORDER BY started_at"#,
            commander_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(|r| DeploymentRecord {
            deployment_id: r.deployment_id,
            commander_id:  r.commander_id,
            session_id:    r.session_id,
            mission_id:    r.mission_id,
            status:        r.status,
            outcome:       r.outcome,
            started_at:    r.started_at,
            closed_at:     r.closed_at,
        }).collect())
    }
}

// ── InMemoryCommanderRepository (unit-test stub) ─────────────────────────────

/// The data behind the in-memory deployment lock. The card queue lives here so that
/// `resolve_deployment` writes KIA, the card and the close in one critical section.
#[derive(Default)]
struct DeploymentStore {
    records:  HashMap<Uuid, DeploymentRecord>,
    cards:    Vec<CardMintPending>,
    /// Mirrors `deployment_sections`: one row per (deployment, Section).
    sections: Vec<DeploymentSectionRow>,
}

/// One `deployment_sections` row. `released_at` is `None` while the deployment is active.
#[derive(Debug, Clone)]
struct DeploymentSectionRow {
    deployment_id: Uuid,
    section_id:    Uuid,
    released_at:   Option<DateTime<Utc>>,
}

/// Stands in for the trigger `deployment_records_release_sections`: every in-memory close
/// calls it.
fn release_sections(sections: &mut [DeploymentSectionRow], deployment_id: Uuid, at: DateTime<Utc>) {
    for row in sections.iter_mut().filter(|r| r.deployment_id == deployment_id && r.released_at.is_none()) {
        row.released_at = Some(at);
    }
}

impl DeploymentStore {

    fn holds_active_section(&self, deployment_id: Uuid, section_ids: &[Uuid]) -> bool {
        self.sections.iter().any(|r| r.deployment_id == deployment_id && r.released_at.is_none() && section_ids.contains(&r.section_id))
    }
}

/// In-memory stub. It models the one-active-deployment rules for Commanders and Sections,
/// the stale rule (R4 included) and the release of Sections at the stale abort, the abort
/// and the resolve close. **Postgres-only:** the three composite FKs on
/// `deployment_sections` (same owner, same campaign), the `RESTRICT`s and the release
/// trigger; this impl cannot see Sections, so the route's `404` is the check here.
pub struct InMemoryCommanderRepository {
    store: DashMap<Uuid, CommanderRecord>,
    /// Every deployment method holds this one lock for its whole body, which stands in
    /// for Postgres' transaction and the partial unique index.
    deployments: Mutex<DeploymentStore>,
}

impl InMemoryCommanderRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new(), deployments: Mutex::new(DeploymentStore::default()) }
    }

    fn lock_deployments(&self) -> Result<std::sync::MutexGuard<'_, DeploymentStore>, RepositoryError> {
        self.deployments.lock().map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    /// The queued death cards, for tests. Not on the trait: nothing in production reads
    /// the queue until Phase 2 §6d.
    pub fn card_mint_pending(&self) -> Result<Vec<CardMintPending>, RepositoryError> {
        Ok(self.lock_deployments()?.cards.clone())
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
        require_grade(&record)?;
        let record = clamp_new_record(record);
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

    async fn start_deployment(&self, d: &NewDeployment) -> Result<DeploymentStart, RepositoryError> {
        let mut deps = self.lock_deployments()?;
        // Mirrors the Postgres foreign key to commander_records.
        if !self.store.contains_key(&d.commander_id) {
            return Err(RepositoryError::Internal(format!(
                "deployment_records.commander_id {} is not in commander_records", d.commander_id
            )));
        }
        // Validate before mutating: a refusal leaves the store exactly as it was, as the
        // dropped Postgres transaction does (the stale abort included).
        let cutoff = stale_cutoff(d.started_at);
        let stale: Vec<Uuid> = deps.records.values()
            .filter(|dep| dep.status == "active" && dep.started_at < cutoff)
            .filter(|dep| dep.commander_id == d.commander_id || deps.holds_active_section(dep.deployment_id, &d.section_ids))
            .map(|dep| dep.deployment_id)
            .collect();
        let live = |dep: &DeploymentRecord| dep.status == "active" && !stale.contains(&dep.deployment_id);
        if deps.records.values().any(|dep| dep.commander_id == d.commander_id && live(dep)) {
            return Ok(DeploymentStart::CommanderBusy);
        }
        // As `deployment_sections_one_active` does, this reads only `released_at`: an
        // unreleased row blocks whatever its deployment's status, so a close that forgets to
        // release is caught (NG6). Rows of deployments aborted just above don't count.
        let section_busy = deps.sections.iter().any(|r| {
            r.released_at.is_none() && !stale.contains(&r.deployment_id) && d.section_ids.contains(&r.section_id)
        });
        if section_busy {
            return Ok(DeploymentStart::SectionBusy);
        }
        for id in &stale {
            if let Some(dep) = deps.records.get_mut(id) {
                dep.status = "aborted".into();
                dep.closed_at = Some(d.started_at);
            }
            release_sections(&mut deps.sections, *id, d.started_at);
        }
        for section_id in &d.section_ids {
            deps.sections.push(DeploymentSectionRow { deployment_id: d.deployment_id, section_id: *section_id, released_at: None });
        }
        deps.records.insert(d.deployment_id, DeploymentRecord {
            deployment_id: d.deployment_id,
            commander_id:  d.commander_id,
            session_id:    d.session_id,
            mission_id:    d.mission_id.clone(),
            status:        "active".into(),
            outcome:       None,
            started_at:    d.started_at,
            closed_at:     None,
        });
        Ok(DeploymentStart::Started)
    }

    async fn abort_deployment(&self, deployment_id: Uuid) -> Result<(), RepositoryError> {
        let mut deps = self.lock_deployments()?;
        let now = Utc::now();
        let dep = deps
            .records
            .get_mut(&deployment_id)
            .filter(|dep| dep.status == "active")
            .ok_or(RepositoryError::NotFound)?;
        dep.status = "aborted".into();
        dep.closed_at = Some(now);
        release_sections(&mut deps.sections, deployment_id, now);
        Ok(())
    }

    async fn resolve_deployment(
        &self,
        deployment_id: Uuid,
        commander_id: Uuid,
        kia: u32,
        tier: u8,
        ticker_contribution_pct: f64,
        outcome: &OutcomeType,
        cfg: &SimConfig,
    ) -> Result<AppliedEffects, RepositoryError> {
        let mut deps = self.lock_deployments()?;
        let DeploymentStore { records, cards, sections } = &mut *deps;
        // Validate everything before mutating anything: no partial write on any refusal.
        let dep = records
            .get_mut(&deployment_id)
            .filter(|dep| dep.commander_id == commander_id && dep.status == "active")
            .ok_or(RepositoryError::NotFound)?;
        // The shard guard stands in for Postgres' row lock: read, compute and write under it.
        let mut rec = self.store.get_mut(&commander_id).ok_or(RepositoryError::NotFound)?;
        if rec.is_kia {
            return Err(RepositoryError::Internal("commander is KIA".into()));
        }
        let mut eff = locked_row_effects(rec.stress, rec.xp, rec.rank, kia, tier, outcome, cfg);
        let (died, glorious) = death_effects(rec.rank, tier, ticker_contribution_pct, outcome);
        rec.stress = eff.stress;
        rec.is_shattered = rec.is_shattered || eff.stress >= 100;
        rec.rank = eff.new_rank;
        rec.xp = eff.xp;
        if died {
            rec.is_kia = true;
            cards.push(CardMintPending {
                card_mint_id:   Uuid::new_v4(),
                player_wallet:  rec.player_wallet.clone(),
                campaign_id:    rec.campaign_id,
                commander_id,
                deployment_id,
                session_id:     dep.session_id,
                commander_name: rec.name.clone(),
                rank:           eff.old_rank, // the locked rank, as Postgres binds (F1)
                foil:           glorious,
                commander_grade: rec.commander_grade.clone(),
            });
            // TODO(phase-2): grief spike — see `mark_commander_kia` (Postgres section).
        }
        eff.is_kia = died;
        eff.glorious = glorious;
        let now = Utc::now();
        dep.status = "resolved".into();
        dep.outcome = Some(outcome_db_str(outcome).into());
        dep.closed_at = Some(now);
        release_sections(sections, deployment_id, now);
        Ok(eff)
    }

    async fn list_deployments_by_commander(&self, commander_id: Uuid) -> Result<Vec<DeploymentRecord>, RepositoryError> {
        let deps = self.lock_deployments()?;
        let mut out: Vec<DeploymentRecord> = deps
            .records
            .values()
            .filter(|dep| dep.commander_id == commander_id)
            .cloned()
            .collect();
        out.sort_by_key(|dep| dep.started_at);
        Ok(out)
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
            origin:          sim_engine::roster::CommanderOrigin::HumanCorporate.as_str().into(),
            faction_bias:    sim_engine::roster::FactionBias::UnionAgitator.as_str().into(),
            specialization:  sim_engine::roster::CommanderSpecialization::TrenchSweeper.as_str().into(),
            fatal_flaw:      sim_engine::roster::CommanderFlaw::Embezzler.as_str().into(),
            veteran_trait:   None,
            is_shattered:    false,
            is_kia:          false,
            prng_seed_state: 0,
            commander_grade: Some(sim_engine::roster::CommanderGrade::StandardIssue.as_str().into()),
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

    // ── deployments + create clamp (brick 4g-1) ──────────────────────────────

    fn new_deployment(commander_id: Uuid, started_at: DateTime<Utc>) -> NewDeployment {
        with_sections(commander_id, started_at, vec![])
    }

    fn with_sections(commander_id: Uuid, started_at: DateTime<Utc>, section_ids: Vec<Uuid>) -> NewDeployment {
        NewDeployment {
            deployment_id: Uuid::new_v4(),
            commander_id,
            session_id: Uuid::new_v4(),
            mission_id: "GUARD_DUTY".into(),
            started_at,
            player_wallet: vec![7u8; 32],
            campaign_id: Uuid::nil(),
            section_ids,
        }
    }

    /// Two Commanders in one in-memory repository.
    async fn two_commander_repo() -> (InMemoryCommanderRepository, Uuid, Uuid) {
        let repo = InMemoryCommanderRepository::new();
        let a = crate::commander_gen::generate_commander(vec![7u8; 32], Uuid::nil(), 42);
        let b = crate::commander_gen::generate_commander(vec![7u8; 32], Uuid::nil(), 43);
        let (ida, idb) = (a.commander_id, b.commander_id);
        repo.create_commander(a).await.unwrap();
        repo.create_commander(b).await.unwrap();
        (repo, ida, idb)
    }

    /// 7b-2 #2 — a second Commander asking for a held Section gets `SectionBusy`, and the
    /// store is unchanged: no deployment row for it, the holder still active.
    #[tokio::test]
    async fn in_memory_start_deployment_refuses_a_busy_section() {
        let (repo, a, b) = two_commander_repo().await;
        let (s1, s2) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Utc::now();
        assert_eq!(repo.start_deployment(&with_sections(a, now, vec![s1])).await.unwrap(), DeploymentStart::Started);
        let r = repo.start_deployment(&with_sections(b, now, vec![s2, s1])).await.unwrap();
        assert_eq!(r, DeploymentStart::SectionBusy);
        assert!(repo.list_deployments_by_commander(b).await.unwrap().is_empty(), "the refused start wrote a deployment");
        let held = repo.list_deployments_by_commander(a).await.unwrap();
        assert_eq!((held.len(), held[0].status.as_str()), (1, "active"));
        let deps = repo.lock_deployments().unwrap();
        assert_eq!(deps.sections.len(), 1, "the refused start wrote join rows");
        assert_eq!(deps.sections[0].section_id, s1);
    }

    /// 7b-2 #3 — each close path (stale abort, abort, resolve close) releases the
    /// deployment's Sections, so another Commander can start with them.
    #[tokio::test]
    async fn in_memory_close_paths_release_sections() {
        let cfg = SimConfig::default();
        let now = Utc::now();
        for path in ["stale", "abort", "resolve"] {
            let (repo, a, b) = two_commander_repo().await;
            let s = Uuid::new_v4();
            let started = if path == "stale" { now - chrono::Duration::minutes(6) } else { now };
            let first = with_sections(a, started, vec![s]);
            assert_eq!(repo.start_deployment(&first).await.unwrap(), DeploymentStart::Started);
            match path {
                "abort" => repo.abort_deployment(first.deployment_id).await.unwrap(),
                "resolve" => {
                    repo.resolve_deployment(first.deployment_id, a, 0, 1, 0.0, &OutcomeType::Success, &cfg).await.unwrap();
                }
                _ => {} // the next start's stale rule closes it
            }
            let r = repo.start_deployment(&with_sections(b, now, vec![s])).await.unwrap();
            assert_eq!(r, DeploymentStart::Started, "the Section must start again after the {path} close");
            let firsts = repo.list_deployments_by_commander(a).await.unwrap();
            assert_ne!(firsts[0].status, "active", "the {path} close left the first deployment active");
        }
    }

    #[tokio::test]
    async fn in_memory_start_deployment_refuses_a_second_active_row() {
        let (repo, id) = seeded_repo(0, 1, |_| {}).await;
        let now = Utc::now();
        assert_eq!(repo.start_deployment(&new_deployment(id, now)).await.unwrap(), DeploymentStart::Started);
        assert_eq!(repo.start_deployment(&new_deployment(id, now)).await.unwrap(), DeploymentStart::CommanderBusy, "second active row must be refused");
        let deps = repo.list_deployments_by_commander(id).await.unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].status, "active");
    }

    #[tokio::test]
    async fn in_memory_start_deployment_aborts_a_stale_active_row() {
        let (repo, id) = seeded_repo(0, 1, |_| {}).await;
        let now = Utc::now();
        let stale = new_deployment(id, now - chrono::Duration::minutes(6));
        assert_eq!(repo.start_deployment(&stale).await.unwrap(), DeploymentStart::Started);
        let fresh = new_deployment(id, now);
        assert_eq!(repo.start_deployment(&fresh).await.unwrap(), DeploymentStart::Started, "a stale active row must not block the next start");
        let deps = repo.list_deployments_by_commander(id).await.unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].deployment_id, stale.deployment_id);
        assert_eq!(deps[0].status, "aborted");
        assert_eq!(deps[0].closed_at, Some(now));
        assert!(deps[0].outcome.is_none(), "an aborted deployment has no outcome");
        assert_eq!(deps[1].status, "active");
    }

    #[tokio::test]
    async fn in_memory_resolve_deployment_on_a_closed_row_changes_nothing() {
        let (repo, id) = seeded_repo(88, 1, |r| r.stress = 30).await;
        let d = new_deployment(id, Utc::now());
        assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
        repo.abort_deployment(d.deployment_id).await.unwrap();
        // Applied, these inputs would Shatter the Commander and rank them up.
        let cfg = SimConfig::default();
        let would_stress = stress_after_casualties(stress_after_deployment(30, &cfg), 12, &cfg);
        let would_rank = crate::commander_gen::rank_for_xp(88 + crate::commander_gen::xp_award(5, &OutcomeType::Success));
        assert_eq!((would_stress, would_rank > 1), (100, true), "precondition");
        let r = repo.resolve_deployment(d.deployment_id, id, 12, 5, 0.0, &OutcomeType::Success, &cfg).await;
        assert!(matches!(r, Err(RepositoryError::NotFound)), "got {:?}", r);
        let rec = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!((rec.stress, rec.xp, rec.rank, rec.is_shattered), (30, 88, 1, false));
        let deps = repo.list_deployments_by_commander(id).await.unwrap();
        assert_eq!(deps[0].status, "aborted");
    }

    /// 4g-2 test (b), in-memory: a Commander killed mid-mission is refused, and nothing
    /// changes — the deployment stays `active` for the route to abort.
    #[tokio::test]
    async fn in_memory_resolve_deployment_refuses_a_kia_commander() {
        let (repo, id) = seeded_repo(88, 1, |r| r.stress = 30).await;
        let d = new_deployment(id, Utc::now());
        assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
        repo.set_kia(id).await.unwrap();
        let cfg = SimConfig::default();
        let r = repo.resolve_deployment(d.deployment_id, id, 10, 5, 0.0, &OutcomeType::Success, &cfg).await;
        assert!(matches!(r, Err(RepositoryError::Internal(_))), "got {:?}", r);
        let rec = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!((rec.stress, rec.xp, rec.rank, rec.is_shattered), (30, 88, 1, false));
        let deps = repo.list_deployments_by_commander(id).await.unwrap();
        assert_eq!(deps[0].status, "active");
    }

    /// 4e-1: a Wipeout marks the Commander KIA and queues one snapshot card in the same
    /// critical section. Rank 3 on a tier-4 mission is Glorious, so the card is foil.
    #[tokio::test]
    async fn test_mark_commander_kia_in_memory_wipeout_queues_one_card() {
        let (repo, id) = seeded_repo(400, 3, |r| r.stress = 30).await;
        let d = new_deployment(id, Utc::now());
        assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
        let cfg = SimConfig::default();
        let eff = repo
            .resolve_deployment(d.deployment_id, id, 2, 4, 0.0, &OutcomeType::Wipeout, &cfg)
            .await
            .unwrap();
        assert!(eff.is_kia && eff.glorious, "got {:?}", eff);
        let rec = repo.get_commander(id).await.unwrap().unwrap();
        assert!(rec.is_kia);
        let cards = repo.card_mint_pending().unwrap();
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert!(card.foil);
        assert_eq!(
            (&card.commander_name, &card.player_wallet, card.campaign_id, card.rank),
            (&rec.name, &rec.player_wallet, rec.campaign_id, 3)
        );
        assert_eq!((card.commander_id, card.deployment_id, card.session_id), (id, d.deployment_id, d.session_id));
        assert!(rec.commander_grade.is_some(), "precondition: the seeded Commander carries a Grade");
        assert_eq!(card.commander_grade, rec.commander_grade, "the card copies the Commander's Grade (F2)");
        let deps = repo.list_deployments_by_commander(id).await.unwrap();
        assert_eq!(deps[0].outcome.as_deref(), Some("Wipeout"));
    }

    /// Reviewer 4e-1 F1: a Rank 5 Commander with low XP is re-ranked by the resolve, but
    /// dies at Rank 5 (which is what makes the death Glorious), so the card says 5.
    #[tokio::test]
    async fn card_stamps_the_locked_rank() {
        let (repo, id) = seeded_repo(0, 5, |r| r.stress = 30).await;
        let d = new_deployment(id, Utc::now());
        assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
        let cfg = SimConfig::default();
        let eff = repo
            .resolve_deployment(d.deployment_id, id, 2, 1, 0.0, &OutcomeType::Wipeout, &cfg)
            .await
            .unwrap();
        assert!(eff.is_kia && eff.glorious, "got {:?}", eff);
        assert!(eff.new_rank < 5, "precondition: the resolve re-ranks from XP, got {:?}", eff);
        let cards = repo.card_mint_pending().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].rank, 5, "the card stamps the rank the Commander died at");
        assert!(cards[0].foil);
    }

    #[tokio::test]
    async fn test_mark_commander_kia_in_memory_non_wipeout_writes_nothing() {
        let (repo, id) = seeded_repo(400, 3, |r| r.stress = 30).await;
        let d = new_deployment(id, Utc::now());
        assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
        let cfg = SimConfig::default();
        let eff = repo
            .resolve_deployment(d.deployment_id, id, 2, 4, 0.0, &OutcomeType::Defeat, &cfg)
            .await
            .unwrap();
        assert!(!eff.is_kia && !eff.glorious, "got {:?}", eff);
        assert!(!repo.get_commander(id).await.unwrap().unwrap().is_kia);
        assert!(repo.card_mint_pending().unwrap().is_empty());
    }

    #[tokio::test]
    async fn in_memory_create_commander_clamps_stress_and_shatters_at_100() {
        let (repo, id) = seeded_repo(0, 1, |r| r.stress = 150).await;
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 100);
        assert!(r.is_shattered, "a record created at 100 is Shattered");

        let (repo, id) = seeded_repo(0, 1, |r| r.stress = -5).await;
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 0);
        assert!(!r.is_shattered);
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
