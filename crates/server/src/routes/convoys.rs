use std::sync::Arc;
use axum::{
    Router,
    routing::post,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::map_gen::hex_axial_distance;
use crate::repository::{
    ConvoyDbRecord, DeploymentTimer, HexOccupant, TimerType, WalletAddress,
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

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/:campaign_id/convoy/dispatch", post(dispatch_convoy))
}

async fn dispatch_convoy(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
    Json(req): Json<DispatchConvoyRequest>,
) -> Response {
    // TODO(phase-2): validate He3 balance before dispatch

    // Decode wallet: base58 string → Vec<u8> for DB storage, [u8; 32] for timer
    let owner_wallet: Vec<u8> = match bs58::decode(&token.wallet_address).into_vec() {
        Ok(v) => v,
        Err(_) => return super::unauthorized("session wallet is not valid base58").into_response(),
    };
    let wallet_bytes: [u8; 32] = match owner_wallet.as_slice().try_into() {
        Ok(b) => b,
        Err(_) => return super::unauthorized("session wallet is not a 32-byte key").into_response(),
    };

    // Look up campaign → derive sector_id
    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None)    => return StatusCode::NOT_FOUND.into_response(),
        Err(e)      => {
            tracing::error!("dispatch_convoy: get_campaign failed for campaign {campaign_id}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let sector_id = campaign.sector_id;

    // Terrain modifier — destination hex only (Phase 1 simplification)
    // TODO(phase-2): use full route waypoints for terrain modifier
    // A sector read error falls back to modifier 1.0, as before; it is logged, not refused.
    let sector = match state.sector_repo.get_sector(sector_id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("dispatch_convoy: get_sector failed for campaign {campaign_id} (sector {sector_id}); using terrain modifier 1.0: {e:?}");
            None
        }
    };
    let terrain_modifier = sector
        .and_then(|s| s.terrain.get(&format!("{},{}", req.destination_q, req.destination_r)).cloned())
        .map(|t| t.travel_time_modifier())
        .unwrap_or(1.0_f64);

    // TODO(phase-1): reject route if destination hex is impassable
    if terrain_modifier == f64::INFINITY {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "destination hex is impassable"}))).into_response();
    }
    // A zero-distance timer fires at once and could race the dispatch commit into the
    // phantom path, so it is refused before any write (GAP-44, W3).
    if (req.origin_q, req.origin_r) == (req.destination_q, req.destination_r) {
        return super::bad_request("origin and destination are the same hex").into_response();
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
        tracing::error!("dispatch_convoy: schedule_timer failed for campaign {campaign_id}, convoy {convoy_id}: {e:?}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
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
        CampaignInstance, CampaignLifecycle, CampaignRepository, ConvoyRepository,
        NewCampaignInstance, OccupationStatus, PostgresConvoyRepository, RepositoryError,
        SectorState, VictoryTickerType,
    };
    use axum::body::Body;
    use axum::http::Request;
    use chrono::DateTime;
    use tower::ServiceExt;

    /// Answers `get_campaign` with one fixed campaign; nothing else is reached by dispatch.
    struct OneCampaign(CampaignInstance);

    #[async_trait::async_trait]
    impl CampaignRepository for OneCampaign {
        async fn create_campaign(&self, _: &NewCampaignInstance) -> Result<CampaignInstance, RepositoryError> {
            unimplemented!()
        }
        async fn get_campaign(&self, id: Uuid) -> Result<Option<CampaignInstance>, RepositoryError> {
            Ok((id == self.0.campaign_id).then(|| self.0.clone()))
        }
        async fn update_sector_state(&self, _: Uuid, _: CampaignLifecycle) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn apply_ticker_delta(&self, _: Uuid, _: VictoryTickerType, _: f64) -> Result<f64, RepositoryError> {
            unimplemented!()
        }
        async fn list_campaigns_by_state(&self, _: CampaignLifecycle) -> Result<Vec<CampaignInstance>, RepositoryError> {
            unimplemented!()
        }
        async fn activate_campaign(&self, _: Uuid, _: DateTime<Utc>, _: serde_json::Value) -> Result<bool, RepositoryError> {
            unimplemented!()
        }
    }

    /// State with one campaign, a signed-in session and in-memory timers; the default
    /// convoy repository is the in-memory stub, which panics if any convoy write is tried.
    fn dispatch_state() -> (AppState, Uuid, Uuid) {
        let campaign_id = Uuid::new_v4();
        let sector_id = Uuid::new_v4();
        let mut state = AppState::new(Arc::new(Config::default()));
        state.campaign_repo = Arc::new(OneCampaign(CampaignInstance {
            campaign_id,
            sector_id,
            map_seed: 1,
            state: CampaignLifecycle::Active,
            victory_tickers: serde_json::json!({}),
            started_at: None,
            ends_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }));
        let wallet = bs58::encode([5u8; 32]).into_string();
        let now = Utc::now().timestamp();
        state.sessions.insert("t".into(), shared::SessionToken {
            token_id: "t".into(),
            account_id: wallet.clone(),
            wallet_address: wallet,
            issued_at: now,
            expires_at: now + 600,
        });
        (state, campaign_id, sector_id)
    }

    async fn dispatch(state: Arc<AppState>, campaign_id: Uuid, to: (i32, i32)) -> StatusCode {
        let body = serde_json::json!({
            "origin_q": 0, "origin_r": 0, "destination_q": to.0, "destination_r": to.1,
            "vehicle_class": "BehemothMegaTruck", "cargo": {}
        });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/campaign/{campaign_id}/convoy/dispatch"))
            .header("content-type", "application/json")
            .header("authorization", "Bearer t")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        router().with_state(state).oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn dispatch_db_failure_keeps_the_timer_for_the_phantom_path() {
        // A closed lazy pool fails every query without a database (INF-3a pattern).
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .expect("lazy pool");
        pool.close().await;
        let (mut state, campaign_id, sector_id) = dispatch_state();
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
    }

    #[tokio::test]
    async fn failed_dispatch_timer_is_cancelled_by_the_phantom_path() {
        let (mut state, campaign_id, _) = dispatch_state();
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
        let (mut state, campaign_id, _) = dispatch_state();
        let timers = Arc::new(crate::repository::InMemoryTimerRepository::new());
        state.timer_repo = timers.clone();

        let status = dispatch(Arc::new(state), campaign_id, (0, 0)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(timers.0.is_empty(), "a refused dispatch schedules nothing");
    }
}
