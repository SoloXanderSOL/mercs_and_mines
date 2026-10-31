//! The ops routes (BP-8a-1 board, BP-8a-2 travel legs, BP-8b-1a the drill, BP-8b-1b the raid,
//! BP-8b-2 the haul, BP-10a the merc base;
//! **demo-only**;
//! Breakpoint plan §4d).
//! The `dev` port deletes this file with `demo_ops.rs`.
//!
//! Every route answers the same body (`OpsView`), so the client never polls: a `POST`
//! returns the whole state. Every route shares one `ops_ip` / `ops_wallet` budget.

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
use sim_engine::roster::ForemanSpecialization;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::demo_ops::{
    dig_threshold, ops_board, ops_time, raid_result, ActiveOpView, BaseBuilding, BuildRefusal, DigStart, DrillOutcome,
    DrillRefusal, OpsBase, OpsBoard, OpsSite, PullRefusal, ReturnRefusal, StartRefusal,
};
use crate::repository::{CampaignInstance, CampaignLifecycle, SectorState};
use crate::state::AppState;

use super::sector::GatewayHex;
use super::{
    bad_request, client_ip, conflict, forbidden, internal_error, not_found, service_unavailable, too_many_requests,
    unauthorized, AppError,
};

/// Request body cap for `ops/start` and `ops/build` (the two routes that read a body), layered on
/// each route: the overflow reaches the handler as a `JsonRejection` after the limiters, so it is
/// counted (`413`).
pub const OPS_BODY_LIMIT: usize = 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/:campaign_id/ops", get(get_ops_board))
        .route("/api/campaign/:campaign_id/ops/start", post(post_ops_start).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/return", post(post_ops_return))
        .route("/api/campaign/:campaign_id/ops/drill", post(post_ops_drill))
        .route("/api/campaign/:campaign_id/ops/pull", post(post_ops_pull))
        .route("/api/campaign/:campaign_id/ops/build", post(post_ops_build).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
}

/// The ops state as the client sees it, from every route. `now` is the server's clock, so the
/// client's crawl survives a phone whose clock is off. `ore_banked` is the campaign's banked ore
/// (BP-8b-2) and `base` its merc base's buildings (BP-10a), 0 and none built for a member with
/// no gateway.
#[derive(Serialize)]
pub struct OpsView<'a> {
    pub campaign_id: Uuid,
    pub gateway: Option<GatewayHex>,
    pub sites: &'a [OpsSite],
    pub active_op: Option<ActiveOpView>,
    pub ore_banked: u32,
    pub base: OpsBase,
    pub now: String,
}

#[derive(Deserialize)]
pub struct OpsStartRequest {
    pub site_id: String,
}

/// `building` is a string, matched after the member checks, so an unknown id is `400` as an
/// unknown site is (a serde enum would be axum's `422`, before them).
#[derive(Deserialize)]
pub struct OpsBuildRequest {
    pub building: String,
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
/// `Active`; the campaign's entry is dropped). Returns the campaign, the member's gateway and
/// the wallet's bytes.
async fn ops_member(
    state: &AppState,
    token: &SessionToken,
    campaign_id: Uuid,
    route: &str,
) -> Result<(CampaignInstance, Option<(i32, i32)>, Vec<u8>), AppError> {
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
    Ok((campaign, gateway, wallet))
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
    let (active_op, ore_banked, base) = state.demo_ops.store.ops_snapshot(campaign_id, now);
    let view = OpsView {
        campaign_id,
        gateway: Some(GatewayHex { q: gw.0, r: gw.1 }),
        sites: &board.sites,
        active_op,
        ore_banked,
        base,
        now: ops_time(now),
    };
    Json(view).into_response()
}

/// Latches a pending battle (a raid, BP-8b-1b, or a haul's ambush, BP-8b-2), run by every ops
/// route right after `load_board`. Nothing pending: nothing is read. Otherwise the store names the dig's Commander and the trigger; the
/// Commander's deployments are read with no store lock held (`list_deployments_by_commander`,
/// one sequential scan, `500` on failure); and the store applies the fight's result only if the
/// trigger is unchanged. A fight not yet started or still `active` leaves the battle pending.
async fn latch_pending_raid(state: &AppState, campaign_id: Uuid, now: DateTime<Utc>, route: &str) -> Result<(), AppError> {
    let Some(pending) = state.demo_ops.store.pending_raid(campaign_id, now) else {
        return Ok(());
    };
    let commander_id = pending.commander_id;
    let deps = state.commander_repo.list_deployments_by_commander(commander_id).await.map_err(|e| {
        internal_error(format!("{route}: list_deployments_by_commander failed for commander {commander_id} of campaign {campaign_id}: {e}"))
    })?;
    let result = raid_result(&deps, &pending.trigger)
        .map_err(|e| internal_error(format!("{route}: raid of campaign {campaign_id}: {e}")))?;
    if let Some(result) = result {
        state.demo_ops.store.latch_raid(campaign_id, &pending.trigger, result, now);
    }
    Ok(())
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
/// - A pending raid is latched after the board (`latch_pending_raid`; `500` on a failed read).
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
    let (campaign, gateway, _) = ops_member(&state, &token, campaign_id, "get_ops_board").await?;
    let Some(gw) = gateway else {
        let view = OpsView { campaign_id, gateway: None, sites: &[], active_op: None, ore_banked: 0, base: OpsBase::default(), now: ops_time(now) };
        return Ok(Json(view).into_response());
    };
    let board = load_board(&state, &campaign, gw, now, false, "get_ops_board").await?;
    latch_pending_raid(&state, campaign_id, now, "get_ops_board").await?;
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Sends the team to a site, `POST /api/campaign/:campaign_id/ops/start {site_id}` (BP-8a-2,
/// demo-only). Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → axum's body rejection,
/// counted (`413` over `OPS_BODY_LIMIT`; `400`, `415`, `422`) → `404` → `403` → `409` not
/// `Active` (entry dropped) → `409` no gateway → the board, a hit proven by `EXISTS` (a lost
/// sector is repaired: `409` once retired, entry dropped) → a pending raid latched (`500` on a
/// failed read) → `400` unknown site → for a dig only, the Crew, Foreman and Commander reads
/// (`500` on failure), `409` no Crew left and `409` no Commander left → `409` an op is under
/// way → `503` the store is full of ops → `200`, the whole state.
/// - The leg: `depart_at = now`, `arrive_at = now + the site's travel_secs`.
/// - The op's seed is drawn here from OS entropy and handed to the store, which never sends it;
///   the drill rolls from it.
/// - A dig reads its Crew, threshold and Commander once, here (`dig_start`); a contract reads none.
/// - The latch runs here too, so a raid already fought (a Wipeout clears the op) frees the start
///   without waiting for a board read.
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
    let (campaign, gateway, wallet) = ops_member(&state, &token, campaign_id, "post_ops_start").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no gateway assigned"));
    };
    let board = load_board(&state, &campaign, gw, now, true, "post_ops_start").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_start").await?;
    let Some(site) = board.sites.iter().find(|s| s.site_id == req.site_id) else {
        return Err(bad_request("unknown site"));
    };
    let dig = if site.kind == "dig" { Some(dig_start(&state, campaign_id, &wallet).await?) } else { None };
    let op_seed = generate_seed();
    match state.demo_ops.store.start_op(campaign_id, gw, Arc::clone(&board), site, op_seed, dig, now) {
        Ok(()) => {}
        Err(StartRefusal::Busy) => return Err(conflict("an op is already under way")),
        Err(StartRefusal::Full) => return Err(service_unavailable("the ops board is full")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// A dig's Crew, threshold (BP-8b-1a) and Commander (BP-8b-1b). The Crew is the member's first
/// by the lister's order (a demo member holds one, the join's); `409` when it has no heads left,
/// since only a new run brings a fresh one. The first Foreman's specialization sets the threshold
/// (`dig_threshold`); no Foreman means the plain threshold. The Commander, who fights the dig's
/// raids, is the first usable one (not KIA, not Shattered: the mission route's refusals); a
/// member holds at most one, so the client's raid fight picks the same. `409` when none is
/// usable. The Crew is checked first, so a member with neither hears about the Crew.
async fn dig_start(state: &AppState, campaign_id: Uuid, wallet: &[u8]) -> Result<DigStart, AppError> {
    let crews = state.crew_repo.list_crews_for_member(campaign_id, wallet).await.map_err(|e| {
        internal_error(format!("post_ops_start: list_crews_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let Some(crew) = crews.first().filter(|c| c.headcount > 0) else {
        return Err(conflict("no Crew left to dig: start a new run"));
    };
    let foremen = state.foreman_repo.list_foremen_for_member(campaign_id, wallet).await.map_err(|e| {
        internal_error(format!("post_ops_start: list_foremen_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let coolant = foremen.first().is_some_and(|f| f.specialization == ForemanSpecialization::CoolantJockey.as_str());
    let commanders = state.commander_repo.list_commanders_for_member(campaign_id, wallet).await.map_err(|e| {
        internal_error(format!("post_ops_start: list_commanders_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let Some(commander) = commanders.iter().find(|c| !c.is_kia && !c.is_shattered) else {
        return Err(conflict("no Commander left to lead: start a new run"));
    };
    Ok(DigStart { crew_id: crew.crew_id, commander_id: commander.commander_id, threshold: dig_threshold(coolant) })
}

/// Brings the team home, `POST /api/campaign/:campaign_id/ops/return` (BP-8a-2, demo-only).
/// No body is read. Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → `404` → `403` →
/// `409` not `Active` (entry dropped) → `409` no op under way (also: no gateway) → the board
/// → a pending raid latched → `409` no op under way → `409` already heading home → `409` still
/// travelling → `409` raiders at the site → `200`, the whole state.
/// - The homeward leg takes the site's `travel_secs` again (§4d: "each way"), not the
///   gateway's terrain: every gateway is City-State (×0.5), which would halve every trip home.
/// - From a dig the team walks away and the ore on site is lost (`ended_by: walked_away`,
///   BP-8b-1a); BP-8b-2's pull is the loaded return.
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
    let (campaign, gateway, _) = ops_member(&state, &token, campaign_id, "post_ops_return").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no op under way"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_return").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_return").await?;
    match state.demo_ops.store.return_op(campaign_id, now) {
        Ok(()) => {}
        Err(ReturnRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(ReturnRefusal::NotArrived) => return Err(conflict("still travelling")),
        Err(ReturnRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
        Err(ReturnRefusal::RaidPending) => return Err(conflict("raiders at the site")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// One drill on a dig, `POST /api/campaign/:campaign_id/ops/drill` (BP-8b-1a, demo-only). No
/// body is read. Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → `404` → `403` → `409`
/// not `Active` (entry dropped) → `409` no op under way (also: no gateway) → the board → a
/// pending raid latched → `409` no op under way → `409` not a dig site → `409` already heading
/// home → `409` still travelling → `409` raiders at the site → `200`, the whole state.
/// - A drill that brings raiders answers `200` with `dig.raid` set; the client opens the fight.
/// - No DB read unless the drill melts down: the Crew and the threshold were read at `ops/start`.
/// - **A meltdown** is committed in the store first (the team is already heading home, so a
///   second drill can't lose the heads twice). Then, with the store's lock released and after
///   `load_board` has returned (its sector repair takes the launch lock itself), the route takes
///   the launch lock and writes the Crew's loss (`CrewLoss::lose_heads`; Reviewer BP-8a-2 CF5).
///   A failed write is `500` and the op stays melted: the team still goes home empty and the
///   Crew keeps its heads, which the journal records.
async fn post_ops_drill(
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
    let (campaign, gateway, _) = ops_member(&state, &token, campaign_id, "post_ops_drill").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no op under way"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_drill").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_drill").await?;
    match state.demo_ops.store.drill_op(campaign_id, now) {
        Ok(DrillOutcome::Drilled) => {}
        Ok(DrillOutcome::Meltdown { crew_id, heads }) => {
            let _launch = state.launch_locks.lock(campaign_id).await;
            if let Err(e) = state.crew_loss.lose_heads(crew_id, heads).await {
                return Err(internal_error(format!(
                    "post_ops_drill: lose_heads({heads}) failed for crew {crew_id} of campaign {campaign_id}: {e}"
                )));
            }
        }
        Err(DrillRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(DrillRefusal::Contract) => return Err(conflict("not a dig site")),
        Err(DrillRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
        Err(DrillRefusal::NotArrived) => return Err(conflict("still travelling")),
        Err(DrillRefusal::RaidPending) => return Err(conflict("raiders at the site")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Hauls the ore home, `POST /api/campaign/:campaign_id/ops/pull` (BP-8b-2, demo-only). No body
/// is read. Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → `404` → `403` → `409` not
/// `Active` (entry dropped) → `409` no op under way (also: no gateway) → the board → a pending
/// raid latched → `409` no op under way → `409` not a dig site → `409` already heading home →
/// `409` still travelling → `409` nothing to haul → `409` raiders at the site → `200`, the whole
/// state.
/// - The ore on site rides home (`carried`, `ended_by: pulled`) on a leg as long as the way
///   out; the arrival banks it into `ore_banked` on the first ops call after it.
/// - The ambush is rolled here and stored, never sent: the body of an ambushed haul reads like
///   a clear one until the ambush fires at the leg's midpoint (`dig.ambush`).
/// - Memory only, no DB write, so no launch lock (Reviewer BP-8a-2 CF5 covers durable writes).
async fn post_ops_pull(
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
    let (campaign, gateway, _) = ops_member(&state, &token, campaign_id, "post_ops_pull").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no op under way"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_pull").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_pull").await?;
    match state.demo_ops.store.pull_op(campaign_id, now) {
        Ok(()) => {}
        Err(PullRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(PullRefusal::Contract) => return Err(conflict("not a dig site")),
        Err(PullRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
        Err(PullRefusal::NotArrived) => return Err(conflict("still travelling")),
        Err(PullRefusal::NothingToHaul) => return Err(conflict("nothing to haul: return instead")),
        Err(PullRefusal::RaidPending) => return Err(conflict("raiders at the site")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Builds at the merc base, `POST /api/campaign/:campaign_id/ops/build {building}` (BP-10a,
/// demo-only). Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → axum's body rejection,
/// counted (`413` over `OPS_BODY_LIMIT`; `400`, `415`, `422`) → `404` → `403` → `409` not `Active`
/// (entry dropped) → `409` no gateway → the board → a pending raid latched → `400` unknown building
/// → `409` the team is out → `409` already built → `409` not enough banked ore → `200`, the whole
/// state.
/// - The cost is debited from `ore_banked` and `base` names what stands. A haul that has come
///   home banks first, so its ore can pay on the same call.
/// - Never while an op is out (a contract until its homeward leg lands, a haul still on the
///   road), so no base number changes under a live op and a haul's ambush stays as the pull
///   rolled it. A pending raid means an op is out, so no `RaidPending` arm is needed.
/// - Memory only, no DB write, so no launch lock (Reviewer BP-8a-2 CF5 covers durable writes).
async fn post_ops_build(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
    body: Result<Json<OpsBuildRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    if let Some(r) = ops_limits(&state, &token, ci, &headers)? {
        return Ok(r);
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return Ok(rej.into_response()),
    };
    let now = Utc::now();
    let (campaign, gateway, _) = ops_member(&state, &token, campaign_id, "post_ops_build").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no gateway assigned"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_build").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_build").await?;
    let Some(building) = BaseBuilding::from_id(&req.building) else {
        return Err(bad_request("unknown building"));
    };
    match state.demo_ops.store.build_op(campaign_id, building, now) {
        Ok(()) => {}
        Err(BuildRefusal::OpOut) => return Err(conflict("the team is out: build when they're home")),
        Err(BuildRefusal::AlreadyBuilt) => return Err(conflict("already built")),
        Err(BuildRefusal::NotEnoughOre) => return Err(conflict("not enough banked ore")),
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
        assert_eq!(keys, ["active_op", "base", "campaign_id", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
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
        assert_eq!(keys, ["active_op", "base", "campaign_id", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
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

    const DIG: &str = "dig:11,-4";

    /// The member's Crew (8 heads), Foreman and Commander, as a join writes them; the Foreman is
    /// the Coolant-Jockey or, when `coolant` is false, the first other specialization. Returns the Crew.
    async fn seed_crew(state: &AppState, campaign: Uuid, coolant: bool) -> Uuid {
        let mut foreman = crate::foreman_gen::generate_foreman(WALLET.to_vec(), campaign, 7);
        let other = ForemanSpecialization::ALL.iter().map(|s| s.as_str()).find(|s| *s != "Coolant-Jockey").unwrap();
        foreman.specialization = if coolant { "Coolant-Jockey" } else { other }.to_string();
        state.foreman_repo.create_foreman(foreman).await.expect("create_foreman");
        let crew = crate::repository::CrewRecord::new_hire(campaign, WALLET.to_vec());
        let id = crew.crew_id;
        state.crew_repo.create_crew(crew).await.expect("create_crew");
        let commander = crate::commander_gen::generate_commander(WALLET.to_vec(), campaign, 42);
        state.commander_repo.create_commander(commander).await.expect("create_commander");
        id
    }

    async fn headcount(state: &AppState, crew: Uuid) -> i16 {
        state.crew_repo.get_crew(crew).await.unwrap().expect("the Crew").headcount
    }

    /// A dig at `DIG`, on site (its arrival crossed) with the seed `seed`.
    async fn dig_on_site(state: &Arc<AppState>, campaign: Uuid, seed: u32) {
        let (status, started, _) = start(state, campaign, DIG).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        state.demo_ops.store.rewind_op(campaign, site_secs(&started, DIG));
        state.demo_ops.store.set_op_seed(campaign, seed);
    }

    async fn drill(state: &Arc<AppState>, campaign: Uuid) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "drill", None).await
    }

    /// A dig campaign: `Active`, the sector seeded, the Crew, a Foreman and a Commander. Returns
    /// the campaign and the Crew.
    async fn dig_campaign(state: &Arc<AppState>, coolant: bool) -> (Uuid, Uuid) {
        let c = campaign_with(state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(state, &c).await;
        let crew = seed_crew(state, c.campaign_id, coolant).await;
        (c.campaign_id, crew)
    }

    /// The drill's refusals in its order, on a contract and on a dig.
    #[tokio::test]
    async fn ops_drill_refusals() {
        let state = Arc::new(session_state());
        let contract = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &contract).await;
        let (status, body, _) = drill(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));
        assert_eq!(start(&state, contract.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = drill(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not a dig site")));
        let (c, _) = dig_campaign(&state, false).await;
        let (status, started, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("still travelling")));
        state.demo_ops.store.rewind_op(c, site_secs(&started, DIG));
        assert_eq!(post(&state, c, "return", None).await.0, StatusCode::OK);
        let (status, body, _) = drill(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
    }

    /// Three drills through the route: heat 15 per drill and ore 10 × n on the tier-1 site,
    /// the dig's exact keys, and no seed or Crew in the body.
    #[tokio::test]
    async fn ops_drill_adds_heat_and_ore() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 2).await;
        let mut seen = Vec::new();
        let mut last = Vec::new();
        for _ in 0..3 {
            let (status, body, bytes) = drill(&state, c).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let d = &body["active_op"]["dig"];
            seen.push((d["drills"].as_u64(), d["heat"].as_u64(), d["ore_on_site"].as_u64(), d["next_ore"].as_u64(), d["threshold"].as_u64()));
            assert!(d["ended_by"].is_null());
            last = bytes;
        }
        assert_eq!(seen, vec![(Some(1), Some(15), Some(10), Some(20), Some(100)), (Some(2), Some(30), Some(30), Some(30), Some(100)), (Some(3), Some(45), Some(60), Some(40), Some(100))]);
        let text = String::from_utf8_lossy(&last);
        assert!(!text.contains("seed") && !text.contains("crew"), "{text}");
    }

    /// Seed 2, threshold 100: the 7th drill melts down, the team heads home empty and the Crew
    /// loses 3 heads (8 → 5) under a launch lock that is released afterwards.
    #[tokio::test]
    async fn ops_meltdown_on_the_7th_costs_the_crew_3_heads() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 2).await;
        for i in 1..=6 {
            let (status, body, _) = drill(&state, c).await;
            assert_eq!(status, StatusCode::OK, "drill {i}: {body}");
            assert_eq!(body["active_op"]["leg"], "outbound", "drill {i}");
        }
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert_eq!(op["leg"], "homeward");
        assert_eq!(op["depart_at"], body["now"]);
        assert_eq!((op["dig"]["heat"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(105), Some(0)));
        assert_eq!(op["dig"]["ended_by"], "meltdown");
        assert_eq!(headcount(&state, crew).await, 5);
        assert_eq!(state.launch_locks.len(), 0, "the launch lock is released");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
        assert_eq!(headcount(&state, crew).await, 5, "the heads are lost once");
    }

    /// A Coolant-Jockey Foreman: threshold 115, so seed 2's 7th drill digs and the 8th melts
    /// down, costing 1 head (8 → 7).
    #[tokio::test]
    async fn ops_coolant_jockey_melts_down_on_the_8th() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, true).await;
        dig_on_site(&state, c, 2).await;
        for i in 1..=7 {
            let (status, body, _) = drill(&state, c).await;
            assert_eq!(status, StatusCode::OK, "drill {i}: {body}");
            assert_eq!(body["active_op"]["dig"]["threshold"], 115, "drill {i}");
            assert_eq!(body["active_op"]["leg"], "outbound", "drill {i}");
        }
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["dig"]["ended_by"], "meltdown");
        assert_eq!(headcount(&state, crew).await, 7);
    }

    /// A dig start needs a Crew with heads: none, then one at 0, are both `409`, and no op starts.
    /// A contract start needs neither (every contract test above runs with no Crew).
    #[tokio::test]
    async fn ops_dig_start_needs_a_crew_with_heads() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = start(&state, c.campaign_id, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Crew left to dig: start a new run")));
        let crew = seed_crew(&state, c.campaign_id, false).await;
        state.crew_loss.lose_heads(crew, 8).await.expect("lose_heads");
        let (status, body, _) = start(&state, c.campaign_id, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Crew left to dig: start a new run")));
        assert!(state.demo_ops.store.op_view(c.campaign_id, chrono::Utc::now()).is_none());
    }

    struct FailingCrewLoss;

    #[async_trait::async_trait]
    impl crate::demo_crew_loss::CrewLoss for FailingCrewLoss {
        async fn lose_heads(&self, _: Uuid, _: i16) -> Result<i16, RepositoryError> {
            Err(RepositoryError::Internal("db down".into()))
        }
    }

    /// A failed Crew write is `500`; the op stays melted and the Crew keeps its heads.
    #[tokio::test]
    async fn ops_meltdown_write_failure_is_500_and_the_op_stays_melted() {
        let mut state = session_state();
        state.crew_loss = Arc::new(FailingCrewLoss);
        let state = Arc::new(state);
        let (c, crew) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 2).await;
        for _ in 1..=6 {
            assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        }
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        let op = state.demo_ops.store.op_view(c, chrono::Utc::now()).expect("op");
        assert_eq!(op.leg, crate::demo_ops::OpLeg::Homeward);
        assert_eq!(op.dig.expect("a dig").ended_by, Some(crate::demo_ops::OpEnd::Meltdown));
        assert_eq!(headcount(&state, crew).await, 8);
        assert_eq!(state.launch_locks.len(), 0);
    }

    /// F6 through the route: a return after two drills walks away from the ore on site.
    #[tokio::test]
    async fn ops_return_after_a_drill_walks_away_from_the_ore() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 2).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = post(&state, c, "return", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert_eq!(op["leg"], "homeward");
        assert_eq!((op["dig"]["drills"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(2), Some(0)));
        assert_eq!(op["dig"]["ended_by"], "walked_away");
        assert_eq!(headcount(&state, crew).await, 8, "walking away costs no heads");
    }

    /// The member's only Commander (the one `seed_crew` writes).
    async fn commander_of(state: &AppState, campaign: Uuid) -> Uuid {
        let all = state.commander_repo.list_commanders_for_member(campaign, &WALLET).await.expect("list");
        assert_eq!(all.len(), 1);
        all[0].commander_id
    }

    /// The raid's fight as the mission route records it, started now (after the trigger): the
    /// deployment opened and resolved with `outcome`, which on a Wipeout also marks the Commander
    /// KIA. The route can't choose an outcome (its seed is OS entropy), so the record is written
    /// through the repository (spec check F1).
    async fn fight(state: &AppState, campaign: Uuid, outcome: sim_engine::game_types::OutcomeType) -> Uuid {
        let commander = commander_of(state, campaign).await;
        let d = crate::repository::NewDeployment {
            deployment_id: Uuid::new_v4(),
            commander_id: commander,
            session_id: Uuid::new_v4(),
            mission_id: "GUARD_DUTY".into(),
            started_at: chrono::Utc::now(),
            player_wallet: WALLET.to_vec(),
            campaign_id: campaign,
            section_ids: vec![],
        };
        assert_eq!(state.commander_repo.start_deployment(&d).await.expect("start"), crate::repository::DeploymentStart::Started);
        let cfg = sim_engine::config::SimConfig::default();
        state.commander_repo.resolve_deployment(d.deployment_id, commander, 0, 1, 0.0, &outcome, &cfg).await.expect("resolve");
        commander
    }

    /// Seed 7 raids on drill 1. Drill and return are refused while the raid waits (a board read
    /// latches nothing before the fight); a won fight is latched on the next read and drilling
    /// resumes.
    #[tokio::test]
    async fn ops_raid_blocks_drill_and_return_until_the_fight_is_won() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 7).await;
        let (status, body, bytes) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let d = &body["active_op"]["dig"];
        assert_eq!(d["raid"], serde_json::json!({"mission_id": "GUARD_DUTY"}));
        assert!(d["next_raid_pct"].is_null());
        assert_eq!((d["heat"].as_u64(), d["ore_on_site"].as_u64()), (Some(15), Some(10)));
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("trigger") && !text.contains("commander"), "{text}");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("raiders at the site")));
        let (status, body, _) = post(&state, c, "return", None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("raiders at the site")));
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["dig"]["raid"]["mission_id"], "GUARD_DUTY", "no fight yet");
        fight(&state, c, sim_engine::game_types::OutcomeType::Success).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let d = &body["active_op"]["dig"];
        assert!(d["raid"].is_null(), "{body}");
        assert_eq!(d["next_raid_pct"], 10);
        assert_eq!(body["active_op"]["leg"], "outbound");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["dig"]["drills"], 2);
    }

    /// A lost raid: the team heads home from the read that latches it, with half the ore on site.
    #[tokio::test]
    async fn ops_raid_defeat_sends_the_team_home_with_half_the_ore() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        fight(&state, c, sim_engine::game_types::OutcomeType::Defeat).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert_eq!(op["leg"], "homeward");
        assert_eq!(op["depart_at"], body["now"]);
        assert_eq!(op["dig"]["ended_by"], "retreat");
        assert_eq!((op["dig"]["carried"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(5), Some(0)));
        let (status, body, _) = drill(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
        assert_eq!(headcount(&state, crew).await, 8, "a retreat costs no heads");
    }

    /// A Wipeout: the op is cleared on the next call (here the start itself), and with the only
    /// Commander KIA the next dig is refused.
    #[tokio::test]
    async fn ops_raid_wipeout_ends_the_op_and_the_next_dig_needs_a_commander() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let commander = fight(&state, c, sim_engine::game_types::OutcomeType::Wipeout).await;
        assert!(state.commander_repo.get_commander(commander).await.unwrap().expect("the Commander").is_kia);
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Commander left to lead: start a new run")));
        assert!(state.demo_ops.store.op_view(c, chrono::Utc::now()).is_none(), "the latch cleared the op");
    }

    /// A dig start needs a usable Commander: a KIA one, then a Shattered one too, are refused; a
    /// third, usable one (listed last) is found.
    #[tokio::test]
    async fn ops_dig_start_needs_a_usable_commander() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        state.crew_repo.create_crew(crate::repository::CrewRecord::new_hire(c.campaign_id, WALLET.to_vec())).await.expect("create_crew");
        let mut kia = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 1);
        kia.is_kia = true;
        state.commander_repo.create_commander(kia).await.expect("create_commander");
        let (status, body, _) = start(&state, c.campaign_id, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Commander left to lead: start a new run")));
        let mut shattered = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 2);
        shattered.is_shattered = true;
        state.commander_repo.create_commander(shattered).await.expect("create_commander");
        let (status, body, _) = start(&state, c.campaign_id, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Commander left to lead: start a new run")));
        assert!(state.demo_ops.store.op_view(c.campaign_id, chrono::Utc::now()).is_none());
        let usable = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 3);
        state.commander_repo.create_commander(usable).await.expect("create_commander");
        let (status, body, _) = start(&state, c.campaign_id, DIG).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn pull(state: &Arc<AppState>, campaign: Uuid) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "pull", None).await
    }

    /// `DIG`'s one-way time, from a board read.
    async fn dig_secs(state: &Arc<AppState>, campaign: Uuid) -> i64 {
        let (status, body) = get(state, Some("t"), campaign).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        site_secs(&body, DIG)
    }

    /// The pull's refusals in its order, on a contract and on a dig, then a pull that goes
    /// through (seed 2, one drill: 10 ore).
    #[tokio::test]
    async fn ops_pull_refusals() {
        let state = Arc::new(session_state());
        let contract = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &contract).await;
        let (status, body, _) = pull(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));
        assert_eq!(start(&state, contract.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not a dig site")));
        let (c, _) = dig_campaign(&state, false).await;
        let (status, started, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let (status, body, _) = pull(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("still travelling")));
        state.demo_ops.store.rewind_op(c, site_secs(&started, DIG));
        state.demo_ops.store.set_op_seed(c, 2);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("nothing to haul: return instead")));
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert_eq!((op["leg"].as_str(), op["dig"]["ended_by"].as_str()), (Some("homeward"), Some("pulled")));
        assert_eq!((op["dig"]["carried"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(10), Some(0)));
        assert_eq!(op["depart_at"], body["now"]);
        assert_eq!(secs_between(op), site_secs(&body, DIG), "the haul takes the site's time");
        let (status, body, _) = pull(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
    }

    /// Reviewer BP-8b-1b F4 through the routes: seed 7's raid is lost, the team retreats with 5
    /// ore, and the first read after the arrival banks it.
    #[tokio::test]
    async fn ops_retreat_banks_carried_on_the_arrival_read() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        fight(&state, c, sim_engine::game_types::OutcomeType::Defeat).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["active_op"]["dig"]["carried"].as_u64(), body["ore_banked"].as_u64()), (Some(5), Some(0)));
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!(body["ore_banked"], 5);
    }

    /// Seed 2 (never ambushed): three drills, a pull of 60 ore, banked by the first read after the
    /// arrival; the bank survives the next dig's start.
    #[tokio::test]
    async fn ops_pull_banks_on_the_arrival_read() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 2).await;
        for _ in 0..3 {
            assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        }
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["active_op"]["dig"]["carried"].as_u64(), body["ore_banked"].as_u64()), (Some(60), Some(0)));
        assert!(body["active_op"]["dig"]["ambush"].is_null());
        let (_, body) = get(&state, Some("t"), c).await;
        assert_eq!((body["active_op"]["arrived"].as_bool(), body["ore_banked"].as_u64()), (Some(false), Some(0)), "on the road");
        state.demo_ops.store.rewind_op(c, dig_secs(&state, c).await);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!(body["ore_banked"], 60);
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ore_banked"], 60, "the bank outlives the op");
    }

    /// Raiders can't be outrun with the ore: seed 7's raid refuses the pull, and nothing moves.
    #[tokio::test]
    async fn ops_pull_while_raiders_wait_is_409() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("raiders at the site")));
        let (_, body) = get(&state, Some("t"), c).await;
        assert_eq!(body["active_op"]["leg"], "outbound");
        assert_eq!((body["active_op"]["dig"]["ore_on_site"].as_u64(), body["active_op"]["dig"]["carried"].as_u64()), (Some(10), Some(0)));
    }

    /// Seed 14 (ambush d100 4): the pull's body shows no ambush; the read after the midpoint fires
    /// it and the team waits on the road; a won fight is latched by the next read and the leg
    /// resumes; the read after its arrival banks all 30 ore.
    #[tokio::test]
    async fn ops_ambush_is_fought_then_the_haul_banks() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 14).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, bytes) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"]["dig"]["ambush"].is_null(), "never sent before it fires");
        assert!(!String::from_utf8_lossy(&bytes).contains("ambush_at"));
        let secs = site_secs(&body, DIG);
        state.demo_ops.store.rewind_op(c, secs);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert_eq!(op["dig"]["ambush"], serde_json::json!({"mission_id": "GUARD_DUTY"}));
        assert!(op["dig"]["raid"].is_null());
        assert_eq!((op["leg"].as_str(), op["arrived"].as_bool(), body["ore_banked"].as_u64()), (Some("homeward"), Some(false), Some(0)));
        fight(&state, c, sim_engine::game_types::OutcomeType::Success).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert!(op["dig"]["ambush"].is_null(), "{body}");
        assert_eq!((op["arrived"].as_bool(), op["dig"]["carried"].as_u64()), (Some(false), Some(30)));
        state.demo_ops.store.rewind_op(c, secs);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!(body["ore_banked"], 30);
    }

    /// Seed 14's ambush ends in a Wipeout: the op is cleared, nothing is banked, and the next dig
    /// needs a Commander.
    #[tokio::test]
    async fn ops_ambush_wipeout_banks_nothing() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 14).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let (_, body) = get(&state, Some("t"), c).await;
        assert_eq!(body["active_op"]["dig"]["ambush"]["mission_id"], "GUARD_DUTY");
        fight(&state, c, sim_engine::game_types::OutcomeType::Wipeout).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "{body}");
        assert_eq!(body["ore_banked"], 0);
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Commander left to lead: start a new run")));
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

    async fn build(state: &Arc<AppState>, campaign: Uuid, building: &str) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "build", Some(&serde_json::json!({ "building": building }).to_string())).await
    }

    /// Seed 2's 60-ore haul lands; the build's own call banks it and pays the Radio Shack's 50,
    /// and `base` shows it. Then already built, then short of ore; a board read shows the same.
    #[tokio::test]
    async fn ops_build_spends_the_bank_and_shows_the_base() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 2).await;
        for _ in 0..3 {
            assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        }
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let none = serde_json::json!({"radio_shack": false, "hobbyist_benches": false, "grease_pit": false, "gunsmith": false});
        assert_eq!(body["base"], none);
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let (status, body, _) = build(&state, c, "radio_shack").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        let shack = serde_json::json!({"radio_shack": true, "hobbyist_benches": false, "grease_pit": false, "gunsmith": false});
        assert_eq!((body["ore_banked"].as_u64(), &body["base"]), (Some(10), &shack));
        let (status, body, _) = build(&state, c, "radio_shack").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already built")));
        let (status, body, _) = build(&state, c, "gunsmith").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not enough banked ore")));
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["ore_banked"].as_u64(), &body["base"]), (Some(10), &shack));
    }

    /// The build's refusals in its order: no gateway; an unknown building, after the member
    /// checks; the team out on a contract, before anything about the bank (0 banked here).
    #[tokio::test]
    async fn ops_build_refusals() {
        let state = Arc::new(session_state());
        let no_gw = campaign_with(&state, WALLET, true, None).await;
        let (status, body, _) = build(&state, no_gw.campaign_id, "radio_shack").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no gateway assigned")));
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = build(&state, c.campaign_id, "barracks").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown building")));
        assert_eq!(start(&state, c.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = build(&state, c.campaign_id, "radio_shack").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("the team is out: build when they're home")));
        let (_, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(body["active_op"]["site_id"], SITE);
        assert_eq!(body["ore_banked"], 0);
    }

    /// Axum's rejections on the build's body, after the limiters and before the campaign is read
    /// (the id is unknown, so anything past the body would be `404`).
    #[tokio::test]
    async fn ops_build_body_rejections() {
        let state = Arc::new(session_state());
        let c = Uuid::new_v4();
        let big = format!("{{\"building\":\"{}\"}}", "x".repeat(2048));
        assert_eq!(post(&state, c, "build", Some(&big)).await.0, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(post(&state, c, "build", Some("{")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(post(&state, c, "build", None).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(post(&state, c, "build", Some("{}")).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
