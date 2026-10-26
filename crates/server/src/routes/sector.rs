use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::map_gen::{HexTerrain, MagmaVeinNode};
use crate::repository::CampaignLifecycle;
use crate::state::AppState;

use super::{conflict, forbidden, internal_error, not_found, unauthorized, AppError};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/campaign/:campaign_id/sector", get(get_campaign_sector))
}

/// The sector as a member may see it (BP-2a). An allowlist: `SectorState`'s `owner`,
/// `occupation_status`, `deployed_unit_count` and `hex_occupancy` are never copied here,
/// and a field added to the stored sector stays out until it is added to this struct.
#[derive(Serialize)]
pub struct SectorView {
    pub campaign_id: Uuid,
    pub sector_id: Uuid,
    pub terrain: HashMap<String, HexTerrain>,
    pub magma_veins: Vec<MagmaVeinNode>,
    pub gateway: Option<GatewayHex>,
}

/// The caller's own gateway hex, from their membership row.
#[derive(Serialize)]
pub struct GatewayHex {
    pub q: i32,
    pub r: i32,
}

/// A member's sector map, `GET /api/campaign/:campaign_id/sector` (BP-2a, keeper). Order:
/// `401` (no session) → `404` (unknown campaign) → `403` (not a member; any membership
/// row) → `409` (not `Active` or `Ending`, as the events socket) → the sector read. A
/// missing sector or a failed read is `500`.
/// - **Pre-fog:** Phase 1 stubs fog of war as full visibility, so every terrain hex and
///   every vein is returned. Phase 2's per-player vision filters inside this projection.
/// - `gateway` is `null` until the launch has assigned it, or if it never did.
/// - Read-only, not behind the join switch, and not rate-limited: one call costs ~0.9 ms
///   in release (measured at the BP-2a spec check).
async fn get_campaign_sector(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let wallet: Vec<u8> = bs58::decode(&token.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;

    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(not_found("campaign not found")),
        Err(e) => {
            return Err(internal_error(format!("get_campaign_sector: get_campaign failed for campaign {campaign_id}: {e}")));
        }
    };
    let membership = match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(m) => m,
        Err(e) => {
            return Err(internal_error(format!("get_campaign_sector: membership read failed for campaign {campaign_id}: {e}")));
        }
    };
    if membership.is_none() {
        return Err(forbidden("not a member of this campaign"));
    }
    let admitted = matches!(campaign.state, CampaignLifecycle::Active | CampaignLifecycle::Ending);
    if !admitted {
        return Err(conflict("campaign is not active"));
    }
    let sector_id = campaign.sector_id;
    let sector = match state.sector_repo.get_sector(sector_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err(internal_error(format!("get_campaign_sector: sector {sector_id} of campaign {campaign_id} is missing")));
        }
        Err(e) => {
            return Err(internal_error(format!("get_campaign_sector: get_sector failed for sector {sector_id} of campaign {campaign_id}: {e}")));
        }
    };
    let gateway = membership.and_then(|m| Some(GatewayHex { q: m.gateway_hex_q?, r: m.gateway_hex_r? }));
    let view = SectorView {
        campaign_id,
        sector_id,
        terrain: sector.terrain,
        magma_veins: sector.magma_veins,
        gateway,
    };
    Ok(Json(view).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::map_gen::generate_sector_map;
    use crate::repository::{CampaignInstance, HexOccupant, NewCampaignInstance, OccupationStatus, SectorState, WalletAddress};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const WALLET: [u8; 32] = [83u8; 32];
    const OTHER: [u8; 32] = [84u8; 32];

    /// A signed-in session `"t"` for `WALLET`.
    fn signed_in() -> Arc<AppState> {
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
        Arc::new(state)
    }

    async fn get(state: &Arc<AppState>, token: Option<&str>, campaign: Uuid) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method("GET").uri(format!("/api/campaign/{campaign}/sector"));
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = router().with_state(Arc::clone(state)).oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    /// A campaign in `lifecycle` with `wallet` as its member at `gateway`. `Pending` is left as
    /// created; `Active` is activated; `Ending` is activated, then moved on.
    async fn campaign_with(
        state: &AppState,
        wallet: [u8; 32],
        lifecycle: CampaignLifecycle,
        gateway: Option<(i32, i32)>,
    ) -> CampaignInstance {
        let c = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 42 })
            .await
            .expect("create_campaign");
        state.membership_repo
            .add_member(&wallet, c.campaign_id, gateway.map(|g| g.0), gateway.map(|g| g.1))
            .await
            .expect("add_member");
        if lifecycle != CampaignLifecycle::Pending {
            let ends_at = chrono::Utc::now() + chrono::Duration::days(60);
            let activated = state.campaign_repo
                .activate_campaign(c.campaign_id, ends_at, serde_json::json!({}))
                .await
                .expect("activate_campaign");
            assert!(activated, "the campaign was Pending");
        }
        if lifecycle == CampaignLifecycle::Ending {
            state.campaign_repo
                .update_sector_state(c.campaign_id, CampaignLifecycle::Ending)
                .await
                .expect("update_sector_state");
        }
        state.campaign_repo.get_campaign(c.campaign_id).await.expect("get_campaign").expect("the campaign exists")
    }

    /// Stores a launch-size sector (radius 35) for `campaign` and returns it. With
    /// `other_wallet_data`, the four fields the view drops are filled with `OTHER`'s data.
    async fn seed_sector(state: &AppState, campaign: &CampaignInstance, other_wallet_data: bool) -> SectorState {
        let map = generate_sector_map(42, 35);
        let mut sector = SectorState {
            sector_id: campaign.sector_id,
            campaign_id: campaign.campaign_id,
            occupation_status: OccupationStatus::Neutral,
            owner: None,
            deployed_unit_count: 0,
            terrain: map.terrain,
            magma_veins: map.magma_veins,
            hex_occupancy: HashMap::new(),
        };
        if other_wallet_data {
            sector.occupation_status = OccupationStatus::Controlled;
            sector.owner = Some(WalletAddress(OTHER));
            sector.deployed_unit_count = 3;
            sector.hex_occupancy.insert("0,0".into(), vec![HexOccupant {
                id: Uuid::new_v4(),
                owner_wallet: OTHER.to_vec(),
                unit_type: "Section".into(),
            }]);
        }
        state.sector_repo.upsert_sector(sector.clone()).await.expect("upsert_sector");
        sector
    }

    /// Exhaustive on purpose (no `_` arm): a new `HexTerrain` variant stops this module
    /// compiling, and so does a variant that gains data, since it no longer matches a unit
    /// pattern. Then the allowlist is re-checked one level down.
    fn unit_only(t: &HexTerrain) -> bool {
        match t {
            HexTerrain::AshenPlains | HexTerrain::ScorchedDesert | HexTerrain::RuinedCity
            | HexTerrain::CityState | HexTerrain::GunOutpost | HexTerrain::MagmaFlow
            | HexTerrain::CooledMagma | HexTerrain::VolcanicCaldera | HexTerrain::Mountain
            | HexTerrain::Ravine | HexTerrain::ToxicZone => true,
        }
    }

    #[tokio::test]
    async fn sector_requires_a_session() {
        let state = signed_in();
        let (status, _) = get(&state, None, Uuid::new_v4()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sector_unknown_campaign_is_404() {
        let state = signed_in();
        let (status, _) = get(&state, Some("t"), Uuid::new_v4()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The sector is seeded, so a broken membership check would answer `200` with the map.
    #[tokio::test]
    async fn sector_by_a_non_member_is_403() {
        let state = signed_in();
        let c = campaign_with(&state, OTHER, CampaignLifecycle::Active, Some((1, -2))).await;
        seed_sector(&state, &c, false).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn sector_of_a_pending_campaign_is_409() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Pending, None).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "campaign is not active");
    }

    /// The second admitted state, and the `null` gateway (a member added with none).
    #[tokio::test]
    async fn sector_of_an_ending_campaign_is_200() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Ending, None).await;
        seed_sector(&state, &c, false).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["gateway"].is_null(), "{}", body["gateway"]);
    }

    /// Reads counts and the gateway only, never `sector_id`.
    #[tokio::test]
    async fn sector_returns_terrain_veins_and_the_callers_gateway() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Active, Some((3, -5))).await;
        let stored = seed_sector(&state, &c, false).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["campaign_id"], c.campaign_id.to_string());
        assert_eq!(body["terrain"].as_object().expect("terrain is an object").len(), stored.terrain.len());
        assert_eq!(body["magma_veins"].as_array().expect("veins are an array").len(), stored.magma_veins.len());
        assert_eq!(body["gateway"], serde_json::json!({"q": 3, "r": -5}));
    }

    /// The allowlist: exact key sets at every level, and none of `OTHER`'s data anywhere.
    /// Checks the gateway's keys, never its values.
    #[tokio::test]
    async fn sector_view_has_exactly_the_allowlisted_keys() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Active, Some((3, -5))).await;
        let stored = seed_sector(&state, &c, true).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK);
        let keys = |v: &serde_json::Value| -> std::collections::BTreeSet<String> {
            v.as_object().expect("an object").keys().cloned().collect()
        };
        let set = |ks: &[&str]| -> std::collections::BTreeSet<String> { ks.iter().map(|k| k.to_string()).collect() };
        assert_eq!(keys(&body), set(&["campaign_id", "sector_id", "terrain", "magma_veins", "gateway"]));
        let veins = body["magma_veins"].as_array().expect("veins are an array");
        assert!(!veins.is_empty(), "the seeded map has veins");
        for v in veins {
            assert_eq!(keys(v), set(&["q", "r", "tier", "is_tectonic_anchor"]));
        }
        assert_eq!(keys(&body["gateway"]), set(&["q", "r"]));
        assert!(body["terrain"].as_object().expect("terrain is an object").values().all(|t| t.is_string()));
        assert!(stored.terrain.values().all(unit_only));
        let other_b58 = bs58::encode(OTHER).into_string();
        let other_array = serde_json::to_string(&OTHER.to_vec()).unwrap();
        let stored_text = serde_json::to_string(&stored).unwrap();
        assert!(stored_text.contains(&other_array), "control: the stored sector carries OTHER's array form");
        let text = body.to_string();
        assert!(!text.contains(&other_b58), "OTHER's address (base58) leaked");
        assert!(!text.contains(&other_array), "OTHER's address (byte array) leaked");
    }

    /// An admitted campaign whose sector was never stored.
    #[tokio::test]
    async fn sector_missing_from_the_store_is_500() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Active, Some((3, -5))).await;
        let (status, _) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
