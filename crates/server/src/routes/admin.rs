use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::campaign_init::{initialize_campaign, InitError};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/admin/campaign/:campaign_id/launch", post(launch_campaign))
}

async fn launch_campaign(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
) -> impl IntoResponse {
    if let Ok(expected_key) = std::env::var("ADMIN_API_KEY") {
        let provided = headers
            .get("x-admin-key")
            .and_then(|v| v.to_str().ok());
        if provided != Some(expected_key.as_str()) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
        }
    }

    // If initialization fails mid-flight after activation (e.g. Redis upsert fails),
    // the campaign may be left in Active state. Recovery requires a manual DB reset:
    // `UPDATE campaign_instances SET state = 'Pending' WHERE campaign_id = '...'`
    match initialize_campaign(
        campaign_id,
        state.campaign_repo.as_ref(),
        state.membership_repo.as_ref(),
        state.sector_repo.as_ref(),
        state.input_log_repo.as_ref(),
    )
    .await
    {
        Ok(()) => match state.campaign_repo.get_campaign(campaign_id).await {
            Ok(Some(c)) => (
                StatusCode::OK,
                Json(json!({
                    "campaign_id": c.campaign_id.to_string(),
                    "state":       c.state,
                    "ends_at":     c.ends_at.map(|t| t.to_rfc3339()),
                })),
            )
                .into_response(),
            Ok(None) => {
                tracing::error!(
                    "launch_campaign internal error: campaign {} not found after successful init",
                    campaign_id
                );
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "internal server error"}))).into_response()
            }
            Err(e) => {
                tracing::error!("launch_campaign internal error: get_campaign failed after init: {:?}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "internal server error"}))).into_response()
            }
        },
        Err(InitError::NotPending) | Err(InitError::WasAlreadyActive) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "campaign is not in Pending state"})),
        )
            .into_response(),
        Err(InitError::CampaignNotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "campaign not found"}))).into_response()
        }
        Err(e) => {
            tracing::error!("launch_campaign internal error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "internal server error"}))).into_response()
        }
    }
}
