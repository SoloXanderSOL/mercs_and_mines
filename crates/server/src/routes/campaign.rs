use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::json;

use sim_engine::rng::generate_seed;

use crate::auth::AuthSession;
use crate::repository::{JoinOutcome, JoinSeeds, StarterIds, SECTOR_PLAYER_CAP};
use crate::state::AppState;

use super::{internal_error, service_unavailable, unauthorized, AppError};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/campaign/join", post(post_campaign_join))
}

/// The response body. Built here so the repository's `StarterIds` never becomes the API
/// contract by accident.
fn starter_ids_body(ids: &StarterIds) -> serde_json::Value {
    json!({
        "campaign_id":  ids.campaign_id,
        "commander_id": ids.commander_id,
        "foreman_id":   ids.foreman_id,
        "section_id":   ids.section_id,
        "crew_id":      ids.crew_id,
    })
}

/// A player enters a campaign (5a-3b): `201` with the new starter set, or `200` with the
/// existing one when the wallet already has a live membership (S1/S7).
///
/// - The account precondition holds: `post_auth_verify` upserts the account before
///   `issue_session`, so a `23503` from the join is a backstop `500`, never a `404`.
/// - Every repository error is a `500`. The text is never matched.
/// - The join is not a logged input: the starters' generator rolls are server decisions
///   stored in their records.
/// - While the join switch is off (5a-3c), every signed-in caller gets `503` before any
///   database read or the join lock.
async fn post_campaign_join(
    AuthSession(session): AuthSession,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.server.join_enabled {
        return Err(service_unavailable("campaign joins are closed"));
    }

    let wallet: Vec<u8> = bs58::decode(&session.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;

    let seeds = JoinSeeds {
        commander: generate_seed(),
        foreman:   generate_seed(),
        map:       generate_seed() as i64,
    };
    let outcome = state
        .campaign_join_repo
        .join_campaign(&wallet, SECTOR_PLAYER_CAP, seeds)
        .await
        .map_err(|e| internal_error(format!("campaign join failed: {e}")))?;

    Ok(match outcome {
        JoinOutcome::Joined(ids) => (StatusCode::CREATED, Json(starter_ids_body(&ids))),
        JoinOutcome::AlreadyMember(ids) => (StatusCode::OK, Json(starter_ids_body(&ids))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::repository::{CampaignLifecycle, NewCampaignInstance};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use uuid::Uuid;

    const WALLET: [u8; 32] = [7u8; 32];

    /// A signed-in session `"t"` for `WALLET`, on `config`.
    fn signed_in(config: Config) -> AppState {
        let state = AppState::new(Arc::new(config));
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

    /// `signed_in` with the join switch on.
    fn joined_state() -> AppState {
        let mut config = Config::default();
        config.server.join_enabled = true;
        signed_in(config)
    }

    async fn join(state: Arc<AppState>, token: Option<&str>) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method("POST").uri("/api/campaign/join");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = router().with_state(state).oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    fn id(body: &serde_json::Value, key: &str) -> Uuid {
        Uuid::parse_str(body[key].as_str().unwrap_or_else(|| panic!("{key} missing: {body}")))
            .unwrap_or_else(|e| panic!("{key} is not a UUID ({e}): {body}"))
    }

    #[tokio::test]
    async fn join_requires_a_session() {
        let (status, _) = join(Arc::new(joined_state()), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// 5a-3c J5: on the default config the switch is off. No session is still `401`; a
    /// session gets `503`, and nothing is written.
    #[tokio::test]
    async fn join_is_503_when_switched_off() {
        let state = Arc::new(signed_in(Config::default()));

        let (status, _) = join(state.clone(), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "the session check comes before the switch");

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body, json!({"error": "campaign joins are closed"}));

        assert!(state.membership_repo.list_campaigns_by_player(&WALLET).await.unwrap().is_empty(),
            "a refused join writes no membership");
        assert!(state.campaign_repo.list_campaigns_by_state(CampaignLifecycle::Pending).await.unwrap().is_empty(),
            "a refused join creates no campaign");
    }

    #[tokio::test]
    async fn first_join_returns_201_with_starter_ids() {
        let state = Arc::new(joined_state());
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let campaign_id = id(&body, "campaign_id");
        let (commander_id, section_id) = (id(&body, "commander_id"), id(&body, "section_id"));
        id(&body, "foreman_id");
        id(&body, "crew_id");

        let commander = state.commander_repo.get_commander(commander_id).await.unwrap().expect("starter Commander");
        assert_eq!(commander.player_wallet, WALLET.to_vec());
        assert_eq!(commander.campaign_id, campaign_id);
        let section = state.section_repo.get_section(section_id).await.unwrap().expect("starter Section");
        assert_eq!(section.player_wallet, WALLET.to_vec());
        assert_eq!(section.campaign_id, campaign_id);
    }

    #[tokio::test]
    async fn repeat_join_returns_200_with_the_same_ids() {
        let state = Arc::new(joined_state());
        let (first_status, first) = join(state.clone(), Some("t")).await;
        assert_eq!(first_status, StatusCode::CREATED, "{first}");
        let (status, again) = join(state, Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again, first, "a repeat join returns the same starter set");
    }

    /// Reviewer 5a-2b CF2: a membership written outside the join has no starter set in the
    /// in-memory join's map, so the join refuses it as an integrity `500`.
    #[tokio::test]
    async fn a_member_without_starters_gets_500() {
        let state = joined_state();
        let campaign = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 1 })
            .await
            .unwrap();
        state.membership_repo.add_member(&WALLET, campaign.campaign_id, None, None).await.unwrap();
        let (status, body) = join(Arc::new(state), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    }
}
