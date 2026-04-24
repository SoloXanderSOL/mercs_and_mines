// HTTP route handlers for all API endpoints.
// Seed handling (Director ruling): the server is the authority on time and entropy.
//   seed_override Some(n) → passed through for deterministic replay.
//   seed_override None    → generated from OS entropy via rng::generate_seed().
// Timestamp is always generated server-side via chrono::Utc::now(); never accepted from client.

pub mod ws_combat;

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sim_engine::{
    equipment::equipment,
    missions::missions,
    resolver::{resolve_combat, resolve_mission, resolve_pack_assault},
    rng::generate_seed,
    types::CombatInitiationType,
    units::unit_definitions,
};

use crate::api_types::{
    convoy_vehicle_from_class, CombatResolveRequest, MissionResolveRequest, PackAssaultRequest,
};
use crate::state::{AppState, CombatSession};

type AppError = (StatusCode, Json<Value>);

fn bad_request(msg: impl Into<String>) -> AppError {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()})))
}

// ── Static data endpoints ──────────────────────────────────────────────────

async fn get_missions() -> Json<Value> {
    Json(serde_json::to_value(missions()).unwrap())
}

async fn get_units() -> Json<Value> {
    Json(serde_json::to_value(unit_definitions()).unwrap())
}

async fn get_equipment() -> Json<Value> {
    Json(serde_json::to_value(equipment()).unwrap())
}

// ── Simulation endpoints ───────────────────────────────────────────────────

async fn post_mission_resolve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MissionResolveRequest>,
) -> Result<impl IntoResponse, AppError> {
    let req_payload = serde_json::to_value(&req).unwrap_or_default();

    let mission = missions()
        .get(req.mission_id.as_str())
        .ok_or_else(|| bad_request(format!("Unknown mission_id: {}", req.mission_id)))?;

    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let report = resolve_mission(&req.squad, mission, &timestamp, Some(seed));

    let session_id = Uuid::new_v4();
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: "mission_session".into(),
        campaign_id: "phase0".into(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    if let Ok(mut w) = crate::log_writer::SessionLogWriter::create(
        &state.log_dir, &session_id.to_string()
    ).await {
        w.write_header(&config).await;
        w.append(&shared::InputLogEntry {
            tick: 0, seq: 0,
            event_type: "session_start".into(),
            player_id: None,
            payload: req_payload,
            narrative_event: None,
        }).await;
        w.append(&shared::InputLogEntry {
            tick: 1, seq: 0,
            event_type: "combat_end".into(),
            player_id: None,
            payload: serde_json::to_value(&report).unwrap_or_default(),
            narrative_event: None,
        }).await;
    }

    Ok(Json(report))
}

async fn post_combat_resolve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CombatResolveRequest>,
) -> impl IntoResponse {
    // Serialize before combat_initiation_type is consumed by unwrap_or.
    let req_payload = serde_json::to_value(&req).unwrap_or_default();

    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let initiation = req.combat_initiation_type.unwrap_or(CombatInitiationType::Spotted);
    let max_ticks = req.max_ticks.unwrap_or(50) as u32;
    let defending = req
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();

    let report = resolve_combat(
        &req.section,
        &req.vehicle,
        &timestamp,
        max_ticks,
        Some(seed),
        initiation,
        defending,
    );

    let session_id = Uuid::new_v4();
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: "combat_session".into(),
        campaign_id: "phase0".into(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    if let Ok(mut w) = crate::log_writer::SessionLogWriter::create(
        &state.log_dir, &session_id.to_string()
    ).await {
        w.write_header(&config).await;
        w.append(&shared::InputLogEntry {
            tick: 0, seq: 0,
            event_type: "session_start".into(),
            player_id: None,
            payload: req_payload,
            narrative_event: None,
        }).await;
        w.append(&shared::InputLogEntry {
            tick: 1, seq: 0,
            event_type: "combat_end".into(),
            player_id: None,
            payload: serde_json::to_value(&report).unwrap_or_default(),
            narrative_event: None,
        }).await;
    }

    Json(report)
}

async fn post_pack_assault(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PackAssaultRequest>,
) -> impl IntoResponse {
    // Serialize before combat_initiation_type is moved into resolve_pack_assault.
    let req_payload = serde_json::to_value(&req).unwrap_or_default();

    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let max_ticks = req.max_ticks.unwrap_or(50) as u32;
    let defending = req
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();

    let report = resolve_pack_assault(
        &req.section,
        &req.pack,
        &timestamp,
        req.combat_initiation_type,
        defending,
        max_ticks,
        Some(seed),
    );

    let session_id = Uuid::new_v4();
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: "pack_assault_session".into(),
        campaign_id: "phase0".into(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    if let Ok(mut w) = crate::log_writer::SessionLogWriter::create(
        &state.log_dir, &session_id.to_string()
    ).await {
        w.write_header(&config).await;
        w.append(&shared::InputLogEntry {
            tick: 0, seq: 0,
            event_type: "session_start".into(),
            player_id: None,
            payload: req_payload,
            narrative_event: None,
        }).await;
        w.append(&shared::InputLogEntry {
            tick: 1, seq: 0,
            event_type: "combat_end".into(),
            player_id: None,
            payload: serde_json::to_value(&report).unwrap_or_default(),
            narrative_event: None,
        }).await;
    }

    Json(report)
}

// ── Streaming combat session ───────────────────────────────────────────────

async fn post_combat_stream_start(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CombatResolveRequest>,
) -> impl IntoResponse {
    let id = Uuid::new_v4();
    state.combat_sessions.insert(id, CombatSession {
        params: req,
        created_at: Instant::now(),
    });
    Json(json!({"session_id": id.to_string()}))
}

// ── Router ─────────────────────────────────────────────────────────────────

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/missions",                  get(get_missions))
        .route("/api/units",                     get(get_units))
        .route("/api/equipment",                 get(get_equipment))
        .route("/api/mission/resolve",           post(post_mission_resolve))
        .route("/api/combat/resolve",            post(post_combat_resolve))
        .route("/api/combat/pack-assault",       post(post_pack_assault))
        .route("/api/combat/stream/start",       post(post_combat_stream_start))
        .route("/api/combat/stream/:session_id", get(ws_combat::ws_stream_handler))
        .with_state(state)
}
