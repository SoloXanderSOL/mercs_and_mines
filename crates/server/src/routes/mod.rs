// HTTP route handlers for all API endpoints.
// Seed handling (Director ruling): the server is the authority on time and entropy.
//   seed_override Some(n) → passed through for deterministic replay.
//   seed_override None    → generated from OS entropy via rng::generate_seed().
// Timestamp is always generated server-side via chrono::Utc::now(); never accepted from client.

pub mod ws_combat;

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::Utc;
use serde::Deserialize;
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

use crate::auth::AuthSession;
use crate::api_types::{
    convoy_vehicle_from_class, CombatResolveRequest, MissionResolveRequest, PackAssaultRequest,
};
use crate::state::{AppState, CombatSession};

type AppError = (StatusCode, Json<Value>);

fn bad_request(msg: impl Into<String>) -> AppError {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()})))
}

// ── Auth request types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChallengeRequest {
    pub wallet_address: String,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    pub wallet_address: String,
    pub challenge: String,
    pub signature: String,
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

// ── Auth endpoints ─────────────────────────────────────────────────────────

async fn post_auth_challenge(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChallengeRequest>,
) -> impl IntoResponse {
    let nonce = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let challenge_text = crate::auth::build_challenge(&req.wallet_address, &nonce, now);

    state.pending_challenges.insert(
        req.wallet_address.clone(),
        crate::state::PendingChallenge {
            challenge_text: challenge_text.clone(),
            expires_at: now + 60,
        },
    );

    Json(json!({
        "challenge":  challenge_text,
        "expires_in": 60,
    }))
}

async fn post_auth_verify(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VerifyRequest>,
) -> Result<impl IntoResponse, AppError> {
    let now = Utc::now().timestamp();

    // Look up and consume the pending challenge (one-time use)
    let pending = state
        .pending_challenges
        .remove(&req.wallet_address)
        .ok_or_else(|| bad_request("no pending challenge for this wallet — call /api/auth/challenge first"))?;

    if now > pending.1.expires_at {
        return Err(bad_request("challenge expired — request a new one"));
    }
    if req.challenge != pending.1.challenge_text {
        return Err(bad_request("challenge mismatch"));
    }

    // Verify the Ed25519 signature
    crate::auth::verify_wallet_signature(&req.wallet_address, &req.challenge, &req.signature)
        .map_err(|e| bad_request(format!("signature invalid: {e}")))?;

    // Issue the 2-hour session
    let token = crate::auth::issue_session(&state, &req.wallet_address);

    // Upsert the player account (idempotent — subsequent logins preserve ledger and trust_standing).
    // Wallet address was verified by verify_wallet_signature above, so decode cannot fail here.
    let pubkey_bytes: [u8; 32] = bs58::decode(&req.wallet_address)
        .into_vec()
        .map_err(|e| bad_request(format!("wallet decode: {e}")))?
        .try_into()
        .map_err(|_| bad_request("wallet address must be 32 bytes"))?;
    state
        .account_repo
        .upsert_account(crate::repository::PlayerAccount {
            wallet: crate::repository::WalletAddress::from_bytes(pubkey_bytes),
            trust_standing: 0,
            profile: crate::repository::PlayerProfile { display_name: None, sector_id: None },
            gcn_ledger: vec![],
        })
        .await
        .map_err(|e| bad_request(format!("account upsert: {e}")))?;

    // Phase 0 stub: founding_courtesy CPI not yet wired (Solana client added in a future batch)
    eprintln!(
        "[auth] wallet {} authenticated — founding_courtesy dispatch pending (Solana client not yet wired)",
        req.wallet_address
    );

    Ok(Json(serde_json::to_value(&token).unwrap()))
}

async fn get_auth_session(
    AuthSession(token): AuthSession,
) -> impl IntoResponse {
    Json(serde_json::to_value(&token).unwrap())
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
            payload: serde_json::json!({
                "request":   req_payload,
                "timestamp": &timestamp,
            }),
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
            payload: serde_json::json!({
                "request":   req_payload,
                "timestamp": &timestamp,
            }),
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
            payload: serde_json::json!({
                "request":   req_payload,
                "timestamp": &timestamp,
            }),
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

// ── Streaming combat session (requires auth) ───────────────────────────────

async fn post_combat_stream_start(
    AuthSession(_session): AuthSession,
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

// ── After-Action Report (deterministic replay) ────────────────────────────

async fn get_combat_aar(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Validate as UUID to prevent path traversal — log files are named by UUID only.
    uuid::Uuid::parse_str(&raw_id)
        .map_err(|_| bad_request("session_id must be a valid UUID"))?;

    let session = crate::log_reader::load_combat_session(&state.log_dir, &raw_id)
        .await
        .map_err(|e| bad_request(format!("Could not load session log: {e}")))?;

    // Reconstruct resolver inputs from the stored log.
    // seed_override forces the exact stored seed — this is the determinism guarantee.
    let seed = session.config.seed as u32;
    let initiation = session
        .request
        .combat_initiation_type
        .unwrap_or(CombatInitiationType::Spotted);
    let max_ticks = session.request.max_ticks.unwrap_or(50) as u32;
    let defending: Vec<_> = session
        .request
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();

    let report = resolve_combat(
        &session.request.section,
        &session.request.vehicle,
        &session.timestamp,     // injected from log; never Utc::now()
        max_ticks,
        Some(seed),             // stored seed; guarantees deterministic output
        initiation,
        defending,
    );

    Ok(Json(json!({
        "session_id":    session.config.session_id,
        "seed":          session.config.seed,
        "build_version": session.config.build_version,
        "replayed_at":   Utc::now().to_rfc3339(),
        "report":        serde_json::to_value(&report).unwrap(),
    })))
}

// ── Router ─────────────────────────────────────────────────────────────────

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/missions",                  get(get_missions))
        .route("/api/units",                     get(get_units))
        .route("/api/equipment",                 get(get_equipment))
        .route("/api/auth/challenge",            post(post_auth_challenge))
        .route("/api/auth/verify",               post(post_auth_verify))
        .route("/api/auth/session",              get(get_auth_session))
        .route("/api/mission/resolve",           post(post_mission_resolve))
        .route("/api/combat/resolve",            post(post_combat_resolve))
        .route("/api/combat/pack-assault",       post(post_pack_assault))
        .route("/api/combat/stream/start",       post(post_combat_stream_start))
        .route("/api/combat/stream/:session_id", get(ws_combat::ws_stream_handler))
        .route("/api/combat/aar/:session_id",    get(get_combat_aar))
        .with_state(state)
}
