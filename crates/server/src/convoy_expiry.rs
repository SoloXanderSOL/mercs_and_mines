use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::convoy_collision::check_same_hex_collision;
use crate::repeat_warn::{KeyedRepeatWarn, RepeatWarn};
use crate::repository::{HexOccupant, TimerType};
use crate::state::AppState;

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
    senders:     &DashMap<Uuid, UnboundedSender<ConvoyEvent>>,
    campaign_id: Uuid,
    event:       ConvoyEvent,
) {
    // NOTE: may fire more than once per convoy if mark_arrived succeeds but timer cleanup
    // fails on the next retry — client must treat convoy_arrived as idempotent.
    if let Some(sender) = senders.get(&campaign_id) {
        let _ = sender.send(event);
    }
}

/// Repeat-warning state for the convoy expiry task: the due-timer scan, and one keyed
/// counter (by timer id) per per-convoy failure site.
#[derive(Debug)]
pub struct ExpiryWarns {
    pub due_timers: RepeatWarn,
    pub mark_arrived: KeyedRepeatWarn,
    pub get_convoy: KeyedRepeatWarn,
    pub cancel_timer: KeyedRepeatWarn,
    pub phantom: KeyedRepeatWarn,
}

impl ExpiryWarns {
    pub fn new() -> Self {
        Self {
            due_timers: RepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            mark_arrived: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            get_convoy: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            cancel_timer: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
            phantom: KeyedRepeatWarn::new(EXPIRY_WARN_EVERY_TICKS),
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
    let timers = match state.timer_repo.get_due_timers(Utc::now()).await {
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
    let live: HashSet<Uuid> = timers
        .iter()
        .filter(|t| matches!(t.timer_type, TimerType::ConvoyArrival))
        .map(|t| t.timer_id)
        .collect();
    for keyed in [&mut warns.mark_arrived, &mut warns.get_convoy, &mut warns.cancel_timer, &mut warns.phantom] {
        keyed.retain(&live);
    }

    for timer in timers {
        match timer.timer_type {
            TimerType::ConvoyArrival => {}
            _ => continue, // other timer types managed by lifecycle task, not convoy expiry
        }
        let id = timer.timer_id;

        // INVARIANT: timer_id == convoy_id — established in dispatch_convoy (3b-2)
        match state.convoy_repo.mark_arrived(id).await {
            Ok(()) => {
                if let Some(n) = warns.mark_arrived.ok(id) {
                    tracing::info!("mark_arrived recovered for convoy {id} after {n} failed polls");
                }
            }
            Err(e) => {
                match warns.mark_arrived.fail(id) {
                    Some(1) => tracing::warn!("mark_arrived failed for convoy {id}: {e:?}"),
                    Some(n) => tracing::warn!("mark_arrived still failing for convoy {id} ({n} consecutive polls): {e:?}"),
                    None => {}
                }
                continue;
            }
        }

        let convoy = match state.convoy_repo.get_convoy(id).await {
            Ok(found) => {
                if let Some(n) = warns.get_convoy.ok(id) {
                    tracing::info!("get_convoy recovered for timer {id} after {n} failed polls");
                }
                match found {
                    Some(c) => c,
                    None => {
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
            }
            Err(e) => {
                match warns.get_convoy.fail(id) {
                    Some(1) => tracing::warn!("get_convoy failed for timer {id}: {e:?}"),
                    Some(n) => tracing::warn!("get_convoy still failing for timer {id} ({n} consecutive polls): {e:?}"),
                    None => {}
                }
                continue;
            }
        };

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
                // don't abort — convoy is already marked arrived; continue with hex update
            }
        }

        let sector_opt = match state.sector_repo.get_sector(timer.sector_id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "get_sector Redis error for timer {}: {:?} — skipping log and WS notify",
                    timer.timer_id, e
                );
                None
            }
        };
        let campaign_id = sector_opt.as_ref().map(|s| s.campaign_id);
        if campaign_id.is_none() {
            tracing::warn!(
                "sector {} not found in Redis for ConvoyArrival timer {} — skipping log entry",
                timer.sector_id, timer.timer_id
            );
        }

        if let Err(e) = state.sector_repo
            .remove_hex_occupant(timer.sector_id, convoy.origin_q, convoy.origin_r, convoy.convoy_id)
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
            .add_hex_occupant(
                timer.sector_id,
                convoy.destination_q,
                convoy.destination_r,
                occupant,
            )
            .await
        {
            tracing::warn!("add_hex_occupant failed for convoy {}: {:?}", convoy.convoy_id, e);
        }
        // TODO: active_timer_ids on SectorState is not updated by schedule_timer or cancel_timer — pre-existing debt

        // Same-hex collision check: build post-update occupants in memory from pre-update sector state
        // (sector_opt is pre-update — sector was read before remove/add_hex_occupant)
        if let Some(ref sector) = sector_opt {
            let dest_key = format!("{},{}", convoy.destination_q, convoy.destination_r);
            let mut occupants: Vec<HexOccupant> = sector
                .hex_occupancy
                .get(&dest_key)
                .cloned()
                .unwrap_or_default();
            let arriving_occupant = HexOccupant {
                id:           convoy.convoy_id,
                owner_wallet: convoy.owner_wallet.clone(),
                unit_type:    convoy.vehicle_class.clone(),
            };
            occupants.push(arriving_occupant);

            if let Some(pair) = check_same_hex_collision(&occupants) {
                tracing::warn!(
                    "collision detected at ({},{}): {:?}",
                    convoy.destination_q, convoy.destination_r, pair
                );
                // TODO(phase-2): call resolveCombat here
            }
        }
        // TODO(phase-2): check_crossing_collision requires ConvoyRepository::list_in_transit — deferred

        if let Some(cid) = campaign_id {
            let player_id = bs58::encode(&convoy.owner_wallet).into_string();
            let entry = shared::InputLogEntry {
                tick: 0,
                seq: 0,
                event_type: "convoy_arrived".to_string(),
                player_id: Some(player_id),
                payload: serde_json::json!({ "convoy_id": convoy.convoy_id }),
                narrative_event: None,
            };
            if let Err(e) = state.input_log_repo.append_campaign_entry(&cid, &entry).await {
                tracing::warn!(
                    "append_campaign_entry failed for convoy {}: {:?}",
                    convoy.convoy_id, e
                );
            }

            let event = ConvoyEvent::ConvoyArrived {
                convoy_id:     convoy.convoy_id,
                destination_q: convoy.destination_q,
                destination_r: convoy.destination_r,
                cargo:         convoy.cargo.clone(),
            };
            notify_player_convoy_arrived(&state.convoy_event_senders, cid, event);
        }
    }
}

pub async fn run_convoy_expiry_task(state: Arc<AppState>) {
    // TODO(phase-2): no saga/compensation for partial convoy dispatch failure —
    // orphaned in_transit=true records need a reconciliation sweep
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
    use crate::repository::{DeploymentTimer, PostgresConvoyRepository, WalletAddress};

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
            state.timer_repo.schedule_timer(DeploymentTimer {
                timer_id: id,
                player_wallet: WalletAddress([0u8; 32]),
                sector_id: Uuid::new_v4(),
                timer_type: TimerType::ConvoyArrival,
                fires_at: Utc::now() - chrono::Duration::seconds(1),
            }).await.expect("in-memory schedule");
        }
        let mut warns = ExpiryWarns::new();

        poll_expiry_once(&state, &mut warns).await;
        assert_eq!(warns.mark_arrived.len(), 2, "each failing timer gets its own counter");

        state.timer_repo.cancel_timer(a).await.expect("in-memory cancel");
        poll_expiry_once(&state, &mut warns).await;
        assert_eq!(warns.mark_arrived.len(), 1, "a timer that left the due set is evicted");
    }
}
