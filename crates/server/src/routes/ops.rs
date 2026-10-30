//! The ops board route (BP-8a-1, **demo-only**; Breakpoint plan §4d). The `dev` port deletes
//! this file with `demo_ops.rs`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::demo_ops::{ops_board, OpsSite};
use crate::repository::CampaignLifecycle;
use crate::state::AppState;

use super::sector::GatewayHex;
use super::{client_ip, conflict, forbidden, internal_error, not_found, too_many_requests, unauthorized, AppError};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/campaign/:campaign_id/ops", get(get_ops_board))
}

/// The board as the client sees it. `active_op` is always `null` and `ore_banked` always 0
/// until BP-8a-2 and BP-8b-2 write them.
#[derive(Serialize)]
pub struct OpsView<'a> {
    pub campaign_id: Uuid,
    pub gateway: Option<GatewayHex>,
    pub sites: &'a [OpsSite],
    pub active_op: Option<serde_json::Value>,
    pub ore_banked: u32,
}

/// A member's ops board, `GET /api/campaign/:campaign_id/ops` (BP-8a-1, demo-only). Order:
/// `401` (no session) → `429` per address (`ops_ip`) → `429` per wallet (`ops_wallet`) →
/// `404` (unknown campaign) → `403` (not a member) → `409` (not `Active`; the campaign's
/// cached entry is dropped) → the board.
/// - `gateway: null` (not assigned) → `sites: []`, nothing cached.
/// - The board comes from the store; a miss reads the sector once, computes and caches it.
///   A missing sector goes to `repair_missing_sector` as the sector route does (`409` once
///   retired). A failed read is `500`.
/// - A retire that lands between the state check and the insert leaves an entry behind; the
///   cap bounds it, and the campaign's next read drops it.
async fn get_ops_board(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.demo_ops.ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.demo_ops.wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let wallet: Vec<u8> = bs58::decode(&token.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;

    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(not_found("campaign not found")),
        Err(e) => {
            return Err(internal_error(format!("get_ops_board: get_campaign failed for campaign {campaign_id}: {e}")));
        }
    };
    let membership = match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(m) => m,
        Err(e) => {
            return Err(internal_error(format!("get_ops_board: membership read failed for campaign {campaign_id}: {e}")));
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
    let Some(gw) = gateway else {
        return Ok(Json(OpsView { campaign_id, gateway: None, sites: &[], active_op: None, ore_banked: 0 }).into_response());
    };
    let board = match state.demo_ops.store.board(campaign_id, gw, now) {
        Some(b) => b,
        None => {
            let sector_id = campaign.sector_id;
            let sector = match state.sector_repo.get_sector(sector_id).await {
                Ok(Some(s)) => s,
                Ok(None) => super::campaign::repair_missing_sector(Arc::clone(&state), campaign_id, sector_id).await?,
                Err(e) => {
                    return Err(internal_error(format!("get_ops_board: get_sector failed for sector {sector_id} of campaign {campaign_id}: {e}")));
                }
            };
            let board = Arc::new(ops_board(&sector.terrain, &sector.magma_veins, gw, campaign_id));
            state.demo_ops.store.insert(campaign_id, gw, Arc::clone(&board), now);
            board
        }
    };
    let view = OpsView {
        campaign_id,
        gateway: Some(GatewayHex { q: gw.0, r: gw.1 }),
        sites: &board.sites,
        active_op: None,
        ore_banked: 0,
    };
    Ok(Json(view).into_response())
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
        assert_eq!(keys, ["active_op", "campaign_id", "gateway", "ore_banked", "sites"].into_iter().collect());
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
        assert_eq!(body2, body);
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
}
