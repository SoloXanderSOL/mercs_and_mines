use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use sim_engine::rng::generate_seed;

use crate::auth::AuthSession;
use crate::repository::{CampaignLifecycle, JoinOutcome, JoinSeeds, ReplacementOutcome, StarterIds, SECTOR_PLAYER_CAP};
use crate::state::AppState;

use super::{
    client_ip, conflict, forbidden, internal_error, not_found, service_unavailable, too_many_requests,
    unauthorized, AppError,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/join", post(post_campaign_join))
        .route("/api/campaign/:campaign_id/replacement-commander", post(post_replacement_commander))
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
/// - Rate limits (INF-6c-1), after the switch: `429` per client IP, then `429` per wallet.
///   While the switch is off nothing is counted.
async fn post_campaign_join(
    AuthSession(session): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !state.config.server.join_enabled {
        return Err(service_unavailable("campaign joins are closed"));
    }

    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.join_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.join_wallet.check(session.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
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
    }
    .into_response())
}

// TODO(phase-2-§10j): alpha-only replacement; removed when the Phase 2 Bootcamp hire ships (ODQ-33).
/// The alpha replacement (5a-4, ODQ-33; ODQ-46 since 5a-5): `201` with a new Rank 1 Commander
/// for a member with no usable Commander (every one KIA or Shattered).
///
/// - Order (5a-3b's house pattern): `401` → `429` per client IP → `429` per wallet (INF-6c-1;
///   a throttled caller costs no read and no row lock) → `404` campaign → `403` non-member (any
///   membership row) → `409` not `Active` (read before the transaction, as dispatch does) →
///   the claim.
/// - **Not behind the join switch** (Reviewer 5a-3c CF3): only a member gets past `403`, and
///   while joins are closed production has none.
/// - In the claim: `403` again if the membership vanished, `409` if any Commander is usable
///   (neither KIA nor Shattered), and `500` for a member with no Commander at all.
/// - Not a logged input: the generator roll is a server decision stored in its record.
async fn post_replacement_commander(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.replacement_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.replacement_wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }

    let wallet: Vec<u8> = bs58::decode(&token.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;

    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(not_found("campaign not found")),
        Err(e) => {
            return Err(internal_error(format!("post_replacement_commander: get_campaign failed for campaign {campaign_id}: {e}")));
        }
    };
    match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(forbidden("not a member of this campaign")),
        Err(e) => {
            return Err(internal_error(format!("post_replacement_commander: membership read failed for campaign {campaign_id}: {e}")));
        }
    }
    if campaign.state != CampaignLifecycle::Active {
        return Err(conflict("campaign is not active"));
    }

    let outcome = state
        .campaign_join_repo
        .claim_replacement(campaign_id, &wallet, generate_seed())
        .await
        .map_err(|e| internal_error(format!("post_replacement_commander: claim failed for campaign {campaign_id}: {e}")))?;

    match outcome {
        ReplacementOutcome::Granted(id) => Ok((
            StatusCode::CREATED,
            Json(json!({"campaign_id": campaign_id, "commander_id": id})),
        )
            .into_response()),
        ReplacementOutcome::NotMember => Err(forbidden("not a member of this campaign")),
        ReplacementOutcome::HasUsable => Err(conflict("member has a usable Commander")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::repository::NewCampaignInstance;
    use crate::routes::{with_test_peer, TEST_PEER};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

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
        let resp = with_test_peer(router().with_state(state), TEST_PEER)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
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

    // ── 5a-4: the alpha KIA replacement ──────────────────────────────────────

    const OTHER: [u8; 32] = [8u8; 32];

    async fn claim(state: Arc<AppState>, token: Option<&str>, campaign: &str) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method("POST")
            .uri(format!("/api/campaign/{campaign}/replacement-commander"));
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = with_test_peer(router().with_state(state), TEST_PEER)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    async fn activate(state: &AppState, campaign_id: Uuid) {
        let ok = state.campaign_repo
            .activate_campaign(campaign_id, chrono::Utc::now() + chrono::Duration::days(90), json!({}))
            .await
            .unwrap();
        assert!(ok, "the campaign must activate");
    }

    /// `WALLET` joins (switch on) and its campaign is launched. Returns (campaign, starter).
    async fn active_member() -> (Arc<AppState>, Uuid, Uuid) {
        let state = Arc::new(joined_state());
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (campaign_id, commander_id) = (id(&body, "campaign_id"), id(&body, "commander_id"));
        activate(&state, campaign_id).await;
        (state, campaign_id, commander_id)
    }

    /// An `Active` campaign that `WALLET` is not a member of (`OTHER` is).
    async fn active_campaign_without_wallet(state: &AppState) -> Uuid {
        let c = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 1 })
            .await
            .unwrap()
            .campaign_id;
        state.membership_repo.add_member(&OTHER, c, None, None).await.unwrap();
        activate(state, c).await;
        c
    }

    /// L1 — no session is `401`.
    #[tokio::test]
    async fn replacement_requires_a_session() {
        let (state, campaign_id, _) = active_member().await;
        let (status, _) = claim(state, None, &campaign_id.to_string()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// L2 — an unknown campaign is `404`.
    #[tokio::test]
    async fn replacement_unknown_campaign_is_404() {
        let state = Arc::new(joined_state());
        let (status, body) = claim(state, Some("t"), &Uuid::new_v4().to_string()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body, json!({"error": "campaign not found"}));
    }

    /// L3 — a non-member is `403`, and nothing is written.
    #[tokio::test]
    async fn replacement_non_member_is_403() {
        let state = joined_state();
        let c = active_campaign_without_wallet(&state).await;
        let state = Arc::new(state);
        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body, json!({"error": "not a member of this campaign"}));
        assert!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().is_empty());
    }

    /// L4 — a member of a campaign that is not `Active` is `409`, even with every Commander KIA.
    #[tokio::test]
    async fn replacement_campaign_not_active_is_409() {
        let state = Arc::new(joined_state());
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (c, starter) = (id(&body, "campaign_id"), id(&body, "commander_id"));
        state.commander_repo.set_kia(starter).await.unwrap();
        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": "campaign is not active"}));
        assert_eq!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().len(), 1);
    }

    /// L5 — a usable starter is `409`.
    #[tokio::test]
    async fn replacement_with_a_living_commander_is_409() {
        let (state, c, _) = active_member().await;
        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": "member has a usable Commander"}));
        assert_eq!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().len(), 1);
    }

    /// L6 — a Shattered Commander is not usable (ODQ-46: Phase 1 has no Rehabilitation), so a
    /// member whose only Commander is Shattered gets a replacement.
    #[tokio::test]
    async fn replacement_with_only_a_shattered_commander_is_granted() {
        let (state, c, starter) = active_member().await;
        state.commander_repo.set_shattered(starter).await.unwrap();
        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_ne!(id(&body, "commander_id"), starter);
        assert_eq!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().len(), 2);
    }

    /// L7 — an all-KIA member gets one Rank 1 replacement; a second claim while it lives is `409`.
    #[tokio::test]
    async fn all_kia_member_gets_one_replacement_then_409() {
        let (state, c, starter) = active_member().await;
        state.commander_repo.set_kia(starter).await.unwrap();

        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(id(&body, "campaign_id"), c);
        let rep = state.commander_repo.get_commander(id(&body, "commander_id")).await.unwrap().expect("replacement");
        assert_ne!(rep.commander_id, starter);
        assert_eq!((rep.rank, rep.xp, rep.stress, rep.is_kia, rep.is_shattered), (1, 0, 0, false, false));
        assert!(rep.commander_grade.is_some(), "a replacement carries its Grade");
        assert_eq!((rep.player_wallet.clone(), rep.campaign_id), (WALLET.to_vec(), c));

        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": "member has a usable Commander"}));
        assert_eq!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().len(), 2);
    }

    /// L8 — a member with no Commander at all is an integrity `500`.
    #[tokio::test]
    async fn replacement_with_zero_commanders_is_500() {
        let state = joined_state();
        let c = active_campaign_without_wallet(&state).await;
        state.membership_repo.add_member(&WALLET, c, None, None).await.unwrap();
        let state = Arc::new(state);
        let (status, body) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().is_empty(), "nothing is granted");
    }

    /// L9 — per wipe, not once per membership: a dead replacement can be replaced.
    #[tokio::test]
    async fn a_dead_replacement_can_be_replaced() {
        let (state, c, starter) = active_member().await;
        state.commander_repo.set_kia(starter).await.unwrap();
        let (status, first) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        state.commander_repo.set_kia(id(&first, "commander_id")).await.unwrap();

        let (status, second) = claim(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        assert_ne!(id(&second, "commander_id"), id(&first, "commander_id"));
        assert_eq!(state.commander_repo.list_commanders_by_campaign(c).await.unwrap().len(), 3);
    }

    /// L10 — a malformed id: unsigned is `401` (the session extractor runs first), signed is
    /// axum's path rejection `400`.
    #[tokio::test]
    async fn replacement_malformed_id_is_401_then_400() {
        let state = Arc::new(joined_state());
        let (status, _) = claim(state.clone(), None, "not-a-uuid").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = claim(state, Some("t"), "not-a-uuid").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── INF-6c-1: rate limits ────────────────────────────────────────────────

    /// Adds a live session `token` for `wallet` (in-memory only).
    fn add_session(state: &AppState, wallet: [u8; 32], token: &str) {
        let wallet = bs58::encode(wallet).into_string();
        let now = chrono::Utc::now().timestamp();
        state.sessions.insert(token.into(), shared::SessionToken {
            token_id: token.into(),
            account_id: wallet.clone(),
            wallet_address: wallet,
            issued_at: now,
            expires_at: now + 600,
        });
    }

    /// `joined_state` plus sessions `"w0"`…`"w{n-1}"` for wallets `[100 + i; 32]`.
    fn with_sessions(n: u8) -> Arc<AppState> {
        let state = joined_state();
        for i in 0..n {
            add_session(&state, [100 + i; 32], &format!("w{i}"));
        }
        Arc::new(state)
    }

    #[tokio::test]
    async fn join_per_wallet_limit() {
        let state = Arc::new(joined_state());
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(join(state.clone(), Some("t")).await.0);
        }
        assert_eq!(
            statuses,
            [StatusCode::CREATED, StatusCode::OK, StatusCode::OK, StatusCode::TOO_MANY_REQUESTS]
        );
    }

    #[tokio::test]
    async fn join_per_ip_limit() {
        let state = with_sessions(11);
        let mut statuses = Vec::new();
        for i in 0..11 {
            statuses.push(join(state.clone(), Some(&format!("w{i}"))).await.0);
        }
        let mut expected = vec![StatusCode::CREATED; 10];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    #[tokio::test]
    async fn replacement_per_wallet_limit() {
        let state = Arc::new(joined_state());
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(claim(state.clone(), Some("t"), &Uuid::new_v4().to_string()).await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 3];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected, "the limit is checked before the campaign read");
    }

    #[tokio::test]
    async fn replacement_per_ip_limit() {
        let state = with_sessions(11);
        let mut statuses = Vec::new();
        for i in 0..11 {
            statuses.push(claim(state.clone(), Some(&format!("w{i}")), &Uuid::new_v4().to_string()).await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 10];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }
}
