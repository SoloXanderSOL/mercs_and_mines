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

use super::campaign::{insert_campaign, NewCampaignInstance};
use super::commander::insert_commander;
use super::crew::{insert_crew, CrewRecord};
use super::foreman::insert_foreman;
use super::membership::insert_membership;
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
/// The advisory key that serialises every join, server-wide.
pub(crate) const JOIN_ADVISORY_KEY: i64 = -0x4D4D_4A4F_494E;
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

/// The refusal for a member whose starter set is missing or partial.
pub(crate) const NO_STARTER_SET: &str = "campaign join: member has no starter set";

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

/// A test hook awaited once inside the join's transaction (see `with_pause_seam`).
pub type PauseHook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The join as one Postgres transaction.
///
/// - **The lock is global** (`JOIN_ADVISORY_KEY`, the transaction's first statement) and
///   serialises every join server-wide. This is accepted at alpha scale.
/// - **`<t>_one_starter` and `uq_player_campaign` are backstops** (Reviewer 5a-1 CF4, 5a-0
///   CF3). If either fires on this path (only possible across two server processes), the
///   client gets a `500`. Never string-match `Internal`'s text for `23505`.
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
    /// check.
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
}
