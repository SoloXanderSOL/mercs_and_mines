// HTTP route handlers for all API endpoints.
// Seed handling (Director ruling): the server is the authority on time and entropy.
//   seed_override Some(n) → passed through for deterministic replay.
//   seed_override None    → generated from OS entropy via rng::generate_seed().
// Timestamp is always generated server-side via chrono::Utc::now(); never accepted from client.

pub mod admin;
pub mod convoy_events;
pub mod convoys;
pub mod ws_combat;

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use tower_http::services::ServeDir;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sim_engine::{
    equipment::equipment,
    missions::missions,
    ore_run::run_ore_run,
    resolver::{apply_commander_buffs, resolve_combat, resolve_mission, resolve_pack_assault},
    rng::generate_seed,
    scenario::Approach,
    types::CombatInitiationType,
    units::unit_definitions,
};

use crate::auth::AuthSession;
use crate::api_types::{
    convoy_vehicle_from_class, CombatResolveRequest, MissionResolveRequest, PackAssaultRequest,
};
use crate::state::AppState;
use crate::repository::{CombatSession, InputLogRepository};

type AppError = (StatusCode, Json<Value>);

fn bad_request(msg: impl Into<String>) -> AppError {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()})))
}

fn not_found(msg: impl Into<String>) -> AppError {
    (StatusCode::NOT_FOUND, Json(json!({"error": msg.into()})))
}

fn internal_error(msg: impl Into<String>) -> AppError {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": msg.into()})))
}

/// Writes a session's header and its `session_start` entry to the input log.
/// `session_start` carries the full request and the wall-clock timestamp handed to the
/// resolver: for the one-shot routes it *is* the player's input, and it is what the AAR
/// replays. Fire-and-forget: a failure is logged and the caller's response is unchanged.
/// Without a header the entry could be neither hashed nor replayed, so a failed header
/// write skips the entry.
async fn log_session_start(
    repo: &(dyn InputLogRepository + Send + Sync),
    config: &shared::SessionConfig,
    session_id: Uuid,
    request: Value,
    timestamp: &str,
) {
    if let Err(e) = repo.save_session_config(config).await {
        tracing::warn!("failed to persist session config for {session_id}: {e}");
        return;
    }
    let entry = shared::InputLogEntry {
        tick: 0,
        seq: 0,
        event_type: "session_start".into(),
        player_id: None,
        payload: json!({ "request": request, "timestamp": timestamp }),
        narrative_event: None,
    };
    if let Err(e) = repo.append_entry(&session_id, &entry).await {
        tracing::warn!("failed to append session_start for {session_id}: {e}");
    }
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
    let expiry_secs = state.config.server.challenge_expiry_secs;

    state.pending_challenges.insert(
        req.wallet_address.clone(),
        crate::state::PendingChallenge {
            challenge_text: challenge_text.clone(),
            expires_at: now + expiry_secs,
        },
    );

    Json(json!({
        "challenge":  challenge_text,
        "expires_in": expiry_secs,
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
            gcn_balance: 0,
            profile: crate::repository::PlayerProfile { display_name: None, sector_id: None },
            gcn_ledger: vec![],
        })
        .await
        .map_err(|e| bad_request(format!("account upsert: {e}")))?;

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
    if req.squad.units.is_empty() {
        return Err(bad_request("A mission needs at least one unit"));
    }
    let req_payload = serde_json::to_value(&req).unwrap_or_default();

    let mission = missions()
        .get(req.mission_id.as_str())
        .ok_or_else(|| bad_request(format!("Unknown mission_id: {}", req.mission_id)))?;

    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let report = resolve_mission(&req.squad, mission, &timestamp, Some(seed), &state.config.sim);

    let session_id = Uuid::new_v4();
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: Uuid::nil(),
        campaign_id: Uuid::nil(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    log_session_start(state.input_log_repo.as_ref(), &config, session_id, req_payload, &timestamp).await;

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
    let max_ticks = req.max_ticks.unwrap_or(state.config.server.default_max_ticks) as u32;
    let defending = req
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();

    let mut section = req.section;
    if let Some(mut cmd) = req.commander {
        apply_commander_buffs(&mut cmd, &mut section);
    }

    let report = resolve_combat(
        &section,
        &req.vehicle,
        &timestamp,
        max_ticks,
        Some(seed),
        initiation,
        defending,
        &state.config.sim,
    );

    let session_id = Uuid::new_v4();
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: Uuid::nil(),
        campaign_id: Uuid::nil(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    log_session_start(state.input_log_repo.as_ref(), &config, session_id, req_payload, &timestamp).await;

    Json(serde_json::json!({
        "session_id": session_id.to_string(),
        "report":     serde_json::to_value(&report).unwrap(),
    }))
}

async fn post_pack_assault(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PackAssaultRequest>,
) -> impl IntoResponse {
    // Serialize before combat_initiation_type is moved into resolve_pack_assault.
    let req_payload = serde_json::to_value(&req).unwrap_or_default();

    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let max_ticks = req.max_ticks.unwrap_or(state.config.server.default_max_ticks) as u32;
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
        &state.config.sim,
    );

    let session_id = Uuid::new_v4();
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: Uuid::nil(),
        campaign_id: Uuid::nil(),
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    log_session_start(state.input_log_repo.as_ref(), &config, session_id, req_payload, &timestamp).await;

    Json(report)
}

// ── Streaming combat session (requires auth) ───────────────────────────────

async fn post_combat_stream_start(
    AuthSession(_session): AuthSession,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CombatResolveRequest>,
) -> impl IntoResponse {
    let id = Uuid::new_v4();
    let session = CombatSession {
        params: req,
        created_at: Utc::now(),
    };
    if let Err(e) = state.session_repo.save_session(id, session).await {
        eprintln!("[session] failed to save combat session {id}: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "failed to create session"}))).into_response();
    }
    Json(json!({"session_id": id.to_string()})).into_response()
}

// ── Session integrity anchor ───────────────────────────────────────────────

async fn get_session_integrity(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let session_id = uuid::Uuid::parse_str(&raw_id)
        .map_err(|_| bad_request("session_id must be a valid UUID"))?;

    let integrity = crate::integrity::compute_session_integrity(
        state.input_log_repo.as_ref(),
        session_id,
    )
    .await
    .map_err(|e| bad_request(format!("Could not compute integrity: {e}")))?;

    Ok(Json(json!({
        "session_id":    integrity.session_id,
        "seed":          integrity.seed,
        "build_version": integrity.build_version,
        "log_hash":      integrity.log_hash,
        "algorithm":     "SHA-256",
        "note":          "Tuple (session_id, seed, log_hash) is the on-chain anchor record. Replaying the session log against this seed must produce the same log_hash.",
    })))
}

// ── After-Action Report (deterministic replay) ────────────────────────────

async fn get_combat_aar(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let session_id = Uuid::parse_str(&raw_id)
        .map_err(|_| bad_request("session_id must be a valid UUID"))?;

    let repo = state.input_log_repo.as_ref();
    let config = repo
        .get_session_config(&session_id)
        .await
        .map_err(|e| internal_error(format!("input log read failed: {e}")))?
        .ok_or_else(|| not_found("no input log for this session"))?;
    let entries = repo
        .get_entries_by_session(&session_id)
        .await
        .map_err(|e| internal_error(format!("input log read failed: {e}")))?;
    let start = entries
        .iter()
        .find(|e| e.event_type == "session_start")
        .ok_or_else(|| not_found("no session_start entry for this session"))?;
    let request: CombatResolveRequest = serde_json::from_value(start.payload["request"].clone())
        .map_err(|e| bad_request(format!("session_start is not a combat request: {e}")))?;
    let timestamp = start.payload["timestamp"]
        .as_str()
        .ok_or_else(|| bad_request("session_start payload missing timestamp"))?
        .to_string();

    // Rebuild the resolver inputs exactly as post_combat_resolve built them.
    // The stored seed is forced: this is the determinism guarantee.
    let seed = config.seed as u32;
    let initiation = request.combat_initiation_type.unwrap_or(CombatInitiationType::Spotted);
    let max_ticks = request.max_ticks.unwrap_or(state.config.server.default_max_ticks) as u32;
    let defending: Vec<_> = request
        .defending_convoy_vehicles
        .unwrap_or_default()
        .iter()
        .map(convoy_vehicle_from_class)
        .collect();
    let mut section = request.section;
    if let Some(mut cmd) = request.commander {
        apply_commander_buffs(&mut cmd, &mut section);
    }

    let report = resolve_combat(
        &section,
        &request.vehicle,
        &timestamp,     // injected from the log; never Utc::now()
        max_ticks,
        Some(seed),
        initiation,
        defending,
        &state.config.sim,
    );

    Ok(Json(json!({
        "session_id":    config.session_id,
        "seed":          config.seed,
        "build_version": config.build_version,
        "replayed_at":   Utc::now().to_rfc3339(),
        "report":        serde_json::to_value(&report).unwrap(),
    })))
}

// ── Ore Run (hackathon demo) ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OreRunRequest {
    approach: Approach,
    seed_override: Option<u32>,
}

async fn post_ore_run_start(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<OreRunRequest>,
) -> impl IntoResponse {
    // TODO Phase 1: assign session_id and persist session config via save_session_config
    // before ore-run outcomes are attached to campaign resource events. The seed is
    // currently returned to the client but not anchored server-side — any run wired
    // into campaign state without this fix will be un-auditable.
    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let (result, ticks) = run_ore_run(req.approach, seed, &timestamp);
    Json(json!({
        "result": serde_json::to_value(&result).unwrap(),
        "ticks":  serde_json::to_value(&ticks).unwrap(),
        "seed":   seed,
    }))
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
        .route("/api/ore-run/start",             post(post_ore_run_start))
        .route("/api/session/:id/integrity",     get(get_session_integrity))
        .merge(admin::router())
        .merge(convoys::router())
        .merge(convoy_events::router())
        .with_state(state)
        .fallback_service(ServeDir::new("app").append_index_html_on_directories(true))
}
