use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use axum::{
    Router,
    routing::post,
    extract::{rejection::JsonRejection, ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::map_gen::{hex_axial_distance, MAP_RADIUS_HEXES};
use crate::repository::{
    CampaignLifecycle, ConvoyDbRecord, DeploymentTimer, HexOccupant, TimerType, WalletAddress,
};
use crate::state::AppState;

#[derive(Deserialize, Serialize)]
pub struct DispatchConvoyRequest {
    pub origin_q:      i32,
    pub origin_r:      i32,
    pub destination_q: i32,
    pub destination_r: i32,
    pub vehicle_class: String,
    pub cargo:         serde_json::Value,
}

/// The largest dispatch body (BP-1e-2). An honest one is about 130 bytes.
pub const CONVOY_BODY_LIMIT: usize = 4096;
/// The largest `cargo`, serialised. It is logged, stored and sent on the owner's events
/// socket, so it is bounded on its own.
pub const CONVOY_CARGO_MAX_BYTES: usize = 1024;
/// The longest `vehicle_class`, in bytes. It is logged, stored and shown on the sector map.
pub const MAX_VEHICLE_CLASS_BYTES: usize = 64;

/// Whether an axial hex is on the sector map: its cube distance from the centre is within
/// `MAP_RADIUS_HEXES`. Computed in `i64`, so any client value is safe; `hex_axial_distance`
/// subtracts raw `i32`s and runs only once both hexes pass.
fn on_the_map(q: i32, r: i32) -> bool {
    let (q, r) = (q as i64, r as i64);
    let dist = (q.abs() + r.abs() + (q + r).abs()) / 2;
    dist <= MAP_RADIUS_HEXES as i64
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/:campaign_id/convoy/dispatch", post(dispatch_convoy).layer(DefaultBodyLimit::max(CONVOY_BODY_LIMIT)))
}

/// `POST /api/campaign/:campaign_id/convoy/dispatch`. Order (BP-1e-2): 401 → 400 path →
/// 500 without a peer → 429 `convoy_ip` → 429 `convoy_wallet` → axum's body rejection,
/// counted (413 over `CONVOY_BODY_LIMIT`; 400, 415, 422) → 401 wallet → 400 shape
/// (`vehicle_class`, `cargo`, a hex off the map, zero distance) → 404 campaign → 403 member →
/// 409 not Active → 409 sector state missing (GAP-85; 500 on a read error) → 400 impassable
/// → timer, write, marker → 201.
async fn dispatch_convoy(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    body: Result<Json<DispatchConvoyRequest>, JsonRejection>,
) -> Response {
    // TODO(phase-2): validate He3 balance before dispatch
    let ip = match super::client_ip(ci, &headers, &state.rate_limits) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let now_i = Instant::now();
    if let Err(r) = state.rate_limits.convoy_ip.check(ip, now_i) {
        return super::too_many_requests(r);
    }
    if let Err(r) = state.rate_limits.convoy_wallet.check(token.wallet_address.clone(), now_i) {
        return super::too_many_requests(r);
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return rej.into_response(),
    };

    // Decode wallet: base58 string → Vec<u8> for DB storage, [u8; 32] for timer
    let owner_wallet: Vec<u8> = match bs58::decode(&token.wallet_address).into_vec() {
        Ok(v) => v,
        Err(_) => return super::unauthorized("session wallet is not valid base58").into_response(),
    };
    let wallet_bytes: [u8; 32] = match owner_wallet.as_slice().try_into() {
        Ok(b) => b,
        Err(_) => return super::unauthorized("session wallet is not a 32-byte key").into_response(),
    };

    // 400 — the body's shape (BP-1e-2). Each check is a pure function of the request, so the
    // answer never depends on the campaign, and a malformed body costs no read.
    if req.vehicle_class.len() > MAX_VEHICLE_CLASS_BYTES {
        return super::bad_request(format!("vehicle_class is longer than {MAX_VEHICLE_CLASS_BYTES} bytes")).into_response();
    }
    let cargo_bytes = serde_json::to_vec(&req.cargo).map(|v| v.len()).unwrap_or(usize::MAX);
    if cargo_bytes > CONVOY_CARGO_MAX_BYTES {
        return super::bad_request(format!("cargo is larger than {CONVOY_CARGO_MAX_BYTES} bytes")).into_response();
    }
    if !on_the_map(req.origin_q, req.origin_r) || !on_the_map(req.destination_q, req.destination_r) {
        return super::bad_request("hex is off the map").into_response();
    }
    // A zero-distance timer fires at once and could race the dispatch commit into the
    // phantom path, so it is refused before any write (GAP-44, W3).
    if (req.origin_q, req.origin_r) == (req.destination_q, req.destination_r) {
        return super::bad_request("origin and destination are the same hex").into_response();
    }

    // Look up campaign → derive sector_id
    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None)    => return super::not_found("campaign not found").into_response(),
        Err(e)      => {
            return super::internal_error(format!("dispatch_convoy: get_campaign failed for campaign {campaign_id}: {e}")).into_response();
        }
    };

    // GAP-74 (5a-3b): 403 for a non-member, then 409 unless Active, in that order, so a
    // non-member can't probe a campaign's state. Any membership row counts (S7: no
    // production writer sets is_active = false).
    match state.membership_repo.get_membership(&owner_wallet, campaign_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return super::forbidden("not a member of this campaign").into_response(),
        Err(e) => {
            return super::internal_error(format!("dispatch_convoy: membership read failed for campaign {campaign_id}: {e}")).into_response();
        }
    }
    if campaign.state != CampaignLifecycle::Active {
        return super::conflict("campaign is not active").into_response();
    }
    let sector_id = campaign.sector_id;

    // Terrain modifier — destination hex only (Phase 1 simplification)
    // TODO(phase-2): use full route waypoints for terrain modifier
    // GAP-85: the sector must be readable. A plain-terrain default would skip the impassable
    // check and give a wrong travel time, so a read error is a 500 and a missing sector a 409.
    let sector = match state.sector_repo.get_sector(sector_id).await {
        Ok(s) => s,
        Err(e) => {
            return super::internal_error(format!("dispatch_convoy: get_sector failed for campaign {campaign_id} (sector {sector_id}): {e:?}")).into_response();
        }
    };
    if sector.is_none() {
        tracing::warn!(%campaign_id, %sector_id, "dispatch_convoy: sector state missing; refusing the dispatch");
        return super::conflict("campaign sector state is unavailable").into_response();
    }
    let terrain_modifier = sector
        .and_then(|s| s.terrain.get(&format!("{},{}", req.destination_q, req.destination_r)).cloned())
        .map(|t| t.travel_time_modifier())
        .unwrap_or(1.0_f64);

    // TODO(phase-1): reject route if destination hex is impassable
    if terrain_modifier == f64::INFINITY {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "destination hex is impassable"}))).into_response();
    }

    // Compute arrival time
    let hex_dist = hex_axial_distance(req.origin_q, req.origin_r, req.destination_q, req.destination_r);
    // TODO(phase-2): vehicle class speed modifiers
    let speed_modifier = 1.0_f64;
    let travel_mins = (hex_dist as f64) * 20.0 * terrain_modifier * speed_modifier;
    // Truncated to µs, the precision Postgres stores, so the log row, the convoy record
    // and the timer payload all carry the same instant (Reviewer INF-5 F5).
    let arrival_time = (Utc::now() + Duration::minutes(travel_mins as i64)).trunc_subsecs(6);

    // Generate convoy_id — INVARIANT: this is also used as timer_id (see schedule_timer below)
    let convoy_id = Uuid::new_v4();

    // Build straight-line route for Phase 1 (full pathfinding is Phase 2)
    let route = serde_json::json!([
        {"q": req.origin_q,      "r": req.origin_r},
        {"q": req.destination_q, "r": req.destination_r}
    ]);

    // The log row carries the player's whole input plus the arrival time, which is not
    // recomputable (wall clock, and a sector read error changes the terrain modifier).
    // Built before any write, so a failure here leaves nothing behind.
    let mut payload = match serde_json::to_value(&req) {
        Ok(v) => v,
        Err(e) => return super::internal_error(format!("dispatch_convoy: request serialization failed: {e}")).into_response(),
    };
    payload["convoy_id"] = serde_json::json!(convoy_id);
    payload["arrival_time"] = serde_json::json!(arrival_time);
    let entry = shared::InputLogEntry {
        tick:            0,
        seq:             0,
        event_type:      "convoy_dispatched".to_string(),
        player_id:       Some(token.wallet_address.clone()),
        payload,
        narrative_event: None,
    };

    let record = ConvoyDbRecord {
        convoy_id,
        sector_id,
        owner_wallet: owner_wallet.clone(),
        origin_q:      req.origin_q,
        origin_r:      req.origin_r,
        destination_q: req.destination_q,
        destination_r: req.destination_r,
        route,
        arrival_time,
        vehicle_class: req.vehicle_class.clone(),
        cargo:         req.cargo.clone(),
        in_transit:    true,
        // DB DEFAULT now() fills created_at on insert; field is required by Rust struct
        created_at:    Utc::now(),
    };
    // Write order (GAP-44): the Redis timer, then the convoy and its log row in one
    // transaction, then the cosmetic origin marker. Each failure leaves either nothing or
    // a timer with no convoy. A failed dispatch keeps its timer deliberately (Reviewer
    // INF-5 F2): the commit may have landed even though it reported an error, so at fire
    // time the expiry poll lands the convoy if the row exists, and its phantom path
    // cancels the timer if not.

    // 1. The arrival timer.
    // INVARIANT: timer_id == convoy_id so poll_expiry_once (3c-1) can land the convoy
    // by timer.timer_id without a separate convoy lookup field.
    let timer = DeploymentTimer {
        timer_id:      convoy_id, // ← load-bearing: timer_id IS the convoy_id
        player_wallet: WalletAddress(wallet_bytes),
        sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      arrival_time,
    };
    if let Err(e) = state.timer_repo.schedule_timer(timer).await {
        return super::internal_error(format!("dispatch_convoy: schedule_timer failed for campaign {campaign_id}, convoy {convoy_id}: {e:?}")).into_response();
    }

    // 2. The convoy and its `convoy_dispatched` row, together or not at all.
    if let Err(e) = state.convoy_repo.dispatch_convoy(record, campaign_id, &entry).await {
        return super::internal_error(format!("dispatch_convoy: dispatch write failed for campaign {campaign_id}, convoy {convoy_id}: {e:?}")).into_response();
    }

    // 3. The in-transit marker at the origin hex. Display only, and the dispatch has
    //    happened, so a failure is warned and the request still succeeds.
    let occupant = HexOccupant {
        id:           convoy_id,
        owner_wallet: owner_wallet.clone(),
        unit_type:    req.vehicle_class.clone(),
    };
    if let Err(e) = state.sector_repo.add_hex_occupant(sector_id, req.origin_q, req.origin_r, occupant).await {
        tracing::warn!("dispatch_convoy: add_hex_occupant failed for campaign {campaign_id}, convoy {convoy_id}; dispatched without an origin marker: {e:?}");
    }

    (StatusCode::CREATED, Json(serde_json::json!({ "convoy_id": convoy_id }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::convoy_expiry::{poll_expiry_once, ExpiryWarns};
    use crate::repository::{
        ConvoyRepository, NewCampaignInstance, OccupationStatus, PostgresConvoyRepository,
        RepositoryError, SectorState,
    };
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Inserts a signed-in session `token` for `wallet`.
    fn sign_in(state: &AppState, token: &str, wallet: [u8; 32]) {
        let wallet = bs58::encode(wallet).into_string();
        let now = Utc::now().timestamp();
        state.sessions.insert(token.into(), shared::SessionToken {
            token_id: token.into(),
            account_id: wallet.clone(),
            wallet_address: wallet,
            issued_at: now,
            expires_at: now + 600,
        });
    }

    /// State with one Active campaign that `[5u8; 32]` is a member of, a signed-in session
    /// `"t"` for that wallet and in-memory timers. Every write goes through `AppState::new`'s
    /// shared stores. The default convoy repository is the in-memory stub, which panics if
    /// any convoy write is tried.
    async fn dispatch_state() -> (AppState, Uuid, Uuid) {
        let sector_id = Uuid::new_v4();
        let state = AppState::new(Arc::new(Config::default()));
        let campaign_id = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id, map_seed: 1 })
            .await
            .expect("create_campaign")
            .campaign_id;
        let activated = state.campaign_repo
            .activate_campaign(campaign_id, Utc::now() + Duration::days(90), serde_json::json!({}))
            .await
            .expect("activate_campaign");
        assert!(activated, "the fixture campaign must activate");
        state.membership_repo.add_member(&[5u8; 32], campaign_id, None, None).await.expect("add_member");
        sign_in(&state, "t", [5u8; 32]);
        // GAP-85: dispatch refuses a campaign whose sector state is missing.
        state.sector_repo.upsert_sector(SectorState {
            sector_id,
            campaign_id,
            occupation_status: OccupationStatus::Neutral,
            owner: None,
            deployed_unit_count: 0,
            terrain: Default::default(),
            magma_veins: vec![],
            hex_occupancy: Default::default(),
        }).await.expect("in-memory upsert_sector");
        (state, campaign_id, sector_id)
    }

    async fn dispatch_as(state: Arc<AppState>, token: &str, campaign_id: Uuid, to: (i32, i32)) -> (StatusCode, Vec<u8>) {
        let body = serde_json::json!({
            "origin_q": 0, "origin_r": 0, "destination_q": to.0, "destination_r": to.1,
            "vehicle_class": "BehemothMegaTruck", "cargo": {}
        });
        post_dispatch(state, token, campaign_id, Some(&body)).await
    }

    /// One dispatch POST from `TEST_PEER`. `body: None` sends no content type (axum's `415`,
    /// which the limiters count).
    async fn post_dispatch(state: Arc<AppState>, token: &str, campaign_id: Uuid, body: Option<&serde_json::Value>) -> (StatusCode, Vec<u8>) {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/campaign/{campaign_id}/convoy/dispatch"))
            .header("authorization", format!("Bearer {token}"));
        let req = match body {
            Some(b) => req.header("content-type", "application/json").body(Body::from(serde_json::to_vec(b).unwrap())),
            None => req.body(Body::from("{}")),
        }
        .unwrap();
        let app = super::super::with_test_peer(router().with_state(state), super::super::TEST_PEER);
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        (status, resp.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    async fn dispatch(state: Arc<AppState>, campaign_id: Uuid, to: (i32, i32)) -> StatusCode {
        dispatch_as(state, "t", campaign_id, to).await.0
    }

    /// GAP-74: a signed-in wallet with no membership is refused `403`, and nothing is scheduled.
    #[tokio::test]
    async fn dispatch_refuses_a_non_member() {
        let (mut state, campaign_id, _) = dispatch_state().await;
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();
        sign_in(&state, "outsider", [6u8; 32]);

        let (status, body) = dispatch_as(Arc::new(state), "outsider", campaign_id, (1, 0)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{}", String::from_utf8_lossy(&body));
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
    }

    /// GAP-74: a member of a campaign that is not Active is refused `409`.
    #[tokio::test]
    async fn dispatch_refuses_a_campaign_that_is_not_active() {
        let (mut state, _, _) = dispatch_state().await;
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();
        let pending = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 2 })
            .await
            .expect("create_campaign")
            .campaign_id;
        state.membership_repo.add_member(&[5u8; 32], pending, None, None).await.expect("add_member");

        let (status, body) = dispatch_as(Arc::new(state), "t", pending, (1, 0)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&body));
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
    }

    /// An unknown campaign is `404` with the route's JSON body.
    #[tokio::test]
    async fn dispatch_unknown_campaign_is_404_with_a_body() {
        let (state, _, _) = dispatch_state().await;
        let (status, body) = dispatch_as(Arc::new(state), "t", Uuid::new_v4(), (1, 0)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let v: serde_json::Value = serde_json::from_slice(&body).expect("a JSON body");
        assert_eq!(v, serde_json::json!({"error": "campaign not found"}));
    }

    /// Reviewer 5a-3b CF5: a failed campaign read is a `500` built by `internal_error`, so
    /// it carries the route's JSON body. Since GAP-76 that body is generic, so the arm is
    /// identified by construction, not by text: the closed pool sits only on
    /// `campaign_repo`, and `get_campaign` is the first fallible I/O after the wallet decode.
    #[tokio::test]
    async fn dispatch_campaign_read_failure_is_500_with_a_body() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .expect("lazy pool");
        pool.close().await;
        let (mut state, campaign_id, _) = dispatch_state().await;
        state.campaign_repo = Arc::new(crate::repository::PostgresCampaignRepository::new(pool));

        let (status, body) = dispatch_as(Arc::new(state), "t", campaign_id, (1, 0)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let v: serde_json::Value = serde_json::from_slice(&body).expect("a JSON body");
        let id: Uuid = v["error_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("error_id is not a UUID: {v}"));
        assert_eq!(v, serde_json::json!({"error": "internal error", "error_id": id}));
        assert!(!v.to_string().contains("get_campaign"), "the message leaked: {v}");
    }

    // ── BP-1e-2: dispatch's limits, shape checks and GAP-85 (in-memory) ──

    /// `dispatch_state()` with the convoy write failing (`NoConvoyRows`) and its own timers.
    /// A body that passes every check therefore ends in `500` at the write, after its timer,
    /// so the tests read `500` + one timer as "accepted", and a refusal leaves no timer.
    async fn guarded_state() -> (Arc<AppState>, Arc<crate::repository::InMemoryTimerRepository>, Uuid, Uuid) {
        let (mut state, campaign_id, sector_id) = dispatch_state().await;
        state.convoy_repo = Arc::new(NoConvoyRows);
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();
        (Arc::new(state), timers, campaign_id, sector_id)
    }

    fn dispatch_body(to: (i32, i32), vehicle_class: &str, cargo: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "origin_q": 0, "origin_r": 0, "destination_q": to.0, "destination_r": to.1,
            "vehicle_class": vehicle_class, "cargo": cargo
        })
    }

    fn error_of(body: &[u8]) -> String {
        let v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
        v["error"].as_str().unwrap_or("").to_string()
    }

    #[tokio::test]
    async fn dispatch_counts_before_reading_the_body() {
        let (state, timers, campaign_id, _) = guarded_state().await;
        for i in 0..crate::rate_limit::CONVOY_PER_WALLET {
            let (status, _) = post_dispatch(state.clone(), "t", campaign_id, None).await;
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "request {i}");
        }
        let body = dispatch_body((1, 0), "BehemothMegaTruck", serde_json::json!({}));
        let (status, _) = post_dispatch(state, "t", campaign_id, Some(&body)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
    }

    #[tokio::test]
    async fn dispatch_refuses_oversized_cargo() {
        let (state, timers, campaign_id, _) = guarded_state().await;
        let over = serde_json::json!({ "x": "a".repeat(1017) });
        assert_eq!(serde_json::to_vec(&over).unwrap().len(), CONVOY_CARGO_MAX_BYTES + 1);
        let (status, body) = post_dispatch(state.clone(), "t", campaign_id, Some(&dispatch_body((1, 0), "Hauler", over))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&body));
        assert!(error_of(&body).contains("cargo"), "{}", String::from_utf8_lossy(&body));
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
        let edge = serde_json::json!({ "x": "a".repeat(1016) });
        let (status, _) = post_dispatch(state, "t", campaign_id, Some(&dispatch_body((1, 0), "Hauler", edge))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "a cargo of exactly the limit passes every check");
        assert_eq!(timers.0.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_refuses_a_long_vehicle_class() {
        let (state, timers, campaign_id, _) = guarded_state().await;
        let long = "x".repeat(MAX_VEHICLE_CLASS_BYTES + 1);
        let (status, body) = post_dispatch(state.clone(), "t", campaign_id, Some(&dispatch_body((1, 0), &long, serde_json::json!({})))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&body));
        assert!(error_of(&body).contains("vehicle_class"), "{}", String::from_utf8_lossy(&body));
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
        let edge = "x".repeat(MAX_VEHICLE_CLASS_BYTES);
        let (status, _) = post_dispatch(state, "t", campaign_id, Some(&dispatch_body((1, 0), &edge, serde_json::json!({})))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "a vehicle_class of exactly the limit passes every check");
        assert_eq!(timers.0.len(), 1);
    }

    /// Off-map hexes are refused before `hex_axial_distance`, whose raw `i32` subtraction
    /// would overflow on extreme values. The first case is cheap and on the axis. Six
    /// dispatches in all: `CONVOY_PER_WALLET`.
    #[tokio::test]
    async fn dispatch_refuses_a_hex_off_the_map() {
        let (state, timers, campaign_id, _) = guarded_state().await;
        let r = MAP_RADIUS_HEXES as i32;
        for to in [(r + 1, 0), (r, 1), (i32::MAX, 0), (i32::MIN, i32::MIN)] {
            let (status, body) = post_dispatch(state.clone(), "t", campaign_id, Some(&dispatch_body(to, "Hauler", serde_json::json!({})))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{to:?}: {}", String::from_utf8_lossy(&body));
            assert_eq!(error_of(&body), "hex is off the map", "{to:?}");
        }
        let mut origin_off = dispatch_body((1, 0), "Hauler", serde_json::json!({}));
        origin_off["origin_q"] = serde_json::json!(i32::MIN);
        let (status, _) = post_dispatch(state.clone(), "t", campaign_id, Some(&origin_off)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "an origin off the map is refused too");
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
        let (status, _) = post_dispatch(state, "t", campaign_id, Some(&dispatch_body((r, 0), "Hauler", serde_json::json!({})))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "the map's edge passes every check");
        assert_eq!(timers.0.len(), 1);
    }

    /// GAP-85: a campaign whose sector state is missing refuses the dispatch with `409`,
    /// never a plain-terrain default.
    #[tokio::test]
    async fn dispatch_refuses_a_missing_sector() {
        let (state, timers, campaign_id, sector_id) = guarded_state().await;
        state.sector_repo.delete_sector(sector_id).await.expect("delete_sector");
        let (status, body) = post_dispatch(state, "t", campaign_id, Some(&dispatch_body((1, 0), "Hauler", serde_json::json!({})))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&body));
        assert_eq!(error_of(&body), "campaign sector state is unavailable");
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
    }

    #[tokio::test]
    async fn dispatch_db_failure_keeps_the_timer_for_the_phantom_path() {
        // A closed lazy pool fails every query without a database (INF-3a pattern).
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .expect("lazy pool");
        pool.close().await;
        let (mut state, campaign_id, sector_id) = dispatch_state().await;
        state.convoy_repo = Arc::new(PostgresConvoyRepository::new(pool));
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();

        let status = dispatch(Arc::new(state), campaign_id, (1, 0)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(timers.0.len(), 1, "a failed dispatch write keeps its timer for the phantom path");
        let timer = timers.0.iter().next().expect("the kept timer");
        assert!(matches!(timer.timer_type, TimerType::ConvoyArrival), "the kept timer is a convoy arrival timer");
        assert_eq!(timer.sector_id, sector_id, "the kept timer is in the dispatching campaign's sector");
        assert_eq!(timer.player_wallet.0, [5u8; 32], "the kept timer belongs to the dispatching wallet");
        assert!(timer.fires_at > Utc::now(), "the kept timer fires in the future");
    }

    /// No convoy row exists: dispatch fails, and the arrival poll finds nothing to land.
    struct NoConvoyRows;

    #[async_trait::async_trait]
    impl ConvoyRepository for NoConvoyRows {
        async fn create_convoy(&self, _: ConvoyDbRecord) -> Result<Uuid, RepositoryError> {
            unimplemented!()
        }
        async fn get_convoy(&self, _: Uuid) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
            Ok(None)
        }
        async fn dispatch_convoy(&self, _: ConvoyDbRecord, _: Uuid, _: &shared::InputLogEntry) -> Result<(), RepositoryError> {
            Err(RepositoryError::Internal("test: dispatch write fails".into()))
        }
        async fn land_convoy(&self, _: Uuid, _: Uuid, _: &shared::InputLogEntry) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
            Ok(None)
        }
        async fn has_convoy_in_transit(&self, _: Uuid, _: &[u8]) -> Result<bool, RepositoryError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn failed_dispatch_timer_is_cancelled_by_the_phantom_path() {
        let (mut state, campaign_id, _) = dispatch_state().await;
        state.convoy_repo = Arc::new(NoConvoyRows);
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();
        let state = Arc::new(state);

        let status = dispatch(state.clone(), campaign_id, (1, 0)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        // The phantom path is reached only once the timer's sector resolves (GAP-59).
        let sector_id = {
            let mut timer = timers.0.iter_mut().next().expect("the kept timer");
            // poll_expiry_once reads the wall clock, so move the timer into the past.
            timer.fires_at = Utc::now() - Duration::seconds(1);
            timer.sector_id
        };
        state.sector_repo.upsert_sector(SectorState {
            sector_id,
            campaign_id,
            occupation_status: OccupationStatus::Neutral,
            owner: None,
            deployed_unit_count: 0,
            terrain: Default::default(),
            magma_veins: vec![],
            hex_occupancy: Default::default(),
        }).await.expect("in-memory upsert_sector");

        let mut warns = ExpiryWarns::new();
        poll_expiry_once(&state, &mut warns).await;
        assert!(timers.0.is_empty(), "the phantom path cancels a timer whose convoy row does not exist");
        assert_eq!(warns.phantom.len(), 1, "the cleanup is warned as a phantom timer");
    }

    #[tokio::test]
    async fn dispatch_refuses_zero_distance() {
        let (mut state, campaign_id, _) = dispatch_state().await;
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();

        let status = dispatch(Arc::new(state), campaign_id, (0, 0)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
    }
}
