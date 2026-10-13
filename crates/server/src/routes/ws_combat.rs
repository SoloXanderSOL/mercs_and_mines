use std::sync::Arc;

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
    config::SimConfig,
    constants::TICK_CHANNEL_BUFFER,
    resolver::{apply_commander_buffs, resolve_combat_streaming},
    rng::generate_seed,
    types::CombatInitiationType,
};

use crate::{
    api_types::{convoy_vehicle_from_class, CombatResolveRequest},
    repository::InputLogRepository,
    state::AppState,
};

pub async fn ws_stream_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
    let session = match state.session_repo.consume_session(session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            eprintln!("[session] consume_session error for {session_id}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let stale_secs = state.config.server.combat_session_stale_secs;
    if Utc::now().signed_duration_since(session.created_at)
        > chrono::Duration::seconds(stale_secs as i64)
    {
        return StatusCode::GONE.into_response();
    }

    let sim_cfg = state.config.sim.clone();
    let default_max_ticks = state.config.server.default_max_ticks;
    let input_log_repo = Arc::clone(&state.input_log_repo);
    ws.on_upgrade(move |socket| {
        handle_ws(socket, session.params, session_id, sim_cfg, default_max_ticks, input_log_repo)
    })
    .into_response()
}

async fn handle_ws(
    mut socket: WebSocket,
    params: CombatResolveRequest,
    session_id: Uuid,
    sim_cfg: SimConfig,
    default_max_ticks: usize,
    input_log_repo: Arc<dyn InputLogRepository + Send + Sync>,
) {
    // Serialize before any field is consumed by unwrap_or.
    let params_payload = serde_json::to_value(&params).unwrap_or_default();

    let seed = params.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let initiation = params.combat_initiation_type.unwrap_or(CombatInitiationType::Spotted);
    let max_ticks = params.max_ticks.unwrap_or(default_max_ticks) as u32;
    let defending: Vec<_> = params
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();

    let mut section = params.section;
    let vehicle     = params.vehicle;
    if let Some(mut cmd) = params.commander {
        apply_commander_buffs(&mut cmd, &mut section);
    }

    let session_config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: Uuid::nil(),
        campaign_id: Uuid::nil(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };

    super::log_session_start(input_log_repo.as_ref(), &session_config, session_id, params_payload, &timestamp).await;

    let (tick_tx, mut tick_rx) = mpsc::channel::<CombatTickEvent>(TICK_CHANNEL_BUFFER);
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Spawn resolver — section/vehicle/timestamp/sim_cfg are moved into the async block
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
            &sim_cfg,
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
                                // The logged tick is the last tick forwarded to the client, not necessarily the
                                // tick at which the resolver honoured the cancel (GAP-42).
                                let entry = shared::InputLogEntry {
                                    tick: last_tick,
                                    seq: 1,
                                    event_type: "player_input".into(),
                                    player_id: None,
                                    payload: serde_json::json!({"action": "retreat"}),
                                    narrative_event: None,
                                };
                                if let Err(e) = input_log_repo.append_entry(&session_id, &entry).await {
                                    tracing::warn!("failed to append retreat for {session_id}: {e}");
                                }
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

                        last_tick = event.tick_index as u64;

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
    let _ = resolver_handle.await;
    let _ = socket.close().await;
}
