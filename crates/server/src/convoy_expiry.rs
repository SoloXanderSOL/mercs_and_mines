use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::convoy_collision::check_same_hex_collision;
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

pub async fn poll_expiry_once(state: &AppState) {
    let timers = match state.timer_repo.get_due_timers(Utc::now()).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("poll_expiry_once: get_due_timers failed: {e}");
            return;
        }
    };

    for timer in timers {
        match timer.timer_type {
            TimerType::ConvoyArrival => {}
            _ => continue, // other timer types managed by lifecycle task, not convoy expiry
        }

        // INVARIANT: timer_id == convoy_id — established in dispatch_convoy (3b-2)
        if let Err(e) = state.convoy_repo.mark_arrived(timer.timer_id).await {
            tracing::warn!("mark_arrived failed for convoy {}: {:?}", timer.timer_id, e);
            continue;
        }

        let convoy = match state.convoy_repo.get_convoy(timer.timer_id).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::warn!(
                    "ConvoyArrival timer {} fired but no convoy record found (phantom timer) — cleaning up",
                    timer.timer_id
                );
                let _ = state.timer_repo.cancel_timer(timer.timer_id).await;
                continue;
            }
            Err(e) => {
                tracing::warn!("get_convoy failed for timer {}: {:?}", timer.timer_id, e);
                continue;
            }
        };

        if let Err(e) = state.timer_repo.cancel_timer(timer.timer_id).await {
            tracing::warn!(
                "cancel_timer failed for convoy {} — Redis keys may be stale: {:?}",
                timer.timer_id, e
            );
            // don't abort — convoy is already marked arrived; continue with hex update
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
        // NOTE: arrival at impassable terrain is possible if sector was cold at dispatch time;
        // Phase 2 must validate route fully before dispatch
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
                // Phase 2: call resolveCombat here
                if let Some(cid) = campaign_id {
                    let collision_entry = shared::InputLogEntry {
                        tick: 0,
                        seq: 0,
                        event_type: "collision_detected".to_string(),
                        player_id: Some(bs58::encode(&pair.a.owner_wallet).into_string()),
                        payload: serde_json::json!({
                            "hex_q": convoy.destination_q,
                            "hex_r": convoy.destination_r,
                            "unit_a": pair.a.id,
                            "unit_b": pair.b.id,
                            "owner_b": bs58::encode(&pair.b.owner_wallet).into_string(),
                        }),
                        narrative_event: None,
                    };
                    if let Err(e) = state.input_log_repo.append_campaign_entry(&cid, &collision_entry).await {
                        tracing::warn!(
                            "failed to log collision_detected for campaign {}: {:?}", cid, e
                        );
                    }
                }
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
    loop {
        interval.tick().await;
        poll_expiry_once(state.as_ref()).await;
    }
}
