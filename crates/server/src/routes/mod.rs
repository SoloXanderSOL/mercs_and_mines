// HTTP route handlers for all API endpoints.
// Seed handling (Director ruling): the server is the authority on time and entropy.
//   seed_override Some(n) → passed through for deterministic replay.
//   seed_override None    → generated from OS entropy via rng::generate_seed().
// Timestamp is always generated server-side via chrono::Utc::now(); never accepted from client.

pub mod admin;
mod campaign;
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
    game_types::{Squad, UnitStatus},
    missions::missions,
    ore_run::run_ore_run,
    resolver::{
        apply_commander_buffs, resolve_combat, resolve_mission, resolve_pack_assault,
    },
    rng::generate_seed,
    scenario::Approach,
    types::CombatInitiationType,
    units::unit_definitions,
};

use crate::auth::AuthSession;
use crate::api_types::{
    convoy_vehicle_from_class, CombatResolveRequest, MissionResolveBody, PackAssaultRequest,
};
use crate::commander_gen::mission_tier;
use crate::state::AppState;
use crate::repository::commander::log_rank_up;
use crate::repository::{CampaignLifecycle, CombatSession, DeploymentStart, InputLogRepository, NewDeployment, RepositoryError};
use shared::command::{command_cap, CommandCap};

type AppError = (StatusCode, Json<Value>);

fn bad_request(msg: impl Into<String>) -> AppError {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()})))
}

fn not_found(msg: impl Into<String>) -> AppError {
    (StatusCode::NOT_FOUND, Json(json!({"error": msg.into()})))
}

/// The only builder of a `500`. The client gets a fixed body and an `error_id`;
/// the journal line carries the full message, the same id and the call site.
#[track_caller]
fn internal_error(msg: impl Into<String>) -> AppError {
    let msg = msg.into();
    let error_id = Uuid::new_v4();
    tracing::error!(at = %std::panic::Location::caller(), %error_id, "{msg}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "internal error", "error_id": error_id})),
    )
}

/// A `422` with the route's JSON error body, which tells it apart from axum's plain-text
/// `422` for an undeserialisable body.
fn unprocessable(msg: impl Into<String>) -> AppError {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": msg.into()})))
}

fn conflict(msg: impl Into<String>) -> AppError {
    (StatusCode::CONFLICT, Json(json!({"error": msg.into()})))
}

fn forbidden(msg: impl Into<String>) -> AppError {
    (StatusCode::FORBIDDEN, Json(json!({"error": msg.into()})))
}

/// A `503` with the house JSON body. No journal line: a closed switch is configuration,
/// not a fault.
fn service_unavailable(msg: impl Into<String>) -> AppError {
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": msg.into()})))
}

fn unauthorized(msg: impl Into<String>) -> AppError {
    (StatusCode::UNAUTHORIZED, Json(json!({"error": msg.into()})))
}

/// A session's first input-log entry. `session_start` carries the full request and the
/// wall-clock timestamp handed to the resolver: for the one-shot routes it *is* the
/// player's input, and it is what the AAR replays. Every caller builds it here, so the
/// payload shape `{request, timestamp}` cannot drift between routes.
fn session_start_entry(request: Value, timestamp: &str) -> shared::InputLogEntry {
    shared::InputLogEntry {
        tick: 0,
        seq: 0,
        event_type: "session_start".into(),
        player_id: None,
        payload: json!({ "request": request, "timestamp": timestamp }),
        narrative_event: None,
    }
}

/// Writes a session's header and its `session_start` entry to the input log.
/// Policy by caller: combat / pack assault / WS: fire-and-forget — a failure is logged
/// and the caller's response is unchanged. The mission route does not call this; it uses
/// the fallible `InputLogRepository::start_session` before the roll (§4g item 9).
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
    let entry = session_start_entry(request, timestamp);
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

async fn get_missions() -> Result<Json<Value>, AppError> {
    let v = serde_json::to_value(missions())
        .map_err(|e| internal_error(format!("missions serialization failed: {e}")))?;
    Ok(Json(v))
}

async fn get_units() -> Result<Json<Value>, AppError> {
    let v = serde_json::to_value(unit_definitions())
        .map_err(|e| internal_error(format!("units serialization failed: {e}")))?;
    Ok(Json(v))
}

async fn get_equipment() -> Result<Json<Value>, AppError> {
    let v = serde_json::to_value(equipment())
        .map_err(|e| internal_error(format!("equipment serialization failed: {e}")))?;
    Ok(Json(v))
}

/// Public and stateless; writes nothing. `version` is `CARGO_PKG_VERSION`, the value
/// `session_configs.build_version` logs. `build_sha` is embedded at compile time from
/// `MERCS_BUILD_SHA` (set by CI on the release build) so the deploy script can prove
/// which build is serving; it is not recorded in the audit trail.
async fn get_version() -> Json<Value> {
    Json(json!({
        "version":   env!("CARGO_PKG_VERSION"),
        "build_sha": option_env!("MERCS_BUILD_SHA").unwrap_or("unknown"),
    }))
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

    // Upsert the player account (idempotent — subsequent logins preserve ledger and trust_standing).
    // It runs before the session is issued, so a failed upsert leaves no live token (GAP-52).
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
        .map_err(|e| internal_error(format!("account upsert: {e}")))?;

    // Issue the 2-hour session
    let token = crate::auth::issue_session(&state, &req.wallet_address);

    let v = serde_json::to_value(&token)
        .map_err(|e| internal_error(format!("session token serialization failed: {e}")))?;
    Ok(Json(v))
}

async fn get_auth_session(
    AuthSession(token): AuthSession,
) -> Result<impl IntoResponse, AppError> {
    let v = serde_json::to_value(&token)
        .map_err(|e| internal_error(format!("session token serialization failed: {e}")))?;
    Ok(Json(v))
}

// ── Simulation endpoints ───────────────────────────────────────────────────

/// Marks a deployment `aborted` after a failure. A failed abort is logged, not returned:
/// the next `start_deployment` for this Commander aborts the row once it is stale.
async fn abort_deployment_or_log(state: &AppState, deployment_id: Uuid) {
    if let Err(e) = state.commander_repo.abort_deployment(deployment_id).await {
        tracing::error!(%deployment_id, "failed to abort deployment ({e}); the next start's stale rule heals it");
    }
}

/// The `409` for a Commander who may not deploy, KIA checked first.
fn deploy_refusal(rec: &crate::repository::CommanderRecord) -> Option<AppError> {
    if rec.is_kia {
        return Some(conflict("Commander is KIA"));
    }
    if rec.is_shattered {
        return Some(conflict("Commander is Shattered and cannot deploy until rehabilitated"));
    }
    None
}

/// Mission ↔ Commander wiring (brick 4g-1, Phase_1_Section_4 §4g; Sections since 7b-2).
/// Check order: 401 (extractor) → 400 (body; then `section_ids` empty or duplicate) →
/// 404 (Commander unknown or not yours) → 409 (the Commander's campaign is not Active,
/// S3, 5a-3b) → 422 (more Sections than the rank's command cap,
/// GAP-50) → 404 (a Section unknown, not yours or not in the Commander's campaign; one
/// body) → 409 (KIA, Shattered) → active deployment, 409 (Commander or a Section already
/// deployed) → re-read, 409 (KIA, Shattered; deployment aborted; 4g-2) → input log before
/// the roll (500 on failure, deployment aborted) → resolve → one atomic write of stress,
/// XP, rank, (on a Wipeout) KIA and the death card, and the close, computed from the
/// locked Commander row. The close releases the Sections (trigger, `0027`).
async fn post_mission_resolve(
    AuthSession(session): AuthSession,
    State(state): State<Arc<AppState>>,
    Json(body): Json<MissionResolveBody>,
) -> Result<impl IntoResponse, AppError> {
    let MissionResolveBody { request: req, section_ids } = body;
    // 400 — body shape. Refused fields are never silently ignored.
    if req.squad.commander.is_some() {
        return Err(bad_request("A mission cannot carry a client Commander; send commander_id"));
    }
    if req.seed_override.is_some() {
        return Err(bad_request("seed_override is not accepted on this route"));
    }
    if req.squad.units.is_empty() {
        return Err(bad_request("A mission needs at least one unit"));
    }
    let mission = missions()
        .get(req.mission_id.as_str())
        .ok_or_else(|| bad_request(format!("Unknown mission_id: {}", req.mission_id)))?;
    if section_ids.is_empty() {
        return Err(bad_request("A mission needs at least one Section; send section_ids"));
    }
    let distinct: std::collections::HashSet<Uuid> = section_ids.iter().copied().collect();
    if distinct.len() != section_ids.len() {
        return Err(bad_request("section_ids must not repeat a Section"));
    }

    // 404 — an unknown Commander and someone else's get the same body, so the route
    // never confirms which UUIDs exist.
    let wallet: Vec<u8> = bs58::decode(&session.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;
    let rec = state
        .commander_repo
        .get_commander(req.commander_id)
        .await
        .map_err(|e| internal_error(format!("commander read failed: {e}")))?
        .filter(|r| r.player_wallet == wallet)
        .ok_or_else(|| not_found("Commander not found"))?;

    // 409 — S3: missions need an Active campaign. An unlocked read, so a campaign that
    // leaves Active mid-request resolves one last mission (accepted for Phase 1, the
    // lifecycle task's stale-read class). It sits before start_deployment, so a refused
    // mission opens no deployment row.
    let campaign = state
        .campaign_repo
        .get_campaign(rec.campaign_id)
        .await
        .map_err(|e| internal_error(format!("campaign read failed: {e}")))?
        .ok_or_else(|| internal_error(format!("commander {} has no campaign {}", rec.commander_id, rec.campaign_id)))?;
    if campaign.state != CampaignLifecycle::Active {
        return Err(conflict("campaign is not active"));
    }

    // 422 — the rank's command cap (GAP-50). A pure check before any Section I/O, so it
    // also bounds the read below. Rank only rises, so this first read is never too lenient.
    let cap = command_cap(rec.rank).ok_or_else(|| {
        internal_error(format!("commander {} has rank {} outside 1..=5", rec.commander_id, rec.rank))
    })?;
    if let CommandCap::Sections(max) = cap {
        if section_ids.len() > max as usize {
            return Err(unprocessable(format!(
                "A Rank {} Commander can take at most {max} Sections on a mission", rec.rank
            )));
        }
    }

    // 404 — every Section must be the caller's and in the Commander's campaign. One body
    // for every cause, so the route never confirms which Section UUIDs exist. The database
    // holds the same rule through deployment_sections' composite FKs (GAP-66).
    let owned = state
        .section_repo
        .owned_section_ids(&section_ids, &wallet, rec.campaign_id)
        .await
        .map_err(|e| internal_error(format!("section read failed: {e}")))?;
    if owned.len() != section_ids.len() {
        return Err(not_found("Section not found"));
    }

    // 409 — the flag gates deployment (§4g item 7). A fast path: the re-read under the
    // deployment lock below is the check that holds.
    if let Some(refusal) = deploy_refusal(&rec) {
        return Err(refusal);
    }

    // Everything the log needs is fixed before any write. The seed is the server's only.
    let seed = generate_seed();
    let now = Utc::now();
    let timestamp = now.to_rfc3339();
    let session_id = Uuid::new_v4();
    let deployment_id = Uuid::new_v4();
    // The payload is the request body only — auth lives in the extractor, never in `req`.
    let req_payload = serde_json::to_value(&req)
        .map_err(|e| internal_error(format!("request serialization failed: {e}")))?;

    let started = state
        .commander_repo
        .start_deployment(&NewDeployment {
            deployment_id,
            commander_id: rec.commander_id,
            session_id,
            mission_id: mission.id.clone(),
            started_at: now,
            player_wallet: wallet.clone(),
            campaign_id: rec.campaign_id,
            section_ids: section_ids.clone(),
        })
        .await
        .map_err(|e| internal_error(format!("deployment write failed: {e}")))?;
    match started {
        DeploymentStart::Started => {}
        DeploymentStart::CommanderBusy => return Err(conflict("Commander is already deployed")),
        DeploymentStart::SectionBusy => return Err(conflict("A Section is already deployed")),
    }

    // Re-read under the deployment lock (4g-2): another mission may have closed and
    // Shattered this Commander, or a non-mission writer killed them, since the first read.
    // A refusal here aborts before anything is logged or rolled.
    let rec = match state.commander_repo.get_commander(req.commander_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            abort_deployment_or_log(&state, deployment_id).await;
            return Err(internal_error("commander re-read found no record"));
        }
        Err(e) => {
            abort_deployment_or_log(&state, deployment_id).await;
            return Err(internal_error(format!("commander re-read failed: {e}")));
        }
    };
    if let Some(refusal) = deploy_refusal(&rec) {
        abort_deployment_or_log(&state, deployment_id).await;
        return Err(refusal);
    }

    // The input is logged before the roll: the log never holds fewer inputs than the
    // database holds effects (§4g item 6.2).
    let config = shared::SessionConfig {
        session_id: session_id.to_string(),
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        seed: seed as u64,
        sector_id: Uuid::nil(),
        campaign_id: rec.campaign_id,
        sector_tier: "Contested".into(),
        ruleset: "standard_v1".into(),
    };
    let mut entry = session_start_entry(req_payload, &timestamp);
    // The Sections are a player input, so the log holds them; additive beside `request`,
    // in request order (7b-2 R1, R5).
    entry.payload["selection"] = json!({ "section_ids": section_ids });
    if let Err(e) = state.input_log_repo.start_session(&config, &entry).await {
        abort_deployment_or_log(&state, deployment_id).await;
        return Err(internal_error(format!("input log write failed: {e}")));
    }

    // A record Commander adds 0 to the roll (§4g item 3): the sim sees no Commander.
    let squad = Squad { units: req.squad.units, commander: None };
    let cfg = &state.config.sim;
    let mut report = resolve_mission(&squad, mission, &timestamp, Some(seed), cfg);

    // The repository computes the effects from the Commander row it locks (4g-2); the
    // route supplies only what the mission determined.
    let kia = report
        .unit_results
        .iter()
        .filter(|u| matches!(u.final_status, UnitStatus::Kia))
        .count() as u32;
    let tier = mission_tier(mission.difficulty);

    // Presentation only: the report names the record's Commander. Serialised before the
    // resolve, so nothing fallible follows the commit that may kill the Commander
    // (Reviewer 4e-1 F2).
    report.commander_name = Some(rec.name.clone());
    let report_value = match serde_json::to_value(&report) {
        Ok(v) => v,
        Err(e) => {
            abort_deployment_or_log(&state, deployment_id).await;
            return Err(internal_error(format!("report serialization failed: {e}")));
        }
    };

    let eff = match state
        .commander_repo
        // §2c supplies the mission's ticker contribution; 0 until then (§4e R3).
        .resolve_deployment(deployment_id, rec.commander_id, kia, tier, 0.0, &report.outcome, cfg)
        .await
    {
        Ok(eff) => eff,
        Err(e) => {
            // The input is already logged with no effects, which §4g item 6 permits.
            abort_deployment_or_log(&state, deployment_id).await;
            return Err(internal_error(format!("mission result write failed: {e}")));
        }
    };
    log_rank_up(rec.commander_id, eff.old_rank, eff.new_rank);

    // The death is a Commander effect, not sim output, so it sits beside `report` (§4e R4).
    Ok(Json(json!({
        "session_id": session_id.to_string(),
        "report":     report_value,
        "commander":  {"kia": eff.is_kia, "glorious": eff.glorious},
    })))
}

async fn post_combat_resolve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CombatResolveRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Serialize before combat_initiation_type is consumed by unwrap_or. A failure refuses
    // the request before anything is logged, so the log never holds a null request.
    let req_payload = serde_json::to_value(&req)
        .map_err(|e| internal_error(format!("request serialization failed: {e}")))?;

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

    let report_value = serde_json::to_value(&report)
        .map_err(|e| internal_error(format!("report serialization failed: {e}")))?;
    Ok(Json(serde_json::json!({
        "session_id": session_id.to_string(),
        "report":     report_value,
    })))
}

async fn post_pack_assault(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PackAssaultRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Serialize before combat_initiation_type is moved into resolve_pack_assault. A failure
    // refuses the request before anything is logged.
    let req_payload = serde_json::to_value(&req)
        .map_err(|e| internal_error(format!("request serialization failed: {e}")))?;

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

    Ok(Json(report))
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
        return internal_error(format!("[session] failed to save combat session {id}: {e}")).into_response();
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
    .map_err(|e| match e {
        RepositoryError::NotFound    => not_found("no input log for this session"),
        RepositoryError::Internal(m) => internal_error(format!("Could not compute integrity: {m}")),
    })?;

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
        .map_err(|e| {
            tracing::warn!(%session_id, "AAR: session_start is not a combat request: {e}");
            bad_request("session_start is not a combat request")
        })?;
    let timestamp = start.payload["timestamp"]
        .as_str()
        .ok_or_else(|| internal_error("session_start payload missing timestamp"))?
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

    let report_value = serde_json::to_value(&report)
        .map_err(|e| internal_error(format!("report serialization failed: {e}")))?;
    Ok(Json(json!({
        "session_id":    config.session_id,
        "seed":          config.seed,
        "build_version": config.build_version,
        "replayed_at":   Utc::now().to_rfc3339(),
        "report":        report_value,
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
) -> Result<impl IntoResponse, AppError> {
    // TODO Phase 1: assign session_id and persist session config via save_session_config
    // before ore-run outcomes are attached to campaign resource events. The seed is
    // currently returned to the client but not anchored server-side — any run wired
    // into campaign state without this fix will be un-auditable.
    let seed = req.seed_override.unwrap_or_else(generate_seed);
    let timestamp = Utc::now().to_rfc3339();
    let (result, ticks) = run_ore_run(req.approach, seed, &timestamp);
    let result_value = serde_json::to_value(&result)
        .map_err(|e| internal_error(format!("ore-run result serialization failed: {e}")))?;
    let ticks_value = serde_json::to_value(&ticks)
        .map_err(|e| internal_error(format!("ore-run ticks serialization failed: {e}")))?;
    Ok(Json(json!({
        "result": result_value,
        "ticks":  ticks_value,
        "seed":   seed,
    })))
}

// ── Router ─────────────────────────────────────────────────────────────────

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/missions",                  get(get_missions))
        .route("/api/units",                     get(get_units))
        .route("/api/version",                   get(get_version))
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
        .merge(campaign::router())
        .merge(convoys::router())
        .merge(convoy_events::router())
        .with_state(state)
        .fallback_service(ServeDir::new("app").append_index_html_on_directories(true))
}

#[cfg(test)]
mod tests {
    use super::internal_error;
    use axum::{http::StatusCode, Json};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    /// A capture buffer for the journal line. `Arc<Mutex<Vec<u8>>>` is not itself a
    /// `MakeWriter`, so the test hands the subscriber a closure returning this wrapper.
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// GAP-76: the client sees a fixed body and an id; the journal keeps the message, the
    /// same id and the call site.
    #[test]
    fn internal_error_journals_the_message_and_returns_a_generic_body() {
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt().with_writer(move || w.clone()).with_ansi(false).finish();
        let (status, Json(body)) =
            tracing::subscriber::with_default(sub, || internal_error("secret-text-7f3a"));
        let journal = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let id: Uuid = body["error_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("error_id is not a UUID: {body}"));
        assert_eq!(body, json!({"error": "internal error", "error_id": id}));
        assert!(!body.to_string().contains("secret-text-7f3a"), "the message leaked: {body}");
        assert!(journal.contains("secret-text-7f3a"), "the journal lost the message: {journal}");
        assert!(journal.contains(&format!("error_id={id}")), "the journal lacks the id: {journal}");
        assert!(journal.contains("routes/mod.rs"), "the journal lacks the call site: {journal}");
    }
}
