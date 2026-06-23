use std::sync::Arc;
use axum::{
    Router,
    routing::post,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::map_gen::hex_axial_distance;
use crate::repository::{
    ConvoyDbRecord, DeploymentTimer, HexOccupant, TimerType, WalletAddress,
};
use crate::state::AppState;

#[derive(Deserialize)]
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
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let wallet_bytes: [u8; 32] = match owner_wallet.as_slice().try_into() {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    // Look up campaign → derive sector_id
    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None)    => return StatusCode::NOT_FOUND.into_response(),
        Err(_)      => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let sector_id = campaign.sector_id;

    // Terrain modifier — destination hex only (Phase 1 simplification)
    // TODO(phase-2): use full route waypoints for terrain modifier
    let terrain_modifier = state.sector_repo
        .get_sector(sector_id)
        .await
        .ok()
        .flatten()
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
    let arrival_time = Utc::now() + Duration::minutes(travel_mins as i64);

    // Generate convoy_id — INVARIANT: this is also used as timer_id (see schedule_timer below)
    let convoy_id = Uuid::new_v4();

    // Build straight-line route for Phase 1 (full pathfinding is Phase 2)
    let route = serde_json::json!([
        {"q": req.origin_q,      "r": req.origin_r},
        {"q": req.destination_q, "r": req.destination_r}
    ]);

    // Persist convoy record
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
    if let Err(_) = state.convoy_repo.create_convoy(record).await {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Schedule arrival timer.
    // INVARIANT: timer_id == convoy_id so poll_expiry_once (3c-1) can call
    // mark_arrived(timer.timer_id) without a separate convoy lookup field.
    let timer = DeploymentTimer {
        timer_id:      convoy_id, // ← load-bearing: timer_id IS the convoy_id
        player_wallet: WalletAddress(wallet_bytes),
        sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      arrival_time,
    };
    if let Err(_) = state.timer_repo.schedule_timer(timer).await {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Register convoy as a hex occupant at the origin hex (in-transit marker)
    let occupant = HexOccupant {
        id:           convoy_id,
        owner_wallet: owner_wallet.clone(),
        unit_type:    req.vehicle_class.clone(),
    };
    if let Err(_) = state.sector_repo.add_hex_occupant(sector_id, req.origin_q, req.origin_r, occupant).await {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Append input log entry
    let entry = shared::InputLogEntry {
        tick:            0,
        seq:             0,
        event_type:      "convoy_dispatched".to_string(),
        player_id:       Some(token.wallet_address.clone()),
        payload:         serde_json::json!({ "convoy_id": convoy_id }),
        narrative_event: None,
    };
    if let Err(_) = state.input_log_repo.append_campaign_entry(&campaign_id, &entry).await {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (StatusCode::CREATED, Json(serde_json::json!({ "convoy_id": convoy_id }))).into_response()
}
