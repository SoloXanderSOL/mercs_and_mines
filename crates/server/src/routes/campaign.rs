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
use crate::campaign_init::{initialize_campaign, retire_campaign, InitError, RetireError};
use crate::repository::{CampaignLifecycle, JoinOutcome, JoinSeeds, ReplacementOutcome, SectorState, StarterIds};
use crate::state::AppState;

use super::{
    client_ip, conflict, forbidden, internal_error, not_found, service_unavailable, too_many_requests,
    unauthorized, AppError,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/join", post(post_campaign_join))
        .route("/api/campaign/:campaign_id/replacement-commander", post(post_replacement_commander))
        .route("/api/campaign/:campaign_id/abandon", post(post_abandon_campaign))
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

/// The demo's Sector player cap (BP-1, ODQ-51 (a), demo-only): one wallet per campaign.
const DEMO_PLAYER_CAP: i64 = 1;

/// A player enters a campaign (5a-3b): `201` with the new starter set, or `200` with the
/// existing one when the wallet already has a live membership (S1/S7).
///
/// - The join creates the wallet's `player_accounts` row (`insert_account_if_absent`,
///   GAP-79), so a `23503` from it is a backstop `500`, never a `404`.
/// - **The demo (BP-1, demo-only):** one wallet per campaign (`DEMO_PLAYER_CAP`), and every
///   join launches its campaign, with no admin key. `begin_launch`'s `NotPending` is success,
///   so only a still-`Pending` campaign is relaunched. The answers:
///   - a launch failure **before** activation is `500`; the campaign stays `Pending`, and
///     the next join relaunches it;
///   - a failure **after** activation retires the campaign and deletes its sector
///     (`retire_campaign`), then `500`; the next join gets a new campaign;
///   - if the retire fails, finds the campaign no longer `Active`, or the sector delete
///     fails, it is still `500`, journalled with the campaign and sector ids. A campaign
///     left `Active` keeps answering the member's joins with its ids; the member recovers
///     with `POST /api/campaign/:campaign_id/abandon`, then a new join (BP-1b);
///   - `503 {"error":"the demo is full"}` when the join would create a campaign beyond the
///     demo's live-campaign cap; a member still gets in.
/// - **Same-wallet race:** two concurrent joins can both reach the launch. One sees
///   `NotPending` and answers `200`, even if the other's launch then fails and retires the
///   campaign. The next join recovers, unless that retire failed (above).
/// - **Cancel-safe (BP-1c, Reviewer BP-3a-1b F1):** the join and the launch run in a spawned
///   task the route awaits, so a client that goes away mid-launch no longer leaves an
///   `Active` campaign without a sector. A join that finds its campaign `Active` with no
///   sector (a process killed mid-launch, or a campaign stuck before BP-1c) retires it and
///   answers `500`; the client's retry gets a new campaign. Each campaign's launch and
///   repair run under its `launch_locks` entry, so a repair never retires a launch still in
///   flight. An `Ending` campaign is never repaired (`retire_if_active` matches `Active`
///   only; the demo never reaches `Ending`).
/// - **Abuse bound:** any signed-in wallet, behind the join switch, `join_ip` 30/min and
///   `join_wallet` 3/min, and at most 1,000 live campaigns. It holds `JOIN_ADVISORY_KEY` for
///   the join transaction, then again for `begin_launch`'s short transaction. Map generation
///   and the Redis write run after that commit, outside the lock.
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
    // BP-1c: the work runs in its own task. Dropping the `JoinHandle` (the client went away,
    // hyper dropped this future) detaches the task; it never cancels it. The route never
    // calls `abort()`, so the only `JoinError` is a panic.
    let task_state = state.clone();
    let handle = tokio::spawn(async move { join_and_launch(task_state, wallet, seeds).await });
    match handle.await {
        Ok(result) => result,
        Err(e) => Err(internal_error(format!("demo join task failed for wallet {}: {e}", session.wallet_address))),
    }
}

/// The demo join's work (BP-1c): join, launch, and repair a sectorless `Active` campaign.
/// It runs inside the task `post_campaign_join` spawns, so it always runs to the end.
async fn join_and_launch(state: Arc<AppState>, wallet: Vec<u8>, seeds: JoinSeeds) -> Result<Response, AppError> {
    let outcome = state
        .campaign_join_repo
        .join_campaign(&wallet, DEMO_PLAYER_CAP, seeds)
        .await
        .map_err(|e| internal_error(format!("campaign join failed: {e}")))?;

    let (status, ids) = match outcome {
        JoinOutcome::Joined(ids) => (StatusCode::CREATED, ids),
        JoinOutcome::AlreadyMember(ids) => (StatusCode::OK, ids),
        JoinOutcome::DemoFull => return Err(service_unavailable("the demo is full")),
    };
    let c = ids.campaign_id;
    // One launch or repair per campaign at a time (BP-1c).
    let _launch = state.launch_locks.lock(c).await;
    match initialize_campaign(c, &*state.campaign_join_repo, state.membership_repo.as_ref(),
                              state.sector_repo.as_ref(), state.input_log_repo.as_ref()).await {
        Ok(()) => {}
        Err(InitError::NotPending) => repair_if_sectorless(&state, c).await?,
        Err(InitError::AfterActivation { sector_id, cause }) => {
            return Err(match retire_campaign(state.campaign_repo.as_ref(), state.sector_repo.as_ref(), c, sector_id).await {
                Ok(true) => internal_error(format!(
                    "demo launch failed after activation: campaign {c} retired, sector {sector_id} deleted: {cause:?}")),
                Ok(false) => internal_error(format!(
                    "demo launch failed after activation: campaign {c} no longer Active, sector {sector_id} kept: {cause:?}")),
                Err(RetireError::Retire(e)) => internal_error(format!(
                    "demo launch failed after activation and the retire failed: campaign {c} stuck Active, sector {sector_id} kept: {cause:?}; retire: {e}")),
                Err(RetireError::DeleteSector(e)) => internal_error(format!(
                    "demo launch failed after activation: campaign {c} retired, sector {sector_id} NOT deleted (SREM sectors:all + DEL by hand): {cause:?}; delete: {e}")),
            });
        }
        Err(e) => return Err(internal_error(format!(
            "demo launch failed before activation: campaign {c} stays Pending: {e:?}"))),
    }
    Ok((status, Json(starter_ids_body(&ids))).into_response())
}

/// The sectorless repair (BP-1c): a campaign left `Active` with no sector state is retired,
/// its sector deleted, and the join answers `500`; the client's retry gets a new campaign.
/// The caller holds the campaign's launch lock. Healthy (sector present) and non-`Active`
/// campaigns pass through. One Postgres read and one Redis `EXISTS`; never `get_sector`.
async fn repair_if_sectorless(state: &AppState, c: Uuid) -> Result<(), AppError> {
    let campaign = state.campaign_repo.get_campaign(c).await
        .map_err(|e| internal_error(format!("demo join repair: reading campaign {c} failed: {e}")))?
        .ok_or_else(|| internal_error(format!("demo join repair: campaign {c} not found")))?;
    if campaign.state != CampaignLifecycle::Active {
        return Ok(());
    }
    let sector_id = campaign.sector_id;
    let exists = state.sector_repo.sector_exists(sector_id).await
        .map_err(|e| internal_error(format!("demo join repair: sector check for campaign {c} failed: {e}")))?;
    if exists {
        return Ok(());
    }
    Err(match retire_campaign(state.campaign_repo.as_ref(), state.sector_repo.as_ref(), c, sector_id).await {
        Ok(true) => internal_error(format!(
            "demo join repaired: campaign {c} had no sector {sector_id}, retired")),
        Ok(false) => internal_error(format!(
            "demo join repair: campaign {c} had no sector {sector_id} and was no longer Active")),
        Err(RetireError::Retire(e)) => internal_error(format!(
            "demo join repair: retire failed, campaign {c} stuck Active, sector {sector_id} missing: {e}")),
        Err(RetireError::DeleteSector(e)) => internal_error(format!(
            "demo join repair: campaign {c} retired, sector {sector_id} delete failed: {e}")),
    })
}

/// The sector route's repair (BP-1d, demo-only; Reviewer BP-3a-2 F1). Called once, from
/// `routes/sector.rs`'s missing-sector arm; `dev`'s route keeps its `500`. A campaign left
/// `Active` with no sector state (Redis lost its keys: the demo's Redis persists by RDB
/// snapshot only) is retired, its sector deleted, and the route answers `409 campaign is not
/// active`, so the client's `409` → re-join-once path starts a new campaign.
/// - **Spawned**, as the demo join: a client that goes away while this waits detaches the
///   task, never cancels it, so the launch lock entry is always released and a retire always
///   reaches its sector delete. The only `JoinError` is a panic.
/// - **Locked** (see `repair_missing_sector_locked`). Only the miss path locks; the hot
///   read never does.
pub(super) async fn repair_missing_sector(state: Arc<AppState>, campaign_id: Uuid, sector_id: Uuid)
    -> Result<SectorState, AppError>
{
    let task = tokio::spawn(repair_missing_sector_locked(state, campaign_id, sector_id));
    match task.await {
        Ok(result) => result,
        Err(e) => Err(internal_error(format!("sector repair task failed for campaign {campaign_id}: {e}"))),
    }
}

/// The repair's work, under the campaign's `launch_locks` entry, taken first and never while
/// a transaction or `JOIN_ADVISORY_KEY` is held (the join's order). Both reads are repeated
/// under the lock, campaign first:
/// - no longer `Active` → `409` (an abandon or a join repair retired it: Reviewer BP-2a F2);
/// - `Ending`, or the row gone → `500`, as before (`retire_if_active` retires only `Active`,
///   and the demo can't reach `Ending` before its 90-day `ends_at`);
/// - the sector now present (a launch finished while this waited) → served. Its `gateway`
///   comes from the membership read before the wait, so it may be `null`: the launch writes
///   the sector before the gateways. The demo client never sends that race;
/// - still missing → retired (one `warn!` per retire, so each Redis-loss repair is in the
///   journal) → `409`.
///
/// A convoy in transit does not refuse the repair: its sector is already gone, so its timer
/// already polls a missing sector (demo runbook: clear that timer by hand).
async fn repair_missing_sector_locked(state: Arc<AppState>, campaign_id: Uuid, sector_id: Uuid)
    -> Result<SectorState, AppError>
{
    let _launch = state.launch_locks.lock(campaign_id).await;
    let campaign = state.campaign_repo.get_campaign(campaign_id).await
        .map_err(|e| internal_error(format!("sector repair: re-reading campaign {campaign_id} failed: {e}")))?;
    match campaign.map(|c| c.state) {
        Some(CampaignLifecycle::Active) => {}
        Some(CampaignLifecycle::Ending) | None => return Err(internal_error(format!(
            "get_campaign_sector: sector {sector_id} of campaign {campaign_id} is missing"))),
        Some(_) => return Err(conflict("campaign is not active")),
    }
    let found = state.sector_repo.get_sector(sector_id).await
        .map_err(|e| internal_error(format!("sector repair: re-reading sector {sector_id} of campaign {campaign_id} failed: {e}")))?;
    if let Some(sector) = found {
        return Ok(sector);
    }
    match retire_campaign(state.campaign_repo.as_ref(), state.sector_repo.as_ref(), campaign_id, sector_id).await {
        Ok(true) => {
            tracing::warn!(%campaign_id, %sector_id, "sector repair: campaign had no sector, retired");
            Err(conflict("campaign is not active"))
        }
        Ok(false) => Err(conflict("campaign is not active")),
        Err(RetireError::Retire(e)) => Err(internal_error(format!(
            "sector repair: retire failed, campaign {campaign_id} stuck Active, sector {sector_id} missing: {e}"))),
        Err(RetireError::DeleteSector(e)) => Err(internal_error(format!(
            "sector repair: campaign {campaign_id} retired, sector {sector_id} delete failed: {e}"))),
    }
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

/// The demo's "new run" (BP-1b, ODQ-56 (a); demo-only, never merged back): `200
/// {"campaign_id"}` after retiring the caller's campaign through `retire_campaign`
/// (`Archived`, sector deleted). The client then joins again and gets a new campaign.
///
/// - Order (the replacement route's): `401` → `429` per client IP → `429` per wallet →
///   `429` per wallet per day → `404` campaign → `403` non-member (any membership row) →
///   `409` convoy → the retire.
/// - **No sole-member check:** the demo's join cap is 1, so a member is the sole member.
/// - **`409 member has a convoy in transit`:** the expiry poll never cancels a timer whose
///   sector is gone, so the abandon waits while a convoy can still land.
/// - **`409 campaign is not active`:** the retire found it not `Active`: already abandoned,
///   or a launch stuck `Pending` (the next join relaunches that one).
/// - It is the recovery for a campaign stuck `Active` after a failed launch (Reviewer BP-1
///   F1). A failed retire or sector delete is `500`, journalled with both ids.
/// - No `JOIN_ADVISORY_KEY`: it moves `Active` → `Archived`, never out of `Pending`. Not
///   behind the join switch, like the replacement route. Not a logged input.
/// - **Under the launch lock (BP-1d, Reviewer BP-1c F2):** the retire waits for an
///   in-flight launch of the same campaign, so it never leaves an orphan sector; an abandon
///   that meets a launch still running retires the launched campaign (`200`).
async fn post_abandon_campaign(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.abandon_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.abandon_wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.abandon_wallet_daily.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }

    let wallet: Vec<u8> = bs58::decode(&token.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;

    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(not_found("campaign not found")),
        Err(e) => {
            return Err(internal_error(format!("post_abandon_campaign: get_campaign failed for campaign {campaign_id}: {e}")));
        }
    };
    let member = match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(m) => m.is_some(),
        Err(e) => {
            return Err(internal_error(format!("post_abandon_campaign: membership read failed for campaign {campaign_id}: {e}")));
        }
    };
    if !member {
        return Err(forbidden("not a member of this campaign"));
    }
    let sector_id = campaign.sector_id;
    let in_transit = match state.convoy_repo.has_convoy_in_transit(sector_id, &wallet).await {
        Ok(t) => t,
        Err(e) => {
            return Err(internal_error(format!("post_abandon_campaign: convoy read failed for campaign {campaign_id}: {e}")));
        }
    };
    if in_transit {
        return Err(conflict("member has a convoy in transit"));
    }

    // BP-1d (Reviewer BP-1c F2): the retire runs under the campaign's launch lock, so an
    // abandon during an in-flight launch waits for the sector write and then deletes it (no
    // orphan key), and in its own task, so a client that goes away can neither split the
    // retire from the sector delete nor leave the lock entry behind.
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        let _launch = task_state.launch_locks.lock(campaign_id).await;
        retire_campaign(task_state.campaign_repo.as_ref(), task_state.sector_repo.as_ref(), campaign_id, sector_id).await
    });
    let retire = match task.await {
        Ok(r) => r,
        Err(e) => {
            return Err(internal_error(format!("post_abandon_campaign: the retire task failed for campaign {campaign_id}: {e}")));
        }
    };
    let retired = match retire {
        Ok(r) => r,
        Err(RetireError::Retire(e)) => {
            return Err(internal_error(format!(
                "post_abandon_campaign: the retire failed: campaign {campaign_id} may still be Active, sector {sector_id} kept: {e}")));
        }
        Err(RetireError::DeleteSector(e)) => {
            return Err(internal_error(format!(
                "post_abandon_campaign: campaign {campaign_id} retired, sector {sector_id} NOT deleted (SREM sectors:all + DEL by hand): {e}")));
        }
    };
    if !retired {
        return Err(conflict("campaign is not active"));
    }
    Ok((StatusCode::OK, Json(json!({"campaign_id": campaign_id}))).into_response())
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

        // BP-1: the join launched the campaign.
        let campaign = state.campaign_repo.get_campaign(campaign_id).await.unwrap().expect("campaign");
        assert_eq!(campaign.state, CampaignLifecycle::Active, "the join launches its campaign");
        assert!(state.sector_repo.get_sector(campaign.sector_id).await.unwrap().is_some(), "the launch wrote the sector");
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

    /// `WALLET` joins (switch on), which launches its campaign (BP-1). Returns (campaign, starter).
    async fn active_member() -> (Arc<AppState>, Uuid, Uuid) {
        let state = Arc::new(joined_state());
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (campaign_id, commander_id) = (id(&body, "campaign_id"), id(&body, "commander_id"));
        let campaign = state.campaign_repo.get_campaign(campaign_id).await.unwrap().expect("campaign");
        assert_eq!(campaign.state, CampaignLifecycle::Active, "the join must launch the campaign");
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
        // BP-1: the join launched it, so retire it to make it not `Active`.
        assert!(state.campaign_repo.retire_if_active(c).await.unwrap(), "the launched campaign retires");
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
        let state = with_sessions(31);
        let mut statuses = Vec::new();
        for i in 0..31 {
            statuses.push(join(state.clone(), Some(&format!("w{i}"))).await.0);
        }
        let mut expected = vec![StatusCode::CREATED; 30];
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
        let state = with_sessions(31);
        let mut statuses = Vec::new();
        for i in 0..31 {
            statuses.push(claim(state.clone(), Some(&format!("w{i}")), &Uuid::new_v4().to_string()).await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 30];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    // ── BP-1: the demo's auto-launch and caps ────────────────────────────────

    use crate::repository::{
        CampaignInstance, CampaignJoinRepository, CampaignRepository, InputLogRepository, LaunchStart,
        RepositoryError, VictoryTickerType,
    };
    use chrono::{DateTime, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn forced(what: &str) -> RepositoryError {
        RepositoryError::Internal(format!("forced test failure: {what}"))
    }

    /// True (and one fewer left) while failures remain. The doubles are called one request
    /// at a time, so a load then a decrement is enough.
    fn take_failure(fails_left: &AtomicUsize) -> bool {
        if fails_left.load(Ordering::SeqCst) == 0 {
            return false;
        }
        fails_left.fetch_sub(1, Ordering::SeqCst);
        true
    }

    /// (a) Fails `append_campaign_entry` once (after `upsert_sector`, so a sector exists
    /// for the delete to remove); everything else delegates.
    struct FailingCampaignLog {
        inner: Arc<dyn InputLogRepository + Send + Sync>,
        fails_left: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl InputLogRepository for FailingCampaignLog {
        async fn append_entry(&self, s: &Uuid, e: &shared::InputLogEntry) -> Result<(), RepositoryError> {
            self.inner.append_entry(s, e).await
        }
        async fn get_entries_by_session(&self, s: &Uuid) -> Result<Vec<shared::InputLogEntry>, RepositoryError> {
            self.inner.get_entries_by_session(s).await
        }
        async fn save_session_config(&self, c: &shared::SessionConfig) -> Result<(), RepositoryError> {
            self.inner.save_session_config(c).await
        }
        async fn get_session_config(&self, s: &Uuid) -> Result<Option<shared::SessionConfig>, RepositoryError> {
            self.inner.get_session_config(s).await
        }
        async fn append_campaign_entry(&self, c: &Uuid, e: &shared::InputLogEntry) -> Result<(), RepositoryError> {
            if take_failure(&self.fails_left) {
                return Err(forced("append_campaign_entry"));
            }
            self.inner.append_campaign_entry(c, e).await
        }
        async fn start_session(&self, c: &shared::SessionConfig, e: &shared::InputLogEntry) -> Result<(), RepositoryError> {
            self.inner.start_session(c, e).await
        }
    }

    /// (b) Fails `begin_launch` while `fails_left` is above 0. It wraps the state's own join
    /// instance, so the join still writes the state's stores.
    struct FailFirstLaunch {
        inner: Arc<dyn CampaignJoinRepository>,
        fails_left: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl CampaignJoinRepository for FailFirstLaunch {
        async fn join_campaign(&self, wallet: &[u8], cap: i64, seeds: JoinSeeds) -> Result<JoinOutcome, RepositoryError> {
            self.inner.join_campaign(wallet, cap, seeds).await
        }
        async fn begin_launch(
            &self,
            campaign_id: Uuid,
            max_members: usize,
            ends_at: DateTime<Utc>,
            victory_tickers: serde_json::Value,
        ) -> Result<LaunchStart, RepositoryError> {
            if take_failure(&self.fails_left) {
                return Err(forced("begin_launch"));
            }
            self.inner.begin_launch(campaign_id, max_members, ends_at, victory_tickers).await
        }
        async fn claim_replacement(&self, campaign_id: Uuid, wallet: &[u8], seed: u32) -> Result<ReplacementOutcome, RepositoryError> {
            self.inner.claim_replacement(campaign_id, wallet, seed).await
        }
    }

    /// (c) `retire_if_active` fails; everything else delegates. A deliberate exception to
    /// code `CLAUDE.md` §5's "swap the join-read repo together" rule: it delegates to the
    /// **same** store, so the join and the route still share one store.
    struct FailingRetire {
        inner: Arc<dyn CampaignRepository + Send + Sync>,
    }

    #[async_trait::async_trait]
    impl CampaignRepository for FailingRetire {
        async fn create_campaign(&self, p: &NewCampaignInstance) -> Result<CampaignInstance, RepositoryError> {
            self.inner.create_campaign(p).await
        }
        async fn get_campaign(&self, c: Uuid) -> Result<Option<CampaignInstance>, RepositoryError> {
            self.inner.get_campaign(c).await
        }
        async fn update_sector_state(&self, c: Uuid, s: CampaignLifecycle) -> Result<(), RepositoryError> {
            self.inner.update_sector_state(c, s).await
        }
        async fn apply_ticker_delta(&self, c: Uuid, t: VictoryTickerType, d: f64) -> Result<f64, RepositoryError> {
            self.inner.apply_ticker_delta(c, t, d).await
        }
        async fn list_campaigns_by_state(&self, s: CampaignLifecycle) -> Result<Vec<CampaignInstance>, RepositoryError> {
            self.inner.list_campaigns_by_state(s).await
        }
        async fn activate_campaign(&self, c: Uuid, e: DateTime<Utc>, v: serde_json::Value) -> Result<bool, RepositoryError> {
            self.inner.activate_campaign(c, e, v).await
        }
        async fn retire_if_active(&self, _: Uuid) -> Result<bool, RepositoryError> {
            Err(forced("retire_if_active"))
        }
    }

    fn fail_campaign_log_once(state: &mut AppState) {
        state.input_log_repo = Arc::new(FailingCampaignLog { inner: state.input_log_repo.clone(), fails_left: AtomicUsize::new(1) });
    }

    fn fail_first_launch(state: &mut AppState) {
        state.campaign_join_repo = Arc::new(FailFirstLaunch { inner: state.campaign_join_repo.clone(), fails_left: AtomicUsize::new(1) });
    }

    /// The campaign `wallet` is a member of (exactly one).
    async fn member_campaign(state: &AppState, wallet: &[u8]) -> CampaignInstance {
        let rows = state.membership_repo.list_campaigns_by_player(wallet).await.unwrap();
        assert_eq!(rows.len(), 1, "one membership");
        state.campaign_repo.get_campaign(rows[0].campaign_id).await.unwrap().expect("campaign")
    }

    async fn seed_active_campaigns(state: &AppState, n: i64) {
        for i in 0..n {
            let c = state.campaign_repo
                .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: i })
                .await
                .unwrap();
            activate(state, c.campaign_id).await;
        }
    }

    /// A failure before activation leaves the campaign `Pending` with no sector; the next
    /// join relaunches the same campaign.
    #[tokio::test]
    async fn join_relaunches_after_a_pre_activation_failure() {
        let mut state = joined_state();
        fail_first_launch(&mut state);
        let state = Arc::new(state);

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let first = member_campaign(&state, &WALLET).await;
        assert_eq!(first.state, CampaignLifecycle::Pending, "still Pending");
        assert!(state.sector_repo.get_sector(first.sector_id).await.unwrap().is_none(), "no sector yet");

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(id(&body, "campaign_id"), first.campaign_id, "the same campaign");
        let again = state.campaign_repo.get_campaign(first.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(again.state, CampaignLifecycle::Active, "the retry relaunched it");
        assert!(state.sector_repo.get_sector(first.sector_id).await.unwrap().is_some(), "with a sector");
    }

    /// A failure after activation retires the campaign and deletes its sector; the next
    /// join gets a new campaign.
    #[tokio::test]
    async fn join_retires_after_a_post_activation_failure() {
        let mut state = joined_state();
        fail_campaign_log_once(&mut state);
        let state = Arc::new(state);

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let failed = member_campaign(&state, &WALLET).await;
        assert_eq!(failed.state, CampaignLifecycle::Archived, "retired");
        assert!(state.sector_repo.get_sector(failed.sector_id).await.unwrap().is_none(), "its sector is deleted");

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let fresh = id(&body, "campaign_id");
        assert_ne!(fresh, failed.campaign_id, "a new campaign");
        let fresh = state.campaign_repo.get_campaign(fresh).await.unwrap().expect("campaign");
        assert_eq!(fresh.state, CampaignLifecycle::Active);
    }

    /// When the retire itself fails, the campaign stays `Active` and keeps its sector.
    #[tokio::test]
    async fn join_is_500_and_keeps_the_sector_when_the_retire_fails() {
        let mut state = joined_state();
        fail_campaign_log_once(&mut state);
        state.campaign_repo = Arc::new(FailingRetire { inner: state.campaign_repo.clone() });
        let state = Arc::new(state);

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let stuck = member_campaign(&state, &WALLET).await;
        assert_eq!(stuck.state, CampaignLifecycle::Active, "stuck Active");
        assert!(state.sector_repo.get_sector(stuck.sector_id).await.unwrap().is_some(), "the sector is kept");
    }

    /// ODQ-55 (a): 1,000 live campaigns (the literal) and a join that would create another
    /// is `503`, with nothing written.
    #[tokio::test]
    async fn join_is_503_when_the_demo_is_full() {
        let state = joined_state();
        seed_active_campaigns(&state, 1000).await;
        let state = Arc::new(state);

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body, json!({"error": "the demo is full"}));
        assert!(state.membership_repo.list_campaigns_by_player(&WALLET).await.unwrap().is_empty(), "no membership");
    }

    /// Pins `DEMO_PLAYER_CAP` = 1: a launch that failed before activation leaves a 1-member
    /// `Pending` campaign, and a second wallet never joins it.
    #[tokio::test]
    async fn a_second_wallet_never_joins_a_failed_launch() {
        let mut state = joined_state();
        fail_first_launch(&mut state);
        add_session(&state, OTHER, "o");
        let state = Arc::new(state);

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let a = member_campaign(&state, &WALLET).await;
        assert_eq!(a.state, CampaignLifecycle::Pending);
        assert_eq!(state.membership_repo.list_members_by_campaign(a.campaign_id).await.unwrap().len(), 1);

        let (status, body) = join(state.clone(), Some("o")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let b = id(&body, "campaign_id");
        assert_ne!(b, a.campaign_id, "B gets its own campaign");
        let b = state.campaign_repo.get_campaign(b).await.unwrap().expect("campaign");
        assert_eq!(b.state, CampaignLifecycle::Active);
    }

    // ── BP-1b: the demo's abandon ("new run") ────────────────────────────────

    use crate::repository::{ConvoyDbRecord, ConvoyRepository};

    async fn abandon(state: Arc<AppState>, token: Option<&str>, campaign: &str) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method("POST")
            .uri(format!("/api/campaign/{campaign}/abandon"));
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

    /// Answers the abandon's convoy read with a fixed value; nothing else is reachable.
    struct ConvoysInTransit(bool);

    #[async_trait::async_trait]
    impl ConvoyRepository for ConvoysInTransit {
        async fn create_convoy(&self, _: ConvoyDbRecord) -> Result<Uuid, RepositoryError> {
            unimplemented!()
        }
        async fn get_convoy(&self, _: Uuid) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
            unimplemented!()
        }
        async fn dispatch_convoy(&self, _: ConvoyDbRecord, _: Uuid, _: &shared::InputLogEntry) -> Result<(), RepositoryError> {
            unimplemented!()
        }
        async fn land_convoy(&self, _: Uuid, _: Uuid, _: &shared::InputLogEntry) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
            unimplemented!()
        }
        async fn has_convoy_in_transit(&self, _: Uuid, _: &[u8]) -> Result<bool, RepositoryError> {
            Ok(self.0)
        }
    }

    /// `WALLET` joins (which launches its campaign) on a state whose convoy read answers
    /// `in_transit`. Returns the campaign.
    async fn abandon_member(in_transit: bool) -> (Arc<AppState>, Uuid) {
        let mut state = joined_state();
        state.convoy_repo = Arc::new(ConvoysInTransit(in_transit));
        let state = Arc::new(state);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (state, id(&body, "campaign_id"))
    }

    async fn campaign_of(state: &AppState, campaign_id: Uuid) -> CampaignInstance {
        state.campaign_repo.get_campaign(campaign_id).await.unwrap().expect("campaign")
    }

    #[tokio::test]
    async fn abandon_requires_a_session() {
        let (state, c) = abandon_member(false).await;
        let (status, _) = abandon(state, None, &c.to_string()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn abandon_per_ip_limit() {
        let state = with_sessions(31);
        let mut statuses = Vec::new();
        for i in 0..31 {
            statuses.push(abandon(state.clone(), Some(&format!("w{i}")), &Uuid::new_v4().to_string()).await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 30];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    #[tokio::test]
    async fn abandon_per_wallet_limit() {
        let state = Arc::new(joined_state());
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(abandon(state.clone(), Some("t"), &Uuid::new_v4().to_string()).await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 3];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected, "the limit is checked before the campaign read");
    }

    #[tokio::test]
    async fn abandon_unknown_campaign_is_404() {
        let state = Arc::new(joined_state());
        let (status, body) = abandon(state, Some("t"), &Uuid::new_v4().to_string()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body, json!({"error": "campaign not found"}));
    }

    #[tokio::test]
    async fn abandon_by_a_non_member_is_403() {
        let mut state = joined_state();
        state.convoy_repo = Arc::new(ConvoysInTransit(false));
        let c = active_campaign_without_wallet(&state).await;
        let state = Arc::new(state);

        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body, json!({"error": "not a member of this campaign"}));
        assert_eq!(campaign_of(&state, c).await.state, CampaignLifecycle::Active, "a non-member retires nothing");
    }

    #[tokio::test]
    async fn abandon_with_a_convoy_in_transit_is_409() {
        let (state, c) = abandon_member(true).await;
        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": "member has a convoy in transit"}));
        let campaign = campaign_of(&state, c).await;
        assert_eq!(campaign.state, CampaignLifecycle::Active, "still Active");
        assert!(state.sector_repo.get_sector(campaign.sector_id).await.unwrap().is_some(), "the sector is kept");
    }

    #[tokio::test]
    async fn abandon_twice_is_409() {
        let (state, c) = abandon_member(false).await;
        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = abandon(state, Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": "campaign is not active"}));
    }

    #[tokio::test]
    async fn abandon_retires_deletes_the_sector_and_the_next_join_gets_a_new_campaign() {
        let (state, c) = abandon_member(false).await;
        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"campaign_id": c}));
        let old = campaign_of(&state, c).await;
        assert_eq!(old.state, CampaignLifecycle::Archived, "retired");
        assert!(state.sector_repo.get_sector(old.sector_id).await.unwrap().is_none(), "its sector is deleted");

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let fresh = id(&body, "campaign_id");
        assert_ne!(fresh, c, "a new campaign");
        assert_eq!(campaign_of(&state, fresh).await.state, CampaignLifecycle::Active);
    }

    /// Reviewer BP-1 F1: an `Active` campaign with no sector (a launch that failed before
    /// its sector write and whose retire failed) is recovered by the abandon.
    #[tokio::test]
    async fn abandon_recovers_a_campaign_whose_sector_is_missing() {
        let (state, c) = abandon_member(false).await;
        let sector_id = campaign_of(&state, c).await.sector_id;
        state.sector_repo.delete_sector(sector_id).await.unwrap();

        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(campaign_of(&state, c).await.state, CampaignLifecycle::Archived);

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_ne!(id(&body, "campaign_id"), c, "a new campaign");
    }

    #[tokio::test]
    async fn abandon_is_500_when_the_retire_fails() {
        let mut state = joined_state();
        state.convoy_repo = Arc::new(ConvoysInTransit(false));
        state.campaign_repo = Arc::new(FailingRetire { inner: state.campaign_repo.clone() });
        let state = Arc::new(state);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let c = id(&body, "campaign_id");

        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let campaign = campaign_of(&state, c).await;
        assert_eq!(campaign.state, CampaignLifecycle::Active, "still Active");
        assert!(state.sector_repo.get_sector(campaign.sector_id).await.unwrap().is_some(), "the sector is kept");
    }

    /// A launch stuck `Pending` is not abandoned: `409`, and it stays `Pending` for the next
    /// join to relaunch.
    #[tokio::test]
    async fn abandon_of_a_pending_campaign_is_409() {
        let mut state = joined_state();
        state.convoy_repo = Arc::new(ConvoysInTransit(false));
        fail_first_launch(&mut state);
        let state = Arc::new(state);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let c = member_campaign(&state, &WALLET).await.campaign_id;

        let (status, body) = abandon(state.clone(), Some("t"), &c.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": "campaign is not active"}));
        assert_eq!(campaign_of(&state, c).await.state, CampaignLifecycle::Pending, "still Pending");
    }

    // ── BP-1c: a cancel-safe demo join ───────────────────────────────────────

    use crate::repository::{
        HexOccupant, InMemorySectorStateRepository, SectorId, SectorState, SectorStateRepository, WalletAddress,
    };
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// The BP-1c pause seam, a test double around an in-memory sector store. While
    /// `pause_next` is set, the next `upsert_sector` signals `parked` and waits for `release`
    /// before writing: a launch held after `begin_launch` committed and before its sector
    /// exists. It also counts the `sector_exists`, `get_sector` and `delete_sector` calls.
    struct PausedSector {
        inner: Arc<InMemorySectorStateRepository>,
        pause_next: AtomicBool,
        parked: Notify,
        release: Notify,
        exists_calls: AtomicUsize,
        get_calls: AtomicUsize,
        delete_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SectorStateRepository for PausedSector {
        async fn get_sector(&self, s: SectorId) -> Result<Option<SectorState>, RepositoryError> {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.get_sector(s).await
        }
        async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError> {
            if self.pause_next.swap(false, Ordering::SeqCst) {
                self.parked.notify_one();
                self.release.notified().await;
            }
            self.inner.upsert_sector(state).await
        }
        async fn list_sectors(&self) -> Result<Vec<SectorState>, RepositoryError> {
            self.inner.list_sectors().await
        }
        async fn get_player_presence(&self, s: SectorId) -> Result<Vec<WalletAddress>, RepositoryError> {
            self.inner.get_player_presence(s).await
        }
        async fn set_player_presence(&self, s: SectorId, p: &[WalletAddress]) -> Result<(), RepositoryError> {
            self.inner.set_player_presence(s, p).await
        }
        async fn add_hex_occupant(&self, s: SectorId, q: i32, r: i32, e: HexOccupant) -> Result<(), RepositoryError> {
            self.inner.add_hex_occupant(s, q, r, e).await
        }
        async fn remove_hex_occupant(&self, s: SectorId, q: i32, r: i32, u: Uuid) -> Result<(), RepositoryError> {
            self.inner.remove_hex_occupant(s, q, r, u).await
        }
        async fn delete_sector(&self, s: SectorId) -> Result<(), RepositoryError> {
            self.delete_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.delete_sector(s).await
        }
        async fn sector_exists(&self, s: SectorId) -> Result<bool, RepositoryError> {
            self.exists_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.sector_exists(s).await
        }
    }

    /// A joined state with session `"t"` for `wallet`, whose sector store is a `PausedSector`
    /// (parking the first sector write when `pause` is set). `sector_repo` is not a join-read
    /// field, so the join's own stores are untouched. The convoy read answers "none in transit"
    /// (BP-1d's abandon tests; the in-memory convoy store is `unimplemented!()`).
    fn paused_state(wallet: [u8; 32], pause: bool) -> (Arc<AppState>, Arc<PausedSector>) {
        let mut config = Config::default();
        config.server.join_enabled = true;
        let mut state = AppState::new(Arc::new(config));
        let w = bs58::encode(wallet).into_string();
        let now = chrono::Utc::now().timestamp();
        state.sessions.insert("t".into(), shared::SessionToken {
            token_id: "t".into(),
            account_id: w.clone(),
            wallet_address: w,
            issued_at: now,
            expires_at: now + 600,
        });
        let paused = Arc::new(PausedSector {
            inner: Arc::new(InMemorySectorStateRepository::new()),
            pause_next: AtomicBool::new(pause),
            parked: Notify::new(),
            release: Notify::new(),
            exists_calls: AtomicUsize::new(0),
            get_calls: AtomicUsize::new(0),
            delete_calls: AtomicUsize::new(0),
        });
        state.sector_repo = paused.clone();
        state.convoy_repo = Arc::new(ConvoysInTransit(false));
        (Arc::new(state), paused)
    }

    /// Polls `check` every 10 ms for up to 2 s: a detached launch finishes in the background.
    async fn eventually<F, Fut>(mut check: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..200 {
            if check().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// The request is dropped while its launch is parked between `begin_launch`'s commit and
    /// the sector write (the client went away mid-launch). The launch still finishes: the
    /// sector and the gateway are written and the campaign's lock is released.
    #[tokio::test]
    async fn join_launch_survives_a_dropped_request() {
        let wallet = [91u8; 32];
        let (state, sector) = paused_state(wallet, true);

        tokio::select! {
            r = join(state.clone(), Some("t")) => panic!("the join answered while its launch was parked: {r:?}"),
            _ = sector.parked.notified() => {}
        }
        let campaign = member_campaign(&state, &wallet).await;
        assert_eq!(campaign.state, CampaignLifecycle::Active, "begin_launch committed");
        assert!(!sector.inner.sector_exists(campaign.sector_id).await.unwrap(), "no sector yet");

        sector.release.notify_one();
        let (st, sc) = (state.clone(), sector.clone());
        let (campaign_id, sector_id) = (campaign.campaign_id, campaign.sector_id);
        let finished = eventually(move || {
            let (st, sc) = (st.clone(), sc.clone());
            async move {
                let gateway = st.membership_repo.get_membership(&wallet, campaign_id).await.unwrap()
                    .and_then(|m| m.gateway_hex_q)
                    .is_some();
                sc.inner.sector_exists(sector_id).await.unwrap() && gateway && st.launch_locks.len() == 0
            }
        })
        .await;
        assert!(finished, "the detached launch wrote the sector and the gateway, then released its lock");
    }

    /// Class (a): a campaign `Active` with no sector (a process killed mid-launch). The next
    /// join retires it and answers the generic `500`; the join after that gets a new campaign.
    #[tokio::test]
    async fn join_repairs_an_active_campaign_with_no_sector() {
        let wallet = [92u8; 32];
        let (state, sector) = paused_state(wallet, false);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let first = member_campaign(&state, &wallet).await;
        sector.inner.delete_sector(first.sector_id).await.unwrap();

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(body["error"], "internal error", "the generic 500 body: {body}");
        let retired = state.campaign_repo.get_campaign(first.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(retired.state, CampaignLifecycle::Archived, "the sectorless campaign was retired");
        assert!(!sector.inner.sector_exists(first.sector_id).await.unwrap(), "no sector left behind");

        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let fresh = id(&body, "campaign_id");
        assert_ne!(fresh, first.campaign_id, "the retry gets a new campaign");
        let campaign = state.campaign_repo.get_campaign(fresh).await.unwrap().expect("campaign");
        assert_eq!(campaign.state, CampaignLifecycle::Active, "launched");
        assert!(sector.inner.sector_exists(campaign.sector_id).await.unwrap(), "with a sector");
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// A's launch is parked before its sector write; B (the same wallet: a double tap, or a
    /// retry after the client's deadline) finds the campaign `Active` with no sector. B must
    /// wait on the campaign's launch lock, not retire a campaign whose sector is on its way.
    #[tokio::test]
    async fn join_repair_waits_for_an_inflight_launch() {
        let wallet = [93u8; 32];
        let (state, sector) = paused_state(wallet, true);
        let a = tokio::spawn(join(state.clone(), Some("t")));
        sector.parked.notified().await;
        let campaign = member_campaign(&state, &wallet).await;

        let mut b = tokio::spawn(join(state.clone(), Some("t")));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut b).await.is_err(),
            "B answered while A's launch was still writing the sector"
        );

        sector.release.notify_one();
        let (status_a, body_a) = a.await.unwrap();
        let (status_b, body_b) = b.await.unwrap();
        assert_eq!(status_a, StatusCode::CREATED, "{body_a}");
        assert_eq!(status_b, StatusCode::OK, "{body_b}");
        assert_eq!(id(&body_b, "campaign_id"), campaign.campaign_id, "B joined A's campaign");
        let after = state.campaign_repo.get_campaign(campaign.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(after.state, CampaignLifecycle::Active, "not retired");
        assert!(sector.inner.sector_exists(campaign.sector_id).await.unwrap(), "A's sector stands");
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// A repeat join of a healthy campaign is `200` with the same ids and retires nothing;
    /// the repair costs one `EXISTS` and never loads the ~86 KB sector.
    #[tokio::test]
    async fn join_with_a_live_sector_does_not_retire() {
        let wallet = [91u8; 32];
        let (state, sector) = paused_state(wallet, false);
        let (status, first) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        let (status, again) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again, first, "the same starter set");

        let campaign = member_campaign(&state, &wallet).await;
        assert_eq!(campaign.state, CampaignLifecycle::Active, "not retired");
        assert!(sector.inner.sector_exists(campaign.sector_id).await.unwrap(), "the sector stands");
        assert_eq!(sector.get_calls.load(Ordering::SeqCst), 0, "the repair never loads the sector");
        assert_eq!(sector.exists_calls.load(Ordering::SeqCst), 1, "one EXISTS, on the repeat join only");
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    // ── BP-1d: the sector route's repair and the abandon lock ────────────────

    /// `GET /api/campaign/:campaign_id/sector` on `state`, through the sector route's router.
    async fn sector_get(state: Arc<AppState>, token: Option<&str>, campaign: Uuid) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method("GET").uri(format!("/api/campaign/{campaign}/sector"));
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = with_test_peer(crate::routes::sector::router().with_state(state), TEST_PEER)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    /// A launch is parked before its sector write, holding the lock; a GET misses the sector
    /// and must wait for the launch, then serve the map it wrote, not retire the campaign.
    #[tokio::test]
    async fn sector_waits_for_an_inflight_launch() {
        let wallet = [95u8; 32];
        let (state, sector) = paused_state(wallet, true);
        let a = tokio::spawn(join(state.clone(), Some("t")));
        sector.parked.notified().await;
        let campaign = member_campaign(&state, &wallet).await;

        let mut g = tokio::spawn(sector_get(state.clone(), Some("t"), campaign.campaign_id));
        let sc = sector.clone();
        assert!(
            eventually(move || {
                let sc = sc.clone();
                async move { sc.get_calls.load(Ordering::SeqCst) == 1 }
            })
            .await,
            "the GET read the sector once"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut g).await.is_err(),
            "the GET answered while the launch was still writing the sector"
        );

        sector.release.notify_one();
        let (status_a, body_a) = a.await.unwrap();
        assert_eq!(status_a, StatusCode::CREATED, "{body_a}");
        let (status, body) = g.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["campaign_id"], campaign.campaign_id.to_string());
        assert!(!body["terrain"].as_object().expect("terrain is an object").is_empty(), "the launch's map");
        let after = state.campaign_repo.get_campaign(campaign.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(after.state, CampaignLifecycle::Active, "not retired");
        assert!(sector.inner.sector_exists(campaign.sector_id).await.unwrap(), "the sector stands");
        assert_eq!(sector.delete_calls.load(Ordering::SeqCst), 0, "nothing deleted");
        assert_eq!(sector.get_calls.load(Ordering::SeqCst), 2, "the first read and the locked re-read");
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// The sector is gone and someone else (an abandon or a join repair) holds the lock and
    /// retires the campaign; the waiting GET answers `409` and neither reads the sector again
    /// nor deletes it a second time.
    #[tokio::test]
    async fn sector_repair_waits_for_a_retire_and_does_not_repeat_it() {
        let wallet = [96u8; 32];
        let (state, sector) = paused_state(wallet, false);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let campaign = member_campaign(&state, &wallet).await;
        let (c, s) = (campaign.campaign_id, campaign.sector_id);
        sector.inner.delete_sector(s).await.unwrap();

        let guard = state.launch_locks.lock(c).await;
        let mut g = tokio::spawn(sector_get(state.clone(), Some("t"), c));
        let sc = sector.clone();
        assert!(
            eventually(move || {
                let sc = sc.clone();
                async move { sc.get_calls.load(Ordering::SeqCst) == 1 }
            })
            .await,
            "the GET read the sector once"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut g).await.is_err(),
            "the GET answered while another holder had the campaign's lock"
        );
        let retired = crate::campaign_init::retire_campaign(state.campaign_repo.as_ref(), state.sector_repo.as_ref(), c, s)
            .await
            .expect("retire_campaign");
        assert!(retired, "the holder's retire");
        assert_eq!(sector.delete_calls.load(Ordering::SeqCst), 1, "the holder's delete");
        drop(guard);

        let (status, body) = g.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "campaign is not active");
        assert_eq!(sector.delete_calls.load(Ordering::SeqCst), 1, "no second delete");
        assert_eq!(sector.get_calls.load(Ordering::SeqCst), 1, "the repair stopped at the campaign state");
        assert_eq!(campaign_of(&state, c).await.state, CampaignLifecycle::Archived);
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// An abandon during an in-flight launch waits for the sector write, then retires the
    /// launched campaign and deletes its sector: no orphan key (Reviewer BP-1c F2).
    #[tokio::test]
    async fn abandon_waits_for_an_inflight_launch() {
        let wallet = [97u8; 32];
        let (state, sector) = paused_state(wallet, true);
        let a = tokio::spawn(join(state.clone(), Some("t")));
        sector.parked.notified().await;
        let campaign = member_campaign(&state, &wallet).await;

        let (st, cs) = (state.clone(), campaign.campaign_id.to_string());
        let mut b = tokio::spawn(async move { abandon(st, Some("t"), &cs).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut b).await.is_err(),
            "the abandon answered while the launch was still writing the sector"
        );

        sector.release.notify_one();
        let (status_a, body_a) = a.await.unwrap();
        assert_eq!(status_a, StatusCode::CREATED, "{body_a}");
        let (status, body) = b.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, serde_json::json!({"campaign_id": campaign.campaign_id}));
        assert_eq!(campaign_of(&state, campaign.campaign_id).await.state, CampaignLifecycle::Archived);
        assert!(!sector.inner.sector_exists(campaign.sector_id).await.unwrap(), "no orphan sector");
        assert_eq!(sector.delete_calls.load(Ordering::SeqCst), 1, "the abandon's delete");
        assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
    }

    /// The GET is dropped while its repair waits on the lock (the client went away). The
    /// repair still runs once the lock frees: the campaign is retired and the lock released.
    #[tokio::test]
    async fn sector_repair_survives_a_dropped_request() {
        let wallet = [98u8; 32];
        let (state, sector) = paused_state(wallet, false);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let campaign = member_campaign(&state, &wallet).await;
        let c = campaign.campaign_id;
        sector.inner.delete_sector(campaign.sector_id).await.unwrap();

        let guard = state.launch_locks.lock(c).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), sector_get(state.clone(), Some("t"), c)).await.is_err(),
            "the GET answered while the lock was held"
        );
        drop(guard);

        let st = state.clone();
        let finished = eventually(move || {
            let st = st.clone();
            async move {
                let archived = st.campaign_repo.get_campaign(c).await.unwrap().map(|x| x.state) == Some(CampaignLifecycle::Archived);
                archived && st.launch_locks.len() == 0
            }
        })
        .await;
        assert!(finished, "the detached repair retired the campaign and released its lock");
    }

    /// The abandon is dropped while it waits on the lock. Its detached task still retires
    /// the campaign and deletes the sector once the lock frees.
    #[tokio::test]
    async fn abandon_survives_a_dropped_request() {
        let wallet = [99u8; 32];
        let (state, sector) = paused_state(wallet, false);
        let (status, body) = join(state.clone(), Some("t")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let campaign = member_campaign(&state, &wallet).await;
        let (c, s) = (campaign.campaign_id, campaign.sector_id);
        let cs = c.to_string();

        let guard = state.launch_locks.lock(c).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), abandon(state.clone(), Some("t"), &cs)).await.is_err(),
            "the abandon answered while the lock was held"
        );
        drop(guard);

        let (st, sc) = (state.clone(), sector.clone());
        let finished = eventually(move || {
            let (st, sc) = (st.clone(), sc.clone());
            async move {
                let archived = st.campaign_repo.get_campaign(c).await.unwrap().map(|x| x.state) == Some(CampaignLifecycle::Archived);
                archived && !sc.inner.sector_exists(s).await.unwrap() && st.launch_locks.len() == 0
            }
        })
        .await;
        assert!(finished, "the detached abandon retired the campaign, deleted the sector and released its lock");
    }
}
