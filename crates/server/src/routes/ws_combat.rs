use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use shared::ws_events::{ClientCommand, CombatTickEvent};
use sim_engine::{
    resolver::resolve_combat_streaming,
    rng::generate_seed,
    types::CombatInitiationType,
};

use crate::{
    api_types::{convoy_vehicle_from_class, CombatResolveRequest},
    state::AppState,
};

pub async fn ws_stream_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
    let session = match state.combat_sessions.remove(&session_id) {
        Some((_, s)) => s,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    if session.created_at.elapsed() > Duration::from_secs(300) {
        return StatusCode::GONE.into_response();
    }

    let log_dir = state.log_dir.clone();
    ws.on_upgrade(move |socket| handle_ws(socket, session.params, session_id, log_dir))
        .into_response()
}

async fn handle_ws(
    mut socket: WebSocket,
    params: CombatResolveRequest,
    session_id: Uuid,
    log_dir: PathBuf,
) {
    // Serialize before any field is consumed by unwrap_or.
    let params_payload = serde_json::to_value(&params).unwrap_or_default();

    let seed = params.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let initiation = params.combat_initiation_type.unwrap_or(CombatInitiationType::Spotted);
    let max_ticks = params.max_ticks.unwrap_or(50) as u32;
    let defending: Vec<_> = params
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();

    let section = params.section;
    let vehicle = params.vehicle;

    // Open the log file. Failure is non-fatal — game session continues, error goes to stderr.
    let mut log = match crate::log_writer::SessionLogWriter::create(
        &log_dir, &session_id.to_string()
    ).await {
        Ok(w) => Some(w),
        Err(e) => { eprintln!("[batch4] failed to create log for {session_id}: {e}"); None }
    };

    let session_config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: "combat_session".into(),
        campaign_id: "phase0".into(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };

    if let Some(w) = log.as_mut() {
        w.write_header(&session_config).await;
        w.append(&shared::InputLogEntry {
            tick: 0,
            seq: 0,
            event_type: "session_start".into(),
            player_id: None,
            payload: serde_json::json!({
                "request":   params_payload,
                "timestamp": &timestamp,
            }),
            narrative_event: None,
        }).await;
    }

    let (tick_tx, mut tick_rx) = mpsc::channel::<CombatTickEvent>(32);
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Spawn resolver — section/vehicle/timestamp are moved into the async block
    // so the future is 'static and safe for tokio::spawn.
    let resolver_handle = tokio::spawn(async move {
        resolve_combat_streaming(
            &section,
            &vehicle,
            &timestamp,
            max_ticks,
            Some(seed),
            initiation,
            defending,
            tick_tx,
            cancel_rx,
        )
        .await;
    });

    let mut cancel_tx = Some(cancel_tx);
    let mut last_tick: u64 = 0;

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if serde_json::from_str::<ClientCommand>(&text)
                            .map(|c| matches!(c, ClientCommand::Retreat))
                            .unwrap_or(false)
                        {
                            if let Some(tx) = cancel_tx.take() {
                                let _ = tx.send(());
                            }
                            if let Some(w) = log.as_mut() {
                                w.append(&shared::InputLogEntry {
                                    tick: last_tick,
                                    seq: 1,
                                    event_type: "player_input".into(),
                                    player_id: None,
                                    payload: serde_json::json!({"action": "retreat"}),
                                    narrative_event: None,
                                }).await;
                            }
                        }
                    }
                    // Client disconnected or WS error — cancel resolver
                    None | Some(Err(_)) => {
                        if let Some(tx) = cancel_tx.take() {
                            let _ = tx.send(());
                        }
                        break;
                    }
                    _ => {} // ping/pong/binary — ignore
                }
            }
            tick_event = tick_rx.recv() => {
                match tick_event {
                    Some(event) => {
                        let ended = event.combat_ended;

                        if let Some(w) = log.as_mut() {
                            let entry = shared::InputLogEntry {
                                tick: event.tick_index as u64,
                                seq: 0,
                                event_type: if event.combat_ended {
                                    "combat_end".into()
                                } else {
                                    "combat_tick".into()
                                },
                                player_id: None,
                                payload: serde_json::to_value(&event).unwrap_or_default(),
                                narrative_event: event.narrative.clone(),
                            };
                            w.append(&entry).await;
                            last_tick = event.tick_index as u64;
                        }

                        match serde_json::to_string(&event) {
                            Ok(json) => {
                                if socket.send(Message::Text(json)).await.is_err() {
                                    if let Some(tx) = cancel_tx.take() {
                                        let _ = tx.send(());
                                    }
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                        if ended {
                            break;
                        }
                    }
                    None => break, // Resolver finished — channel closed
                }
            }
        }
    }

    // Ensure the resolver task is stopped before we return.
    // If cancel_tx was already fired this is a no-op; if not, the task ends
    // naturally when tick_tx is dropped (which happens when the task returns).
    let _ = resolver_handle.await;
    let _ = socket.close().await;
}
