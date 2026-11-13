//! The demo reaper (BP-3c-1b-1, ODQ-67 (b), **demo-only**). Every 5 minutes it archives each
//! `Active` demo campaign that has been idle past its line (below), so a slot an attendee
//! walked away from frees itself (the demo holds at most 1,000 live campaigns, ODQ-55). A
//! campaign with a convoy in transit is kept until the convoy
//! lands, as the abandon keeps it. The `dev` port deletes this file, its `lib.rs` line and its
//! `main.rs` spawn.
//!
//! Per campaign: the launch lock, then `retire_if_idle`, then the sector delete (retire first,
//! then delete, as `retire_campaign`), never inside a transaction or under `JOIN_ADVISORY_KEY`.
//! The idle test sits inside the retire's `UPDATE`, so a mission whose deployment row commits
//! before it saves the campaign. Accepted residual: a mission that passed the route's unlocked
//! `Active` check but inserts after the retire resolves on an `Archived` campaign; the player's
//! next call gets `409` and the client re-joins.
//!
//! The ops layer (BP-8a-2): each sweep first prunes the ops store to the `Active` campaigns.
//! Accepted residual: `ops/start` takes no launch lock, so an op started between the keep
//! check and the retire is retired with the campaign.
//! A drill counts as moving (BP-8b-1a), so a team still digging on site is kept.
//!
//! **Two idle lines (BP-42, ODQ-67 (b) amended 2026-11-07).** A played campaign (a mission
//! start, or a move the ops store has seen, `DemoOpsStore::last_moved`) is retired after 48
//! hours idle; an unplayed one after 60 minutes (BP-46; 15 before). Its idle time is the latest of its
//! launch, its last mission start and its last ops move. The select chooses the line per
//! campaign, with the ops moves passed in as arrays, so kept campaigns never fill the `LIMIT`;
//! the retire re-checks with the move read under the launch lock. **The safety valve:** at
//! `DEMO_REAP_PRESSURE_AT` live campaigns the sweep lists by the 15-minute line for everyone,
//! oldest-idle first, and each retire uses it while the live count less this sweep's retires
//! still reaches the valve (`under_pressure`); after that, played campaigns are judged by the
//! 48-hour line again. Accepted residuals: a played campaign listed under pressure and kept
//! once the count drops still takes a `LIMIT` place until the next sweep; a failed count reads
//! as no pressure (the 1,000 cap still refuses joins); and a campaign played only through ops
//! loses that reading at a deploy (the store is in memory), so the boot sweep retires it as
//! unplayed (Linux F2, ruled with BP-42).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::repeat_warn::{KeyedRepeatWarn, RepeatWarn};
use crate::repository::{CampaignLifecycle, CampaignRepository, PauseHook, RepositoryError};
use crate::state::AppState;

/// How often the reaper sweeps.
pub const DEMO_REAP_INTERVAL: Duration = Duration::from_secs(300);
/// An unplayed campaign is idle once its launch is this old (60 minutes; BP-46, was 15).
pub const DEMO_REAP_IDLE_AFTER_SECS: i64 = 3600;
/// BP-46: under pressure, every campaign is idle once its latest activity is this old (15
/// minutes, the valve's line as before; it no longer follows `DEMO_REAP_IDLE_AFTER_SECS`).
pub const DEMO_REAP_PRESSURE_IDLE_SECS: i64 = 900;
/// BP-42: a played campaign is idle once its latest activity is this old (48 hours).
pub const DEMO_REAP_IDLE_PLAYED_SECS: i64 = 172_800;
/// BP-42: at this many live campaigns (80% of `DEMO_LIVE_CAMPAIGN_CAP`), the 15-minute line
/// applies to every campaign until the count is back under it.
pub const DEMO_REAP_PRESSURE_AT: i64 = 800;
/// At most this many campaigns per sweep, oldest first, so one sweep's Postgres work stays
/// bounded (about 0.5 s at 20,000 deployment rows) and 1,000 idle campaigns clear in 25 minutes.
pub const DEMO_REAP_MAX_PER_SWEEP: i64 = 200;
/// A failure logs its first occurrence, then about hourly.
const REAP_WARN_EVERY_SWEEPS: u32 = 12;

/// The two idle lines a retire is judged against (BP-42).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReapCutoffs {
    /// A played campaign whose latest activity is before this is idle.
    pub played: DateTime<Utc>,
    /// An unplayed campaign launched before this is idle.
    pub unplayed: DateTime<Utc>,
}

impl ReapCutoffs {
    /// The normal pass: `unplayed` is the 60-minute line, and the played line sits
    /// `DEMO_REAP_IDLE_PLAYED_SECS - DEMO_REAP_IDLE_AFTER_SECS` before it, so both measure
    /// from the same `now`.
    pub fn normal(unplayed: DateTime<Utc>) -> Self {
        let gap = chrono::Duration::seconds(DEMO_REAP_IDLE_PLAYED_SECS - DEMO_REAP_IDLE_AFTER_SECS);
        Self { played: unplayed - gap, unplayed }
    }

    /// One line for every campaign: the pressure pass, and the single-cutoff form.
    pub fn flat(cutoff: DateTime<Utc>) -> Self {
        Self { played: cutoff, unplayed: cutoff }
    }
}

/// BP-42's safety valve: the `live` campaigns less this sweep's `retired` still reach
/// `DEMO_REAP_PRESSURE_AT`.
pub fn under_pressure(live: i64, retired: usize) -> bool {
    live - retired as i64 >= DEMO_REAP_PRESSURE_AT
}

/// What the reaper reads and writes. Pairs are `(campaign_id, sector_id)`.
#[async_trait]
pub trait ReaperStore: Send + Sync {
    /// Up to `limit` `Active` campaigns idle under `cutoffs`, oldest-idle first (BP-42).
    /// `ops_ids` and `ops_at` pair each campaign the ops store has seen move with its latest
    /// move (`DemoOpsStore::moved_list`): such a campaign is played, and the move counts as
    /// activity. The line is chosen in the select, so kept campaigns never fill the `LIMIT`.
    async fn list_idle(
        &self,
        cutoffs: ReapCutoffs,
        ops_ids: &[Uuid],
        ops_at: &[DateTime<Utc>],
        limit: i64,
    ) -> Result<Vec<(Uuid, Uuid)>, RepositoryError>;
    /// Archives the campaign only if it is still `Active` and still idle under `cutoffs`, with
    /// `ops_at` its latest ops move, read under the launch lock.
    async fn retire_idle(
        &self,
        campaign_id: Uuid,
        cutoffs: ReapCutoffs,
        ops_at: Option<DateTime<Utc>>,
    ) -> Result<bool, RepositoryError>;
    /// The live demo campaigns (`Pending`, `Active`, `Ending`), as `DEMO_LIVE_CAMPAIGN_CAP`
    /// counts them.
    async fn count_live(&self) -> Result<i64, RepositoryError>;

    /// The single-cutoff form that predates BP-42: one line for every campaign, no ops moves.
    async fn list_idle_campaigns(&self, cutoff: DateTime<Utc>, limit: i64) -> Result<Vec<(Uuid, Uuid)>, RepositoryError> {
        self.list_idle(ReapCutoffs::flat(cutoff), &[], &[], limit).await
    }
    /// The single-cutoff form of `retire_idle`, with no ops move.
    async fn retire_if_idle(&self, campaign_id: Uuid, cutoff: DateTime<Utc>) -> Result<bool, RepositoryError> {
        self.retire_idle(campaign_id, ReapCutoffs::flat(cutoff), None).await
    }
}

pub struct PostgresReaperStore {
    pool: PgPool,
}

impl PostgresReaperStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ReaperStore for PostgresReaperStore {
    /// One grouped scan of `deployment_records`, which has no index on `commander_id`: a
    /// correlated subquery per campaign measured 13.5 s at 100,000 rows, this form 39 ms.
    async fn list_idle(
        &self,
        cutoffs: ReapCutoffs,
        ops_ids: &[Uuid],
        ops_at: &[DateTime<Utc>],
        limit: i64,
    ) -> Result<Vec<(Uuid, Uuid)>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT c.campaign_id AS "campaign_id!", c.sector_id AS "sector_id!"
               FROM campaign_instances c
               LEFT JOIN (SELECT cr.campaign_id, max(d.started_at) AS last_start
                          FROM deployment_records d
                          JOIN commander_records cr ON cr.commander_id = d.commander_id
                          GROUP BY cr.campaign_id) m ON m.campaign_id = c.campaign_id
               LEFT JOIN unnest($2::uuid[], $3::timestamptz[]) AS o(id, at) ON o.id = c.campaign_id
               WHERE c.state = $1
                 AND GREATEST(COALESCE(m.last_start, c.started_at), o.at)
                     < CASE WHEN m.last_start IS NOT NULL OR o.id IS NOT NULL
                            THEN $4::timestamptz ELSE $5::timestamptz END
                 AND NOT EXISTS (SELECT 1 FROM convoy_records v
                                 WHERE v.sector_id = c.sector_id AND v.in_transit
                                   AND v.arrival_time > now() - interval '5 minutes')
               ORDER BY GREATEST(COALESCE(m.last_start, c.started_at), o.at)
               LIMIT $6"#,
            CampaignLifecycle::Active as CampaignLifecycle,
            ops_ids,
            ops_at,
            cutoffs.played,
            cutoffs.unplayed,
            limit,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(|r| (r.campaign_id, r.sector_id)).collect())
    }

    /// The idle and convoy tests are re-evaluated inside the `UPDATE`, so a deployment row
    /// committed after the reaper's select and before this statement keeps the campaign.
    async fn retire_idle(
        &self,
        campaign_id: Uuid,
        cutoffs: ReapCutoffs,
        ops_at: Option<DateTime<Utc>>,
    ) -> Result<bool, RepositoryError> {
        let result = sqlx::query!(
            r#"UPDATE campaign_instances c SET state = $1, updated_at = now()
               WHERE c.campaign_id = $2 AND c.state = $3
                 AND GREATEST(COALESCE((SELECT max(d.started_at) FROM deployment_records d
                                        JOIN commander_records cr ON cr.commander_id = d.commander_id
                                        WHERE cr.campaign_id = c.campaign_id), c.started_at), $4::timestamptz)
                     < CASE WHEN $4::timestamptz IS NOT NULL
                                 OR EXISTS (SELECT 1 FROM deployment_records d
                                            JOIN commander_records cr ON cr.commander_id = d.commander_id
                                            WHERE cr.campaign_id = c.campaign_id)
                            THEN $5::timestamptz ELSE $6::timestamptz END
                 AND NOT EXISTS (SELECT 1 FROM convoy_records v
                                 WHERE v.sector_id = c.sector_id AND v.in_transit
                                   AND v.arrival_time > now() - interval '5 minutes')"#,
            CampaignLifecycle::Archived as CampaignLifecycle,
            campaign_id,
            CampaignLifecycle::Active as CampaignLifecycle,
            ops_at,
            cutoffs.played,
            cutoffs.unplayed,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected() == 1)
    }

    /// The join's own count (`campaign_join.rs`), the same text: one scan, every sweep.
    async fn count_live(&self) -> Result<i64, RepositoryError> {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM campaign_instances WHERE state IN ('Pending','Active','Ending')"#
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}

/// The in-memory mirror. It sees no deployments and no convoys: a campaign is played when the
/// ops arrays name it, and its idle time is the later of its launch and that move. Its
/// `retire_idle` keeps a campaign whose `ops_at` is at or after the played line, then checks
/// `Active` only (as `retire_if_active`). The Postgres tests in `db_infrastructure.rs` are the
/// oracle.
pub struct InMemoryReaperStore {
    campaigns: Arc<dyn CampaignRepository + Send + Sync>,
}

impl InMemoryReaperStore {
    pub fn new(campaigns: Arc<dyn CampaignRepository + Send + Sync>) -> Self {
        Self { campaigns }
    }
}

#[async_trait]
impl ReaperStore for InMemoryReaperStore {
    async fn list_idle(
        &self,
        cutoffs: ReapCutoffs,
        ops_ids: &[Uuid],
        ops_at: &[DateTime<Utc>],
        limit: i64,
    ) -> Result<Vec<(Uuid, Uuid)>, RepositoryError> {
        let moved: std::collections::HashMap<Uuid, DateTime<Utc>> =
            ops_ids.iter().copied().zip(ops_at.iter().copied()).collect();
        let mut idle: Vec<(DateTime<Utc>, Uuid, Uuid)> = self
            .campaigns
            .list_campaigns_by_state(CampaignLifecycle::Active)
            .await?
            .into_iter()
            .filter_map(|c| {
                let started = c.started_at?;
                let at = moved.get(&c.campaign_id).copied();
                let idle_at = at.map_or(started, |t| t.max(started));
                let line = if at.is_some() { cutoffs.played } else { cutoffs.unplayed };
                (idle_at < line).then_some((idle_at, c.campaign_id, c.sector_id))
            })
            .collect();
        idle.sort_by_key(|(s, ..)| *s);
        Ok(idle.into_iter().take(limit.max(0) as usize).map(|(_, c, s)| (c, s)).collect())
    }

    async fn retire_idle(
        &self,
        campaign_id: Uuid,
        cutoffs: ReapCutoffs,
        ops_at: Option<DateTime<Utc>>,
    ) -> Result<bool, RepositoryError> {
        if ops_at.is_some_and(|t| t >= cutoffs.played) {
            return Ok(false);
        }
        self.campaigns.retire_if_active(campaign_id).await
    }

    async fn count_live(&self) -> Result<i64, RepositoryError> {
        let mut n = 0;
        for s in [CampaignLifecycle::Pending, CampaignLifecycle::Active, CampaignLifecycle::Ending] {
            n += self.campaigns.list_campaigns_by_state(s).await?.len() as i64;
        }
        Ok(n)
    }
}

/// Repeat-warning state: the select, the ops prune's listing, and one keyed counter per
/// campaign for the retire. Built fresh by the task, so a supervised restart warns again.
#[derive(Debug)]
pub struct ReaperWarns {
    pub select: RepeatWarn,
    pub prune: RepeatWarn,
    pub retire: KeyedRepeatWarn,
    /// BP-42: the live count the valve reads.
    pub count: RepeatWarn,
}

impl ReaperWarns {
    pub fn new() -> Self {
        Self {
            select: RepeatWarn::new(REAP_WARN_EVERY_SWEEPS),
            prune: RepeatWarn::new(REAP_WARN_EVERY_SWEEPS),
            retire: KeyedRepeatWarn::new(REAP_WARN_EVERY_SWEEPS),
            count: RepeatWarn::new(REAP_WARN_EVERY_SWEEPS),
        }
    }
}

impl Default for ReaperWarns {
    fn default() -> Self {
        Self::new()
    }
}

/// The ops store's prune (BP-8a-2): keeps the entries of `Active` campaigns and any entry
/// touched after the listing was taken (a campaign launched meanwhile). A failed listing
/// skips the prune: retain only after a full listing.
async fn prune_ops(state: &AppState, warns: &mut ReaperWarns) {
    let listed_at = Utc::now();
    match state.campaign_repo.list_campaigns_by_state(CampaignLifecycle::Active).await {
        Ok(active) => {
            if let Some(n) = warns.prune.ok() {
                tracing::info!("demo reaper: the ops prune's campaign list recovered after {n} failed sweeps");
            }
            let ids: HashSet<Uuid> = active.iter().map(|c| c.campaign_id).collect();
            state.demo_ops.store.retain_campaigns(&ids, listed_at);
        }
        Err(e) => match warns.prune.fail() {
            Some(1) => tracing::warn!("demo reaper: the ops prune's campaign list failed: {e}"),
            Some(n) => tracing::warn!("demo reaper: the ops prune's campaign list still failing ({n} consecutive sweeps): {e}"),
            None => {}
        },
    }
}

/// One sweep. Returns how many campaigns it archived. `cutoff` is the unplayed line (now less
/// `DEMO_REAP_IDLE_AFTER_SECS` in production); the played line comes from it
/// (`ReapCutoffs::normal`). The valve's line, for everyone (`ReapCutoffs::flat`), sits
/// `DEMO_REAP_IDLE_AFTER_SECS - DEMO_REAP_PRESSURE_IDLE_SECS` after it (BP-46).
pub async fn reap_once(state: &AppState, store: &dyn ReaperStore, cutoff: DateTime<Utc>, warns: &mut ReaperWarns) -> usize {
    reap_once_with_seam(state, store, cutoff, warns, None).await
}

/// `reap_once` with a test hook awaited once, after the select and before the first lock.
#[doc(hidden)]
pub async fn reap_once_with_seam(
    state: &AppState,
    store: &dyn ReaperStore,
    cutoff: DateTime<Utc>,
    warns: &mut ReaperWarns,
    seam: Option<&PauseHook>,
) -> usize {
    prune_ops(state, warns).await;
    let live = match store.count_live().await {
        Ok(n) => {
            if let Some(n) = warns.count.ok() {
                tracing::info!("demo reaper: the live-campaign count recovered after {n} failed sweeps");
            }
            n
        }
        Err(e) => {
            match warns.count.fail() {
                Some(1) => tracing::warn!("demo reaper: the live-campaign count failed (no pressure this sweep): {e}"),
                Some(n) => tracing::warn!("demo reaper: the live-campaign count still failing ({n} consecutive sweeps): {e}"),
                None => {}
            }
            0
        }
    };
    let valve = cutoff + chrono::Duration::seconds(DEMO_REAP_IDLE_AFTER_SECS - DEMO_REAP_PRESSURE_IDLE_SECS); // BP-46
    let (normal, flat) = (ReapCutoffs::normal(cutoff), ReapCutoffs::flat(valve));
    let listing = if under_pressure(live, 0) { flat } else { normal };
    let (ops_ids, ops_at) = state.demo_ops.store.moved_list(Utc::now());
    let idle = match store.list_idle(listing, &ops_ids, &ops_at, DEMO_REAP_MAX_PER_SWEEP).await {
        Ok(v) => {
            if let Some(n) = warns.select.ok() {
                tracing::info!("demo reaper: the idle-campaign select recovered after {n} failed sweeps");
            }
            v
        }
        Err(e) => {
            match warns.select.fail() {
                Some(1) => tracing::warn!("demo reaper: the idle-campaign select failed: {e}"),
                Some(n) => tracing::warn!("demo reaper: the idle-campaign select still failing ({n} consecutive sweeps): {e}"),
                None => {}
            }
            return 0;
        }
    };
    let listed: HashSet<Uuid> = idle.iter().map(|(c, _)| *c).collect();
    warns.retire.retain(&listed);
    if let Some(seam) = seam {
        seam().await;
    }

    let mut retired = 0;
    for (campaign_id, sector_id) in idle {
        let _launch = state.launch_locks.lock(campaign_id).await;
        let ops_at = state.demo_ops.store.last_moved(campaign_id, Utc::now());
        let cutoffs = if under_pressure(live, retired) { flat } else { normal };
        match store.retire_idle(campaign_id, cutoffs, ops_at).await {
            Ok(r) => {
                if let Some(n) = warns.retire.ok(campaign_id) {
                    tracing::info!("demo reaper: the retire of campaign {campaign_id} recovered after {n} failed sweeps");
                }
                if !r {
                    continue;
                }
            }
            Err(e) => {
                match warns.retire.fail(campaign_id) {
                    Some(1) => tracing::warn!("demo reaper: the retire of campaign {campaign_id} failed: {e}"),
                    Some(n) => tracing::warn!("demo reaper: the retire of campaign {campaign_id} still failing ({n} consecutive sweeps): {e}"),
                    None => {}
                }
                continue;
            }
        }
        retired += 1;
        state.demo_ops.store.remove(campaign_id);
        if let Err(e) = state.sector_repo.delete_sector(sector_id).await {
            tracing::warn!(
                "demo reaper: campaign {campaign_id} retired, sector {sector_id} NOT deleted (SREM sectors:all + DEL by hand); this sweep stops: {e:?}"
            );
            break;
        }
    }
    if retired > 0 {
        tracing::info!(retired, "demo reaper: retired idle campaigns");
    }
    retired
}

/// The supervised task: a sweep every `DEMO_REAP_INTERVAL`, the first at boot. Stateless, so
/// a restart costs nothing.
pub async fn run_demo_reaper_task(state: Arc<AppState>, store: Arc<dyn ReaperStore>) {
    let mut interval = tokio::time::interval(DEMO_REAP_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut warns = ReaperWarns::new();
    loop {
        interval.tick().await;
        let cutoff = Utc::now() - chrono::Duration::seconds(DEMO_REAP_IDLE_AFTER_SECS);
        reap_once(state.as_ref(), store.as_ref(), cutoff, &mut warns).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::repository::{
        HexOccupant, InMemorySectorStateRepository, NewCampaignInstance, OccupationStatus, SectorId, SectorState,
        SectorStateRepository, WalletAddress,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn state() -> AppState {
        AppState::new(Arc::new(Config::default()))
    }

    fn store(state: &AppState) -> InMemoryReaperStore {
        InMemoryReaperStore::new(Arc::clone(&state.campaign_repo))
    }

    fn sector(campaign_id: Uuid, sector_id: Uuid) -> SectorState {
        SectorState {
            sector_id,
            campaign_id,
            occupation_status: OccupationStatus::Neutral,
            owner: None,
            deployed_unit_count: 0,
            terrain: std::collections::HashMap::new(),
            magma_veins: vec![],
            hex_occupancy: std::collections::HashMap::new(),
        }
    }

    /// A campaign in `state`'s in-memory store, launched now when `active`, with its sector.
    async fn campaign(state: &AppState, active: bool) -> (Uuid, Uuid) {
        let sector_id = Uuid::new_v4();
        let c = state
            .campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id, map_seed: 0x3C1B })
            .await
            .expect("create_campaign")
            .campaign_id;
        if active {
            let ok = state
                .campaign_repo
                .activate_campaign(c, Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
                .await
                .expect("activate_campaign");
            assert!(ok, "the campaign activates");
        }
        state.sector_repo.upsert_sector(sector(c, sector_id)).await.expect("upsert_sector");
        (c, sector_id)
    }

    async fn lifecycle(state: &AppState, c: Uuid) -> CampaignLifecycle {
        state.campaign_repo.get_campaign(c).await.expect("get_campaign").expect("the campaign exists").state
    }

    /// The canon numbers of ODQ-67 (b) (amended 2026-11-07, BP-42) and ODQ-84 (a), as literals.
    #[test]
    fn reaper_numbers_are_odq_67s() {
        assert_eq!(DEMO_REAP_INTERVAL, Duration::from_secs(300), "every 5 minutes");
        assert_eq!(DEMO_REAP_IDLE_AFTER_SECS, 3600, "an unplayed campaign: idle after 60 minutes (BP-46)");
        assert_eq!(DEMO_REAP_PRESSURE_IDLE_SECS, 900, "the valve: idle after 15 minutes");
        assert_eq!(DEMO_REAP_MAX_PER_SWEEP, 200);
        assert_eq!(DEMO_REAP_IDLE_PLAYED_SECS, 172_800, "a played campaign: idle after 48 hours");
        assert_eq!(DEMO_REAP_PRESSURE_AT, 800, "the valve at 80% of 1,000");
        assert_eq!(crate::demo_address_cap::DEMO_ADDRESS_LIVE_CAP, 300, "ODQ-84 (a)");
    }

    /// The in-memory mirror retires `Active` campaigns launched before the cutoff, and only those.
    #[tokio::test]
    async fn in_memory_reaper_retires_only_active_campaigns_before_the_cutoff() {
        let state = state();
        let store = store(&state);
        let (active, active_sector) = campaign(&state, true).await;
        let (pending, _) = campaign(&state, false).await;
        let (archived, _) = campaign(&state, true).await;
        assert!(state.campaign_repo.retire_if_active(archived).await.expect("retire_if_active"));
        let mut warns = ReaperWarns::new();

        let past = DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(reap_once(&state, &store, past, &mut warns).await, 0, "nothing launched before 2000");
        assert_eq!(lifecycle(&state, active).await, CampaignLifecycle::Active);

        let future = Utc::now() + chrono::Duration::hours(1);
        assert_eq!(reap_once(&state, &store, future, &mut warns).await, 1);
        assert_eq!(lifecycle(&state, active).await, CampaignLifecycle::Archived);
        assert!(!state.sector_repo.sector_exists(active_sector).await.expect("sector_exists"), "its sector is deleted");
        assert_eq!(lifecycle(&state, pending).await, CampaignLifecycle::Pending);
        assert_eq!(lifecycle(&state, archived).await, CampaignLifecycle::Archived);
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// The in-memory sector store, with a `delete_sector` that always fails.
    struct FailingDelete {
        inner: InMemorySectorStateRepository,
        deletes: AtomicUsize,
    }

    #[async_trait]
    impl SectorStateRepository for FailingDelete {
        async fn get_sector(&self, sector_id: SectorId) -> Result<Option<SectorState>, RepositoryError> {
            self.inner.get_sector(sector_id).await
        }
        async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError> {
            self.inner.upsert_sector(state).await
        }
        async fn list_sectors(&self) -> Result<Vec<SectorState>, RepositoryError> {
            self.inner.list_sectors().await
        }
        async fn get_player_presence(&self, sector_id: SectorId) -> Result<Vec<WalletAddress>, RepositoryError> {
            self.inner.get_player_presence(sector_id).await
        }
        async fn set_player_presence(&self, sector_id: SectorId, players: &[WalletAddress]) -> Result<(), RepositoryError> {
            self.inner.set_player_presence(sector_id, players).await
        }
        async fn add_hex_occupant(&self, sector_id: SectorId, q: i32, r: i32, entry: HexOccupant) -> Result<(), RepositoryError> {
            self.inner.add_hex_occupant(sector_id, q, r, entry).await
        }
        async fn remove_hex_occupant(&self, sector_id: SectorId, q: i32, r: i32, unit_id: Uuid) -> Result<(), RepositoryError> {
            self.inner.remove_hex_occupant(sector_id, q, r, unit_id).await
        }
        async fn delete_sector(&self, _: SectorId) -> Result<(), RepositoryError> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            Err(RepositoryError::Internal("redis is down".into()))
        }
        async fn sector_exists(&self, sector_id: SectorId) -> Result<bool, RepositoryError> {
            self.inner.sector_exists(sector_id).await
        }
    }

    /// A failed sector delete stops the sweep: one campaign is archived with its sector kept,
    /// and the other idle campaign waits for the next sweep.
    #[tokio::test]
    async fn a_failed_sector_delete_stops_the_sweep() {
        let mut state = state();
        let failing = Arc::new(FailingDelete { inner: InMemorySectorStateRepository::new(), deletes: AtomicUsize::new(0) });
        state.sector_repo = failing.clone();
        let store = store(&state);
        let (a, sa) = campaign(&state, true).await;
        let (b, sb) = campaign(&state, true).await;
        let mut warns = ReaperWarns::new();

        let reaped = reap_once(&state, &store, Utc::now() + chrono::Duration::hours(1), &mut warns).await;

        assert_eq!(reaped, 1, "one retire, then the sweep stopped");
        assert_eq!(failing.deletes.load(Ordering::SeqCst), 1, "one delete attempted");
        let mut states = vec![lifecycle(&state, a).await, lifecycle(&state, b).await];
        states.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(states, vec![CampaignLifecycle::Active, CampaignLifecycle::Archived]);
        assert!(state.sector_repo.sector_exists(sa).await.expect("sector_exists"), "the sector is kept");
        assert!(state.sector_repo.sector_exists(sb).await.expect("sector_exists"));
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// The reaper takes the campaign's launch lock: it does not retire while another holder
    /// has it, and retires once the holder lets go.
    #[tokio::test]
    async fn the_reaper_waits_for_the_launch_lock() {
        let state = Arc::new(state());
        let (c, _) = campaign(&state, true).await;
        let guard = state.launch_locks.lock(c).await;

        let st = Arc::clone(&state);
        let mut task = tokio::spawn(async move {
            let store = store(&st);
            let mut warns = ReaperWarns::new();
            reap_once(&st, &store, Utc::now() + chrono::Duration::hours(1), &mut warns).await
        });
        let early = tokio::time::timeout(Duration::from_millis(300), &mut task).await;
        let waited = early.is_err();
        let held_state = lifecycle(&state, c).await;
        drop(guard);
        // A finished handle is never polled again: its result is `early`'s.
        let reaped = match early {
            Ok(r) => r,
            Err(_) => task.await,
        }
        .expect("the reaper task");

        assert!(waited, "the reaper finished while another holder had the campaign's lock");
        assert_eq!(held_state, CampaignLifecycle::Active, "nothing retired under another holder's lock");
        assert_eq!(reaped, 1);
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Archived);
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    fn ops_board() -> Arc<crate::demo_ops::OpsBoard> {
        Arc::new(crate::demo_ops::OpsBoard { sites: vec![], contract_pool: vec![], routes: std::collections::HashMap::new() })
    }

    fn ops_site() -> crate::demo_ops::OpsSite {
        crate::demo_ops::OpsSite {
            site_id: "contract:GUARD_DUTY".into(),
            kind: "contract",
            q: 8,
            r: 0,
            distance: 3,
            travel_secs: 10,
            tier: None,
            mission_id: Some("GUARD_DUTY"),
            name: None,
        }
    }

    /// BP-8a-2: a campaign whose team departed after the cutoff is kept; one whose entry holds
    /// only a board (reading is not moving) is retired.
    #[tokio::test]
    async fn reaper_keeps_a_campaign_whose_op_moved_recently() {
        let state = state();
        let store = store(&state);
        let (moving, _) = campaign(&state, true).await;
        let (reading, _) = campaign(&state, true).await;
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let later = cutoff + chrono::Duration::seconds(1);
        state.demo_ops.store.start_op(moving, (5, 0), ops_board(), &ops_site(), 7, None, None, later).expect("start");
        state.demo_ops.store.put_board(reading, (5, 0), ops_board(), later);
        let mut warns = ReaperWarns::new();

        assert_eq!(reap_once(&state, &store, cutoff, &mut warns).await, 1);
        assert_eq!(lifecycle(&state, moving).await, CampaignLifecycle::Active);
        assert_eq!(lifecycle(&state, reading).await, CampaignLifecycle::Archived);
        assert_eq!(state.demo_ops.store.len(), 1, "the retired campaign's entry is dropped");
    }

    fn ops_dig() -> crate::demo_ops::OpsSite {
        crate::demo_ops::OpsSite { site_id: "dig:9,1".into(), kind: "dig", tier: Some(1), mission_id: None, ..ops_site() }
    }

    fn ops_crew() -> Option<crate::demo_ops::DigStart> {
        Some(crate::demo_ops::DigStart { crew_id: Uuid::new_v4(), commander_id: Uuid::new_v4(), threshold: 100, section_id: None, escort: true })
    }

    /// CF1 (BP-8b-1a): the team arrived 990 s before the cutoff and drilled after it: kept.
    #[tokio::test]
    async fn reaper_keeps_a_team_still_drilling() {
        let state = state();
        let store = store(&state);
        let (c, _) = campaign(&state, true).await;
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let secs = chrono::Duration::seconds;
        state.demo_ops.store.start_op(c, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, cutoff - secs(1000)).expect("start");
        state.demo_ops.store.drill_op(c, cutoff + secs(1)).expect("drill");

        assert_eq!(reap_once(&state, &store, cutoff, &mut ReaperWarns::new()).await, 0);
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Active);
    }

    /// BP-42: the same team parked on site with no drill since is kept. Its move, 1,000 s before
    /// the cutoff, makes the campaign played and lies inside the 48-hour line.
    #[tokio::test]
    async fn reaper_keeps_a_team_parked_on_site_for_48_hours() {
        let state = state();
        let store = store(&state);
        let (c, _) = campaign(&state, true).await;
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let secs = chrono::Duration::seconds;
        state.demo_ops.store.start_op(c, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, cutoff - secs(1000)).expect("start");

        assert_eq!(reap_once(&state, &store, cutoff, &mut ReaperWarns::new()).await, 0);
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Active);
        assert_eq!(state.demo_ops.store.len(), 1, "the entry is kept");
    }

    /// BP-42 L2: a team parked past the 48-hour line is retired. The cutoff is now + 49 h, so the
    /// played line is cutoff − 169,200 s; the move at cutoff − 172,000 s (its arrival 10 s later,
    /// at cutoff − 171,990 s) lies before it.
    #[tokio::test]
    async fn reaper_retires_a_team_parked_past_48_hours() {
        let state = state();
        let store = store(&state);
        let (c, _) = campaign(&state, true).await;
        let cutoff = Utc::now() + chrono::Duration::hours(49);
        let secs = chrono::Duration::seconds;
        state.demo_ops.store.start_op(c, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, cutoff - secs(172_000)).expect("start");

        assert_eq!(reap_once(&state, &store, cutoff, &mut ReaperWarns::new()).await, 1);
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Archived);
    }

    /// BP-42 L3: the valve reads the live count less this sweep's retires, against 800.
    #[test]
    fn under_pressure_reads_the_live_count_less_the_retired() {
        assert!(under_pressure(800, 0));
        assert!(!under_pressure(800, 1));
        assert!(!under_pressure(799, 0));
        assert!(!under_pressure(1000, 201));
        assert!(under_pressure(1000, 200));
    }

    /// BP-42: the in-memory store with a fixed live count, so a test reaches the valve without
    /// 800 campaigns. Everything else is the inner store's.
    struct FixedCount {
        inner: InMemoryReaperStore,
        live: i64,
    }

    #[async_trait]
    impl ReaperStore for FixedCount {
        async fn list_idle(
            &self,
            cutoffs: ReapCutoffs,
            ops_ids: &[Uuid],
            ops_at: &[DateTime<Utc>],
            limit: i64,
        ) -> Result<Vec<(Uuid, Uuid)>, RepositoryError> {
            self.inner.list_idle(cutoffs, ops_ids, ops_at, limit).await
        }
        async fn retire_idle(&self, campaign_id: Uuid, cutoffs: ReapCutoffs, ops_at: Option<DateTime<Utc>>) -> Result<bool, RepositoryError> {
            self.inner.retire_idle(campaign_id, cutoffs, ops_at).await
        }
        async fn count_live(&self) -> Result<i64, RepositoryError> {
            Ok(self.live)
        }
    }

    fn fixed(state: &AppState, live: i64) -> FixedCount {
        FixedCount { inner: store(state), live }
    }

    /// BP-42 L4: a played campaign idle 16 minutes (its move at cutoff − 1,000 s) is retired at
    /// 800 live campaigns and kept at 799. Decides the listing site.
    #[tokio::test]
    async fn pressure_retires_a_played_campaign_idle_16_minutes() {
        let secs = chrono::Duration::seconds;
        for (live, retired, after) in [(800, 1, CampaignLifecycle::Archived), (799, 0, CampaignLifecycle::Active)] {
            let state = state();
            let (c, _) = campaign(&state, true).await;
            let cutoff = Utc::now() + chrono::Duration::hours(1);
            state.demo_ops.store.start_op(c, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, cutoff - secs(1000)).expect("start");

            assert_eq!(reap_once(&state, &fixed(&state, live), cutoff, &mut ReaperWarns::new()).await, retired, "at {live} live");
            assert_eq!(lifecycle(&state, c).await, after, "at {live} live");
        }
    }

    /// BP-42 L5: at 800 live campaigns the first retire brings the count under the valve, so the
    /// second played campaign is judged by the 48-hour line again and kept. The older goes.
    /// Decides the per-retire site.
    #[tokio::test]
    async fn pressure_stops_once_the_count_is_back_under_800() {
        let state = state();
        let (older, _) = campaign(&state, true).await;
        let (newer, _) = campaign(&state, true).await;
        let cutoff = Utc::now() + chrono::Duration::hours(1);
        let secs = chrono::Duration::seconds;
        state.demo_ops.store.start_op(older, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, cutoff - secs(2000)).expect("start");
        state.demo_ops.store.start_op(newer, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, cutoff - secs(1000)).expect("start");

        assert_eq!(reap_once(&state, &fixed(&state, 800), cutoff, &mut ReaperWarns::new()).await, 1);
        assert_eq!(lifecycle(&state, older).await, CampaignLifecycle::Archived, "the older goes");
        assert_eq!(lifecycle(&state, newer).await, CampaignLifecycle::Active, "back under 800: kept");
    }

    /// BP-46 V1: the valve keeps the 15-minute line while the unplayed line moves to 60. The cutoff
    /// is production's (`DEMO_REAP_IDLE_AFTER_SECS` before "now"), with "now" `minutes_in` after
    /// the launch. An unplayed campaign 30 minutes in is kept at 799 live and retired at 800; one
    /// 10 minutes in is kept at 800. Decides the valve's offset at `reap_once`'s `flat`.
    #[tokio::test]
    async fn the_valve_keeps_15_minutes_for_an_unplayed_campaign() {
        for (minutes_in, live, retired, after) in [
            (30, 799, 0, CampaignLifecycle::Active),
            (30, 800, 1, CampaignLifecycle::Archived),
            (10, 800, 0, CampaignLifecycle::Active),
        ] {
            let state = state();
            let (c, _) = campaign(&state, true).await;
            let cutoff = Utc::now() + chrono::Duration::minutes(minutes_in) - chrono::Duration::seconds(DEMO_REAP_IDLE_AFTER_SECS);

            assert_eq!(reap_once(&state, &fixed(&state, live), cutoff, &mut ReaperWarns::new()).await, retired, "{minutes_in} min in, at {live} live");
            assert_eq!(lifecycle(&state, c).await, after, "{minutes_in} min in, at {live} live");
        }
    }

    /// BP-46 V2: a played campaign whose last move was 2,000 s (33 minutes) before "now" is past
    /// the valve's 15-minute line and inside the 48-hour line: retired at 800 live, kept at 799.
    /// The move is an hour ahead of the launch, so it is the campaign's latest activity.
    #[tokio::test]
    async fn the_valve_retires_a_played_campaign_idle_33_minutes() {
        let secs = chrono::Duration::seconds;
        for (live, retired, after) in [(800, 1, CampaignLifecycle::Archived), (799, 0, CampaignLifecycle::Active)] {
            let state = state();
            let (c, _) = campaign(&state, true).await;
            let moved = Utc::now() + chrono::Duration::hours(1);
            state.demo_ops.store.start_op(c, (5, 0), ops_board(), &ops_dig(), 7, ops_crew(), None, moved).expect("start");
            let cutoff = moved + secs(2000) - secs(DEMO_REAP_IDLE_AFTER_SECS);

            assert_eq!(reap_once(&state, &fixed(&state, live), cutoff, &mut ReaperWarns::new()).await, retired, "at {live} live");
            assert_eq!(lifecycle(&state, c).await, after, "at {live} live");
        }
    }

    /// BP-42 L6: the mirror's live count is `Pending` + `Active` + `Ending`.
    #[tokio::test]
    async fn in_memory_count_live_counts_pending_active_ending() {
        let state = state();
        let store = store(&state);
        let _pending = campaign(&state, false).await;
        let _a = campaign(&state, true).await;
        let _b = campaign(&state, true).await;
        let (archived, _) = campaign(&state, true).await;
        assert!(state.campaign_repo.retire_if_active(archived).await.expect("retire_if_active"));

        assert_eq!(store.count_live().await.expect("count_live"), 3);
    }

    /// BP-42 L9: store level. A campaign whose array move lies between the lines (played =
    /// now − 1 h, unplayed = now + 1 h, move at now) is not listed; with no array entry it is
    /// unplayed and listed under the same lines.
    #[tokio::test]
    async fn in_memory_list_idle_judges_a_moved_campaign_by_the_played_line() {
        let state = state();
        let store = store(&state);
        let (c, s) = campaign(&state, true).await;
        let now = Utc::now();
        let cutoffs = ReapCutoffs { played: now - chrono::Duration::hours(1), unplayed: now + chrono::Duration::hours(1) };

        assert_eq!(store.list_idle(cutoffs, &[c], &[now], 10).await.expect("list_idle"), vec![]);
        assert_eq!(store.list_idle(cutoffs, &[], &[], 10).await.expect("list_idle"), vec![(c, s)]);
    }

    /// BP-42 L10: store level. A move at the played line keeps the campaign; with no move it is
    /// retired.
    #[tokio::test]
    async fn in_memory_retire_idle_keeps_a_move_after_the_played_line() {
        let state = state();
        let store = store(&state);
        let (c, _) = campaign(&state, true).await;
        let now = Utc::now();
        let cutoffs = ReapCutoffs { played: now, unplayed: now + chrono::Duration::hours(1) };

        assert!(!store.retire_idle(c, cutoffs, Some(now)).await.expect("retire_idle"));
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Active);
        assert!(store.retire_idle(c, cutoffs, None).await.expect("retire_idle"));
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Archived);
    }

    /// BP-8a-2: the sweep prunes the entries of campaigns that are no longer `Active`, even
    /// when it retires nothing itself.
    #[tokio::test]
    async fn reaper_sweep_prunes_entries_of_non_active_campaigns() {
        let state = state();
        let store = store(&state);
        let (live, _) = campaign(&state, true).await;
        let (gone, _) = campaign(&state, true).await;
        let earlier = Utc::now() - chrono::Duration::hours(1);
        state.demo_ops.store.put_board(live, (5, 0), ops_board(), earlier);
        state.demo_ops.store.put_board(gone, (5, 0), ops_board(), earlier);
        assert!(state.campaign_repo.retire_if_active(gone).await.expect("retire_if_active"));
        let past = DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z").unwrap().with_timezone(&Utc);

        assert_eq!(reap_once(&state, &store, past, &mut ReaperWarns::new()).await, 0);
        assert_eq!(state.demo_ops.store.len(), 1);
        assert!(state.demo_ops.store.board(live, (5, 0), Utc::now()).is_some(), "the live campaign's entry is kept");
    }

    /// BP-8a-2: the reaper drops the entry of a campaign it retires, in the same sweep.
    #[tokio::test]
    async fn reaper_drops_the_entry_of_a_campaign_it_retires() {
        let state = state();
        let store = store(&state);
        let (c, _) = campaign(&state, true).await;
        state.demo_ops.store.put_board(c, (5, 0), ops_board(), Utc::now());

        assert_eq!(reap_once(&state, &store, Utc::now() + chrono::Duration::hours(1), &mut ReaperWarns::new()).await, 1);
        assert_eq!(lifecycle(&state, c).await, CampaignLifecycle::Archived);
        assert!(state.demo_ops.store.is_empty());
    }
}
