use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::campaign_init::{initialize_campaign, InitError};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/admin/campaign/:campaign_id/launch", post(launch_campaign))
}

/// Admin auth check. FAIL CLOSED: `expected` being `None` (ADMIN_API_KEY unset) or
/// empty denies the request. The check never skips — not in production, not in dev.
///
/// Canon: Phase_1_Section_2_Campaign_State_Machine.md, brick 2b-5 —
/// "401 if wrong; 401 if the var is unset — the check never skips."
///
/// `expected` is passed in rather than read here so this is a pure function and its
/// tests do not have to mutate process-global environment state.
fn admin_key_matches(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let expected = match expected {
        Some(k) if !k.is_empty() => k,
        _ => return false,
    };
    headers
        .get("x-admin-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|provided| {
            // ODQ-20: compare fixed-length digests with a XOR-fold, so the time taken
            // depends on neither the key's length nor the first differing byte.
            let a = Sha256::digest(provided.as_bytes());
            let b = Sha256::digest(expected.as_bytes());
            a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
        })
}

async fn launch_campaign(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
) -> impl IntoResponse {
    let expected_key = std::env::var("ADMIN_API_KEY").ok();
    if expected_key.as_deref().is_none_or(str::is_empty) {
        tracing::error!(
            "ADMIN_API_KEY is unset or empty — denying admin request. \
             Set it in the service environment; the check does not skip."
        );
    }
    if !admin_key_matches(&headers, expected_key.as_deref()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
    }

    // If initialization fails after activation (e.g. the Redis upsert), the campaign is left
    // Active and uninitialised. Recovery: retire it, never reset it to Pending (a Pending
    // campaign is open to joins, so a relaunch can meet a second member; 5a-3b):
    //   UPDATE campaign_instances SET state = 'Archived', updated_at = now()
    //    WHERE campaign_id = '...' AND state = 'Active';
    // Its members' next join places them in a new campaign (S1 counts only Pending, Active
    // and Ending); their starters in the retired campaign stay behind.
    match initialize_campaign(
        campaign_id,
        &*state.campaign_join_repo,
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
            Ok(None) => super::internal_error(format!(
                "launch_campaign internal error: campaign {} not found after successful init",
                campaign_id
            ))
            .into_response(),
            Err(e) => super::internal_error(format!(
                "launch_campaign internal error: get_campaign failed after init: {:?}",
                e
            ))
            .into_response(),
        },
        Err(InitError::NotPending) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "campaign is not in Pending state"})),
        )
            .into_response(),
        Err(InitError::TooManyMembers(_)) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "campaign has more than one member"})),
        )
            .into_response(),
        Err(InitError::CampaignNotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "campaign not found"}))).into_response()
        }
        Err(e) => super::internal_error(format!("launch_campaign internal error: {:?}", e)).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::admin_key_matches;
    use axum::http::HeaderMap;

    fn headers_with(key: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-admin-key", key.parse().unwrap());
        h
    }

    #[test]
    fn unset_key_denies_even_with_no_header() {
        assert!(!admin_key_matches(&HeaderMap::new(), None));
    }

    #[test]
    fn unset_key_denies_even_when_a_header_is_supplied() {
        // The old fail-open bug: no ADMIN_API_KEY meant the check was skipped entirely.
        assert!(!admin_key_matches(&headers_with("anything-at-all"), None));
    }

    #[test]
    fn empty_key_denies() {
        // ADMIN_API_KEY= would otherwise let a client in with an empty header.
        assert!(!admin_key_matches(&headers_with(""), Some("")));
        assert!(!admin_key_matches(&HeaderMap::new(), Some("")));
    }

    #[test]
    fn set_key_with_missing_header_denies() {
        assert!(!admin_key_matches(&HeaderMap::new(), Some("correct-key")));
    }

    #[test]
    fn set_key_with_wrong_header_denies() {
        assert!(!admin_key_matches(&headers_with("wrong-key"), Some("correct-key")));
    }

    #[test]
    fn set_key_with_correct_header_allows() {
        assert!(admin_key_matches(&headers_with("correct-key"), Some("correct-key")));
    }
}
