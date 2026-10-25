use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::mpsc::{self, error::TrySendError};
use uuid::Uuid;

use crate::convoy_collision::check_same_hex_collision;
use crate::rate_limit::RateWarn;
use crate::repeat_warn::{KeyedRepeatWarn, RepeatWarn};
use crate::repository::{HexOccupant, TimerType};
use crate::state::{AppState, CONVOY_EVENT_CHANNEL};

#[derive(Debug, Clone, serde::Serialize)]
pub enum ConvoyEvent {
    ConvoyArrived {
        convoy_id:     Uuid,
        destination_q: i32,
        destination_r: i32,
        cargo:         serde_json::Value,
    },
}

pub const EXPIRY_POLL_INTERVAL_SECS: u64 = 10;

/// Repeat a persistent failure, per call or per convoy, about once an hour (GAP-38).
pub const EXPIRY_WARN_EVERY_TICKS: u32 = (3600 / EXPIRY_POLL_INTERVAL_SECS) as u32;

fn notify_player_convoy_arrived(
    senders: &DashMap<Uuid, mpsc::Sender<ConvoyEvent>>,
    campaign_id: Uuid,
    event: ConvoyEvent,
    full: &RateWarn,
) {
    // Clone the sender out so the map guard is dropped before any removal: a
    // removal under the guard waits on the shard it holds.
    let Some(ours) = senders.get(&campaign_id).map(|s| s.clone()) else {
        return;
    };
    deliver(senders, campaign_id, ours, event, full, Instant::now());
}

/// Sends without waiting. A full channel means the socket has stopped reading, so its
/// sender is dropped (journalled at most hourly); a closed one is a normal disconnect
/// and stays silent. Only `ours` is removed, never a sender that replaced it after the clone.
fn deliver(
    senders: &DashMap<Uuid, mpsc::Sender<ConvoyEvent>>,
    campaign_id: Uuid,
    ours: mpsc::Sender<ConvoyEvent>,
    event: ConvoyEvent,
    full: &RateWarn,
    now: Instant,
) {
    if let Err(e) = ours.try_send(event) {
        if matches!(e, TrySendError::Full(_)) {
            if let Some(n) = full.hit(now) {
                tracing::warn!(
                    campaign_id = %campaign_id,
                    dropped = n,
                    capacity = CONVOY_EVENT_CHANNEL,
                    "convoy events channel full: the socket is not reading, its sender is dropped"
                );
            }
        }
        senders.remove_if(&campaign_id, |_, s| s.same_channel(&ours));
    }
}

/// Repeat-warning state for the convoy expiry task: the due-timer scan, and one keyed
/// counter (by timer id) per per-convoy failure site.
#[derive(Debug)]
pub struct ExpiryWarns {
    pub due_timers: RepeatWarn,
    pub sector: KeyedRepeatWarn,
    pub land: KeyedRepeatWarn,
    pub get_convoy: KeyedRepeatWarn,
    pub cancel_timer: KeyedRepeatWarn,
    pub phantom: KeyedRepeatWarn,
    /// Keyed by the raw zset member: a poisoned member may not be a UUID (GAP-53).
    pub poisoned: KeyedRepeatWarn<String>,
    /// A convoy events socket whose channel was full (it stopped reading); journalled
    /// on the first drop, then at most hourly with a count.
    pub events_full: RateWarn,
}

impl ExpiryWarns {
    pub fn new() -> Self {
        Self {
            due_timers: RepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            sector: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            land: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            get_convoy: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            cancel_timer: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            phantom: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            poisoned: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            events_full: RateWarn::new(),
        }
    }
}

impl Default for ExpiryWarns {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs one scan of due timers and lands each arrived convoy. Every failure, of the scan
/// or of one convoy, logs its first occurrence and then about once an hour, and logs its
/// recovery once. Per-convoy counters are evicted when the timer leaves the due set.
pub async fn poll_expiry_once(state: &AppState, warns: &mut ExpiryWarns) {
    let scan = match state.timer_repo.get_due_timers(Utc::now()).await {
        Ok(v) => {
            if let Some(n) = warns.due_timers.ok() {
                tracing::info!("poll_expiry_once: get_due_timers recovered after {n} failed polls");
            }
            v
        }
        Err(e) => {
            match warns.due_timers.fail() {
                Some(1) => tracing::warn!("poll_expiry_once: get_due_timers failed: {e}"),
                Some(n) => tracing::warn!("poll_expiry_once: get_due_timers still failing ({n} consecutive polls): {e}"),
                None => {}
            }
            return;
        }
    };

    // A poisoned member is taken out of the scan, so it cannot stall the good timers (GAP-53).
    let poisoned_live: HashSet<String> = scan.poisoned.iter().map(|p| p.member.clone()).collect();
    warns.poisoned.retain(&poisoned_live);
    for p in &scan.poisoned {
        let member = &p.member;
        let quarantine = match state.timer_repo.quarantine_timer(member).await {
            Ok(()) => "quarantined".to_string(),
            Err(e) => format!("quarantine failed, retrying next poll: {e:?}"),
        };
        match warns.poisoned.fail(member.clone()) {
            Some(1) => tracing::warn!("poisoned timer member {member:?} ({}) — {quarantine}", p.reason),
            Some(n) => tracing::warn!("poisoned timer member {member:?} still due ({n} consecutive polls; {}) — {quarantine}", p.reason),
            None => {}
        }
    }

    let timers = scan.due;
    let live: HashSet<Uuid> = timers
        .iter()
        .filter(|t| matches!(t.timer_type, TimerType::ConvoyArrival))
        .map(|t| t.timer_id)
        .collect();
    for keyed in [&mut warns.sector, &mut warns.land, &mut warns.get_convoy, &mut warns.cancel_timer, &mut warns.phantom] {
        keyed.retain(&live);
    }

    // Per convoy (GAP-44): resolve the campaign, then land and log in one transaction,
    // then the Redis side effects, and cancel the timer last. A retry after any failure
    // re-runs only what has not happened: the log row is written exactly once.
    for timer in timers {
        match timer.timer_type {
            TimerType::ConvoyArrival => {}
            _ => continue, // no consumer yet for other timer types
        }
        let id = timer.timer_id;

        // 1. The campaign the arrival is logged under. Without it nothing is written,
        //    and the next poll retries.
        let sector = match state.sector_repo.get_sector(timer.sector_id).await {
            Ok(Some(s)) => {
                if let Some(n) = warns.sector.ok(id) {
                    tracing::info!("sector lookup recovered for convoy {id} after {n} failed polls");
                }
                s
            }
            found => {
                let why = match found {
                    Err(e) => format!("get_sector failed: {e:?}"),
                    _ => format!("sector {} not found in Redis", timer.sector_id),
                };
                match warns.sector.fail(id) {
                    Some(1) => tracing::warn!("convoy {id} not landed — {why}"),
                    Some(n) => tracing::warn!("convoy {id} still not landed ({n} consecutive polls) — {why}"),
                    None => {}
                }
                continue;
            }
        };
        let cid = sector.campaign_id;

        // 2. Land and log together. INVARIANT: timer_id == convoy_id (3b-2).
        let entry = shared::InputLogEntry {
            tick: 0,
            seq: 0,
            event_type: "convoy_arrived".to_string(),
            player_id: Some(bs58::encode(timer.player_wallet.0).into_string()),
            payload: serde_json::json!({ "convoy_id": id }),
            narrative_event: None,
        };
        let landed = match state.convoy_repo.land_convoy(id, cid, &entry).await {
            Ok(l) => {
                if let Some(n) = warns.land.ok(id) {
                    tracing::info!("land_convoy recovered for convoy {id} after {n} failed polls");
                }
                l
            }
            Err(e) => {
                match warns.land.fail(id) {
                    Some(1) => tracing::warn!("land_convoy failed for convoy {id}: {e:?}"),
                    Some(n) => tracing::warn!("land_convoy still failing for convoy {id} ({n} consecutive polls): {e:?}"),
                    None => {}
                }
                continue;
            }
        };

        match landed {
            // 3. Landed by this call: the side effects run here and only here.
            Some(convoy) => land_side_effects(state, &sector, cid, &convoy, &warns.events_full).await,
            // 4. Nothing landed: either an earlier poll landed it and only the cancel is
            //    outstanding, or there is no convoy at all (a phantom timer).
            None => match state.convoy_repo.get_convoy(id).await {
                Ok(found) => {
                    if let Some(n) = warns.get_convoy.ok(id) {
                        tracing::info!("get_convoy recovered for timer {id} after {n} failed polls");
                    }
                    if found.is_none() {
                        let cleanup = match state.timer_repo.cancel_timer(id).await {
                            Ok(()) => "timer cancelled".to_string(),
                            Err(e) => format!("cancel_timer failed: {e:?}"),
                        };
                        match warns.phantom.fail(id) {
                            Some(1) => tracing::warn!(
                                "ConvoyArrival timer {id} fired but no convoy record found (phantom timer) — cleaning up: {cleanup}"
                            ),
                            Some(n) => tracing::warn!(
                                "phantom ConvoyArrival timer {id} still due ({n} consecutive polls) — cleaning up: {cleanup}"
                            ),
                            None => {}
                        }
                        continue;
                    }
                }
                Err(e) => {
                    match warns.get_convoy.fail(id) {
                        Some(1) => tracing::warn!("get_convoy failed for timer {id}: {e:?}"),
                        Some(n) => tracing::warn!("get_convoy still failing for timer {id} ({n} consecutive polls): {e:?}"),
                        None => {}
                    }
                    continue;
                }
            },
        }

        // 5. Cancel last. If it fails, the next poll's land_convoy returns None, so only
        //    this cancel is retried.
        match state.timer_repo.cancel_timer(id).await {
            Ok(()) => {
                if let Some(n) = warns.cancel_timer.ok(id) {
                    tracing::info!("cancel_timer recovered for convoy {id} after {n} failed polls");
                }
            }
            Err(e) => {
                match warns.cancel_timer.fail(id) {
                    Some(1) => tracing::warn!("cancel_timer failed for convoy {id} — Redis keys may be stale: {e:?}"),
                    Some(n) => tracing::warn!("cancel_timer still failing for convoy {id} ({n} consecutive polls) — Redis keys may be stale: {e:?}"),
                    None => {}
                }
            }
        }
    }
}

/// The Redis side of an arrival, run once by the poll that landed the convoy: move the
/// occupant, check same-hex collision, notify. Failures are warned, never retried, since
/// the arrival itself is already committed and logged.
async fn land_side_effects(
    state:  &AppState,
    sector: &crate::repository::SectorState,
    cid:    Uuid,
    convoy: &crate::repository::ConvoyDbRecord,
    full:   &RateWarn,
) {
    let sector_id = convoy.sector_id;
    if let Err(e) = state.sector_repo
        .remove_hex_occupant(sector_id, convoy.origin_q, convoy.origin_r, convoy.convoy_id)
        .await
    {
        tracing::warn!("remove_hex_occupant failed for convoy {}: {:?}", convoy.convoy_id, e);
    }

    let occupant = HexOccupant {
        id: convoy.convoy_id,
        owner_wallet: convoy.owner_wallet.clone(),
        unit_type: convoy.vehicle_class.clone(),
    };
    // NOTE: arrival at impassable terrain is possible if sector was cold at dispatch time.
    // TODO(phase-2): validate the route fully before dispatch.
    if let Err(e) = state.sector_repo
        .add_hex_occupant(sector_id, convoy.destination_q, convoy.destination_r, occupant)
        .await
    {
        tracing::warn!("add_hex_occupant failed for convoy {}: {:?}", convoy.convoy_id, e);
    }

    // Same-hex collision check: build post-update occupants in memory from pre-update sector state
    // (`sector` is pre-update — it was read before remove/add_hex_occupant)
    let dest_key = format!("{},{}", convoy.destination_q, convoy.destination_r);
    let mut occupants: Vec<HexOccupant> = sector
        .hex_occupancy
        .get(&dest_key)
        .cloned()
        .unwrap_or_default();
    occupants.push(HexOccupant {
        id:           convoy.convoy_id,
        owner_wallet: convoy.owner_wallet.clone(),
        unit_type:    convoy.vehicle_class.clone(),
    });
    if let Some(pair) = check_same_hex_collision(&occupants) {
        tracing::warn!(
            "collision detected at ({},{}): {:?}",
            convoy.destination_q, convoy.destination_r, pair
        );
        // TODO(phase-2): call resolveCombat here
    }
    // TODO(phase-2): check_crossing_collision requires ConvoyRepository::list_in_transit — deferred

    let event = ConvoyEvent::ConvoyArrived {
        convoy_id:     convoy.convoy_id,
        destination_q: convoy.destination_q,
        destination_r: convoy.destination_r,
        cargo:         convoy.cargo.clone(),
    };
    notify_player_convoy_arrived(&state.convoy_event_senders, cid, event, full);
}

pub async fn run_convoy_expiry_task(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(EXPIRY_POLL_INTERVAL_SECS));
    let mut warns = ExpiryWarns::new();
    loop {
        interval.tick().await;
        poll_expiry_once(state.as_ref(), &mut warns).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::repository::{
        DeploymentTimer, DueTimers, OccupationStatus, PoisonedTimer, PostgresConvoyRepository,
        RepositoryError, SectorState, TimerRepository, WalletAddress,
    };

    /// A journal capture buffer (the `routes/mod.rs` pattern).
    #[derive(Clone, Default)]
    struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs `f` under a capturing subscriber and returns the journal text.
    fn journal(f: impl FnOnce()) -> String {
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt().with_writer(move || w.clone()).with_ansi(false).finish();
        tracing::subscriber::with_default(sub, f);
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        text
    }

    fn arrived() -> ConvoyEvent {
        ConvoyEvent::ConvoyArrived {
            convoy_id: Uuid::new_v4(),
            destination_q: 3,
            destination_r: 2,
            cargo: serde_json::json!({}),
        }
    }

    #[test]
    fn notify_drops_a_full_sender_and_journals() {
        let senders = DashMap::new();
        let id = Uuid::new_v4();
        let (tx, _rx) = mpsc::channel::<ConvoyEvent>(1);
        senders.insert(id, tx);
        let full = RateWarn::new();

        let text = journal(|| {
            notify_player_convoy_arrived(&senders, id, arrived(), &full);
            assert!(senders.contains_key(&id), "a send with room keeps the sender");
            notify_player_convoy_arrived(&senders, id, arrived(), &full);
        });
        assert!(!senders.contains_key(&id), "a full channel drops its sender");
        assert!(full.fired(), "the drop is counted");
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("convoy events channel full")).collect();
        assert_eq!(lines.len(), 1, "one journal line: {text}");
        assert!(lines[0].contains(&id.to_string()), "the line names the campaign: {text}");
    }

    #[test]
    fn notify_drops_a_closed_sender_silently() {
        let senders = DashMap::new();
        let id = Uuid::new_v4();
        let (tx, rx) = mpsc::channel::<ConvoyEvent>(1);
        drop(rx);
        senders.insert(id, tx);
        let full = RateWarn::new();

        let text = journal(|| notify_player_convoy_arrived(&senders, id, arrived(), &full));
        assert!(!senders.contains_key(&id), "a closed channel's sender is removed");
        assert!(!full.fired(), "a disconnect is not a full channel");
        assert_eq!(text.lines().filter(|l| l.contains("channel full")).count(), 0, "{text}");
    }

    #[test]
    fn deliver_keeps_a_sender_that_replaced_ours() {
        let senders = DashMap::new();
        let id = Uuid::new_v4();
        let (b, _rx_b) = mpsc::channel::<ConvoyEvent>(1);
        let b_probe = b.clone();
        senders.insert(id, b);
        let (a, _rx_a) = mpsc::channel::<ConvoyEvent>(1);
        let full = RateWarn::new();
        let now = Instant::now();

        deliver(&senders, id, a.clone(), arrived(), &full, now); // fills A
        deliver(&senders, id, a.clone(), arrived(), &full, now); // A is full
        let kept = senders.get(&id).map(|s| s.same_channel(&b_probe));
        assert_eq!(kept, Some(true), "a stale sender's drop must not take the one that replaced it");
    }

    #[tokio::test]
    async fn poll_expiry_suppresses_per_timer_and_evicts() {
        // A closed lazy pool fails every query without a database (INF-3a pattern).
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .expect("lazy pool");
        pool.close().await;

        let mut state = AppState::new(Arc::new(Config::default()));
        state.convoy_repo = Arc::new(PostgresConvoyRepository::new(pool));
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [a, b] {
            // Each timer's sector exists, so the poll reaches land_convoy, which fails.
            let sector_id = Uuid::new_v4();
            state.sector_repo.upsert_sector(SectorState {
                sector_id,
                campaign_id: Uuid::new_v4(),
                occupation_status: OccupationStatus::Neutral,
                owner: None,
                deployed_unit_count: 0,
                terrain: Default::default(),
                magma_veins: vec![],
                hex_occupancy: Default::default(),
            }).await.expect("in-memory upsert_sector");
            state.timer_repo.schedule_timer(DeploymentTimer {
                timer_id: id,
                player_wallet: WalletAddress([0u8; 32]),
                sector_id,
                timer_type: TimerType::ConvoyArrival,
                fires_at: Utc::now() - chrono::Duration::seconds(1),
            }).await.expect("in-memory schedule");
        }
        let mut warns = ExpiryWarns::new();

        poll_expiry_once(&state, &mut warns).await;
        assert_eq!(warns.land.len(), 2, "each failing timer gets its own counter");

        state.timer_repo.cancel_timer(a).await.expect("in-memory cancel");
        poll_expiry_once(&state, &mut warns).await;
        assert_eq!(warns.land.len(), 1, "a timer that left the due set is evicted");
    }

    /// Reports `poisoned` members (none due) and records every quarantine call.
    struct PoisonedScan {
        poisoned: std::sync::Mutex<Vec<String>>,
        quarantined: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl TimerRepository for PoisonedScan {
        async fn schedule_timer(&self, _: DeploymentTimer) -> Result<(), RepositoryError> {
            Ok(())
        }
        async fn cancel_timer(&self, _: Uuid) -> Result<(), RepositoryError> {
            Ok(())
        }
        async fn get_due_timers(&self, _: chrono::DateTime<Utc>) -> Result<DueTimers, RepositoryError> {
            let poisoned = self.poisoned.lock().unwrap().iter()
                .map(|m| PoisonedTimer { member: m.clone(), reason: "test".into() })
                .collect();
            Ok(DueTimers { due: Vec::new(), poisoned })
        }
        async fn quarantine_timer(&self, member: &str) -> Result<(), RepositoryError> {
            self.quarantined.lock().unwrap().push(member.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn poll_expiry_quarantines_poisoned_members() {
        let stub = Arc::new(PoisonedScan {
            poisoned: std::sync::Mutex::new(vec!["not-a-uuid".to_string()]),
            quarantined: std::sync::Mutex::new(Vec::new()),
        });
        let mut state = AppState::new(Arc::new(Config::default()));
        state.timer_repo = stub.clone();
        let mut warns = ExpiryWarns::new();

        poll_expiry_once(&state, &mut warns).await;
        assert_eq!(*stub.quarantined.lock().unwrap(), ["not-a-uuid"], "the poisoned member is quarantined");
        assert_eq!(warns.poisoned.len(), 1, "the poisoned member is warned under its own key");

        stub.poisoned.lock().unwrap().clear();
        poll_expiry_once(&state, &mut warns).await;
        assert_eq!(warns.poisoned.len(), 0, "a member that left the scan is evicted");
    }
}
