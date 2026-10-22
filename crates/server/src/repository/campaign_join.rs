//! The campaign join (5a-2b): a player enters a campaign and receives the free starter set
//! (ODQ-32, ODQ-41). Shared types, the trait and the Postgres impl. The in-memory impl lives
//! in `campaign_join_in_memory.rs`, so this file never calls a `create_*` trait method
//! (Reviewer 5a-0 CF4 is a file-level grep).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use chrono::{DateTime, Utc};

use super::campaign::{activate_row, campaign_row, insert_campaign, CampaignInstance, CampaignLifecycle, NewCampaignInstance};
use super::commander::insert_commander;
use super::crew::{insert_crew, CrewRecord};
use super::foreman::insert_foreman;
use super::membership::{insert_membership, member_rows, PlayerCampaignMembership};
use super::section::{insert_section, starter_section};
use super::{AcquiredVia, RepositoryError};
use crate::commander_gen::generate_commander;
use crate::foreman_gen::generate_foreman;

/// The Phase 1 Sector player cap (S6): the canon floor of 200–500. Passed into
/// `join_campaign` as a parameter, so tests use a small cap.
pub const SECTOR_PLAYER_CAP: i64 = 200;

// sqlx 0.8.6's migrator key is `0x3d32ad9e * crc32_iso_hdlc(db_name)`
// (`sqlx-postgres-0.8.6/src/migrate.rs:326–330`): both factors are ≥ 0 and the product is
// < `i64::MAX`, so the migrator's key is never negative and cannot equal this one.
/// The advisory key that serialises every join and every launch, server-wide. Public so the
/// integration tests can filter `pg_locks` by it rather than repeat the literal.
pub const JOIN_ADVISORY_KEY: i64 = -0x4D4D_4A4F_494E;
const _: () = assert!(JOIN_ADVISORY_KEY < 0);

/// The seeds a join rolls from. The route fills them with `generate_seed()` twice and
/// `generate_seed() as i64`; tests pin literals. Equal seeds are **not refused**: a collision
/// (1 in 2³²) only correlates the Commander's and the Foreman's Grade bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinSeeds {
    pub commander: u32,
    pub foreman:   u32,
    /// The new campaign's `map_seed`, used only when the join creates a campaign.
    pub map:       i64,
}

/// A member's campaign and its four starter rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StarterIds {
    pub campaign_id:  Uuid,
    pub commander_id: Uuid,
    pub foreman_id:   Uuid,
    pub section_id:   Uuid,
    pub crew_id:      Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    /// A new membership and starter set were written.
    Joined(StarterIds),
    /// The wallet already has a live membership; nothing was written.
    AlreadyMember(StarterIds),
}

/// The launch's view of a campaign under the join lock (5a-3a). Outcomes, not errors (the
/// `DeploymentStart` precedent).
#[derive(Debug)]
pub enum LaunchStart {
    /// The campaign was `Pending` with 1..=`max_members` members and is now `Active`.
    /// `campaign` is the snapshot read before the activation; `members` are in
    /// `joined_at ASC` order.
    Started { campaign: CampaignInstance, members: Vec<PlayerCampaignMembership> },
    NotFound,
    NotPending,
    NoMembers,
    TooManyMembers(usize),
}

/// What a replacement claim did (5a-4, ODQ-33; usable since 5a-5, ODQ-46). Outcomes, not errors.
/// A Commander is **usable** when it is neither KIA nor Shattered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementOutcome {
    /// No Commander is usable; this is the new replacement's `commander_id`.
    Granted(Uuid),
    /// No membership row for this wallet and campaign.
    NotMember,
    /// The member has a usable Commander (a usable replacement counts).
    HasUsable,
}

/// The refusal for a member whose starter set is missing or partial.
pub(crate) const NO_STARTER_SET: &str = "campaign join: member has no starter set";

/// The refusal for a member with no Commander at all. The join writes a starter with every
/// membership, so only test rows can produce it; the route maps it to an integrity `500`.
pub(crate) const NO_COMMANDERS: &str = "replacement claim: member has no Commanders";

/// A player entering a campaign.
///
/// - **Precondition:** a `player_accounts` row exists for `wallet`. In production,
///   `post_auth_verify` upserts it before issuing a session. Without it, Postgres fails
///   `23503` → `Internal`.
/// - **S1/S7:** a membership is live when its campaign is `Pending`, `Active` or `Ending`,
///   **whatever its `is_active`**. A live membership returns `AlreadyMember` (oldest
///   `joined_at`, then `campaign_id`). The join never reactivates.
/// - **Invariant:** at most one starter per `(campaign, wallet)` is guaranteed by the
///   `<t>_one_starter` index, and exactly one by this transaction.
/// - A member with no starter set, or a partial one, is
///   `Internal("campaign join: member has no starter set")`. Only test rows can produce it,
///   because the join is the only membership writer. The route maps it to an integrity `500`.
#[async_trait]
pub trait CampaignJoinRepository: Send + Sync {
    async fn join_campaign(
        &self,
        wallet: &[u8],
        cap: i64,
        seeds: JoinSeeds,
    ) -> Result<JoinOutcome, RepositoryError>;

    /// The admin launch's state change, serialised against every join (Reviewer 5a-2b F1):
    /// under the join lock it reads the campaign and its members, refuses an empty or
    /// over-`max_members` campaign, and activates it with `ends_at` and `victory_tickers`.
    /// Once `Active`, no join can enter the campaign, so map gen, gateway hexes, Redis and
    /// `campaign_started` run after this returns, from `Started`'s members.
    async fn begin_launch(
        &self,
        campaign_id: Uuid,
        max_members: usize,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<LaunchStart, RepositoryError>;

    // TODO(phase-2-§10j): alpha-only replacement; removed when the Phase 2 Bootcamp hire ships (ODQ-33).
    /// The alpha KIA replacement (5a-4, ODQ-33). In one transaction under the member's
    /// membership row lock (`FOR UPDATE`, never `JOIN_ADVISORY_KEY`): `NotMember` without a
    /// membership row (whatever its `is_active`); `Internal(NO_COMMANDERS)` when the member has
    /// no Commander at all; `HasUsable` when any Commander is neither KIA nor Shattered
    /// (ODQ-46, 5a-5); otherwise a Rank 1
    /// Commander rolled from `seed` and written with `acquired_via = 'replacement'`. The seed is
    /// provenance, not a logged input. Off every hire counter.
    async fn claim_replacement(
        &self,
        campaign_id: Uuid,
        wallet: &[u8],
        seed: u32,
    ) -> Result<ReplacementOutcome, RepositoryError>;
}

// ── Postgres reads, on the transaction's connection only ─────────────────────
// A pool read inside the tx would need a second connection and can starve the seam tests.

/// Takes the join lock for the rest of the transaction.
pub(crate) async fn lock_joins(conn: &mut PgConnection) -> Result<(), RepositoryError> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", JOIN_ADVISORY_KEY)
        .execute(&mut *conn)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(())
}

/// The wallet's live membership (S1/S7): any row, active or not, in a `Pending`, `Active` or
/// `Ending` campaign; the oldest first.
pub(crate) async fn live_membership(
    conn: &mut PgConnection,
    wallet: &[u8],
) -> Result<Option<Uuid>, RepositoryError> {
    sqlx::query_scalar!(
        r#"SELECT m.campaign_id
           FROM player_campaign_membership m
           JOIN campaign_instances c ON c.campaign_id = m.campaign_id
           WHERE m.wallet_address = $1 AND c.state IN ('Pending','Active','Ending')
           ORDER BY m.joined_at, m.campaign_id
           LIMIT 1"#,
        wallet,
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))
}

/// The oldest `Pending` campaign with fewer than `cap` members. Counts `m.membership_id`,
/// never `count(*)`: under the `LEFT JOIN`, `count(*)` counts an empty campaign as 1. Every
/// membership row counts (S7). The `campaign_id` tiebreak is load-bearing: Postgres `now()`
/// ties within a transaction (Reviewer 5a-2a CF1). No `FOR UPDATE`: the advisory lock
/// serialises every join, and `FOR UPDATE` is illegal with `GROUP BY`.
pub(crate) async fn pick_pending_campaign(
    conn: &mut PgConnection,
    cap: i64,
) -> Result<Option<Uuid>, RepositoryError> {
    sqlx::query_scalar!(
        r#"SELECT c.campaign_id
           FROM campaign_instances c
           LEFT JOIN player_campaign_membership m ON m.campaign_id = c.campaign_id
           WHERE c.state = 'Pending'
           GROUP BY c.campaign_id
           HAVING count(m.membership_id) < $1
           ORDER BY c.created_at, c.campaign_id
           LIMIT 1"#,
        cap,
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))
}

/// The member's four starter ids, read by `acquired_via = 'starter'`. Each `<t>_one_starter`
/// index makes each subquery return at most one row; a KIA starter is still returned.
pub(crate) async fn starter_ids(
    conn: &mut PgConnection,
    campaign_id: Uuid,
    wallet: &[u8],
) -> Result<Option<StarterIds>, RepositoryError> {
    let row = sqlx::query!(
        r#"SELECT
            (SELECT commander_id FROM commander_records
              WHERE campaign_id = $1 AND player_wallet = $2 AND acquired_via = 'starter') AS "commander_id?",
            (SELECT foreman_id FROM foreman_records
              WHERE campaign_id = $1 AND player_wallet = $2 AND acquired_via = 'starter') AS "foreman_id?",
            (SELECT section_id FROM section_records
              WHERE campaign_id = $1 AND player_wallet = $2 AND acquired_via = 'starter') AS "section_id?",
            (SELECT crew_id FROM crew_records
              WHERE campaign_id = $1 AND player_wallet = $2 AND acquired_via = 'starter') AS "crew_id?""#,
        campaign_id,
        wallet,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(match (row.commander_id, row.foreman_id, row.section_id, row.crew_id) {
        (Some(commander_id), Some(foreman_id), Some(section_id), Some(crew_id)) => {
            Some(StarterIds { campaign_id, commander_id, foreman_id, section_id, crew_id })
        }
        _ => None,
    })
}

// ── PostgresCampaignJoinRepository ───────────────────────────────────────────

/// A test hook awaited once inside a transaction of this repository (see `with_pause_seam`):
/// the join, the launch and the replacement claim each fire it at one point.
pub type PauseHook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The join as one Postgres transaction.
///
/// - **The lock is global** (`JOIN_ADVISORY_KEY`, the transaction's first statement) and
///   serialises every join server-wide. This is accepted at alpha scale.
/// - **`<t>_one_starter` and `uq_player_campaign` are backstops** (Reviewer 5a-1 CF4, 5a-0
///   CF3). `pg_advisory_xact_lock` is database-wide, so the join lock serialises joins from
///   any number of server processes, and either backstop is reachable only through an
///   out-of-band write. If one fires, the client gets a `500`. Never string-match
///   `Internal`'s text for `23505`.
/// - The generators' `assert!`s (`pick_band`, `pick`) are reachable only through a bad
///   constant (Reviewer 7d-1 CF3): the join uses the non-`_with_weights` forms, i.e.
///   `DEFAULT_GRADE_WEIGHTS`.
/// - The join calls the `insert_*` helpers, never the `create_*` trait methods, which would
///   commit on a pool connection outside this transaction (Reviewer 5a-0 CF4).
pub struct PostgresCampaignJoinRepository {
    pool:  PgPool,
    pause: Option<PauseHook>,
}

impl PostgresCampaignJoinRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, pause: None }
    }

    /// The join with a pause hook, awaited once after the live-membership check and the
    /// pick or create, before any insert. It exists for the race tests in the integration
    /// binary, which never compiles the lib with `cfg(test)`. Production pays one `Option`
    /// check. **The hook fires in all three methods:** `join_campaign` as above;
    /// `begin_launch` after the guard, before the activation; `claim_replacement` right after
    /// the eligibility read, before the zero/usable branch. A test that parks only one racer
    /// gives the other its own instance from `new` on the same pool.
    #[doc(hidden)]
    pub fn with_pause_seam(pool: PgPool, hook: PauseHook) -> Self {
        Self { pool, pause: Some(hook) }
    }
}

#[async_trait]
impl CampaignJoinRepository for PostgresCampaignJoinRepository {
    async fn join_campaign(
        &self,
        wallet: &[u8],
        cap: i64,
        seeds: JoinSeeds,
    ) -> Result<JoinOutcome, RepositoryError> {
        // Any `Err` returns at once: Postgres has aborted the tx (`25P02` on every later
        // statement), and dropping `tx` queues the `ROLLBACK`, which releases the xact lock
        // (asynchronously, on the pool's return ping). There is no manual unlock.
        let mut tx = self.pool.begin().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        lock_joins(&mut tx).await?;

        // S1: `AlreadyMember` comes only from this read, never from an insert error.
        if let Some(campaign_id) = live_membership(&mut tx, wallet).await? {
            return match starter_ids(&mut tx, campaign_id, wallet).await? {
                Some(ids) => Ok(JoinOutcome::AlreadyMember(ids)),
                None => Err(RepositoryError::Internal(NO_STARTER_SET.into())),
            };
        }

        let campaign_id = match pick_pending_campaign(&mut tx, cap).await? {
            Some(id) => id,
            None => {
                insert_campaign(&mut tx, &NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: seeds.map })
                    .await?
                    .campaign_id
            }
        };

        // The only seam: after S1 and the pick or create, before any insert.
        if let Some(hook) = &self.pause {
            hook().await;
        }

        // Membership first, and the order is load-bearing (Reviewer 5a-1 CF2): the `0030`
        // `<t>_membership_fkey`s are NOT DEFERRABLE, so a roster row before its membership
        // fails `23503`. Never "fix" an order error with DEFERRABLE.
        insert_membership(&mut tx, wallet, campaign_id, None, None).await?;

        let commander = generate_commander(wallet.to_vec(), campaign_id, seeds.commander);
        let commander_id = commander.commander_id;
        insert_commander(&mut tx, commander, AcquiredVia::Starter).await?;

        let foreman = generate_foreman(wallet.to_vec(), campaign_id, seeds.foreman);
        let foreman_id = foreman.foreman_id;
        insert_foreman(&mut tx, foreman, AcquiredVia::Starter).await?;

        let section = starter_section(campaign_id, wallet.to_vec());
        let section_id = section.section_id;
        insert_section(&mut tx, section, AcquiredVia::Starter).await?;

        let crew = CrewRecord::new_hire(campaign_id, wallet.to_vec());
        let crew_id = crew.crew_id;
        insert_crew(&mut tx, crew, AcquiredVia::Starter).await?;

        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(JoinOutcome::Joined(StarterIds { campaign_id, commander_id, foreman_id, section_id, crew_id }))
    }

    async fn begin_launch(
        &self,
        campaign_id: Uuid,
        max_members: usize,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<LaunchStart, RepositoryError> {
        // Every early return drops `tx`; the rollback releases the xact lock, as in the join.
        let mut tx = self.pool.begin().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        lock_joins(&mut tx).await?;

        let Some(campaign) = campaign_row(&mut tx, campaign_id).await? else {
            return Ok(LaunchStart::NotFound);
        };
        if campaign.state != CampaignLifecycle::Pending {
            return Ok(LaunchStart::NotPending);
        }

        let members = member_rows(&mut tx, campaign_id).await?;
        if members.is_empty() {
            return Ok(LaunchStart::NoMembers);
        }
        // The guard, read under the lock: no join can add a member until this tx ends.
        if members.len() > max_members {
            return Ok(LaunchStart::TooManyMembers(members.len()));
        }

        if let Some(hook) = &self.pause {
            hook().await;
        }

        if !activate_row(&mut tx, campaign_id, ends_at, victory_tickers).await? {
            // Unreachable from the join under the lock; only an out-of-band state write
            // reaches it (backstop, the `uq_player_campaign` class).
            return Ok(LaunchStart::NotPending);
        }

        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(LaunchStart::Started { campaign, members })
    }

    /// `FOR UPDATE` is deliberate, not `FOR NO KEY UPDATE`: every roster insert's membership-FK
    /// check takes `FOR KEY SHARE` on this row, which `FOR UPDATE` blocks, so the "none usable"
    /// read holds against any roster writer for this member until commit. The row lock
    /// serialises claims from any number of server processes; the index below is reachable
    /// only through an out-of-band write or a future second writer of `'replacement'`.
    /// `resolve_deployment` and `set_shattered` only ever turn KIA or Shattered on, never off
    /// (no `src` path clears `is_shattered`), so a race can only make a claim answer
    /// `HasUsable` too conservatively. `commander_records_one_live_replacement` (0032) is the
    /// backstop; its `23505` surfaces as `Internal`.
    async fn claim_replacement(
        &self,
        campaign_id: Uuid,
        wallet: &[u8],
        seed: u32,
    ) -> Result<ReplacementOutcome, RepositoryError> {
        // Every early return drops `tx`, which rolls back and releases the row lock.
        let mut tx = self.pool.begin().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        let member = sqlx::query_scalar!(
            "SELECT membership_id FROM player_campaign_membership WHERE campaign_id = $1 AND wallet_address = $2 FOR UPDATE",
            campaign_id,
            wallet,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if member.is_none() {
            return Ok(ReplacementOutcome::NotMember);
        }

        // TODO(phase-2-§14): Shattered clause retires with Rehabilitation (ODQ-46).
        let counts = sqlx::query!(
            r#"SELECT count(*) AS "total!", count(*) FILTER (WHERE NOT is_kia AND NOT is_shattered) AS "usable!" FROM commander_records WHERE campaign_id = $1 AND player_wallet = $2"#,
            campaign_id,
            wallet,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        if let Some(hook) = &self.pause {
            hook().await;
        }

        if counts.total == 0 {
            return Err(RepositoryError::Internal(NO_COMMANDERS.into()));
        }
        if counts.usable > 0 {
            return Ok(ReplacementOutcome::HasUsable);
        }

        let record = generate_commander(wallet.to_vec(), campaign_id, seed);
        let commander_id = record.commander_id;
        insert_commander(&mut tx, record, AcquiredVia::Replacement).await?;

        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(ReplacementOutcome::Granted(commander_id))
    }
}
