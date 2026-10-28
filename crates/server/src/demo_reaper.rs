//! The demo reaper (BP-3c-1b-1, ODQ-67 (b), **demo-only**). Every 5 minutes it archives each
//! `Active` demo campaign whose latest mission start (or, with none, its launch) is more than
//! 15 minutes old, so a slot an attendee walked away from frees itself (the demo holds at most
//! 1,000 live campaigns, ODQ-55). A campaign with a convoy in transit is kept until the convoy
//! lands, as the abandon keeps it. The `dev` port deletes this file, its `lib.rs` line and its
//! `main.rs` spawn.
//!
//! Per campaign: the launch lock, then `retire_if_idle`, then the sector delete (retire first,
//! then delete, as `retire_campaign`), never inside a transaction or under `JOIN_ADVISORY_KEY`.
//! The idle test sits inside the retire's `UPDATE`, so a mission whose deployment row commits
//! before it saves the campaign. Accepted residual: a mission that passed the route's unlocked
//! `Active` check but inserts after the retire resolves on an `Archived` campaign; the player's
//! next call gets `409` and the client re-joins.

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
/// A campaign is idle once its latest mission start (or, with none, its launch) is this old.
pub const DEMO_REAP_IDLE_AFTER_SECS: i64 = 900;
/// At most this many campaigns per sweep, oldest first, so one sweep's Postgres work stays
/// bounded (about 0.5 s at 20,000 deployment rows) and 1,000 idle campaigns clear in 25 minutes.
pub const DEMO_REAP_MAX_PER_SWEEP: i64 = 200;
/// A failure logs its first occurrence, then about hourly.
const REAP_WARN_EVERY_SWEEPS: u32 = 12;

/// What the reaper reads and writes. Pairs are `(campaign_id, sector_id)`.
#[async_trait]
pub trait ReaperStore: Send + Sync {
    /// Up to `limit` `Active` campaigns idle since before `cutoff`, oldest first.
    async fn list_idle_campaigns(&self, cutoff: DateTime<Utc>, limit: i64) -> Result<Vec<(Uuid, Uuid)>, RepositoryError>;
    /// Archives the campaign only if it is still `Active` and still idle since before `cutoff`.
    async fn retire_if_idle(&self, campaign_id: Uuid, cutoff: DateTime<Utc>) -> Result<bool, RepositoryError>;
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
    async fn list_idle_campaigns(&self, cutoff: DateTime<Utc>, limit: i64) -> Result<Vec<(Uuid, Uuid)>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT c.campaign_id AS "campaign_id!", c.sector_id AS "sector_id!"
               FROM campaign_instances c
               LEFT JOIN (SELECT cr.campaign_id, max(d.started_at) AS last_start
                          FROM deployment_records d
                          JOIN commander_records cr ON cr.commander_id = d.commander_id
                          GROUP BY cr.campaign_id) m ON m.campaign_id = c.campaign_id
               WHERE c.state = $1
                 AND (COALESCE(m.last_start, c.started_at) < $2)
                 AND NOT EXISTS (SELECT 1 FROM convoy_records v
                                 WHERE v.sector_id = c.sector_id AND v.in_transit
                                   AND v.arrival_time > now() - interval '5 minutes')
               ORDER BY COALESCE(m.last_start, c.started_at)
               LIMIT $3"#,
            CampaignLifecycle::Active as CampaignLifecycle,
            cutoff,
            limit,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(|r| (r.campaign_id, r.sector_id)).collect())
    }

    /// The idle and convoy tests are re-evaluated inside the `UPDATE`, so a deployment row
    /// committed after the reaper's select and before this statement keeps the campaign.
    async fn retire_if_idle(&self, campaign_id: Uuid, cutoff: DateTime<Utc>) -> Result<bool, RepositoryError> {
        let result = sqlx::query!(
            r#"UPDATE campaign_instances c SET state = $1, updated_at = now()
               WHERE c.campaign_id = $2 AND c.state = $3
                 AND (COALESCE((SELECT max(d.started_at) FROM deployment_records d
                                JOIN commander_records cr ON cr.commander_id = d.commander_id
                                WHERE cr.campaign_id = c.campaign_id), c.started_at) < $4)
                 AND NOT EXISTS (SELECT 1 FROM convoy_records v
                                 WHERE v.sector_id = c.sector_id AND v.in_transit
                                   AND v.arrival_time > now() - interval '5 minutes')"#,
            CampaignLifecycle::Archived as CampaignLifecycle,
            campaign_id,
            CampaignLifecycle::Active as CampaignLifecycle,
            cutoff,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected() == 1)
    }
}

/// The in-memory mirror. It sees no deployments and no convoys: it lists `Active` campaigns
/// launched before `cutoff`, and its `retire_if_idle` checks `Active` only (as
/// `retire_if_active`). The Postgres tests in `db_infrastructure.rs` are the oracle.
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
    async fn list_idle_campaigns(&self, cutoff: DateTime<Utc>, limit: i64) -> Result<Vec<(Uuid, Uuid)>, RepositoryError> {
        let mut idle: Vec<(DateTime<Utc>, Uuid, Uuid)> = self
            .campaigns
            .list_campaigns_by_state(CampaignLifecycle::Active)
            .await?
            .into_iter()
            .filter_map(|c| c.started_at.filter(|s| *s < cutoff).map(|s| (s, c.campaign_id, c.sector_id)))
            .collect();
        idle.sort_by_key(|(s, ..)| *s);
        Ok(idle.into_iter().take(limit.max(0) as usize).map(|(_, c, s)| (c, s)).collect())
    }

    async fn retire_if_idle(&self, campaign_id: Uuid, _cutoff: DateTime<Utc>) -> Result<bool, RepositoryError> {
        self.campaigns.retire_if_active(campaign_id).await
    }
}

/// Repeat-warning state: the select, and one keyed counter per campaign for the retire.
/// Built fresh by the task, so a supervised restart warns again.
#[derive(Debug)]
pub struct ReaperWarns {
    pub select: RepeatWarn,
    pub retire: KeyedRepeatWarn,
}

impl ReaperWarns {
    pub fn new() -> Self {
        Self {
            select: RepeatWarn::new(REAP_WARN_EVERY_SWEEPS),
            retire: KeyedRepeatWarn::new(REAP_WARN_EVERY_SWEEPS),
        }
    }
}

impl Default for ReaperWarns {
    fn default() -> Self {
        Self::new()
    }
}

/// One sweep. Returns how many campaigns it archived.
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
    let idle = match store.list_idle_campaigns(cutoff, DEMO_REAP_MAX_PER_SWEEP).await {
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
        match store.retire_if_idle(campaign_id, cutoff).await {
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

    /// The canon numbers of ODQ-67 (b), as literals.
    #[test]
    fn reaper_numbers_are_odq_67s() {
        assert_eq!(DEMO_REAP_INTERVAL, Duration::from_secs(300), "every 5 minutes");
        assert_eq!(DEMO_REAP_IDLE_AFTER_SECS, 900, "idle after 15 minutes");
        assert_eq!(DEMO_REAP_MAX_PER_SWEEP, 200);
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
}
