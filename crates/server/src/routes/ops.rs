//! The ops routes (BP-8a-1 board, BP-8a-2 travel legs; **demo-only**; Breakpoint plan §4d).
//! The `dev` port deletes this file with `demo_ops.rs`.
//!
//! Every route answers the same body (`OpsView`), so the client never polls: a `POST`
//! returns the whole state. The three share one `ops_ip` / `ops_wallet` budget.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{rejection::JsonRejection, ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shared::SessionToken;
use sim_engine::rng::generate_seed;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::demo_ops::{ops_board, ops_time, ActiveOpView, OpsBoard, OpsSite, ReturnRefusal, StartRefusal};
use crate::repository::{CampaignInstance, CampaignLifecycle, SectorState};
use crate::state::AppState;

use super::sector::GatewayHex;
use super::{
    bad_request, client_ip, conflict, forbidden, internal_error, not_found, service_unavailable, too_many_requests,
    unauthorized, AppError,
};

/// Request body cap for `ops/start`, layered on the route: the overflow reaches the handler as
/// a `JsonRejection` after the limiters, so it is counted (`413`).
pub const OPS_BODY_LIMIT: usize = 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/:campaign_id/ops", get(get_ops_board))
        .route("/api/campaign/:campaign_id/ops/start", post(post_ops_start).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/return", post(post_ops_return))
}

/// The ops state as the client sees it, from every route. `now` is the server's clock, so the
/// client's crawl survives a phone whose clock is off. `ore_banked` is always 0 until BP-8b-2.
#[derive(Serialize)]
pub struct OpsView<'a> {
    pub campaign_id: Uuid,
    pub gateway: Option<GatewayHex>,
    pub sites: &'a [OpsSite],
    pub active_op: Option<ActiveOpView>,
    pub ore_banked: u32,
    pub now: String,
}

#[derive(Deserialize)]
pub struct OpsStartRequest {
    pub site_id: String,
}

/// The shared budget: `429` per address (`ops_ip`), then per wallet (`ops_wallet`).
fn ops_limits(
    state: &AppState,
    token: &SessionToken,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
) -> Result<Option<Response>, AppError> {
    let ip = client_ip(ci, headers, &state.rate_limits)?;
    let tick = Instant::now();
    if let Err(r) = state.demo_ops.ip.check(ip, tick) {
        return Ok(Some(too_many_requests(r)));
    }
    if let Err(r) = state.demo_ops.wallet.check(token.wallet_address.clone(), tick) {
        return Ok(Some(too_many_requests(r)));
    }
    Ok(None)
}

/// `401` (wallet not base58) → `404` (unknown campaign) → `403` (not a member) → `409` (not
/// `Active`; the campaign's entry is dropped). Returns the campaign and the member's gateway.
async fn ops_member(
    state: &AppState,
    token: &SessionToken,
    campaign_id: Uuid,
    route: &str,
) -> Result<(CampaignInstance, Option<(i32, i32)>), AppError> {
    let wallet: Vec<u8> = bs58::decode(&token.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;
    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(not_found("campaign not found")),
        Err(e) => {
            return Err(internal_error(format!("{route}: get_campaign failed for campaign {campaign_id}: {e}")));
        }
    };
    let membership = match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(m) => m,
        Err(e) => {
            return Err(internal_error(format!("{route}: membership read failed for campaign {campaign_id}: {e}")));
        }
    };
    if membership.is_none() {
        return Err(forbidden("not a member of this campaign"));
    }
    if campaign.state != CampaignLifecycle::Active {
        state.demo_ops.store.remove(campaign_id);
        return Err(conflict("campaign is not active"));
    }
    let gateway = membership.and_then(|m| Some((m.gateway_hex_q?, m.gateway_hex_r?)));
    Ok((campaign, gateway))
}

/// BP-1d's repair for a sector that is gone. When it retires the campaign (`409`), the
/// campaign's entry goes with it.
async fn repair_or_drop(state: &Arc<AppState>, campaign_id: Uuid, sector_id: Uuid) -> Result<SectorState, AppError> {
    match super::campaign::repair_missing_sector(Arc::clone(state), campaign_id, sector_id).await {
        Ok(s) => Ok(s),
        Err(e) => {
            if e.0 == StatusCode::CONFLICT {
                state.demo_ops.store.remove(campaign_id);
            }
            Err(e)
        }
    }
}

/// The member's board: from the store, or on a miss from the sector (computed and cached; a
/// store full of ops serves it uncached). A missing sector goes to the repair.
/// - `prove_sector`: a cache hit is no proof the map still exists (Reviewer BP-8a-1 F2), so a
///   route that starts something checks it with one `EXISTS` before trusting the hit. The
///   board read skips it: it starts nothing, and the next sector read repairs.
async fn load_board(
    state: &Arc<AppState>,
    campaign: &CampaignInstance,
    gw: (i32, i32),
    now: DateTime<Utc>,
    prove_sector: bool,
    route: &str,
) -> Result<Arc<OpsBoard>, AppError> {
    let (campaign_id, sector_id) = (campaign.campaign_id, campaign.sector_id);
    if let Some(board) = state.demo_ops.store.board(campaign_id, gw, now) {
        if prove_sector {
            match state.sector_repo.sector_exists(sector_id).await {
                Ok(true) => {}
                Ok(false) => {
                    repair_or_drop(state, campaign_id, sector_id).await?;
                }
                Err(e) => {
                    return Err(internal_error(format!("{route}: sector_exists failed for sector {sector_id} of campaign {campaign_id}: {e}")));
                }
            }
        }
        return Ok(board);
    }
    let sector = match state.sector_repo.get_sector(sector_id).await {
        Ok(Some(s)) => s,
        Ok(None) => repair_or_drop(state, campaign_id, sector_id).await?,
        Err(e) => {
            return Err(internal_error(format!("{route}: get_sector failed for sector {sector_id} of campaign {campaign_id}: {e}")));
        }
    };
    let board = Arc::new(ops_board(&sector.terrain, &sector.magma_veins, gw, campaign_id));
    state.demo_ops.store.put_board(campaign_id, gw, Arc::clone(&board), now);
    Ok(board)
}

fn ops_response(state: &AppState, campaign_id: Uuid, gw: (i32, i32), board: &OpsBoard, now: DateTime<Utc>) -> Response {
    let view = OpsView {
        campaign_id,
        gateway: Some(GatewayHex { q: gw.0, r: gw.1 }),
        sites: &board.sites,
        active_op: state.demo_ops.store.op_view(campaign_id, now),
        ore_banked: 0,
        now: ops_time(now),
    };
    Json(view).into_response()
}

/// A member's ops board, `GET /api/campaign/:campaign_id/ops` (BP-8a-1, demo-only). Order:
/// `401` (no session) → `429` per address (`ops_ip`) → `429` per wallet (`ops_wallet`) →
/// `404` (unknown campaign) → `403` (not a member) → `409` (not `Active`; the campaign's
/// cached entry is dropped) → the board.
/// - `gateway: null` (not assigned) → `sites: []`, nothing cached.
/// - The board comes from the store; a miss reads the sector once, computes and caches it.
///   A missing sector goes to `repair_missing_sector` as the sector route does (`409` once
///   retired). A failed read is `500`. A read never refuses: a store full of ops serves the
///   board uncached.
/// - A retire that lands between the state check and the insert leaves an entry behind; the
///   campaign's next read drops it, and the reaper's prune within 5 minutes.
async fn get_ops_board(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    if let Some(r) = ops_limits(&state, &token, ci, &headers)? {
        return Ok(r);
    }
    let now = Utc::now();
    let (campaign, gateway) = ops_member(&state, &token, campaign_id, "get_ops_board").await?;
    let Some(gw) = gateway else {
        let view = OpsView { campaign_id, gateway: None, sites: &[], active_op: None, ore_banked: 0, now: ops_time(now) };
        return Ok(Json(view).into_response());
    };
    let board = load_board(&state, &campaign, gw, now, false, "get_ops_board").await?;
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Sends the team to a site, `POST /api/campaign/:campaign_id/ops/start {site_id}` (BP-8a-2,
/// demo-only). Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → axum's body rejection,
/// counted (`413` over `OPS_BODY_LIMIT`; `400`, `415`, `422`) → `404` → `403` → `409` not
/// `Active` (entry dropped) → `409` no gateway → the board, a hit proven by `EXISTS` (a lost
/// sector is repaired: `409` once retired, entry dropped) → `400` unknown site → `409` an op
/// is under way → `503` the store is full of ops → `200`, the whole state.
/// - The leg: `depart_at = now`, `arrive_at = now + the site's travel_secs`.
/// - The op's seed is drawn here from OS entropy and handed to the store, which never sends it.
///   Nothing rolls from it until BP-8b-1.
/// - No launch lock: an op that starts while the reaper retires the campaign is retired with
///   it, and the next ops call answers `409` (an accepted residual, as BP-3c-1b-1's).
async fn post_ops_start(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
    body: Result<Json<OpsStartRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    if let Some(r) = ops_limits(&state, &token, ci, &headers)? {
        return Ok(r);
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return Ok(rej.into_response()),
    };
    let now = Utc::now();
    let (campaign, gateway) = ops_member(&state, &token, campaign_id, "post_ops_start").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no gateway assigned"));
    };
    let board = load_board(&state, &campaign, gw, now, true, "post_ops_start").await?;
    let Some(site) = board.sites.iter().find(|s| s.site_id == req.site_id) else {
        return Err(bad_request("unknown site"));
    };
    let op_seed = generate_seed();
    match state.demo_ops.store.start_op(campaign_id, gw, Arc::clone(&board), site, op_seed, now) {
        Ok(()) => {}
        Err(StartRefusal::Busy) => return Err(conflict("an op is already under way")),
        Err(StartRefusal::Full) => return Err(service_unavailable("the ops board is full")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Brings the team home, `POST /api/campaign/:campaign_id/ops/return` (BP-8a-2, demo-only).
/// No body is read. Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → `404` → `403` →
/// `409` not `Active` (entry dropped) → `409` no op under way (also: no gateway) → the board
/// → `409` still travelling → `409` already heading home → `200`, the whole state.
/// - The homeward leg takes the site's `travel_secs` again (§4d: "each way"), not the
///   gateway's terrain: every gateway is City-State (×0.5), which would halve every trip home.
/// - From a dig site the team comes home empty; BP-8b-2's pull is the loaded return.
async fn post_ops_return(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    if let Some(r) = ops_limits(&state, &token, ci, &headers)? {
        return Ok(r);
    }
    let now = Utc::now();
    let (campaign, gateway) = ops_member(&state, &token, campaign_id, "post_ops_return").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no op under way"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_return").await?;
    match state.demo_ops.store.return_op(campaign_id, now) {
        Ok(()) => {}
        Err(ReturnRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(ReturnRefusal::NotArrived) => return Err(conflict("still travelling")),
        Err(ReturnRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::map_gen::generate_sector_map;
    use crate::rate_limit::{FixedWindow, RATE_LIMIT_MAP_CAP, RATE_LIMIT_WINDOW};
    use crate::repository::{CampaignInstance, HexOccupant, NewCampaignInstance, OccupationStatus, SectorState, WalletAddress};
    use crate::repository::{RepositoryError, SectorId, SectorStateRepository};
    use crate::routes::{with_test_peer, TEST_PEER};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use tower::ServiceExt;

    const WALLET: [u8; 32] = [254u8; 32];
    const OTHER: [u8; 32] = [255u8; 32];
    const GATEWAY: (i32, i32) = (5, 0);

    /// A signed-in session `"t"` for `WALLET`, not yet shared, so a test can swap a store.
    fn session_state() -> AppState {
        let state = AppState::new(Arc::new(Config::default()));
        let wallet = bs58::encode(WALLET).into_string();
        let now = chrono::Utc::now().timestamp();
        state.sessions.insert("t".into(), shared::SessionToken {
            token_id: "t".into(),
            account_id: wallet.clone(),
            wallet_address: wallet,
            issued_at: now,
            expires_at: now + 600,
        });
        state
    }

    async fn get(state: &Arc<AppState>, token: Option<&str>, campaign: Uuid) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method("GET").uri(format!("/api/campaign/{campaign}/ops"));
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = with_test_peer(router().with_state(Arc::clone(state)), TEST_PEER).oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    /// An `Active` campaign (or `Pending`, with `active == false`) with `wallet` as its member.
    async fn campaign_with(state: &AppState, wallet: [u8; 32], active: bool, gateway: Option<(i32, i32)>) -> CampaignInstance {
        let c = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 42 })
            .await
            .expect("create_campaign");
        state.membership_repo
            .add_member(&wallet, c.campaign_id, gateway.map(|g| g.0), gateway.map(|g| g.1))
            .await
            .expect("add_member");
        if active {
            let ends_at = chrono::Utc::now() + chrono::Duration::days(60);
            let activated = state.campaign_repo
                .activate_campaign(c.campaign_id, ends_at, serde_json::json!({}))
                .await
                .expect("activate_campaign");
            assert!(activated, "the campaign was Pending");
        }
        state.campaign_repo.get_campaign(c.campaign_id).await.expect("get_campaign").expect("the campaign exists")
    }

    /// Stores a launch-size sector (radius 35) for `campaign` and returns it.
    async fn seed_sector(state: &AppState, campaign: &CampaignInstance) -> SectorState {
        let map = generate_sector_map(42, 35);
        let sector = SectorState {
            sector_id: campaign.sector_id,
            campaign_id: campaign.campaign_id,
            occupation_status: OccupationStatus::Neutral,
            owner: None,
            deployed_unit_count: 0,
            terrain: map.terrain,
            magma_veins: map.magma_veins,
            hex_occupancy: HashMap::new(),
        };
        state.sector_repo.upsert_sector(sector.clone()).await.expect("upsert_sector");
        sector
    }

    #[tokio::test]
    async fn ops_requires_a_session() {
        let state = Arc::new(session_state());
        let (status, _) = get(&state, None, Uuid::new_v4()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ops_unknown_campaign_is_404() {
        let state = Arc::new(session_state());
        let (status, _) = get(&state, Some("t"), Uuid::new_v4()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The sector is seeded and OTHER has a gateway, so a broken membership check answers `200`.
    #[tokio::test]
    async fn ops_by_a_non_member_is_403() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, OTHER, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(state.demo_ops.store.is_empty());
    }

    #[tokio::test]
    async fn ops_of_a_pending_campaign_is_409() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, false, None).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "campaign is not active");
    }

    /// The first read caches the board; a read after the campaign is retired drops it.
    #[tokio::test]
    async fn ops_of_a_retired_campaign_is_409_and_drops_the_board() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(get(&state, Some("t"), c.campaign_id).await.0, StatusCode::OK);
        assert_eq!(state.demo_ops.store.len(), 1);
        assert!(state.campaign_repo.retire_if_active(c.campaign_id).await.unwrap());
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(state.demo_ops.store.len(), 0);
    }

    /// Exact keys, the board the generator computes, and a second read served from the cache:
    /// the sector is deleted in between, so a read that went back to the sector would answer `409`.
    #[tokio::test]
    async fn ops_returns_the_board_and_serves_it_from_the_cache() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        let stored = seed_sector(&state, &c).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let keys: std::collections::BTreeSet<&str> = body.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["active_op", "campaign_id", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
        assert_eq!(body["campaign_id"], c.campaign_id.to_string());
        assert_eq!(body["gateway"], serde_json::json!({"q": 5, "r": 0}));
        assert!(body["active_op"].is_null());
        assert_eq!(body["ore_banked"], 0);
        let expected = ops_board(&stored.terrain, &stored.magma_veins, GATEWAY, c.campaign_id);
        assert_eq!(body["sites"], serde_json::to_value(&expected.sites).unwrap());
        assert_eq!(body["sites"].as_array().unwrap().iter().filter(|s| s["kind"] == "contract").count(), 4);
        state.sector_repo.delete_sector(c.sector_id).await.unwrap();
        let (again, body2) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(again, StatusCode::OK, "{body2}");
        let strip = |mut v: serde_json::Value| { v.as_object_mut().expect("an object").remove("now"); v };
        assert_eq!(strip(body2), strip(body), "the same board, only the clock moved");
        assert_eq!(state.demo_ops.store.len(), 1);
    }

    #[tokio::test]
    async fn ops_without_a_gateway_has_no_sites() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, None).await;
        seed_sector(&state, &c).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["gateway"].is_null());
        assert_eq!(body["sites"], serde_json::json!([]));
        assert!(state.demo_ops.store.is_empty());
    }

    /// BP-1d: a missing sector goes to the repair, which retires the campaign: `409`.
    #[tokio::test]
    async fn ops_sector_missing_is_retired_409() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let after = state.campaign_repo.get_campaign(c.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(after.state, CampaignLifecycle::Archived);
        assert!(state.demo_ops.store.is_empty());
    }

    struct FailingSectors;

    #[async_trait::async_trait]
    impl SectorStateRepository for FailingSectors {
        async fn get_sector(&self, _: SectorId) -> Result<Option<SectorState>, RepositoryError> {
            Err(RepositoryError::Internal("redis down".into()))
        }
        async fn upsert_sector(&self, _: SectorState) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn list_sectors(&self) -> Result<Vec<SectorState>, RepositoryError> {
            unimplemented!()
        }
        async fn get_player_presence(&self, _: SectorId) -> Result<Vec<WalletAddress>, RepositoryError> {
            unimplemented!()
        }
        async fn set_player_presence(&self, _: SectorId, _: &[WalletAddress]) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn add_hex_occupant(&self, _: SectorId, _: i32, _: i32, _: HexOccupant) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn remove_hex_occupant(&self, _: SectorId, _: i32, _: i32, _: Uuid) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn delete_sector(&self, _: SectorId) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn sector_exists(&self, _: SectorId) -> Result<bool, RepositoryError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn ops_sector_read_failure_is_500() {
        let mut state = session_state();
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        state.sector_repo = Arc::new(FailingSectors);
        let state = Arc::new(state);
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(state.demo_ops.store.is_empty());
    }

    #[tokio::test]
    async fn ops_per_ip_limit() {
        let mut state = session_state();
        state.demo_ops.ip = FixedWindow::new("ops_ip", 2, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP);
        let state = Arc::new(state);
        let mut statuses = Vec::new();
        for _ in 0..3 {
            statuses.push(get(&state, Some("t"), Uuid::new_v4()).await.0);
        }
        assert_eq!(statuses, vec![StatusCode::NOT_FOUND, StatusCode::NOT_FOUND, StatusCode::TOO_MANY_REQUESTS]);
    }

    #[tokio::test]
    async fn ops_per_wallet_limit() {
        let mut state = session_state();
        state.demo_ops.wallet = FixedWindow::new("ops_wallet", 2, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP);
        let state = Arc::new(state);
        let mut statuses = Vec::new();
        for _ in 0..3 {
            statuses.push(get(&state, Some("t"), Uuid::new_v4()).await.0);
        }
        assert_eq!(statuses, vec![StatusCode::NOT_FOUND, StatusCode::NOT_FOUND, StatusCode::TOO_MANY_REQUESTS]);
    }

    async fn post(state: &Arc<AppState>, campaign: Uuid, action: &str, body: Option<&str>) -> (StatusCode, serde_json::Value, Vec<u8>) {
        let mut req = Request::builder()
            .method("POST")
            .uri(format!("/api/campaign/{campaign}/ops/{action}"))
            .header("authorization", "Bearer t");
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        let req = req.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap();
        let resp = with_test_peer(router().with_state(Arc::clone(state)), TEST_PEER).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null), bytes)
    }

    async fn start(state: &Arc<AppState>, campaign: Uuid, site_id: &str) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "start", Some(&serde_json::json!({ "site_id": site_id }).to_string())).await
    }

    const SITE: &str = "contract:GUARD_DUTY";

    fn secs_between(op: &serde_json::Value) -> i64 {
        let t = |k: &str| chrono::DateTime::parse_from_rfc3339(op[k].as_str().expect(k)).expect(k);
        (t("arrive_at") - t("depart_at")).num_seconds()
    }

    fn site_secs(body: &serde_json::Value, site_id: &str) -> i64 {
        let sites = body["sites"].as_array().expect("sites");
        sites.iter().find(|s| s["site_id"] == site_id).expect("the site is on the board")["travel_secs"].as_i64().unwrap()
    }

    /// A started op: the whole state, the op's exact keys (no seed anywhere in the body), and a
    /// leg as long as the site's `travel_secs`.
    #[tokio::test]
    async fn ops_start_returns_the_whole_state_without_the_seed() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, bytes) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let keys: std::collections::BTreeSet<&str> = body.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["active_op", "campaign_id", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
        let op = &body["active_op"];
        let op_keys: std::collections::BTreeSet<&str> = op.as_object().expect("an op").keys().map(|k| k.as_str()).collect();
        assert_eq!(op_keys, ["arrive_at", "arrived", "depart_at", "kind", "leg", "site_id"].into_iter().collect());
        assert_eq!((op["leg"].as_str(), op["site_id"].as_str(), op["kind"].as_str()), (Some("outbound"), Some(SITE), Some("contract")));
        assert_eq!(op["arrived"], false);
        assert_eq!(op["depart_at"], body["now"]);
        assert_eq!(secs_between(op), site_secs(&body, SITE));
        assert!(!String::from_utf8_lossy(&bytes).contains("seed"), "no seed leaves the server");
    }

    #[tokio::test]
    async fn ops_start_unknown_site_is_400() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = start(&state, c.campaign_id, "dig:99,99").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "unknown site");
        assert!(state.demo_ops.store.op_view(c.campaign_id, chrono::Utc::now()).is_none());
    }

    #[tokio::test]
    async fn ops_start_while_an_op_is_out_is_409() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(start(&state, c.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = start(&state, c.campaign_id, "contract:SUPPLY_RAID").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "an op is already under way");
        assert_eq!(state.demo_ops.store.op_view(c.campaign_id, chrono::Utc::now()).expect("op").site_id, SITE);
    }

    /// Axum's rejections, answered after the limiters and before the campaign is read (the id
    /// is unknown, so anything past the body would be `404`).
    #[tokio::test]
    async fn ops_start_body_rejections() {
        let state = Arc::new(session_state());
        let c = Uuid::new_v4();
        let big = format!("{{\"site_id\":\"{}\"}}", "x".repeat(2048));
        assert_eq!(post(&state, c, "start", Some(&big)).await.0, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(post(&state, c, "start", Some("{")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(post(&state, c, "start", None).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(post(&state, c, "start", Some("{}")).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn ops_start_of_a_retired_campaign_is_409_and_drops_the_entry() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(get(&state, Some("t"), c.campaign_id).await.0, StatusCode::OK);
        assert!(state.campaign_repo.retire_if_active(c.campaign_id).await.unwrap());
        let (status, body, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "campaign is not active");
        assert!(state.demo_ops.store.is_empty());
    }

    #[tokio::test]
    async fn ops_start_without_a_gateway_is_409() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, None).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "no gateway assigned");
        assert!(state.demo_ops.store.is_empty());
    }

    /// Cap 1, held by the first campaign's op: the second campaign's start is `503`.
    #[tokio::test]
    async fn ops_start_with_the_store_full_of_ops_is_503() {
        let mut state = session_state();
        state.demo_ops.store = crate::demo_ops::DemoOpsStore::new(1);
        let state = Arc::new(state);
        let first = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &first).await;
        let second = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &second).await;
        assert_eq!(start(&state, first.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = start(&state, second.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["error"], "the ops board is full");
        assert_eq!(state.demo_ops.store.len(), 1);
        assert!(state.demo_ops.store.op_view(first.campaign_id, chrono::Utc::now()).is_some());
    }

    /// Reviewer BP-8a-1 F2: the board is cached, then the sector is lost. The start proves the
    /// sector, the repair retires the campaign, and the entry goes with it.
    #[tokio::test]
    async fn ops_start_on_a_lost_sector_retires_409() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(get(&state, Some("t"), c.campaign_id).await.0, StatusCode::OK);
        state.sector_repo.delete_sector(c.sector_id).await.unwrap();
        let (status, body, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let after = state.campaign_repo.get_campaign(c.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(after.state, CampaignLifecycle::Archived);
        assert!(state.demo_ops.store.is_empty());
    }

    #[tokio::test]
    async fn ops_return_refusals_then_the_homeward_leg() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = post(&state, c.campaign_id, "return", None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));
        let (status, started, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let (status, body, _) = post(&state, c.campaign_id, "return", None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("still travelling")));
        state.demo_ops.store.rewind_op(c.campaign_id, site_secs(&started, SITE));
        let (status, home, _) = post(&state, c.campaign_id, "return", None).await;
        assert_eq!(status, StatusCode::OK, "{home}");
        assert_eq!(home["active_op"]["leg"], "homeward");
        assert_eq!(home["active_op"]["arrived"], false);
        assert_eq!(secs_between(&home["active_op"]), site_secs(&started, SITE), "each way takes the site's time");
        let (status, body, _) = post(&state, c.campaign_id, "return", None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
    }

    /// CF1 through the route: an op is out, the member's gateway changes, so the next read
    /// misses the cache and writes a new board. The op survives the write.
    #[tokio::test]
    async fn ops_board_miss_keeps_the_op() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(get(&state, Some("t"), c.campaign_id).await.0, StatusCode::OK);
        assert_eq!(start(&state, c.campaign_id, SITE).await.0, StatusCode::OK);
        state.membership_repo.assign_gateway_hex(&WALLET, c.campaign_id, 0, 5).await.unwrap();
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["gateway"], serde_json::json!({"q": 0, "r": 5}), "the read missed and recomputed");
        assert_eq!(body["active_op"]["site_id"], SITE, "the op survives the board write");
    }

    #[tokio::test]
    async fn ops_homeward_arrival_frees_a_new_start() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (_, started, _) = start(&state, c.campaign_id, SITE).await;
        let secs = site_secs(&started, SITE);
        state.demo_ops.store.rewind_op(c.campaign_id, secs);
        assert_eq!(post(&state, c.campaign_id, "return", None).await.0, StatusCode::OK);
        state.demo_ops.store.rewind_op(c.campaign_id, secs);
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: the op is cleared");
        let (status, body, _) = start(&state, c.campaign_id, "contract:SUPPLY_RAID").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["site_id"], "contract:SUPPLY_RAID");
    }

    /// One `ops_wallet` budget for every ops route: a board read and a start spend it, so the
    /// return is the third call and is refused.
    #[tokio::test]
    async fn ops_posts_share_the_wallet_budget() {
        let mut state = session_state();
        state.demo_ops.wallet = FixedWindow::new("ops_wallet", 2, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP);
        let state = Arc::new(state);
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(get(&state, Some("t"), c.campaign_id).await.0, StatusCode::OK);
        assert_eq!(start(&state, c.campaign_id, SITE).await.0, StatusCode::OK);
        assert_eq!(post(&state, c.campaign_id, "return", None).await.0, StatusCode::TOO_MANY_REQUESTS);
    }
}
