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

    ws.on_upgrade(move |socket| handle_ws(socket, session.params))
        .into_response()
}

async fn handle_ws(mut socket: WebSocket, params: CombatResolveRequest) {
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
