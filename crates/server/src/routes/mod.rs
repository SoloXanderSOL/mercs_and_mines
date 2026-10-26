// HTTP route handlers for all API endpoints.
// Seed handling (Director ruling): the server is the authority on time and entropy.
//   seed_override Some(n) → passed through for deterministic replay.
//   seed_override None    → generated from OS entropy via rng::generate_seed().
// Timestamp is always generated server-side via chrono::Utc::now(); never accepted from client.

pub mod admin;
mod campaign;
pub mod convoy_events;
pub mod convoys;
mod sector;
pub mod ws_combat;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{connect_info::MockConnectInfo, rejection::JsonRejection, ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
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
use crate::rate_limit::{client_key, ClientKey, RateLimits, RetryAfter};
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

/// A `503` with the house JSON body. The helper journals nothing; a caller that refuses under load journals through its own `RateWarn`.
fn service_unavailable(msg: impl Into<String>) -> AppError {
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": msg.into()})))
}

/// A throttled request (INF-6c-1): the fixed JSON body plus `Retry-After` in whole seconds.
/// No journal line: a limit is policy, not a fault.
fn too_many_requests(r: RetryAfter) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, r.0.to_string())],
        Json(json!({"error": "rate limited"})),
    )
        .into_response()
}

/// The request's rate-limit key (`rate_limit::client_key`). `None` means the router was
/// served without `into_make_service_with_connect_info`, which production never does, so it
/// is a `500` through `internal_error`. A `Shared` key is journalled on its first request, then at most hourly with a count.
fn client_ip(
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    limits: &RateLimits,
) -> Result<ClientKey, AppError> {
    let Some(ConnectInfo(peer)) = ci else {
        return Err(internal_error("no ConnectInfo: router served without into_make_service_with_connect_info"));
    };
    let key = client_key(peer, headers);
    if key == ClientKey::Shared {
        if let Some(n) = limits.shared_warn.hit(Instant::now()) {
            tracing::warn!(
                requests = n,
                "client IP unknown: a loopback peer without exactly one X-Real-IP; using the shared \
                 rate-limit bucket. Check nginx's `proxy_set_header X-Real-IP $remote_addr`"
            );
        }
    }
    Ok(key)
}

/// The peer the test helpers present (TEST-NET-1, non-loopback, so keyed by the peer).
#[doc(hidden)]
pub const TEST_PEER: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1)), 1);

/// Gives a test router a peer address for `ConnectInfo`. A real `ConnectInfo` extension
/// takes precedence over `MockConnectInfo`, so a misuse in production is a no-op. `pub`
/// (not `cfg(test)`) because integration tests use it.
#[doc(hidden)]
pub fn with_test_peer(router: Router, peer: SocketAddr) -> Router {
    router.layer(MockConnectInfo(peer))
}

/// Serves `app` on an ephemeral loopback port (with connect info, as production does)
/// and returns its address, for tests that need a live WebSocket.
#[cfg(test)]
pub(crate) async fn serve_on_loopback(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    addr
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

/// Body cap for the two sign-in routes, layered per route. The overflow reaches the handler as a JsonRejection after the rate-limit check, so it is counted.
pub const AUTH_BODY_LIMIT: usize = 4096;

/// Rate-limited per client IP, in one bucket shared with `verify` (INF-6c-1). A malformed
/// body is counted, then answered with axum's own rejection, unchanged.
async fn post_auth_challenge(
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    body: Result<Json<ChallengeRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now_i = Instant::now();
    if let Err(r) = state.rate_limits.sign_in_ip.check(ip, now_i) {
        return Ok(too_many_requests(r));
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return Ok(rej.into_response()),
    };

    if crate::auth::decode_wallet(&req.wallet_address).is_none() {
        return Err(bad_request("invalid wallet address"));
    }

    let nonce = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let challenge_text = crate::auth::build_challenge(&req.wallet_address, &nonce, now);
    let expiry_secs = state.config.server.challenge_expiry_secs;

    state.challenge_cap.insert(
        &state.pending_challenges,
        crate::auth::challenge_key(&challenge_text),
        crate::state::PendingChallenge {
            wallet_address: req.wallet_address.clone(),
            challenge_text: challenge_text.clone(),
            expires_at: now + expiry_secs,
        },
        now,
        now_i,
    );

    Ok(Json(json!({
        "challenge":  challenge_text,
        "expires_in": expiry_secs,
    }))
    .into_response())
}

/// Rate-limited per client IP, in the bucket shared with `challenge` (INF-6c-1). No
/// per-wallet limit: the body's wallet is unverified here (GAP-77).
async fn post_auth_verify(
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    body: Result<Json<VerifyRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now_i = Instant::now();
    if let Err(r) = state.rate_limits.sign_in_ip.check(ip, now_i) {
        return Ok(too_many_requests(r));
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return Ok(rej.into_response()),
    };

    let now = Utc::now().timestamp();

    // Look up and consume the pending challenge (one-time use)
    let pending = state
        .pending_challenges
        .remove(&crate::auth::challenge_key(&req.challenge))
        .ok_or_else(|| bad_request("no pending challenge — call /api/auth/challenge first"))?;

    if now > pending.1.expires_at {
        return Err(bad_request("challenge expired — request a new one"));
    }
    if req.challenge != pending.1.challenge_text {
        return Err(bad_request("challenge mismatch"));
    }
    // The key is no longer the wallet, so the wallet is checked here: otherwise key B could
    // redeem a challenge issued to wallet A.
    if pending.1.wallet_address != req.wallet_address {
        return Err(bad_request("challenge mismatch"));
    }

    // Verify the Ed25519 signature
    crate::auth::verify_wallet_signature(&req.wallet_address, &req.challenge, &req.signature)
        .map_err(|e| bad_request(format!("signature invalid: {e}")))?;

    // Admit the session: the per-address quota evicts that address's oldest; the backstop
    // refuses only when nothing would be evicted (ODQ-48). The challenge is already
    // consumed, so a refused client asks for a new one. No `Retry-After`: a slot frees only
    // when a session expires.
    let token = crate::auth::admit_session(&state, &req.wallet_address, ip)
        .map_err(|_| service_unavailable("sign-in is at capacity, try again later"))?;

    let v = serde_json::to_value(&token)
        .map_err(|e| internal_error(format!("session token serialization failed: {e}")))?;
    Ok(Json(v).into_response())
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
        .route("/api/auth/challenge",            post(post_auth_challenge).layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT)))
        .route("/api/auth/verify",               post(post_auth_verify).layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT)))
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
        .merge(sector::router())
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
        // keep line!() on the same line as the internal_error call: T1 pins #[track_caller]
        let call = || (internal_error("secret-text-7f3a"), line!());
        let ((status, Json(body)), line) = tracing::subscriber::with_default(sub, call);
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
        assert!(journal.contains(&format!("routes/mod.rs:{line}:")), "journal names the call site: {journal}");
    }

    // ── INF-6c-1: sign-in rate limit, client IP ───────────────────────────────

    use super::{router, with_test_peer, TEST_PEER};
    use crate::config::Config;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{header, HeaderMap, Request};
    use axum::Router;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn fresh_state() -> Arc<AppState> {
        Arc::new(AppState::new(Arc::new(Config::default())))
    }

    /// One POST through `app`. `json_body: false` sends no `content-type`.
    async fn post(
        app: Router,
        uri: &str,
        body: serde_json::Value,
        json_body: bool,
        real_ip: Option<&str>,
    ) -> (StatusCode, HeaderMap, serde_json::Value) {
        let mut req = Request::builder().method("POST").uri(uri);
        if json_body {
            req = req.header("content-type", "application/json");
        }
        if let Some(ip) = real_ip {
            req = req.header("x-real-ip", ip);
        }
        let resp = app.oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    /// A valid wallet address: bs58 of 32 bytes (in memory only).
    fn test_wallet() -> String {
        bs58::encode([9u8; 32]).into_string()
    }

    async fn challenge(state: &Arc<AppState>, peer: SocketAddr, real_ip: Option<&str>) -> StatusCode {
        let app = with_test_peer(router(state.clone()), peer);
        post(app, "/api/auth/challenge", json!({"wallet_address": test_wallet()}), true, real_ip).await.0
    }

    /// `verify` for a wallet with no pending challenge: a `400`, still counted.
    async fn verify(state: &Arc<AppState>) -> StatusCode {
        let app = with_test_peer(router(state.clone()), TEST_PEER);
        let body = json!({"wallet_address": "nobody", "challenge": "c", "signature": "s"});
        post(app, "/api/auth/verify", body, true, None).await.0
    }

    #[tokio::test]
    async fn sign_in_shares_one_ip_bucket() {
        let state = fresh_state();
        let mut statuses = Vec::new();
        for _ in 0..200 {
            statuses.push(challenge(&state, TEST_PEER, None).await);
        }
        for _ in 0..200 {
            statuses.push(verify(&state).await);
        }
        statuses.push(challenge(&state, TEST_PEER, None).await);
        let mut expected = vec![StatusCode::OK; 200];
        expected.extend([StatusCode::BAD_REQUEST; 200]);
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    #[tokio::test]
    async fn sign_in_counts_a_malformed_body() {
        let state = fresh_state();
        let mut statuses = Vec::new();
        for _ in 0..400 {
            let app = with_test_peer(router(state.clone()), TEST_PEER);
            statuses.push(post(app, "/api/auth/challenge", json!({"wallet_address": "w"}), false, None).await.0);
        }
        statuses.push(challenge(&state, TEST_PEER, None).await);
        let mut expected = vec![StatusCode::UNSUPPORTED_MEDIA_TYPE; 400];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected, "axum's rejection is unchanged, and it is counted");
    }

    #[tokio::test]
    async fn a_spoofed_x_real_ip_from_a_non_loopback_peer_shares_the_peer_bucket() {
        let state = fresh_state();
        let mut statuses = Vec::new();
        for i in 0..401 {
            let spoofed = format!("198.51.{}.{}", 100 + i / 256, i % 256);
            statuses.push(challenge(&state, TEST_PEER, Some(&spoofed)).await);
        }
        let mut expected = vec![StatusCode::OK; 400];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    #[tokio::test]
    async fn rate_limited_answer_has_the_fixed_body_and_retry_after() {
        let state = fresh_state();
        for _ in 0..400 {
            assert_eq!(challenge(&state, TEST_PEER, None).await, StatusCode::OK);
        }
        let app = with_test_peer(router(state.clone()), TEST_PEER);
        let (status, headers, body) =
            post(app, "/api/auth/challenge", json!({"wallet_address": test_wallet()}), true, None).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body, json!({"error": "rate limited"}));
        let secs: u64 = headers[header::RETRY_AFTER].to_str().unwrap().parse().unwrap();
        assert!((1..=60).contains(&secs), "Retry-After {secs}");
    }

    /// BP-0 (ODQ-54 (a), demo-only): one address keeps its newest 100 sessions. 101 real
    /// sign-ins on `AppState::new` cover the production wiring (`state.rs`). The survivor
    /// checks are what a quota of 10 fails; "the first is gone" holds at any quota below 101.
    /// Seed 20 is in-memory only, and its key is not weak. 202 requests stay under the bucket.
    #[tokio::test]
    async fn session_quota_keeps_the_newest_100() {
        use ed25519_dalek::{Signer, SigningKey};
        let state = fresh_state();
        let key = SigningKey::from_bytes(&[20u8; 32]);
        let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
        let mut tokens = Vec::new();
        for i in 0..101 {
            let app = with_test_peer(router(state.clone()), TEST_PEER);
            let (status, _, body) =
                post(app, "/api/auth/challenge", json!({"wallet_address": wallet}), true, None).await;
            assert_eq!(status, StatusCode::OK, "challenge {i}: {body}");
            let text = body["challenge"].as_str().expect("a challenge").to_string();
            let signature = bs58::encode(key.sign(text.as_bytes()).to_bytes()).into_string();
            let app = with_test_peer(router(state.clone()), TEST_PEER);
            let req = json!({"wallet_address": wallet, "challenge": text, "signature": signature});
            let (status, _, body) = post(app, "/api/auth/verify", req, true, None).await;
            assert_eq!(status, StatusCode::OK, "verify {i}: {body}");
            tokens.push(body["token_id"].as_str().expect("a token_id").to_string());
        }
        assert!(!state.sessions.contains_key(&tokens[0]), "the oldest is signed out");
        assert!(tokens[1..].iter().all(|t| state.sessions.contains_key(t)), "the newest 100 survive");
        assert_eq!(state.sessions.len(), 100);
    }

    /// Served without a peer, the route cannot key the request. `challenge` has no other
    /// `500` path, so this one is the missing-`ConnectInfo` arm.
    #[tokio::test]
    async fn missing_connect_info_is_a_500() {
        let state = fresh_state();
        let (status, _, body) =
            post(router(state), "/api/auth/challenge", json!({"wallet_address": "w"}), true, None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "internal error");
        assert!(body["error_id"].is_string(), "an error_id is present: {body}");
    }

    // ── INF-6c-2a: address validation, body limit ─────────────────────────────

    /// One raw JSON POST from `TEST_PEER`; returns the status.
    async fn post_raw(state: &Arc<AppState>, uri: &str, body: String) -> StatusCode {
        let app = with_test_peer(router(state.clone()), TEST_PEER);
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        app.oneshot(req).await.unwrap().status()
    }

    /// `fields` (a JSON object's members, without braces) plus a `pad` field, exactly `n` bytes.
    fn padded(fields: &str, n: usize) -> String {
        let base = format!(r#"{{{fields},"pad":""}}"#).len();
        let body = format!(r#"{{{fields},"pad":"{}"}}"#, "x".repeat(n - base));
        assert_eq!(body.len(), n);
        body
    }

    #[tokio::test]
    async fn challenge_refuses_an_invalid_wallet_address_and_stores_nothing() {
        let state = fresh_state();
        let bad = [
            "w".to_string(),
            "0OIl".to_string(),
            String::new(),
            bs58::encode([9u8; 31]).into_string(),
            bs58::encode([9u8; 33]).into_string(),
            "1".repeat(4000),
        ];
        for addr in &bad {
            let app = with_test_peer(router(state.clone()), TEST_PEER);
            let (status, _, body) =
                post(app, "/api/auth/challenge", json!({"wallet_address": addr}), true, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "address {addr:.20}…: {body}");
            if addr == "w" {
                assert_eq!(body["error"], "invalid wallet address");
            }
        }
        assert!(state.pending_challenges.is_empty(), "a refused address is never stored");
    }

    #[tokio::test]
    async fn challenge_counts_an_invalid_wallet_address() {
        let state = fresh_state();
        let body = json!({"wallet_address": "w"}).to_string();
        for _ in 0..400 {
            assert_eq!(post_raw(&state, "/api/auth/challenge", body.clone()).await, StatusCode::BAD_REQUEST);
        }
        let last = post_raw(&state, "/api/auth/challenge", body).await;
        assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn challenge_counts_an_oversized_body() {
        let state = fresh_state();
        let body = padded(&format!(r#""wallet_address":"{}""#, test_wallet()), 5120);
        for _ in 0..400 {
            let s = post_raw(&state, "/api/auth/challenge", body.clone()).await;
            assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
        }
        let last = post_raw(&state, "/api/auth/challenge", body).await;
        assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn verify_counts_an_oversized_body() {
        let state = fresh_state();
        let body = padded(r#""wallet_address":"w","challenge":"c","signature":"s""#, 5120);
        for _ in 0..400 {
            let s = post_raw(&state, "/api/auth/verify", body.clone()).await;
            assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
        }
        let last = post_raw(&state, "/api/auth/verify", body).await;
        assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn challenge_admits_a_body_of_exactly_the_limit() {
        let state = fresh_state();
        let body = padded(&format!(r#""wallet_address":"{}""#, test_wallet()), 4096);
        assert_eq!(body.len(), 4096);
        assert_eq!(post_raw(&state, "/api/auth/challenge", body).await, StatusCode::OK);
    }

    /// Reviewer INF-6c-2a F2: the limit from the other side, with a literal size.
    #[tokio::test]
    async fn challenge_refuses_a_body_one_byte_over_the_limit() {
        let state = fresh_state();
        let body = padded(&format!(r#""wallet_address":"{}""#, test_wallet()), 4097);
        assert_eq!(body.len(), 4097);
        assert_eq!(post_raw(&state, "/api/auth/challenge", body).await, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // ── INF-6c-2b-1: weak keys, the identity forgery, the sweep task ──────────

    /// The identity point `[1, 0×31]`: a weak key.
    fn identity_wallet() -> String {
        let mut b = [0u8; 32];
        b[0] = 1;
        bs58::encode(b).into_string()
    }

    /// R = identity, s = 0: valid for the identity key under non-strict `verify`, for any text.
    fn forged_signature() -> String {
        let mut b = [0u8; 64];
        b[0] = 1;
        bs58::encode(b).into_string()
    }

    #[tokio::test]
    async fn challenge_refuses_a_weak_wallet_address_and_stores_nothing() {
        let state = fresh_state();
        for addr in ["11111111111111111111111111111111".to_string(), identity_wallet()] {
            let app = with_test_peer(router(state.clone()), TEST_PEER);
            let (status, _, body) =
                post(app, "/api/auth/challenge", json!({"wallet_address": addr}), true, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{addr}: {body}");
            assert_eq!(body["error"], "invalid wallet address", "{addr}");
        }
        assert!(state.pending_challenges.is_empty(), "a refused address is never stored");
    }

    /// `challenge` refuses the identity wallet, so the pending entry is inserted directly.
    #[tokio::test]
    async fn verify_refuses_a_forged_signature_for_the_identity_wallet() {
        let state = fresh_state();
        let text = "Mercs & Mines — forged-signature test";
        state.pending_challenges.insert(
            crate::auth::challenge_key(text),
            crate::state::PendingChallenge {
                wallet_address: identity_wallet(),
                challenge_text: text.to_string(),
                expires_at: chrono::Utc::now().timestamp() + 60,
            },
        );
        let app = with_test_peer(router(state.clone()), TEST_PEER);
        let req = json!({"wallet_address": identity_wallet(), "challenge": text, "signature": forged_signature()});
        let (status, _, body) = post(app, "/api/auth/verify", req, true, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let err = body["error"].as_str().unwrap_or_default();
        assert!(err.starts_with("signature invalid"), "answered before the signature step: {body}");
        assert!(state.sessions.is_empty(), "a forged signature issues no session");
    }

    /// Reviewer INF-6c-2a F1: the spawned task sweeps both maps on its immediate first tick.
    #[tokio::test]
    async fn auth_sweep_task_sweeps_both_maps() {
        use std::time::Duration;
        let state = fresh_state();
        let now = chrono::Utc::now().timestamp();
        let pending = |expires_at| crate::state::PendingChallenge {
            wallet_address: "w".into(),
            challenge_text: "c".into(),
            expires_at,
        };
        let session = |id: &str, expires_at| shared::SessionToken {
            token_id: id.into(),
            account_id: "a".into(),
            wallet_address: "w".into(),
            issued_at: 0,
            expires_at,
        };
        state.pending_challenges.insert([1u8; 32], pending(now - 100));
        state.pending_challenges.insert([2u8; 32], pending(now + 3600));
        state.sessions.insert("expired".into(), session("expired", now - 100));
        state.sessions.insert("live".into(), session("live", now + 3600));
        // Admitted through the index, so the prune has something to prune (N-l).
        let key = crate::rate_limit::ClientKey::V4(std::net::Ipv4Addr::new(192, 0, 2, 9));
        let first = crate::auth::admit_session(&state, "w", key).expect("under the cap").token_id;
        let second = crate::auth::admit_session(&state, "w", key).expect("under the cap").token_id;
        state.sessions.get_mut(&first).expect("admitted").expires_at = now - 100;

        let h = tokio::spawn(crate::auth::run_auth_sweep_task(state.clone()));
        let swept = tokio::time::timeout(Duration::from_secs(5), async {
            while state.pending_challenges.contains_key(&[1u8; 32])
                || state.sessions.contains_key("expired")
                || state.sessions.contains_key(&first)
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        h.abort();
        assert!(swept.is_ok(), "the sweep task did not clear the expired entries");
        assert!(state.pending_challenges.contains_key(&[2u8; 32]), "the live challenge is kept");
        assert!(state.sessions.contains_key("live"), "the live session is kept");
        assert!(state.sessions.contains_key(&second), "the live admitted session is kept");
        assert_eq!(state.session_index.indexed(), 1, "the sweep prunes the index after its retain");
    }

    #[tokio::test]
    async fn a_loopback_peer_without_x_real_ip_warns_at_most_hourly() {
        let state = fresh_state();
        challenge(&state, TEST_PEER, None).await;
        assert!(!state.rate_limits.shared_warn.fired(), "a keyed peer does not warn");
        challenge(&state, "127.0.0.1:1".parse().unwrap(), None).await;
        assert!(state.rate_limits.shared_warn.fired(), "the shared bucket warns");
        let now = std::time::Instant::now();
        assert_eq!(state.rate_limits.shared_warn.hit(now), None, "within the hour: counted, not journalled");
        assert_eq!(
            state.rate_limits.shared_warn.hit(now + std::time::Duration::from_secs(3601)),
            Some(2),
            "after the hour: the count since the last line"
        );
    }

    /// The backstop refuses a real sign-in and logs nobody out (ODQ-48). A small-cap index is
    /// swapped in, so nothing is pre-filled. Seed 20 is in-memory only, and its key is not weak.
    #[tokio::test]
    async fn verify_answers_503_when_sign_in_is_at_capacity() {
        use ed25519_dalek::{Signer, SigningKey};
        let mut s = AppState::new(Arc::new(Config::default()));
        s.session_index = crate::auth::SessionIndex::new(crate::auth::SESSION_QUOTA, 1);
        let state = Arc::new(s);
        let now = chrono::Utc::now().timestamp();
        state.sessions.insert(
            "held".into(),
            shared::SessionToken {
                token_id: "held".into(),
                account_id: "a".into(),
                wallet_address: "w".into(),
                issued_at: now,
                expires_at: now + 3600,
            },
        );
        let key = SigningKey::from_bytes(&[20u8; 32]);
        let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
        let app = with_test_peer(router(state.clone()), TEST_PEER);
        let (status, _, body) = post(app, "/api/auth/challenge", json!({"wallet_address": wallet}), true, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let text = body["challenge"].as_str().expect("a challenge").to_string();
        let signature = bs58::encode(key.sign(text.as_bytes()).to_bytes()).into_string();
        let app = with_test_peer(router(state.clone()), TEST_PEER);
        let req = json!({"wallet_address": wallet, "challenge": text, "signature": signature});
        let (status, _, body) = post(app, "/api/auth/verify", req, true, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["error"], "sign-in is at capacity, try again later");
        assert!(state.sessions.contains_key("held"), "a refusal logs nobody out");
        assert_eq!(state.sessions.len(), 1);
    }
}
