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

/// Largest client frame and message the combat socket accepts (4 KiB). A larger
/// frame ends the session through the receive loop's error arm.
const COMBAT_WS_MAX_MESSAGE: usize = 4096;

pub async fn ws_stream_handler(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
    let session = match state.session_repo.consume_session(session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            return super::internal_error(format!("[session] consume_session error for {session_id}: {e}")).into_response();
        }
    };

    let stale_secs = state.config.server.combat_session_stale_secs;
    if Utc::now().signed_duration_since(session.created_at)
        > chrono::Duration::seconds(stale_secs as i64)
    {
        return StatusCode::GONE.into_response();
    }

    // Serialize the logged request before the upgrade: a failure refuses the request with
    // a 500 instead of logging a null payload, and nothing is logged.
    let params_payload = match serde_json::to_value(&session.params) {
        Ok(v) => v,
        Err(e) => {
            return super::internal_error(format!("request serialization failed for {session_id}: {e}")).into_response();
        }
    };

    let sim_cfg = state.config.sim.clone();
    let default_max_ticks = state.config.server.default_max_ticks;
    let input_log_repo = Arc::clone(&state.input_log_repo);
    ws.max_message_size(COMBAT_WS_MAX_MESSAGE)
        .max_frame_size(COMBAT_WS_MAX_MESSAGE)
        .on_upgrade(move |socket| {
            handle_ws(socket, session.params, params_payload, session_id, sim_cfg, default_max_ticks, input_log_repo)
        })
        .into_response()
}

async fn handle_ws(
    mut socket: WebSocket,
    params: CombatResolveRequest,
    params_payload: serde_json::Value,
    session_id: Uuid,
    sim_cfg: SimConfig,
    default_max_ticks: usize,
    input_log_repo: Arc<dyn InputLogRepository + Send + Sync>,
) {
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
                            Err(e) => {
                                tracing::warn!("combat tick serialization failed for {session_id} at tick {last_tick}: {e}");
                                if let Some(tx) = cancel_tx.take() {
                                    let _ = tx.send(());
                                }
                                break;
                            }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use sim_engine::types::{ArmorTag, CombatWeapon, Section, Vehicle, WeaponTag};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::*;
    use crate::config::Config;
    use crate::repository::CombatSession;

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// A live combat socket: a saved session, ticks every second, up to 1000 ticks.
    async fn combat_client() -> Client {
        let mut config = Config::default();
        config.sim.tick_stream_delay_ms = 1000;
        let state = Arc::new(AppState::new(Arc::new(config)));
        let section = Section {
            id: "test_section".into(),
            name: "Test Rifles".into(),
            max_strength: 4,
            current_strength: 4,
            individual_hp: 10,
            accuracy: 60,
            evasion: 10,
            weapon: CombatWeapon {
                name: "Rifle".into(),
                ap: 2,
                base_damage: 5,
                tag: WeaponTag::Slug,
                accuracy: 0,
            },
            armor_at: 0,
            armor_tag: ArmorTag::Unarmored,
        };
        let vehicle = Vehicle {
            id: "test_vehicle".into(),
            name: "Test Truck".into(),
            hp: 50,
            max_hp: 50,
            at: 2,
            armor_tag: ArmorTag::LightArmor,
            evasion: 5,
            weapons: vec![],
        };
        let params = CombatResolveRequest {
            section,
            vehicle,
            max_ticks: Some(1000),
            seed_override: Some(42),
            combat_initiation_type: None,
            defending_convoy_vehicles: None,
            commander: None,
        };
        let id = Uuid::new_v4();
        state
            .session_repo
            .save_session(id, CombatSession { params, created_at: Utc::now() })
            .await
            .expect("save_session");
        let addr = super::super::serve_on_loopback(super::super::router(state)).await;
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/combat/stream/{id}"))
            .await
            .expect("connect");
        ws
    }

    /// The next client message, within `secs`.
    async fn next(ws: &mut Client, secs: u64) -> Option<Result<WsMessage, tokio_tungstenite::tungstenite::Error>> {
        tokio::time::timeout(Duration::from_secs(secs), ws.next()).await.expect("timed out")
    }

    /// Reads tick 1, checks the combat goes on past it, and returns its index.
    async fn read_first_tick(ws: &mut Client) -> u32 {
        let msg = next(ws, 5).await.expect("stream ended").expect("read error");
        let WsMessage::Text(text) = msg else { panic!("expected tick 1, got {msg:?}") };
        let tick: CombatTickEvent = serde_json::from_str(&text).expect("tick json");
        assert!(!tick.combat_ended, "precondition: the fixture's combat must outlast tick 1");
        tick.tick_index
    }

    #[tokio::test]
    async fn combat_ws_over_cap_frame_ends_the_session() {
        let mut ws = combat_client().await;
        read_first_tick(&mut ws).await;
        ws.send(WsMessage::Text("x".repeat(4097))).await.expect("send");
        let msg = next(&mut ws, 5).await.expect("stream ended").expect("read error");
        assert!(matches!(msg, WsMessage::Close(None)), "expected Close(None), got {msg:?}");
        assert!(matches!(next(&mut ws, 5).await, None | Some(Err(_))), "the socket must end after its Close");
    }

    #[tokio::test]
    async fn combat_ws_reads_a_frame_at_the_cap() {
        let mut ws = combat_client().await;
        let first = read_first_tick(&mut ws).await;
        ws.send(WsMessage::Text("x".repeat(4096))).await.expect("send");
        let msg = next(&mut ws, 5).await.expect("stream ended").expect("read error");
        let WsMessage::Text(text) = msg else { panic!("expected tick 2, got {msg:?}") };
        let tick: CombatTickEvent = serde_json::from_str(&text).expect("tick json");
        assert_eq!(tick.tick_index, first + 1, "the session goes on after a frame at the cap");
    }
}
