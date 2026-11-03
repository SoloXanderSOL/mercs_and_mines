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
use sim_engine::game_types::{BattleReport, LootDrop, Squad};
use sim_engine::missions::missions;
use sim_engine::resolver::resolve_mission_with_modifiers;
use sim_engine::rng::generate_seed;
use sim_engine::roster::ForemanSpecialization;
use uuid::Uuid;

use crate::api_types::MissionResolveRequest;
use crate::auth::AuthSession;
use crate::demo_ops::{
    dig_threshold, fight_losses, fight_wounded, ops_board, ops_time, raid_fight, raid_result, ActiveOpView, BaseBuilding,
    BuildRefusal, CompanyView, ContractStart, CrewFightView, DigStart, DrillOutcome, DrillRefusal, EscortRefusal, FightPay,
    HealRefusal, NextRefusal, OpsBase, OpsBoard, OpsSite, PullRefusal, RaidResult, ReplaceHold, ReplaceRefusal,
    ReturnRefusal, StartRefusal, DEMO_CREW_FIGHT_MORALE_LOSS, DEMO_MELTDOWN_MORALE_LOSS, DEMO_REPLACE_MERC_CREDITS,
    DEMO_REPLACE_MINER_CREDITS,
};
use crate::repository::{outcome_db_str, CampaignInstance, CampaignLifecycle, DeploymentRecord, SectorState};
use crate::state::AppState;

use super::sector::GatewayHex;
use super::{
    bad_request, client_ip, conflict, forbidden, internal_error, not_found, service_unavailable, too_many_requests,
    unauthorized, AppError,
};

/// Request body cap for `ops/start`, `ops/next`, `ops/build`, `ops/heal` and `ops/replace`
/// (the five routes that read a body), layered on each route: the overflow reaches the handler
/// as a `JsonRejection` after the limiters, so it is counted (`413`).
pub const OPS_BODY_LIMIT: usize = 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/campaign/:campaign_id/ops", get(get_ops_board))
        .route("/api/campaign/:campaign_id/ops/start", post(post_ops_start).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/return", post(post_ops_return))
        .route("/api/campaign/:campaign_id/ops/next", post(post_ops_next).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/drill", post(post_ops_drill))
        .route("/api/campaign/:campaign_id/ops/pull", post(post_ops_pull))
        .route("/api/campaign/:campaign_id/ops/build", post(post_ops_build).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/heal", post(post_ops_heal).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/replace", post(post_ops_replace).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)))
        .route("/api/campaign/:campaign_id/ops/escort", post(post_ops_escort))
}

/// The ops state as the client sees it, from every route. `now` is the server's clock, so the
/// client's crawl survives a phone whose clock is off. `ore_banked` is the campaign's banked ore
/// (BP-8b-2), `credits_banked` its banked pay (BP-11a-2) and `base` its merc base's buildings
/// (BP-10a), 0 and none built for a member with no gateway; and `company` the sickbays and the
/// Trophy Shelf (BP-12a-1; empty for a member with no gateway).
#[derive(Serialize)]
pub struct OpsView<'a> {
    pub campaign_id: Uuid,
    pub gateway: Option<GatewayHex>,
    pub sites: &'a [OpsSite],
    pub active_op: Option<ActiveOpView>,
    pub ore_banked: u32,
    pub credits_banked: u32,
    pub base: OpsBase,
    pub company: CompanyView,
    pub now: String,
    /// The last Crew fight (BP-17a), until the next start; absent when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew_fight: Option<CrewFightView>,
}

/// `ops/start`'s and `ops/next`'s body. `escort` (BP-17a) is read by `ops/start` for a dig only,
/// absent = true; `ops/next` always escorts (a hop is the team moving).
#[derive(Deserialize)]
pub struct OpsStartRequest {
    pub site_id: String,
    pub escort: Option<bool>,
}

/// `building` is a string, matched after the member checks, so an unknown id is `400` as an
/// unknown site is (a serde enum would be axum's `422`, before them).
#[derive(Deserialize)]
pub struct OpsBuildRequest {
    pub building: String,
}

/// `section_id` is a string, parsed after the member checks, so a bad id is `400` as an
/// unknown building is (BP-12a-1).
#[derive(Deserialize)]
pub struct OpsHealRequest {
    pub section_id: String,
}

/// `ops/replace`'s body (BP-12a-2): exactly one of `section_id` / `crew_id`, strings parsed after
/// the member checks (as heal's); `count` an `i64`, so every out-of-range number is our `400`,
/// never axum's `422`.
#[derive(Deserialize)]
pub struct OpsReplaceRequest {
    pub section_id: Option<String>,
    pub crew_id: Option<String>,
    pub count: i64,
}

/// The row a replace adds to (BP-12a-2).
#[derive(Clone, Copy, Debug)]
enum ReplaceTarget {
    Section(Uuid),
    Crew(Uuid),
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
    let s = state.demo_ops.store.ops_state(campaign_id, now);
    // BP-11a-3: the sites as they stand (a fought contract off the board, a returned one moved);
    // the route's own board only when the campaign has no entry (a store full of ops).
    let sites = s.sites.as_deref().unwrap_or(&board.sites);
    let view = OpsView {
        campaign_id,
        gateway: Some(GatewayHex { q: gw.0, r: gw.1 }),
        sites,
        active_op: s.op,
        ore_banked: s.ore_banked,
        credits_banked: s.credits_banked,
        base: s.base,
        company: s.company,
        now: ops_time(now),
        crew_fight: s.crew_fight,
    };
    Json(view).into_response()
}

/// Rebuilds a fight's `BattleReport` from the server's own records (BP-11a-1, Linux Q2 (a)): the
/// deployment's logged `session_start` (the request, the timestamp and `selection.section_ids`)
/// and the seed in its session config, with the logged approach and `roll_inputs` (BP-14a; their
/// `gunsmith` key, BP-16), through `resolve_mission_with_modifiers` exactly as the mission route
/// called it (`post_mission_resolve`). Returns the report and the first logged Section, the one
/// that fought. No store lock is held; any missing piece is a `500`. The route fills
/// `commander_name` after its resolve, so the recompute's is `None`.
async fn replay_report(state: &AppState, dep: &DeploymentRecord, route: &str) -> Result<(BattleReport, Uuid), AppError> {
    let (deployment, session, commander) = (dep.deployment_id, dep.session_id, dep.commander_id);
    let at = || format!("{route}: deployment {deployment} (session {session}) of commander {commander}");
    let entries = state.input_log_repo.get_entries_by_session(&session).await.map_err(|e| {
        internal_error(format!("{}: input log read failed: {e}", at()))
    })?;
    let start = entries
        .iter()
        .find(|e| e.event_type == "session_start")
        .ok_or_else(|| internal_error(format!("{}: no session_start entry", at())))?;
    let req: MissionResolveRequest = serde_json::from_value(start.payload["request"].clone())
        .map_err(|e| internal_error(format!("{}: session_start is not a mission request: {e}", at())))?;
    let timestamp = start.payload["timestamp"]
        .as_str()
        .ok_or_else(|| internal_error(format!("{}: session_start has no timestamp", at())))?
        .to_string();
    let section_id = start.payload["selection"]["section_ids"][0]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| internal_error(format!("{}: session_start has no first Section", at())))?;
    let config = state
        .input_log_repo
        .get_session_config(&session)
        .await
        .map_err(|e| internal_error(format!("{}: session config read failed: {e}", at())))?
        .ok_or_else(|| internal_error(format!("{}: no session config", at())))?;
    let mission = missions()
        .get(dep.mission_id.as_str())
        .ok_or_else(|| internal_error(format!("{}: unknown mission {}", at(), dep.mission_id)))?;
    // The rows as they were at fight time (BP-14a). An entry logged before BP-14a has no
    // `roll_inputs` and resolved with no Commander rows, so it replays with none.
    let inputs = match start.payload.get("roll_inputs") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(crate::demo_roll::RollInputs::from_json(v).ok_or_else(|| {
            internal_error(format!("{}: session_start roll_inputs are off the roster", at()))
        })?),
    };
    let modifiers = crate::demo_roll::roll_modifiers(inputs.as_ref(), req.approach, mission);
    let squad = Squad { units: req.squad.units, commander: None };
    let report = resolve_mission_with_modifiers(&squad, mission, &modifiers, &timestamp, Some(config.seed as u32), &state.config.sim);
    Ok((report, section_id))
}

/// What a latched fight costs and pays (BP-11a-1, BP-11a-2): the Section that fought, its
/// soldiers lost, and the credits the recomputed report pays. The pay comes from the server's
/// recompute of the logged fight, never from the client's copy of the report; the squad that
/// fought is still the one the client sent (GAP-93, ODQ-63); and the soldiers wounded and the
/// loot, from the same recompute (BP-12a-1).
struct FightLoss {
    section_id: Uuid,
    losses: i16,
    wounded: u8,
    credits: u32,
    loot: Option<LootDrop>,
}

/// A fight's losses from the recompute (BP-11a-1). A recomputed outcome that differs from the
/// deployment record's (by the database string the route writes) is a journalled `500`, and
/// nothing is applied.
async fn fight_result(state: &AppState, dep: &DeploymentRecord, route: &str) -> Result<FightLoss, AppError> {
    let (report, section_id) = replay_report(state, dep, route).await?;
    let replayed = outcome_db_str(&report.outcome);
    if dep.outcome.as_deref() != Some(replayed) {
        return Err(internal_error(format!(
            "{route}: deployment {} of commander {} recomputes to {replayed} but its record says {:?}; nothing applied",
            dep.deployment_id, dep.commander_id, dep.outcome
        )));
    }
    let losses = fight_losses(&report.unit_results);
    let wounded = fight_wounded(&report.unit_results);
    Ok(FightLoss { section_id, losses, wounded, credits: report.rewards.credits, loot: report.rewards.loot_drop })
}

/// Latches a pending battle (a raid, BP-8b-1b, a haul's ambush, BP-8b-2, or a contract's fight,
/// BP-11a-2), run by every ops route right after `load_board`. Nothing pending: nothing is read.
/// Otherwise the store names the op's Commander, the trigger and its Section, and:
/// - **at 0** (BP-11a-1): a Section with no soldiers left can't fight, so the battle latches as a
///   Wipeout without a fight: no deployment read, no loss write;
/// - otherwise the Commander's deployments are read with no store lock held
///   (`list_deployments_by_commander`, one sequential scan, `500` on failure). A fight not yet
///   started or still `active` leaves the battle pending. A resolved one is recomputed first
///   (`fight_result`, so a failed recompute leaves the battle pending for the next read), then
///   the store applies it only if the trigger is unchanged;
/// - **the losses** (BP-11a-1): only when that latch applied, under the launch lock as the
///   meltdown's write, the Section that fought loses one soldier per KIA or MIA. A failed write is
///   `500`, and the latch stands.
/// - **the pay** (BP-11a-2): the recomputed report's credits are banked by the latch itself, so
///   they land exactly when it applies; **the sickbay and the shelf** (BP-12a-1): the latch also
///   takes the fight's wounded, cut to the survivors (the headcount this read saw, less the
///   fight's losses). The latch's own `lose_soldiers` lands after `latch_raid`, so a second latch
///   read in that gap sees the old headcount: over the cap only, and unreachable, because a
///   second fight needs a new trigger. Replace (BP-12a-2) adds heads only after `begin_replace`
///   found no op out; an add that lands after a later `ops/start` can only make the next
///   latch's cap too low, never too high. Since BP-17a a Section's Requisition also passes
///   during an unguarded dig, so a Section's add begins either with no op out or before
///   `ops/escort` (`section_home`); either way it can only lower the next cap, and the cap reads
///   stay sound. Its loot goes on the shelf.
///
/// **The Crew's own fight (BP-17a):** an ambush revealed before the guard stands comes rolled
/// (`PendingRaid::crew`): no Section or deployment is read. After the seam, the launch lock is
/// taken **before** the latch and held through the heads' write, so a Hire that the cleared op of
/// a wipeout lets begin adds its heads after the loss, never before it (spec check W3). The heads
/// are written only when the latch applied. A handler dropped between the applied latch and the
/// write leaves the Crew its heads: a named residual that favours the player, as the meltdown's.
async fn latch_pending_raid(state: &AppState, campaign_id: Uuid, now: DateTime<Utc>, route: &str) -> Result<(), AppError> {
    let Some(pending) = state.demo_ops.store.pending_raid(campaign_id, now) else {
        return Ok(());
    };
    if let Some(crew) = pending.crew {
        if let Some(seam) = &state.demo_ops.latch_seam {
            seam().await;
        }
        let _launch = state.launch_locks.lock(campaign_id).await;
        let applied = state.demo_ops.store.latch_crew(campaign_id, &pending.trigger, crew, now);
        if applied && crew.heads > 0 {
            if let Err(e) = state.crew_loss.meltdown_loss(crew.crew_id, crew.heads, DEMO_CREW_FIGHT_MORALE_LOSS).await {
                return Err(internal_error(format!(
                    "{route}: crew fight meltdown_loss({}, {DEMO_CREW_FIGHT_MORALE_LOSS}) failed for crew {} of campaign {campaign_id}: {e}",
                    crew.heads, crew.crew_id
                )));
            }
        }
        return Ok(());
    }
    let mut read_headcount: Option<(Uuid, i16)> = None;
    if let Some(section_id) = pending.section_id {
        let section = state
            .section_repo
            .get_section(section_id)
            .await
            .map_err(|e| internal_error(format!("{route}: get_section failed for section {section_id} of campaign {campaign_id}: {e}")))?
            .ok_or_else(|| internal_error(format!("{route}: section {section_id} of campaign {campaign_id} not found")))?;
        if section.headcount == 0 {
            state.demo_ops.store.latch_raid(campaign_id, &pending.trigger, RaidResult::Wiped, FightPay::credits(0), now);
            return Ok(());
        }
        read_headcount = Some((section_id, section.headcount));
    }
    let commander_id = pending.commander_id;
    let deps = state.commander_repo.list_deployments_by_commander(commander_id).await.map_err(|e| {
        internal_error(format!("{route}: list_deployments_by_commander failed for commander {commander_id} of campaign {campaign_id}: {e}"))
    })?;
    let result = raid_result(&deps, &pending.trigger)
        .map_err(|e| internal_error(format!("{route}: raid of campaign {campaign_id}: {e}")))?;
    let Some(result) = result else {
        return Ok(());
    };
    let fight = raid_fight(&deps, &pending.trigger)
        .ok_or_else(|| internal_error(format!("{route}: raid of campaign {campaign_id} has a result but no fight")))?;
    let loss = fight_result(state, fight, route).await?;
    if let Some(seam) = &state.demo_ops.latch_seam {
        seam().await;
    }
    let cap = read_headcount
        .filter(|(id, _)| *id == loss.section_id)
        .map(|(_, h)| (h - loss.losses).clamp(0, 8) as u8);
    let pay = FightPay { credits: loss.credits, section_id: loss.section_id, wounded: loss.wounded, cap, loot: loss.loot };
    let applied = state.demo_ops.store.latch_raid(campaign_id, &pending.trigger, result, pay, now);
    if applied && loss.losses > 0 {
        let _launch = state.launch_locks.lock(campaign_id).await;
        if let Err(e) = state.section_loss.lose_soldiers(loss.section_id, loss.losses).await {
            return Err(internal_error(format!(
                "{route}: lose_soldiers({}) failed for section {} of campaign {campaign_id}: {e}",
                loss.losses, loss.section_id
            )));
        }
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
        let view = OpsView { campaign_id, gateway: None, sites: &[], active_op: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), company: CompanyView::default(), now: ops_time(now), crew_fight: None };
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
/// failed read) → `400` unknown site → for a dig, the Crew, Foreman and Commander reads
/// (`500` on failure), `409` no Crew left and `409` no Commander left; for a contract, the
/// Commander read (`500` on failure) and `409` no Commander left → for any site, the
/// member's first Section read (`500` on failure) and `409` when it is at 0 (BP-11a-1) → `409`
/// an op is under way → `503` the store is full of ops → `200`, the whole state.
/// - The site is looked up as it stands (BP-11a-3): a fought contract is `400 unknown site`
///   (before `409` an op is under way) until it returns 45 s later at another hex. A return
///   landing between the lookup and the start sends the team to the old pin (accepted).
/// - The leg: `depart_at = now`, `arrive_at = now + the site's travel_secs`.
/// - The op's seed is drawn here from OS entropy and handed to the store, which never sends it;
///   the drill rolls from it.
/// - A dig reads its Crew, threshold and Commander once, here (`dig_start`); a contract reads its Commander (`usable_commander`).
/// - The latch runs here too, so a raid already fought (a Wipeout clears the op) frees the start
///   without waiting for a board read.
/// - No launch lock: an op that starts while the reaper retires the campaign is retired with
///   it, and the next ops call answers `409` (an accepted residual, as BP-3c-1b-1's).
/// - **`escort` (BP-17a):** `false` sends the Crew to a dig alone. The Commander is still
///   required (a dead Commander ends the run), but the at-0 check is skipped: the Section stays
///   at base. Absent = true; ignored for a contract.
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
    // BP-11a-3: the site as it stands, so a fought contract is unknown until it returns.
    let Some(site) = state.demo_ops.store.site(campaign_id, &board, &req.site_id, now) else {
        return Err(bad_request("unknown site"));
    };
    let dig = if site.kind == "dig" { Some(dig_start(&state, campaign_id, &wallet, "post_ops_start").await?) } else { None };
    let lead = if site.kind == "contract" { Some(usable_commander(&state, campaign_id, &wallet, "post_ops_start").await?) } else { None };
    // BP-11a-1: the member's first Section (oldest first). At 0 it has no one to send, for any
    // site; after the Commander check, so a KIA Commander still answers first. A member with no
    // Section passes, and the dig then has no at-0 check.
    let sections = state.section_repo.list_sections_for_member(campaign_id, &wallet).await.map_err(|e| {
        internal_error(format!("post_ops_start: list_sections_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let first_section = sections.first();
    let escort = site.kind != "dig" || req.escort.unwrap_or(true);
    if escort && first_section.is_some_and(|s| s.headcount == 0) {
        return Err(conflict("no soldiers left to send: start a new run"));
    }
    let contract = lead.map(|commander_id| ContractStart { commander_id, section_id: first_section.map(|s| s.section_id) });
    let dig = dig.map(|d| DigStart { section_id: first_section.map(|s| s.section_id), escort, ..d });
    let op_seed = generate_seed();
    match state.demo_ops.store.start_op(campaign_id, gw, Arc::clone(&board), &site, op_seed, dig, contract, now) {
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
async fn dig_start(state: &AppState, campaign_id: Uuid, wallet: &[u8], route: &str) -> Result<DigStart, AppError> {
    let crews = state.crew_repo.list_crews_for_member(campaign_id, wallet).await.map_err(|e| {
        internal_error(format!("{route}: list_crews_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let Some(crew) = crews.first().filter(|c| c.headcount > 0) else {
        return Err(conflict("no Crew left to dig: start a new run"));
    };
    let foremen = state.foreman_repo.list_foremen_for_member(campaign_id, wallet).await.map_err(|e| {
        internal_error(format!("{route}: list_foremen_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let coolant = foremen.first().is_some_and(|f| f.specialization == ForemanSpecialization::CoolantJockey.as_str());
    let commander_id = usable_commander(state, campaign_id, wallet, route).await?;
    Ok(DigStart { crew_id: crew.crew_id, commander_id, threshold: dig_threshold(coolant), section_id: None, escort: true })
}

/// The member's first usable Commander (BP-11a-2, extracted from `dig_start`): not KIA, not
/// Shattered (the mission route's refusals). `409` when none is usable. A dig's raids and a
/// contract's fight are both fought by this one.
async fn usable_commander(state: &AppState, campaign_id: Uuid, wallet: &[u8], route: &str) -> Result<Uuid, AppError> {
    let commanders = state.commander_repo.list_commanders_for_member(campaign_id, wallet).await.map_err(|e| {
        internal_error(format!("{route}: list_commanders_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let Some(commander) = commanders.iter().find(|c| !c.is_kia && !c.is_shattered) else {
        return Err(conflict("no Commander left to lead: start a new run"));
    };
    Ok(commander.commander_id)
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

/// Moves the team from the site it stands on straight to another, `POST
/// /api/campaign/:campaign_id/ops/next {site_id}` (BP-11a-4, demo-only). Order: `401` → `429`
/// `ops_ip` → `429` `ops_wallet` → axum's body rejection, counted (`413` over `OPS_BODY_LIMIT`;
/// `400`, `415`, `422`) → `404` → `403` → `409` not `Active` (entry dropped) → `409` no op under
/// way (also: no gateway) → the board, a hit proven by `EXISTS` → a pending battle latched →
/// `400` unknown site → the lead reads as `ops/start` (a dig: Crew, Foreman and Commander; a
/// contract: Commander; `500` on failure, `409` none left) → the member's first Section read
/// (`500` on failure) and `409` when it is at 0 → `409` no op under way → `409` already heading
/// home → `409` still travelling → `409` raiders at the site → `409` the guard hasn't reached
/// the dig (BP-18a-1a: a dig whose guard does not stand) → `409` already there → `200`, the
/// whole state.
/// - A new op from the team's own hex (`next_op`): the hop's hexes at the target's seconds per
///   hex, at least 1 s; home from there is the target's own time.
/// - The ore on a dig's site is walked away from, as `ops/return`; an unfought contract stays on
///   the board unfinished.
/// - The site is looked up as it stands: a fought contract is `400 unknown site` for 45 s, as for
///   `ops/start`. A return landing between the lookup and the next sends the team to the old pin
///   (accepted, as `ops/start`).
/// - No launch lock (as `ops/start`), and no `503`: the campaign's entry already holds the op.
async fn post_ops_next(
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
    let (campaign, gateway, wallet) = ops_member(&state, &token, campaign_id, "post_ops_next").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no op under way"));
    };
    let board = load_board(&state, &campaign, gw, now, true, "post_ops_next").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_next").await?;
    let Some(site) = state.demo_ops.store.site(campaign_id, &board, &req.site_id, now) else {
        return Err(bad_request("unknown site"));
    };
    let dig = if site.kind == "dig" { Some(dig_start(&state, campaign_id, &wallet, "post_ops_next").await?) } else { None };
    let lead = if site.kind == "contract" { Some(usable_commander(&state, campaign_id, &wallet, "post_ops_next").await?) } else { None };
    let sections = state.section_repo.list_sections_for_member(campaign_id, &wallet).await.map_err(|e| {
        internal_error(format!("post_ops_next: list_sections_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let first_section = sections.first();
    if first_section.is_some_and(|s| s.headcount == 0) {
        return Err(conflict("no soldiers left to send: start a new run"));
    }
    let contract = lead.map(|commander_id| ContractStart { commander_id, section_id: first_section.map(|s| s.section_id) });
    let dig = dig.map(|d| DigStart { section_id: first_section.map(|s| s.section_id), ..d });
    let op_seed = generate_seed();
    match state.demo_ops.store.next_op(campaign_id, &site, op_seed, dig, contract, now) {
        Ok(()) => {}
        Err(NextRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(NextRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
        Err(NextRefusal::NotArrived) => return Err(conflict("still travelling")),
        Err(NextRefusal::RaidPending) => return Err(conflict("raiders at the site")),
        Err(NextRefusal::GuardNotHere) => return Err(conflict("the guard hasn't reached the dig")),
        Err(NextRefusal::SameSite) => return Err(conflict("already there")),
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
///   the launch lock and writes the Crew's loss (`CrewLoss::meltdown_loss`: heads and, since
///   BP-11a-1, morale; Reviewer BP-8a-2 CF5).
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
    // BP-17a: the launch lock is taken before the drill and held through any Crew write, so a
    // Hire that a Crew wipeout's cleared op lets begin adds its heads after the loss (W3).
    let launch = state.launch_locks.lock(campaign_id).await;
    match state.demo_ops.store.drill_op(campaign_id, now) {
        Ok(DrillOutcome::Drilled) => {}
        Ok(DrillOutcome::Meltdown { crew_id, heads }) => {
            if let Err(e) = state.crew_loss.meltdown_loss(crew_id, heads, DEMO_MELTDOWN_MORALE_LOSS).await {
                return Err(internal_error(format!(
                    "post_ops_drill: meltdown_loss({heads}, {DEMO_MELTDOWN_MORALE_LOSS}) failed for crew {crew_id} of campaign {campaign_id}: {e}"
                )));
            }
        }
        Ok(DrillOutcome::CrewFight { crew_id, heads }) => {
            if heads > 0 {
                if let Err(e) = state.crew_loss.meltdown_loss(crew_id, heads, DEMO_CREW_FIGHT_MORALE_LOSS).await {
                    return Err(internal_error(format!(
                        "post_ops_drill: crew fight meltdown_loss({heads}, {DEMO_CREW_FIGHT_MORALE_LOSS}) failed for crew {crew_id} of campaign {campaign_id}: {e}"
                    )));
                }
            }
        }
        Err(DrillRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(DrillRefusal::Contract) => return Err(conflict("not a dig site")),
        Err(DrillRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
        Err(DrillRefusal::NotArrived) => return Err(conflict("still travelling")),
        Err(DrillRefusal::RaidPending) => return Err(conflict("raiders at the site")),
    }
    drop(launch);
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Sends the Section to guard an unescorted dig, `POST /api/campaign/:campaign_id/ops/escort`
/// (BP-17a, demo-only; §4e B2's "go guard the dig"). No body is read. Order: `401` → `429`
/// `ops_ip` → `429` `ops_wallet` → `404` → `403` → `409` not `Active` (entry dropped) → `409` no
/// op under way (also: no gateway) → the board → a pending battle latched → the Commander read
/// (`500` on failure) and `409` no Commander left → the member's first Section read (`500` on
/// failure) and `409` when it is at 0 → `409` no op under way → `409` not a dig site → `409`
/// already heading home → `409` already guarded → `200`, the whole state.
/// - The guard stands from `now` + the site's `travel_secs` (`dig.escort_at`): a battle born
///   before then is still the Crew's.
/// - Memory only, no DB write, so no launch lock.
async fn post_ops_escort(
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
    let (campaign, gateway, wallet) = ops_member(&state, &token, campaign_id, "post_ops_escort").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no op under way"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_escort").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_escort").await?;
    let commander_id = usable_commander(&state, campaign_id, &wallet, "post_ops_escort").await?;
    let sections = state.section_repo.list_sections_for_member(campaign_id, &wallet).await.map_err(|e| {
        internal_error(format!("post_ops_escort: list_sections_for_member failed for campaign {campaign_id}: {e}"))
    })?;
    let first_section = sections.first();
    if first_section.is_some_and(|s| s.headcount == 0) {
        return Err(conflict("no soldiers left to send"));
    }
    match state.demo_ops.store.escort_op(campaign_id, commander_id, first_section.map(|s| s.section_id), now) {
        Ok(()) => {}
        Err(EscortRefusal::NoOp) => return Err(conflict("no op under way")),
        Err(EscortRefusal::Contract) => return Err(conflict("not a dig site")),
        Err(EscortRefusal::AlreadyHomeward) => return Err(conflict("already heading home")),
        Err(EscortRefusal::AlreadyEscorted) => return Err(conflict("already guarded")),
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

/// Heals a Section's wounded, `POST /api/campaign/:campaign_id/ops/heal {section_id}` (BP-12a-1,
/// demo-only, §4e B3). Order: `401` → `429` `ops_ip` → `429` `ops_wallet` → axum's body
/// rejection, counted (`413` over `OPS_BODY_LIMIT`; `400`, `415`, `422`) → `404` → `403` → `409`
/// not `Active` (entry dropped) → `409` no gateway → the board → a pending battle latched →
/// `400` unknown section → `409` nobody to heal → `409` already healing → `409` not enough
/// banked credits → `200`, the whole state.
/// - Every wounded soldier of the Section at once, `OpsBase::heal_credits` each from
///   `credits_banked` (40, or 20 with the Hobbyist Benches, BP-16), back after
///   `OpsBase::heal_secs` (15 s, or 8 s). Allowed while the team is out (the sickbay feeds
///   nothing the server rolls).
/// - Abuse bound: memory only, no DB write, so no launch lock; the shared ops limiter (40/min per
///   wallet, 400 per address); at most one bay per Section a latch wrote.
async fn post_ops_heal(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
    body: Result<Json<OpsHealRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    if let Some(r) = ops_limits(&state, &token, ci, &headers)? {
        return Ok(r);
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return Ok(rej.into_response()),
    };
    let now = Utc::now();
    let (campaign, gateway, _) = ops_member(&state, &token, campaign_id, "post_ops_heal").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no gateway assigned"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_heal").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_heal").await?;
    let Ok(section_id) = Uuid::parse_str(&req.section_id) else {
        return Err(bad_request("unknown section"));
    };
    match state.demo_ops.store.heal_op(campaign_id, section_id, now) {
        Ok(()) => {}
        Err(HealRefusal::NobodyToHeal) => return Err(conflict("nobody to heal")),
        Err(HealRefusal::AlreadyHealing) => return Err(conflict("already healing")),
        Err(HealRefusal::NotEnoughCredits) => return Err(conflict("not enough banked credits")),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Replaces a Section's soldiers or a Crew's miners, `POST /api/campaign/:campaign_id/ops/replace
/// {section_id | crew_id, count}` (BP-12a-2, demo-only, §4e). Order: `401` → `429` `ops_ip` →
/// `429` `ops_wallet` → axum's body rejection, counted (`413` over `OPS_BODY_LIMIT`; `400`, `415`,
/// `422`) → `404` → `403` → `409` not `Active` (entry dropped) → `409` no gateway → the board →
/// a pending battle latched → `400` name one of section_id or crew_id → `400` unknown section /
/// unknown crew → `400` count must be 1 to 8 → `409` the team is out → `409` not enough banked
/// credits → the write → `409` no room (credits refunded) or the journalled `500` (refunded) →
/// `200`, the whole state.
/// - `DEMO_REPLACE_MERC_CREDITS` a soldier, `DEMO_REPLACE_MINER_CREDITS` a miner, charged for the
///   heads added, on the same row: never an insert, so a first Section at 0 is lifted (Reviewer
///   BP-11a-1 F2). A Crew replace moves no morale.
/// - The write runs in its own task, so a client that goes away can't cancel it between the debit
///   and `finish_replace`: dropping the `JoinHandle` detaches the task, and the route never calls
///   `abort()`, so the only `JoinError` is a panic.
/// - Abuse bound: one owner-scoped `UPDATE` of one row on the demo's Postgres per accepted
///   replace, at most one per 80 banked credits, under the shared ops limiter (40/min per wallet,
///   400 per address). It holds the store lock for the check and debit (no await), then the
///   campaign's launch lock and one row lock for one statement.
async fn post_ops_replace(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
    body: Result<Json<OpsReplaceRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    if let Some(r) = ops_limits(&state, &token, ci, &headers)? {
        return Ok(r);
    }
    let Json(req) = match body {
        Ok(j) => j,
        Err(rej) => return Ok(rej.into_response()),
    };
    let now = Utc::now();
    let (campaign, gateway, wallet) = ops_member(&state, &token, campaign_id, "post_ops_replace").await?;
    let Some(gw) = gateway else {
        return Err(conflict("no gateway assigned"));
    };
    let board = load_board(&state, &campaign, gw, now, false, "post_ops_replace").await?;
    latch_pending_raid(&state, campaign_id, now, "post_ops_replace").await?;
    let target = match (req.section_id.as_deref(), req.crew_id.as_deref()) {
        (Some(s), None) => match Uuid::parse_str(s) {
            Ok(id) => ReplaceTarget::Section(id),
            Err(_) => return Err(bad_request("unknown section")),
        },
        (None, Some(c)) => match Uuid::parse_str(c) {
            Ok(id) => ReplaceTarget::Crew(id),
            Err(_) => return Err(bad_request("unknown crew")),
        },
        _ => return Err(bad_request("name one of section_id or crew_id")),
    };
    if !(1..=8).contains(&req.count) {
        return Err(bad_request("count must be 1 to 8"));
    }
    let n = req.count as u8;
    let each = match target {
        ReplaceTarget::Section(_) => DEMO_REPLACE_MERC_CREDITS,
        ReplaceTarget::Crew(_) => DEMO_REPLACE_MINER_CREDITS,
    };
    // BP-17a: a Section's Requisition passes while the Section is at base on an unescorted dig.
    let begun = match target {
        ReplaceTarget::Section(_) => state.demo_ops.store.begin_requisition(campaign_id, each * u32::from(n), now),
        ReplaceTarget::Crew(_) => state.demo_ops.store.begin_replace(campaign_id, each * u32::from(n), now),
    };
    let hold = match begun {
        Ok(h) => h,
        Err(ReplaceRefusal::OpOut) => return Err(conflict("the team is out: replacements report at base")),
        Err(ReplaceRefusal::NotEnoughCredits) => return Err(conflict("not enough banked credits")),
    };
    let task_state = state.clone();
    let handle = tokio::spawn(async move { replace_write(task_state, campaign_id, wallet, target, n, hold).await });
    match handle.await {
        Ok(result) => result?,
        Err(e) => return Err(internal_error(format!("post_ops_replace: task failed for campaign {campaign_id}: {e}"))),
    }
    Ok(ops_response(&state, campaign_id, gw, &board, now))
}

/// Replace's durable write (BP-12a-2), run in the task `post_ops_replace` starts, so it always
/// reaches `finish_replace`: kept on an add, refunded on no room or a failed write.
async fn replace_write(
    state: Arc<AppState>,
    campaign_id: Uuid,
    wallet: Vec<u8>,
    target: ReplaceTarget,
    n: u8,
    hold: ReplaceHold,
) -> Result<(), AppError> {
    let _launch = state.launch_locks.lock(campaign_id).await;
    let added = match target {
        ReplaceTarget::Section(id) => state.section_loss.add_soldiers(id, campaign_id, &wallet, i16::from(n)).await,
        ReplaceTarget::Crew(id) => state.crew_loss.add_heads(id, campaign_id, &wallet, i16::from(n)).await,
    };
    match added {
        Ok(Some(_)) => {
            state.demo_ops.store.finish_replace(hold, false);
            Ok(())
        }
        Ok(None) => {
            state.demo_ops.store.finish_replace(hold, true);
            Err(conflict(match target {
                ReplaceTarget::Section(_) => "no room: a Section holds 8",
                ReplaceTarget::Crew(_) => "no room: a Crew holds 8",
            }))
        }
        Err(e) => {
            state.demo_ops.store.finish_replace(hold, true);
            Err(internal_error(format!("post_ops_replace: {target:?} +{n} failed for campaign {campaign_id}: {e}")))
        }
    }
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
        assert_eq!(keys, ["active_op", "base", "campaign_id", "company", "credits_banked", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
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
        seed_commander(&state, c.campaign_id).await;
        let (status, body, bytes) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let keys: std::collections::BTreeSet<&str> = body.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["active_op", "base", "campaign_id", "company", "credits_banked", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
        let op = &body["active_op"];
        let op_keys: std::collections::BTreeSet<&str> = op.as_object().expect("an op").keys().map(|k| k.as_str()).collect();
        assert_eq!(op_keys, ["arrive_at", "arrived", "at", "depart_at", "kind", "leg", "site_id"].into_iter().collect());
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
        seed_commander(&state, c.campaign_id).await;
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
        seed_commander(&state, first.campaign_id).await;
        seed_commander(&state, second.campaign_id).await;
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
        seed_commander(&state, c.campaign_id).await;
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
        seed_commander(&state, c.campaign_id).await;
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
        seed_commander(&state, c.campaign_id).await;
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

    /// A usable Commander for the member, as a join writes one (BP-11a-2: a contract needs one).
    async fn seed_commander(state: &AppState, campaign: Uuid) {
        let commander = crate::commander_gen::generate_commander(WALLET.to_vec(), campaign, 42);
        state.commander_repo.create_commander(commander).await.expect("create_commander");
    }

    /// The member's Crew (8 heads), Foreman and Commander, as a join writes them; the Foreman is
    /// the Coolant-Jockey or, when `coolant` is false, the first other specialization. Returns
    /// the Crew.
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
        seed_commander(&state, contract.campaign_id).await;
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

    /// Three drills through the route: seed 2's seeded heat (12, 17, 19) and ore 20 × n on the tier-1 site,
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
        assert_eq!(seen, vec![(Some(1), Some(12), Some(20), Some(40), Some(100)), (Some(2), Some(29), Some(60), Some(60), Some(100)), (Some(3), Some(48), Some(120), Some(80), Some(100))]);
        let text = String::from_utf8_lossy(&last);
        assert!(!text.contains("seed") && !text.contains("crew"), "{text}");
    }

    /// Seed 57, threshold 100: the 6th drill reaches exactly 100 and melts down, the team heads
    /// home empty and the Crew loses 2 heads (8 → 6) under a launch lock that is released afterwards.
    #[tokio::test]
    async fn ops_meltdown_on_the_6th_costs_the_crew_2_heads() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 57).await;
        for i in 1..=5 {
            let (status, body, _) = drill(&state, c).await;
            assert_eq!(status, StatusCode::OK, "drill {i}: {body}");
            assert_eq!(body["active_op"]["leg"], "outbound", "drill {i}");
        }
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert_eq!(op["leg"], "homeward");
        assert_eq!(op["depart_at"], body["now"]);
        assert_eq!((op["dig"]["heat"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(100), Some(0)));
        assert_eq!(op["dig"]["ended_by"], "meltdown");
        assert_eq!(headcount(&state, crew).await, 6);
        assert_eq!(state.crew_repo.get_crew(crew).await.unwrap().expect("the Crew").morale, 50, "60 − 10 (BP-11a-1)");
        assert_eq!(state.launch_locks.len(), 0, "the launch lock is released");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
        assert_eq!(headcount(&state, crew).await, 6, "the heads are lost once");
    }

    /// A Coolant-Jockey Foreman: threshold 115, so seed 57's 6th drill (heat 100) digs and the
    /// 7th melts down at 119, costing 1 head (8 → 7).
    #[tokio::test]
    async fn ops_coolant_jockey_melts_down_on_the_7th() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, true).await;
        dig_on_site(&state, c, 57).await;
        for i in 1..=6 {
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
        state.crew_loss.meltdown_loss(crew, 8, 0).await.expect("meltdown_loss");
        let (status, body, _) = start(&state, c.campaign_id, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Crew left to dig: start a new run")));
        assert!(state.demo_ops.store.op_view(c.campaign_id, chrono::Utc::now()).is_none());
    }

    struct FailingCrewLoss;

    #[async_trait::async_trait]
    impl crate::demo_crew_loss::CrewLoss for FailingCrewLoss {
        async fn meltdown_loss(&self, _: Uuid, _: i16, _: i16) -> Result<(i16, i16), RepositoryError> {
            Err(RepositoryError::Internal("db down".into()))
        }

        async fn add_heads(&self, _: Uuid, _: Uuid, _: &[u8], _: i16) -> Result<Option<i16>, RepositoryError> {
            Err(RepositoryError::Internal("db down".into()))
        }
    }

    struct FailingSectionLoss;

    #[async_trait::async_trait]
    impl crate::demo_section_loss::SectionLoss for FailingSectionLoss {
        async fn lose_soldiers(&self, _: Uuid, _: i16) -> Result<i16, RepositoryError> {
            Err(RepositoryError::Internal("db down".into()))
        }

        async fn add_soldiers(&self, _: Uuid, _: Uuid, _: &[u8], _: i16) -> Result<Option<i16>, RepositoryError> {
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
        for _ in 1..=5 {
            assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        }
        let (status, body, _) = drill(&state, c).await; // seed 2's 6th drill melts at 116 (BP-13)
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

    /// The fixture's seeds (BP-11a-1), measured with `resolve_mission` on `GUARD_DUTY` before the
    /// brick (the sim untouched) and pinned: `(seed, units sent) → outcome, KIA`. The client's
    /// Rookies (`rookies`); with `Some(seed)` the timestamp moves no draw.
    /// - seed 3, 8 units → Success, 1 KIA
    /// - seed 4, 8 units → Defeat, 1 KIA
    /// - seed 43, 8 units → Defeat, 4 KIA
    /// - seed 3, 1 unit → Wipeout, 1 KIA
    const SUCCESS_SEED: (u32, usize) = (3, 8);
    const DEFEAT_SEED: (u32, usize) = (4, 8);
    const DEFEAT_4_KIA_SEED: (u32, usize) = (43, 8);
    const WIPEOUT_SEED: (u32, usize) = (3, 1);
    /// BP-12a-1, measured with `resolve_mission` on `GUARD_DUTY` before the tests were written
    /// (seeds 0–39, 8 Rookies; the sim untouched): seed 6 → Success, 0 KIA, 3 wounded, 600
    /// credits, no loot. Seed 4 (`DEFEAT_SEED`) → Defeat, 1 KIA, 1 wounded, 0 credits.
    const HEAL_WIN_SEED: (u32, usize) = (6, 8);

    /// `n` of the client's Rookies (`battle.js` `squad()`: the `GHOST_WIRE` stand-in, 10 HP).
    fn rookies(n: usize) -> Vec<sim_engine::game_types::Unit> {
        let def = sim_engine::units::unit_definitions().get("GHOST_WIRE").expect("GHOST_WIRE").clone();
        (0..n)
            .map(|i| sim_engine::game_types::Unit {
                id: format!("r{}", i + 1),
                name: format!("Rookie {}", i + 1),
                skill: def.base_skill,
                definition: def.clone(),
                xp: 0,
                current_hp: 10,
                max_hp: 10,
                status: sim_engine::game_types::UnitStatus::Ready,
                equipment: vec![],
            })
            .collect()
    }

    /// The member's first Section, created as a join writes it (8 soldiers) when there is none.
    async fn member_section(state: &AppState, campaign: Uuid) -> Uuid {
        let sections = state.section_repo.list_sections_for_member(campaign, &WALLET).await.expect("list_sections_for_member");
        if let Some(s) = sections.first() {
            return s.section_id;
        }
        let s = crate::repository::section::starter_section(campaign, WALLET.to_vec());
        let id = s.section_id;
        state.section_repo.create_section(s).await.expect("create_section");
        id
    }

    async fn section_headcount(state: &AppState, section: Uuid) -> i16 {
        state.section_repo.get_section(section).await.unwrap().expect("the Section").headcount
    }

    /// `fight_logged_at`, started now.
    async fn fight_logged(
        state: &AppState,
        campaign: Uuid,
        seed: (u32, usize),
        record_as: Option<sim_engine::game_types::OutcomeType>,
    ) -> (Uuid, Uuid) {
        fight_logged_at(state, campaign, seed, record_as, chrono::Utc::now(), None).await
    }

    /// A raid's fight as the mission route records it (BP-11a-1, spec check F2), started at
    /// `started_at`: the `session_start` logged with `seed`, a `GUARD_DUTY` request of
    /// `units` Rookies and the member's first Section; the report from `resolve_mission`; the
    /// deployment opened with that Section and resolved with the report's outcome and KIA count
    /// (a Wipeout also marks the Commander KIA), or with `record_as` to fake a mismatched record.
    /// Returns the Commander and the Section. `roll_inputs`, when given, is logged as is (BP-16:
    /// a payload no build writes); `None` plants a fight from before BP-14a.
    async fn fight_logged_at(
        state: &AppState,
        campaign: Uuid,
        (seed, units): (u32, usize),
        record_as: Option<sim_engine::game_types::OutcomeType>,
        started_at: chrono::DateTime<chrono::Utc>,
        roll_inputs: Option<serde_json::Value>,
    ) -> (Uuid, Uuid) {
        let commander = commander_of(state, campaign).await;
        let section = member_section(state, campaign).await;
        let req = crate::api_types::MissionResolveRequest {
            squad: crate::api_types::MissionSquadRequest { units: rookies(units), commander: None },
            mission_id: "GUARD_DUTY".into(),
            commander_id: commander,
            seed_override: None,
            approach: None,
        };
        let timestamp = chrono::Utc::now().to_rfc3339();
        let session_id = Uuid::new_v4();
        let config = shared::SessionConfig {
            session_id: session_id.to_string(),
            build_version: env!("CARGO_PKG_VERSION").to_string(),
            seed: seed as u64,
            sector_id: Uuid::nil(),
            campaign_id: campaign,
            sector_tier: "Contested".into(),
            ruleset: "standard_v1".into(),
        };
        let mut entry = shared::InputLogEntry {
            tick: 0,
            seq: 0,
            event_type: "session_start".into(),
            player_id: None,
            payload: serde_json::json!({ "request": &req, "timestamp": timestamp, "selection": { "section_ids": [section] } }),
            narrative_event: None,
        };
        if let Some(r) = roll_inputs {
            entry.payload["roll_inputs"] = r;
        }
        state.input_log_repo.start_session(&config, &entry).await.expect("start_session");
        let mission = missions().get("GUARD_DUTY").expect("GUARD_DUTY");
        let report = sim_engine::resolver::resolve_mission(&Squad { units: req.squad.units, commander: None }, mission, &timestamp, Some(seed), &state.config.sim);
        let kia = report.unit_results.iter().filter(|u| matches!(u.final_status, sim_engine::game_types::UnitStatus::Kia)).count() as u32;
        let d = crate::repository::NewDeployment {
            deployment_id: Uuid::new_v4(),
            commander_id: commander,
            session_id,
            mission_id: "GUARD_DUTY".into(),
            started_at,
            player_wallet: WALLET.to_vec(),
            campaign_id: campaign,
            section_ids: vec![section],
        };
        assert_eq!(state.commander_repo.start_deployment(&d).await.expect("start"), crate::repository::DeploymentStart::Started);
        let outcome = record_as.unwrap_or(report.outcome);
        state.commander_repo.resolve_deployment(d.deployment_id, commander, kia, 1, 0.0, &outcome, &state.config.sim).await.expect("resolve");
        (commander, section)
    }

    /// The raid's fight with `outcome`, from the seed table (`fight_logged`). Returns the Commander.
    async fn fight(state: &AppState, campaign: Uuid, outcome: sim_engine::game_types::OutcomeType) -> Uuid {
        use sim_engine::game_types::OutcomeType;
        let seed = match outcome {
            OutcomeType::Success => SUCCESS_SEED,
            OutcomeType::Defeat => DEFEAT_SEED,
            OutcomeType::Wipeout => WIPEOUT_SEED,
            OutcomeType::TacticalRetreat => unreachable!("never assigned on the mission path"),
        };
        fight_logged(state, campaign, seed, None).await.0
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
        assert_eq!((d["heat"].as_u64(), d["ore_on_site"].as_u64()), (Some(10), Some(20)));
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
        assert_eq!((op["dig"]["carried"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(10), Some(0)));
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
    /// through (seed 2, one drill: 20 ore).
    #[tokio::test]
    async fn ops_pull_refusals() {
        let state = Arc::new(session_state());
        let contract = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &contract).await;
        let (status, body, _) = pull(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));
        seed_commander(&state, contract.campaign_id).await;
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
        assert_eq!((op["dig"]["carried"].as_u64(), op["dig"]["ore_on_site"].as_u64()), (Some(20), Some(0)));
        assert_eq!(op["depart_at"], body["now"]);
        assert_eq!(secs_between(op), site_secs(&body, DIG), "the haul takes the site's time");
        let (status, body, _) = pull(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
    }

    /// Reviewer BP-8b-1b F4 through the routes: seed 7's raid is lost, the team retreats with 10
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
        assert_eq!((body["active_op"]["dig"]["carried"].as_u64(), body["ore_banked"].as_u64()), (Some(10), Some(0)));
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!(body["ore_banked"], 10);
    }

    /// Seed 2 (never ambushed): three drills, a pull of 120 ore, banked by the first read after the
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
        assert_eq!((body["active_op"]["dig"]["carried"].as_u64(), body["ore_banked"].as_u64()), (Some(120), Some(0)));
        assert!(body["active_op"]["dig"]["ambush"].is_null());
        let (_, body) = get(&state, Some("t"), c).await;
        assert_eq!((body["active_op"]["arrived"].as_bool(), body["ore_banked"].as_u64()), (Some(false), Some(0)), "on the road");
        state.demo_ops.store.rewind_op(c, dig_secs(&state, c).await);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!(body["ore_banked"], 120);
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ore_banked"], 120, "the bank outlives the op");
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
        assert_eq!((body["active_op"]["dig"]["ore_on_site"].as_u64(), body["active_op"]["dig"]["carried"].as_u64()), (Some(20), Some(0)));
    }

    /// Seed 14 (ambush d100 4): the pull's body shows no ambush; the read after the midpoint fires
    /// it and the team waits on the road; a won fight is latched by the next read and the leg
    /// resumes; the read after its arrival banks all 60 ore.
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
        assert_eq!((op["arrived"].as_bool(), op["dig"]["carried"].as_u64()), (Some(false), Some(60)));
        state.demo_ops.store.rewind_op(c, secs);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!(body["ore_banked"], 60);
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

    /// BP-11a-1: the recompute equals the mission route's own report. A real `POST
    /// /api/mission/resolve` on a private in-memory state (the ops tests' `WALLET` byte, reused
    /// inside it), then `replay_report` on its deployment. The route fills `commander_name` after
    /// its resolve (`routes/mod.rs:610`), so the response's is set to `null` before comparing.
    /// BP-14a: the Commander (seed 42: Cannoneer, Paranoid) is rank 3 and flanks, so all four
    /// rows are live; they are pinned from the route, and `roll_inputs` from the log.
    #[tokio::test]
    async fn fight_result_replays_the_mission_route_report() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        let mut commander = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 42);
        commander.rank = 3;
        let commander_id = commander.commander_id;
        state.commander_repo.create_commander(commander).await.expect("create_commander");
        let section = member_section(&state, c.campaign_id).await;
        let body = serde_json::json!({
            "squad": { "units": rookies(8) },
            "mission_id": "GUARD_DUTY",
            "commander_id": commander_id,
            "section_ids": [section],
            "approach": "Flank",
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/mission/resolve")
            .header("authorization", "Bearer t")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = with_test_peer(crate::routes::router(Arc::clone(&state)), TEST_PEER).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let mut response: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(status, StatusCode::OK, "{response}");
        let deps = state.commander_repo.list_deployments_by_commander(commander_id).await.expect("deployments");
        assert_eq!(deps.len(), 1);
        let (report, fought) = replay_report(&state, &deps[0], "test").await.expect("replay_report");
        assert_eq!(fought, section);
        response["report"]["commander_name"] = serde_json::Value::Null;
        assert_eq!(serde_json::to_value(&report).unwrap(), response["report"]);
        let rows: Vec<(String, i64)> = response["report"]["score_breakdown"]["modifiers"]
            .as_array()
            .expect("modifiers")
            .iter()
            .map(|m| (m["key"].as_str().expect("key").to_string(), m["points"].as_i64().expect("points")))
            .collect();
        let want: Vec<(String, i64)> = [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10)]
            .iter()
            .map(|(k, p)| (k.to_string(), *p))
            .collect();
        assert_eq!(rows, want);
        let entries = state.input_log_repo.get_entries_by_session(&deps[0].session_id).await.expect("entries");
        assert_eq!(
            entries[0].payload["roll_inputs"],
            serde_json::json!({ "rank": 3, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" })
        );
    }

    /// BP-16: with the Gunsmith standing, the mission route adds its +10 row last and logs
    /// `"gunsmith": true`. The replay reads the log: with the Gunsmith gone from the live base it
    /// still rebuilds the route's report (a later build can't change what a fight paid).
    #[tokio::test]
    async fn mission_route_logs_the_gunsmith_and_the_replay_reads_the_log() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        assert_eq!(get(&state, Some("t"), c.campaign_id).await.0, StatusCode::OK);
        state.demo_ops.store.set_base(c.campaign_id, OpsBase { gunsmith: true, ..OpsBase::default() });
        let mut commander = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 42);
        commander.rank = 3;
        let commander_id = commander.commander_id;
        state.commander_repo.create_commander(commander).await.expect("create_commander");
        let section = member_section(&state, c.campaign_id).await;
        let body = serde_json::json!({
            "squad": { "units": rookies(8) },
            "mission_id": "GUARD_DUTY",
            "commander_id": commander_id,
            "section_ids": [section],
            "approach": "Flank",
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/mission/resolve")
            .header("authorization", "Bearer t")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = with_test_peer(crate::routes::router(Arc::clone(&state)), TEST_PEER).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let mut response: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(status, StatusCode::OK, "{response}");
        let rows: Vec<(String, i64)> = response["report"]["score_breakdown"]["modifiers"]
            .as_array()
            .expect("modifiers")
            .iter()
            .map(|m| (m["key"].as_str().expect("key").to_string(), m["points"].as_i64().expect("points")))
            .collect();
        let want: Vec<(String, i64)> = [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10), ("gunsmith", 10)]
            .iter()
            .map(|(k, p)| (k.to_string(), *p))
            .collect();
        assert_eq!(rows, want);
        let deps = state.commander_repo.list_deployments_by_commander(commander_id).await.expect("deployments");
        assert_eq!(deps.len(), 1);
        let entries = state.input_log_repo.get_entries_by_session(&deps[0].session_id).await.expect("entries");
        assert_eq!(
            entries[0].payload["roll_inputs"],
            serde_json::json!({ "rank": 3, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "gunsmith": true })
        );
        state.demo_ops.store.set_base(c.campaign_id, OpsBase::default());
        let (report, _) = replay_report(&state, &deps[0], "test").await.expect("replay_report");
        response["report"]["commander_name"] = serde_json::Value::Null;
        assert_eq!(serde_json::to_value(&report).unwrap(), response["report"]);
    }

    /// BP-16 (Linux's spec check, on the real resolver): the Gunsmith's +10 alone flips seed 13
    /// on `GUARD_DUTY` with 4 Rookies and the seed-42 Commander at rank 1 (one Specialisation
    /// row). The roll's own draw stays 57; the total moves 56 → 66, Defeat → Success. The rows
    /// come from `roll_modifiers`. `report_id` moves with the outcome, so it is not pinned.
    #[test]
    fn gunsmith_alone_flips_seed_13() {
        use sim_engine::roster::{CommanderFlaw, CommanderSpecialization};
        let mission = missions().get("GUARD_DUTY").expect("GUARD_DUTY");
        let squad = Squad { units: rookies(4), commander: None };
        let config = Config::default();
        let got: Vec<(i32, i32, String)> = [false, true]
            .into_iter()
            .map(|gunsmith| {
                let i = crate::demo_roll::RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: CommanderFlaw::Paranoid, gunsmith };
                let rows = crate::demo_roll::roll_modifiers(Some(&i), None, mission);
                let r = resolve_mission_with_modifiers(&squad, mission, &rows, "2026-11-02T00:00:00Z", Some(13), &config.sim);
                (r.score_breakdown.raw_roll, r.score_breakdown.total_score, format!("{:?}", r.outcome))
            })
            .collect();
        assert_eq!(got, vec![(57, 56, "Defeat".to_string()), (57, 66, "Success".to_string())]);
    }

    /// Reviewer BP-14a F3: a Commander whose record is off the roll roster (a Flaw no build
    /// writes, rank 1 so the command cap passes) is refused with a `500` after its deployment
    /// opened, and the deployment is aborted.
    #[tokio::test]
    async fn mission_route_aborts_a_commander_off_the_roll_roster() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        let mut commander = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 42);
        commander.fatal_flaw = "ZZ".into();
        let commander_id = commander.commander_id;
        state.commander_repo.create_commander(commander).await.expect("create_commander");
        let section = member_section(&state, c.campaign_id).await;
        let body = serde_json::json!({
            "squad": { "units": rookies(8) },
            "mission_id": "GUARD_DUTY",
            "commander_id": commander_id,
            "section_ids": [section],
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/mission/resolve")
            .header("authorization", "Bearer t")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = with_test_peer(crate::routes::router(Arc::clone(&state)), TEST_PEER).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let deps = state.commander_repo.list_deployments_by_commander(commander_id).await.expect("deployments");
        assert_eq!(deps.iter().map(|d| d.status.as_str()).collect::<Vec<_>>(), ["aborted"]);
    }

    /// Reviewer BP-14a F3: a logged `roll_inputs` no build writes (rank 9) makes the replay a
    /// `500`, never a fight with no Commander rows.
    #[tokio::test]
    async fn replay_refuses_roll_inputs_off_the_roster() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, c.campaign_id).await;
        let bad = serde_json::json!({ "rank": 9, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" });
        let (commander, _) = fight_logged_at(&state, c.campaign_id, SUCCESS_SEED, None, chrono::Utc::now(), Some(bad)).await;
        let deps = state.commander_repo.list_deployments_by_commander(commander).await.expect("deployments");
        match replay_report(&state, &deps[0], "test").await {
            Err((status, _)) => assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR),
            Ok(_) => panic!("the replay rebuilt a fight from roll_inputs no build writes"),
        }
    }

    /// BP-14a: an approach off the list is a body rejection (`422`), before anything is opened.
    #[tokio::test]
    async fn mission_route_refuses_an_unknown_approach() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        let commander = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 42);
        let commander_id = commander.commander_id;
        state.commander_repo.create_commander(commander).await.expect("create_commander");
        let section = member_section(&state, c.campaign_id).await;
        let body = serde_json::json!({
            "squad": { "units": rookies(8) },
            "mission_id": "GUARD_DUTY",
            "commander_id": commander_id,
            "section_ids": [section],
            "approach": "Charge",
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/mission/resolve")
            .header("authorization", "Bearer t")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = with_test_peer(crate::routes::router(Arc::clone(&state)), TEST_PEER).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let deps = state.commander_repo.list_deployments_by_commander(commander_id).await.expect("deployments");
        assert!(deps.is_empty());
    }

    /// BP-11a-1: seed 7 raids on drill 1; the fight (seed 3, 8 Rookies: Success, 1 KIA) costs the
    /// Section 1 soldier on the read that latches it, and only once.
    #[tokio::test]
    async fn ops_raid_loss_lowers_the_section_headcount() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"]["dig"]["raid"].is_null(), "latched: {body}");
        assert_eq!(section_headcount(&state, section).await, 7, "8 − 1 KIA");
        assert_eq!(get(&state, Some("t"), c).await.0, StatusCode::OK);
        assert_eq!(section_headcount(&state, section).await, 7, "applied once");
        assert_eq!(state.launch_locks.len(), 0, "the launch lock is released");
    }

    /// BP-11a-1: seed 14's ambush; the fight (seed 43, 8 Rookies: Defeat, 4 KIA) costs the Section
    /// 4 soldiers on the read that latches it, and only once.
    #[tokio::test]
    async fn ops_ambush_loss_lowers_the_section_headcount() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        dig_on_site(&state, c, 14).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let (_, body) = get(&state, Some("t"), c).await;
        assert_eq!(body["active_op"]["dig"]["ambush"]["mission_id"], "GUARD_DUTY");
        fight_logged(&state, c, DEFEAT_4_KIA_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"]["dig"]["ambush"].is_null(), "latched: {body}");
        assert_eq!(section_headcount(&state, section).await, 4, "8 − 4 KIA");
        assert_eq!(get(&state, Some("t"), c).await.0, StatusCode::OK);
        assert_eq!(section_headcount(&state, section).await, 4, "applied once");
    }

    /// BP-11a-1: a Section at 0 can't fight, so seed 7's raid latches as a Wipeout on the next
    /// read without any fight: the op is cleared and the Commander has no deployment.
    #[tokio::test]
    async fn ops_raid_with_the_section_at_zero_wipes_without_a_fight() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        dig_on_site(&state, c, 7).await;
        state.section_repo.update_headcount(section, 0).await.expect("update_headcount");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["dig"]["raid"]["mission_id"], "GUARD_DUTY");
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "wiped: {body}");
        let commander = commander_of(&state, c).await;
        assert!(state.commander_repo.list_deployments_by_commander(commander).await.expect("deployments").is_empty());
    }

    /// BP-11a-1: with the member's first Section at 0 and the Commander alive, `ops/start` refuses.
    #[tokio::test]
    async fn ops_start_refuses_while_the_section_is_at_zero() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        state.section_repo.update_headcount(section, 0).await.expect("update_headcount");
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no soldiers left to send: start a new run")));
        assert!(state.demo_ops.store.op_view(c, chrono::Utc::now()).is_none());
    }

    /// BP-11a-1: a record whose outcome the recompute does not reproduce (seed 3 is a Success,
    /// recorded as a Defeat) is a `500`; nothing is applied and the raid stays pending.
    #[tokio::test]
    async fn fight_result_refuses_an_outcome_that_does_not_match_the_record() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        fight_logged(&state, c, SUCCESS_SEED, Some(sim_engine::game_types::OutcomeType::Defeat)).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(section_headcount(&state, section).await, 8, "nothing applied");
        let op = state.demo_ops.store.op_view(c, chrono::Utc::now()).expect("op");
        assert!(op.dig.expect("a dig").raid.is_some(), "the raid stays pending");
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
        seed_commander(&state, c.campaign_id).await;
        assert_eq!(start(&state, c.campaign_id, SITE).await.0, StatusCode::OK);
        assert_eq!(post(&state, c.campaign_id, "return", None).await.0, StatusCode::TOO_MANY_REQUESTS);
    }

    async fn build(state: &Arc<AppState>, campaign: Uuid, building: &str) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "build", Some(&serde_json::json!({ "building": building }).to_string())).await
    }

    /// Seed 2's 120-ore haul lands; the build's own call banks it and pays the Radio Shack's 50,
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
        let none = serde_json::json!({"radio_shack": false, "hobbyist_benches": false, "grease_pit": false, "gunsmith": false, "basic_smelter": false});
        assert_eq!(body["base"], none);
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let (status, body, _) = build(&state, c, "radio_shack").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "home: {body}");
        let shack = serde_json::json!({"radio_shack": true, "hobbyist_benches": false, "grease_pit": false, "gunsmith": false, "basic_smelter": false});
        assert_eq!((body["ore_banked"].as_u64(), &body["base"]), (Some(70), &shack));
        let (status, body, _) = build(&state, c, "radio_shack").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already built")));
        let (status, body, _) = build(&state, c, "gunsmith").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not enough banked ore")));
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["ore_banked"].as_u64(), &body["base"]), (Some(70), &shack));
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
        seed_commander(&state, c.campaign_id).await;
        assert_eq!(start(&state, c.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = build(&state, c.campaign_id, "barracks").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown building")), "before the team is out");
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

    /// BP-11a-2: a contract campaign (its Commander already seeded) with its sector and its first
    /// Section (8 soldiers), the team sent to `SITE` and its arrival crossed. Returns the
    /// campaign, its Section and the arrival instant as the store now holds it.
    async fn contract_on_site(state: &Arc<AppState>, c: &CampaignInstance) -> (Uuid, Uuid, chrono::DateTime<chrono::Utc>) {
        seed_sector(state, c).await;
        let section = member_section(state, c.campaign_id).await;
        let (status, started, _) = start(state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let secs = site_secs(&started, SITE);
        let arrive_at = chrono::DateTime::parse_from_rfc3339(started["active_op"]["arrive_at"].as_str().expect("arrive_at"))
            .expect("rfc3339")
            .with_timezone(&chrono::Utc);
        state.demo_ops.store.rewind_op(c.campaign_id, secs);
        (c.campaign_id, section, arrive_at - chrono::Duration::seconds(secs))
    }

    /// BP-11a-2: a contract start needs a usable Commander, as a dig does.
    #[tokio::test]
    async fn ops_contract_start_needs_a_commander() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Commander left to lead: start a new run")));
        assert!(state.demo_ops.store.op_view(c.campaign_id, chrono::Utc::now()).is_none());
        seed_commander(&state, c.campaign_id).await;
        let (status, body, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// BP-11a-2: a won contract banks its pay (SUCCESS_SEED pays 400) and costs its losses
    /// (1 KIA), once; the team stays on site with `fought: "won"`. A second fight changes nothing.
    #[tokio::test]
    async fn ops_contract_latch_banks_its_pay() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 400);
        assert_eq!(section_headcount(&state, section).await, 7);
        assert_eq!(body["active_op"]["leg"], "outbound");
        assert_eq!(body["active_op"]["fought"], "won");
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 400, "a second fight on site pays nothing");
        assert_eq!(section_headcount(&state, section).await, 7, "and costs nothing");
    }

    /// BP-11a-2: a lost contract pays nothing, costs its loss, and leaves the team on site
    /// (`fought: "lost"`) until it heads home.
    #[tokio::test]
    async fn ops_contract_defeat_stays_on_site_and_pays_nothing() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, DEFEAT_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 0);
        assert_eq!(section_headcount(&state, section).await, 7);
        assert_eq!(body["active_op"]["fought"], "lost");
        assert_eq!(body["active_op"]["leg"], "outbound");
        let (status, body, _) = post(&state, c, "return", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["leg"], "homeward");
    }

    /// BP-11a-2: a wiped-out contract ends the op and pays nothing.
    #[tokio::test]
    async fn ops_contract_wipeout_ends_the_op() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, WIPEOUT_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "{body}");
        assert_eq!(body["credits_banked"], 0);
        assert_eq!(section_headcount(&state, section).await, 7);
    }

    /// BP-11a-2: a fight started before the arrival (the hub's list) is not the contract's; the
    /// contract stays pending and its own fight, after the arrival, latches.
    #[tokio::test]
    async fn ops_contract_fight_before_arrival_is_not_its_fight() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, arrival) = contract_on_site(&state, &cc).await;
        fight_logged_at(&state, c, SUCCESS_SEED, None, arrival - chrono::Duration::seconds(60), None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 0);
        assert_eq!(section_headcount(&state, section).await, 8);
        assert!(body["active_op"]["fought"].is_null(), "{body}");
        assert_eq!(body["active_op"]["leg"], "outbound");
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 400, "the contract was still pending");
        assert_eq!(section_headcount(&state, section).await, 7);
    }

    /// BP-11a-2: a won raid banks its pay too (W1: every latched fight).
    #[tokio::test]
    async fn ops_raid_win_banks_its_pay() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        dig_on_site(&state, c, 7).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        fight(&state, c, sim_engine::game_types::OutcomeType::Success).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 400);
    }

    /// Reviewer BP-11a-1 F1: two ops reads recompute the same contract fight and both park at the
    /// latch seam; released together, one latch applies, so one loss and one pay land. The
    /// test's own `AppState` reuses `WALLET` (no `[n; 32]` byte is free).
    #[tokio::test]
    async fn ops_contract_latch_applies_once_under_two_racers() {
        let mut state = session_state();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        state.demo_ops.latch_seam = Some(Arc::new(move || { let barrier = Arc::clone(&barrier); Box::pin(async move { barrier.wait().await; }) }));
        let state = Arc::new(state);
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let raced = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(get(&state, Some("t"), c), get(&state, Some("t"), c))
        })
        .await;
        let ((a, body_a), (b, body_b)) = raced.expect("both readers reached the seam");
        assert_eq!(a, StatusCode::OK, "{body_a}");
        assert_eq!(b, StatusCode::OK, "{body_b}");
        assert_eq!(section_headcount(&state, section).await, 7, "one loss");
        assert_eq!(state.demo_ops.store.ops_state(c, Utc::now()).credits_banked, 400, "one pay");
        assert_eq!(state.launch_locks.len(), 0);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["company"]["trophies"].as_array().expect("trophies").len(), 1, "one trophy");
    }

    async fn heal(state: &Arc<AppState>, campaign: Uuid, section_id: &str) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "heal", Some(&serde_json::json!({ "section_id": section_id }).to_string())).await
    }

    /// BP-12a-1: a lost contract (`DEFEAT_SEED`: 1 KIA, 1 wounded, pays 0) puts its wounded in the
    /// Section's sickbay on the latching read, beside the loss; with nothing banked, the heal is
    /// refused.
    #[tokio::test]
    async fn ops_defeat_wounds_one_and_heal_waits_for_credits() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, DEFEAT_SEED, None).await;
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["company"]["sections"],
            serde_json::json!([{ "section_id": section, "wounded": 1, "healing": 0, "healed_at": null }])
        );
        assert_eq!(section_headcount(&state, section).await, 7);
        let (status, body, _) = heal(&state, c, &section.to_string()).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not enough banked credits")));
    }

    /// BP-12a-1: a won contract with wounded (`HEAL_WIN_SEED`: 3 wounded, 600 credits) heals all
    /// three at once for 40 each; they are healing until 15 s after the heal's own `now`.
    #[tokio::test]
    async fn ops_heal_spends_credits() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, HEAL_WIN_SEED, None).await;
        let (status, before) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{before}");
        let wounded = before["company"]["sections"][0]["wounded"].as_u64().expect("wounded");
        assert_eq!(wounded, 3, "HEAL_WIN_SEED's measured wounded: {before}");
        let banked = before["credits_banked"].as_u64().expect("credits");
        assert_eq!(banked, 600, "HEAL_WIN_SEED's measured pay: {before}");
        let (status, body, _) = heal(&state, c, &section.to_string()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"].as_u64(), Some(banked - 40 * wounded));
        let now = chrono::DateTime::parse_from_rfc3339(body["now"].as_str().expect("now")).expect("rfc3339").with_timezone(&chrono::Utc);
        assert_eq!(
            body["company"]["sections"][0],
            serde_json::json!({ "section_id": section, "wounded": 0, "healing": wounded, "healed_at": ops_time(now + chrono::Duration::seconds(15)) })
        );
    }

    /// BP-12a-1: a `section_id` that is not a UUID is `400`; a UUID with no wounded is `409`.
    #[tokio::test]
    async fn ops_heal_refusals() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &c).await;
        let (status, body, _) = heal(&state, c.campaign_id, "nope").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown section")));
        let (status, body, _) = heal(&state, c.campaign_id, &Uuid::new_v4().to_string()).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("nobody to heal")));
    }

    async fn replace(state: &Arc<AppState>, campaign: Uuid, body: serde_json::Value) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "replace", Some(&body.to_string())).await
    }

    /// The campaign's banked credits, from a `GET` (BP-12a-2).
    async fn banked(state: &Arc<AppState>, campaign: Uuid) -> u64 {
        let (status, body) = get(state, Some("t"), campaign).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["credits_banked"].as_u64().expect("credits_banked")
    }

    /// BP-12a-2: an `Active` campaign of `WALLET` with its sector, its entry made by a first `GET`,
    /// and `credits` banked.
    async fn replace_campaign(state: &Arc<AppState>, credits: u32) -> Uuid {
        let c = campaign_with(state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(state, &c).await;
        let (status, body) = get(state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        state.demo_ops.store.set_credits(c.campaign_id, credits);
        c.campaign_id
    }

    /// BP-12a-2: three soldiers for 120 each, onto the same Section row.
    #[tokio::test]
    async fn ops_replace_adds_soldiers_for_120_a_head() {
        let state = Arc::new(session_state());
        let c = replace_campaign(&state, 400).await;
        let section = member_section(&state, c).await;
        state.section_loss.lose_soldiers(section, 3).await.expect("lose_soldiers");
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": section, "count": 3 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 40);
        assert_eq!(section_headcount(&state, section).await, 8);
    }

    /// BP-12a-2: a Section at 8 has no room, and neither has an unknown id (Wiki Q2): `409`, and
    /// the credits come back.
    #[tokio::test]
    async fn ops_replace_at_8_refunds_and_is_409() {
        let state = Arc::new(session_state());
        let c = replace_campaign(&state, 400).await;
        let section = member_section(&state, c).await;
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": section, "count": 1 })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no room: a Section holds 8")));
        assert_eq!(banked(&state, c).await, 400, "refunded");
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": Uuid::new_v4(), "count": 1 })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no room: a Section holds 8")));
        assert_eq!(banked(&state, c).await, 400, "refunded");
        assert_eq!(section_headcount(&state, section).await, 8);
    }

    /// BP-12a-2: refused while the team is out (a contract won with `SUCCESS_SEED`, still on
    /// site: 400 banked, the Section at 7, so only the out check stands in the way), and when the
    /// bank is short (a second campaign of `WALLET`, at home with 0 banked).
    #[tokio::test]
    async fn ops_replace_refusals() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        assert_eq!(banked(&state, c).await, 400);
        assert_eq!(section_headcount(&state, section).await, 7);
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": section, "count": 1 })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("the team is out: replacements report at base")));
        assert_eq!(banked(&state, c).await, 400);
        assert_eq!(section_headcount(&state, section).await, 7);
        let home = replace_campaign(&state, 0).await;
        let home_section = member_section(&state, home).await;
        state.section_loss.lose_soldiers(home_section, 1).await.expect("lose_soldiers");
        let (status, body, _) = replace(&state, home, serde_json::json!({ "section_id": home_section, "count": 1 })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not enough banked credits")));
        assert_eq!(section_headcount(&state, home_section).await, 7);
    }

    /// Reviewer BP-11a-1 F2 (BP-12a-2): replace writes the same Section row, so a first Section at
    /// 0 is lifted. With a second Section at 8 the start refuses; replacing into the second
    /// changes nothing for the start (it reads the first); replacing into the first frees it.
    #[tokio::test]
    async fn ops_replace_lifts_the_first_section_at_0() {
        let state = Arc::new(session_state());
        let c = replace_campaign(&state, 1000).await;
        seed_commander(&state, c).await;
        let first = member_section(&state, c).await;
        let created = state.section_repo.get_section(first).await.unwrap().expect("the first").created_at;
        let mut s = crate::repository::section::starter_section(c, WALLET.to_vec());
        s.created_at = created + chrono::Duration::seconds(1);
        let second = s.section_id;
        state.section_repo.create_section(s).await.expect("create_section");
        state.section_loss.lose_soldiers(first, 8).await.expect("lose_soldiers");
        let (status, body, _) = start(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no soldiers left to send: start a new run")));
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": second, "count": 1 })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no room: a Section holds 8")));
        assert_eq!(banked(&state, c).await, 1000, "refunded");
        state.section_loss.lose_soldiers(second, 4).await.expect("lose_soldiers");
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": second, "count": 4 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 520);
        assert_eq!(section_headcount(&state, second).await, 8);
        let (status, body, _) = start(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no soldiers left to send: start a new run")));
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": first, "count": 3 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 160);
        assert_eq!(section_headcount(&state, first).await, 3);
        let (status, body, _) = start(&state, c, SITE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// BP-12a-2: a Crew at 0 can't dig; three miners for 80 each lift it, with morale unchanged
    /// (Wiki Q3); past 8 there is no room, and the credits come back.
    #[tokio::test]
    async fn ops_replace_crew_lifts_the_dig_at_0() {
        let state = Arc::new(session_state());
        let c = replace_campaign(&state, 1000).await;
        let crew = seed_crew(&state, c, false).await;
        state.crew_loss.meltdown_loss(crew, 8, 0).await.expect("meltdown_loss");
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Crew left to dig: start a new run")));
        let morale = state.crew_repo.get_crew(crew).await.unwrap().expect("the Crew").morale;
        let (status, body, _) = replace(&state, c, serde_json::json!({ "crew_id": crew, "count": 3 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["credits_banked"], 760);
        assert_eq!(headcount(&state, crew).await, 3);
        assert_eq!(state.crew_repo.get_crew(crew).await.unwrap().expect("the Crew").morale, morale, "no morale moves");
        let (status, body, _) = replace(&state, c, serde_json::json!({ "crew_id": crew, "count": 6 })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no room: a Crew holds 8")));
        assert_eq!(banked(&state, c).await, 760, "refunded");
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// BP-12a-2: a failed write is the journalled `500`, and the credits come back.
    #[tokio::test]
    async fn ops_replace_write_failure_refunds_and_is_500() {
        let mut state = session_state();
        state.section_loss = Arc::new(FailingSectionLoss);
        let state = Arc::new(state);
        let c = replace_campaign(&state, 400).await;
        let section = member_section(&state, c).await;
        let (status, body, _) = replace(&state, c, serde_json::json!({ "section_id": section, "count": 1 })).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(banked(&state, c).await, 400, "refunded");
        assert_eq!(state.launch_locks.len(), 0);
    }

    /// BP-12a-2: the body's refusals, ours (`400`, after the member checks) and axum's (`422`);
    /// none spends a credit.
    #[tokio::test]
    async fn ops_replace_body_rejections() {
        let state = Arc::new(session_state());
        let c = replace_campaign(&state, 1000).await;
        let section = member_section(&state, c).await.to_string();
        let crew = Uuid::new_v4().to_string();
        let cases: Vec<(String, StatusCode, Option<&str>)> = vec![
            (serde_json::json!({ "section_id": section, "count": 9 }).to_string(), StatusCode::BAD_REQUEST, Some("count must be 1 to 8")),
            (serde_json::json!({ "section_id": section, "count": 0 }).to_string(), StatusCode::BAD_REQUEST, Some("count must be 1 to 8")),
            (serde_json::json!({ "section_id": section, "count": -1 }).to_string(), StatusCode::BAD_REQUEST, Some("count must be 1 to 8")),
            (serde_json::json!({ "section_id": section, "count": 300 }).to_string(), StatusCode::BAD_REQUEST, Some("count must be 1 to 8")),
            (serde_json::json!({ "section_id": section, "crew_id": crew, "count": 1 }).to_string(), StatusCode::BAD_REQUEST, Some("name one of section_id or crew_id")),
            (serde_json::json!({ "count": 1 }).to_string(), StatusCode::BAD_REQUEST, Some("name one of section_id or crew_id")),
            (serde_json::json!({ "section_id": "nope", "count": 1 }).to_string(), StatusCode::BAD_REQUEST, Some("unknown section")),
            (serde_json::json!({ "crew_id": "nope", "count": 1 }).to_string(), StatusCode::BAD_REQUEST, Some("unknown crew")),
            (serde_json::json!({ "section_id": section, "count": "3" }).to_string(), StatusCode::UNPROCESSABLE_ENTITY, None),
            (serde_json::json!({ "section_id": section }).to_string(), StatusCode::UNPROCESSABLE_ENTITY, None),
        ];
        for (body, want, error) in cases {
            let (status, got, _) = post(&state, c, "replace", Some(&body)).await;
            assert_eq!(status, want, "{body} → {got}");
            if let Some(e) = error {
                assert_eq!(got["error"].as_str(), Some(e), "{body}");
            }
            assert_eq!(banked(&state, c).await, 1000, "{body} spends nothing");
        }
    }

    /// BP-12a-1, the twin of `ops_contract_latch_applies_once_under_two_racers` on `DEFEAT_SEED`
    /// (1 KIA, 1 wounded): two readers park at the latch seam; one latch applies, so one loss and
    /// one wounded land. The test's own `AppState` reuses `WALLET` (no `[n; 32]` byte is free).
    #[tokio::test]
    async fn ops_contract_defeat_latches_wounded_once_under_two_racers() {
        let mut state = session_state();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        state.demo_ops.latch_seam = Some(Arc::new(move || { let barrier = Arc::clone(&barrier); Box::pin(async move { barrier.wait().await; }) }));
        let state = Arc::new(state);
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, section, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, DEFEAT_SEED, None).await;
        let raced = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(get(&state, Some("t"), c), get(&state, Some("t"), c))
        })
        .await;
        let ((a, body_a), (b, body_b)) = raced.expect("both readers reached the seam");
        assert_eq!(a, StatusCode::OK, "{body_a}");
        assert_eq!(b, StatusCode::OK, "{body_b}");
        assert_eq!(section_headcount(&state, section).await, 7, "one loss");
        let company = state.demo_ops.store.ops_state(c, Utc::now()).company;
        assert_eq!(company.sections.len(), 1);
        assert_eq!((company.sections[0].section_id, company.sections[0].wounded), (section, 1), "one wounded, not 2");
    }

    /// BP-11a-3: a won contract leaves the board at its latch (a start of it is `400 unknown
    /// site`) while the team stays at its own hex (`at`) with `fought: "won"`; 45 s later it is
    /// back at another hex and the op is unchanged. The test's own `AppState` reuses `WALLET`.
    #[tokio::test]
    async fn ops_fought_contract_leaves_the_board_and_returns() {
        let state = Arc::new(session_state());
        let c = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, c.campaign_id).await;
        seed_sector(&state, &c).await;
        member_section(&state, c.campaign_id).await;
        let (status, started, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let pin = started["sites"].as_array().expect("sites").iter().find(|s| s["site_id"] == SITE).expect("on the board").clone();
        let hex = serde_json::json!({"q": pin["q"], "r": pin["r"]});
        assert_eq!(started["active_op"]["at"], hex);
        state.demo_ops.store.rewind_op(c.campaign_id, site_secs(&started, SITE));
        fight_logged(&state, c.campaign_id, SUCCESS_SEED, None).await;
        let on_board = |body: &serde_json::Value| body["sites"].as_array().expect("sites").iter().find(|s| s["site_id"] == SITE).cloned();
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["fought"], "won");
        assert_eq!(body["active_op"]["at"], hex, "the team stands where it fought");
        assert_eq!(on_board(&body), None, "off the board at the latch");
        let (status, refused, _) = start(&state, c.campaign_id, SITE).await;
        assert_eq!((status, refused["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown site")));
        state.demo_ops.store.rewind_op(c.campaign_id, 45);
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let back = on_board(&body).expect("back after 45 s");
        assert_ne!(serde_json::json!({"q": back["q"], "r": back["r"]}), hex, "at another hex");
        assert_eq!((&back["mission_id"], &back["kind"]), (&pin["mission_id"], &pin["kind"]));
        assert_eq!(body["active_op"]["at"], hex, "the op is unchanged");
        assert_eq!(body["active_op"]["fought"], "won");
        assert_eq!(body["credits_banked"], 400);
    }

    async fn next(state: &Arc<AppState>, campaign: Uuid, site_id: &str) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "next", Some(&serde_json::json!({ "site_id": site_id }).to_string())).await
    }

    const NEXT_SITE: &str = "contract:SUPPLY_RAID";

    /// BP-11a-4: after a won contract (400 banked), `ops/next` sends the team on from its own hex
    /// as a new op: the whole state, the leg at the target's seconds per hex for the hop, the
    /// pay kept and no `fought`. The test's own `AppState` reuses `WALLET`.
    #[tokio::test]
    async fn ops_next_moves_the_team() {
        let state = Arc::new(session_state());
        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_commander(&state, cc.campaign_id).await;
        let (c, _, _) = contract_on_site(&state, &cc).await;
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let (status, fought) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{fought}");
        assert_eq!(fought["credits_banked"], 400);
        let from = fought["active_op"]["at"].clone();
        let target = fought["sites"].as_array().expect("sites").iter().find(|s| s["site_id"] == NEXT_SITE).expect("on the board").clone();
        let (status, body, _) = next(&state, c, NEXT_SITE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let keys: std::collections::BTreeSet<&str> = body.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["active_op", "base", "campaign_id", "company", "credits_banked", "gateway", "now", "ore_banked", "sites"].into_iter().collect());
        let op = &body["active_op"];
        let op_keys: std::collections::BTreeSet<&str> = op.as_object().expect("an op").keys().map(|k| k.as_str()).collect();
        assert_eq!(op_keys, ["arrive_at", "arrived", "at", "depart_at", "kind", "leg", "site_id"].into_iter().collect());
        assert_eq!((op["site_id"].as_str(), op["leg"].as_str()), (Some(NEXT_SITE), Some("outbound")));
        assert_eq!(op["at"], serde_json::json!({"q": target["q"], "r": target["r"]}));
        let n = |v: &serde_json::Value| v.as_i64().expect("a number");
        let hop = i64::from(crate::map_gen::hex_axial_distance(
            n(&from["q"]) as i32,
            n(&from["r"]) as i32,
            n(&target["q"]) as i32,
            n(&target["r"]) as i32,
        ));
        assert!(hop >= 1, "another hex");
        assert_eq!(secs_between(op), (n(&target["travel_secs"]) / n(&target["distance"]).max(1) * hop).max(1));
        assert_eq!(body["credits_banked"], 400);
        assert!(op["fought"].is_null(), "{body}");
    }

    /// BP-11a-4: `ops/next`'s refusals: no gateway, no op, still travelling, already there, an
    /// unknown or just-fought site, and already heading home. The test's own `AppState` reuses
    /// `WALLET`.
    #[tokio::test]
    async fn ops_next_refusals() {
        let state = Arc::new(session_state());
        let none = campaign_with(&state, WALLET, true, None).await;
        let (status, body, _) = next(&state, none.campaign_id, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));

        let cc = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        let c = cc.campaign_id;
        seed_sector(&state, &cc).await;
        seed_commander(&state, c).await;
        member_section(&state, c).await;
        let (status, body, _) = next(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));
        let (status, started, _) = start(&state, c, SITE).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let (status, body, _) = next(&state, c, NEXT_SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("still travelling")));
        state.demo_ops.store.rewind_op(c, site_secs(&started, SITE));
        let (status, body, _) = next(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already there")));
        let (status, body, _) = next(&state, c, "dig:99,99").await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown site")));
        fight_logged(&state, c, SUCCESS_SEED, None).await;
        let (status, body, _) = next(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown site")), "just fought");
        assert_eq!(state.demo_ops.store.op_view(c, chrono::Utc::now()).expect("op").fought, Some("won"));
        let (status, body, _) = post(&state, c, "return", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body, _) = next(&state, c, NEXT_SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
    }

    /// BP-11a-4: with the member's first Section at 0, `ops/next` refuses as `ops/start` does and
    /// the op stays where it is.
    #[tokio::test]
    async fn ops_next_refuses_at_zero() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        dig_on_site(&state, c, 2).await;
        state.section_repo.update_headcount(section, 0).await.expect("update_headcount");
        let (status, body, _) = next(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no soldiers left to send: start a new run")));
        assert_eq!(state.demo_ops.store.op_view(c, chrono::Utc::now()).expect("op").site_id, DIG);
    }

    /// BP-11a-4: axum's rejections on `ops/next`, after the limiters and before the campaign is
    /// read (the id is unknown, so anything past the body would be `404`).
    #[tokio::test]
    async fn ops_next_body_rejections() {
        let state = Arc::new(session_state());
        let c = Uuid::new_v4();
        let big = format!("{{\"site_id\":\"{}\"}}", "x".repeat(2048));
        assert_eq!(post(&state, c, "next", Some(&big)).await.0, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(post(&state, c, "next", Some("{")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(post(&state, c, "next", None).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(post(&state, c, "next", Some("{}")).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ---- BP-17a: merc escort. Seeds from the Crew stream's pins (`crew_fight_roll_pins`); every
    // raid seed raids on drill 1 (drill d100 at most 5).

    async fn start_alone(state: &Arc<AppState>, campaign: Uuid) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "start", Some(&serde_json::json!({ "site_id": DIG, "escort": false }).to_string())).await
    }

    /// An unescorted dig at `DIG`, on site with the seed `seed`.
    async fn alone_on_site(state: &Arc<AppState>, campaign: Uuid, seed: u32) {
        let (status, started, _) = start_alone(state, campaign).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        assert!(started["active_op"]["dig"]["escort_at"].is_null(), "alone: {started}");
        state.demo_ops.store.rewind_op(campaign, site_secs(&started, DIG));
        state.demo_ops.store.set_op_seed(campaign, seed);
    }

    async fn escort(state: &Arc<AppState>, campaign: Uuid) -> (StatusCode, serde_json::Value, Vec<u8>) {
        post(state, campaign, "escort", None).await
    }

    /// An escorted start shows its guard from the start; an unescorted one shows none.
    #[tokio::test]
    async fn ops_start_shows_the_guard() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["dig"]["escort_at"], body["active_op"]["depart_at"]);
    }

    /// Seed 45 alone: drill 1 brings raiders and the Crew falls back with 2 heads; the drill's own
    /// body has no raid and shows the fight; half the 20 ore rides home; nothing is paid. The
    /// fight outlives the arrival and is cleared by the next start.
    #[tokio::test]
    async fn ops_unescorted_raid_is_the_crews_retreat() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 45).await;
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let op = &body["active_op"];
        assert!(op["dig"]["raid"].is_null(), "never pending: {body}");
        assert_eq!((op["leg"].as_str(), op["dig"]["ended_by"].as_str(), op["dig"]["carried"].as_u64()), (Some("homeward"), Some("retreat"), Some(10)));
        assert_eq!(body["crew_fight"], serde_json::json!({"kind": "raid", "result": "retreat", "heads_lost": 2}));
        assert_eq!(body["credits_banked"], 0);
        assert_eq!(headcount(&state, crew).await, 6);
        assert_eq!(state.launch_locks.len(), 0);
        state.demo_ops.store.rewind_op(c, dig_secs(&state, c).await);
        let (_, body) = get(&state, Some("t"), c).await;
        assert!(body["active_op"].is_null(), "home: {body}");
        assert_eq!((body["ore_banked"].as_u64(), body["crew_fight"]["result"].as_str()), (Some(10), Some("retreat")));
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.get("crew_fight").is_none(), "cleared by the start: {body}");
    }

    /// Seed 191 alone: the Crew holds, loses nobody and drills on.
    #[tokio::test]
    async fn ops_unescorted_raid_held_drills_on() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 191).await;
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["crew_fight"], serde_json::json!({"kind": "raid", "result": "held", "heads_lost": 0}));
        assert_eq!((body["active_op"]["leg"].as_str(), body["active_op"]["dig"]["ore_on_site"].as_u64()), (Some("outbound"), Some(20)));
        assert_eq!(headcount(&state, crew).await, 8);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK, "drills on");
    }

    /// Seed 505 alone: the Crew is wiped out: the op is cleared, the ore lost, every head gone,
    /// and the next dig is refused for want of a Crew.
    #[tokio::test]
    async fn ops_crew_wipeout_ends_the_op_and_takes_every_head() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 505).await;
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"].is_null(), "{body}");
        assert_eq!(body["crew_fight"], serde_json::json!({"kind": "raid", "result": "wiped", "heads_lost": 8}));
        assert_eq!(body["ore_banked"], 0);
        assert_eq!(headcount(&state, crew).await, 0);
        assert_eq!(state.launch_locks.len(), 0);
        let (status, body, _) = start_alone(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no Crew left to dig: start a new run")));
    }

    /// With the Section at 0 the Crew can still dig alone; the escorted start is refused.
    #[tokio::test]
    async fn ops_unescorted_dig_starts_with_the_section_at_0() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        state.section_repo.update_headcount(section, 0).await.expect("update_headcount");
        let (status, body, _) = start(&state, c, DIG).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no soldiers left to send: start a new run")));
        let (status, body, _) = start_alone(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// Seed 45: the guard is sent but has not arrived, so the drill's raiders are still the
    /// Crew's.
    #[tokio::test]
    async fn ops_raid_before_the_guard_arrives_is_the_crews() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 45).await;
        let (status, body, _) = escort(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"]["dig"]["escort_at"].is_string(), "{body}");
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["active_op"]["dig"]["raid"].is_null(), "{body}");
        assert_eq!(body["crew_fight"]["result"], "retreat");
        assert_eq!(headcount(&state, crew).await, 6);
    }

    /// Seed 7: once the guard stands, the drill's raiders are the Section's (a pending raid, no
    /// Crew fight, no heads lost).
    #[tokio::test]
    async fn ops_raid_after_the_guard_arrives_is_the_sections() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 7).await;
        assert_eq!(escort(&state, c).await.0, StatusCode::OK);
        state.demo_ops.store.rewind_op(c, dig_secs(&state, c).await);
        let (status, body, _) = drill(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active_op"]["dig"]["raid"], serde_json::json!({"mission_id": "GUARD_DUTY"}));
        assert!(body.get("crew_fight").is_none(), "{body}");
        assert_eq!(headcount(&state, crew).await, 8);
    }

    /// `ops/escort`'s refusals in its order, then one that goes through.
    #[tokio::test]
    async fn ops_escort_refusals() {
        let state = Arc::new(session_state());
        let contract = campaign_with(&state, WALLET, true, Some(GATEWAY)).await;
        seed_sector(&state, &contract).await;
        seed_commander(&state, contract.campaign_id).await;
        let (status, body, _) = escort(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("no op under way")));
        assert_eq!(start(&state, contract.campaign_id, SITE).await.0, StatusCode::OK);
        let (status, body, _) = escort(&state, contract.campaign_id).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("not a dig site")));
        let (c, _) = dig_campaign(&state, false).await;
        assert_eq!(start(&state, c, DIG).await.0, StatusCode::OK);
        let (status, body, _) = escort(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already guarded")));
        let (d, _) = dig_campaign(&state, false).await;
        alone_on_site(&state, d, 2).await;
        assert_eq!(post(&state, d, "return", None).await.0, StatusCode::OK);
        let (status, body, _) = escort(&state, d).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already heading home")));
    }

    /// The Section is at base during an unescorted dig: a Requisition goes through, a Crew Hire
    /// is refused (the Crew is out), and once the guard is sent the Requisition is refused too.
    #[tokio::test]
    async fn ops_requisition_passes_during_an_unescorted_dig() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        state.section_repo.update_headcount(section, 6).await.expect("update_headcount");
        alone_on_site(&state, c, 2).await;
        state.demo_ops.store.set_credits(c, 1000);
        let body = serde_json::json!({ "section_id": section.to_string(), "count": 1 }).to_string();
        let (status, resp, _) = post(&state, c, "replace", Some(&body)).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert_eq!(section_headcount(&state, section).await, 7);
        let hire = serde_json::json!({ "crew_id": crew.to_string(), "count": 1 }).to_string();
        let (status, resp, _) = post(&state, c, "replace", Some(&hire)).await;
        assert_eq!((status, resp["error"].as_str()), (StatusCode::CONFLICT, Some("the team is out: replacements report at base")));
        assert_eq!(escort(&state, c).await.0, StatusCode::OK);
        let (status, resp, _) = post(&state, c, "replace", Some(&body)).await;
        assert_eq!((status, resp["error"].as_str()), (StatusCode::CONFLICT, Some("the team is out: replacements report at base")));
        assert_eq!(section_headcount(&state, section).await, 7);
    }

    /// Seed 59 alone (two drills, no raid; a 60-ore haul ambushed, d100 14): two reads reveal
    /// the Crew's ambush and both park at the latch seam; released together, one latch applies,
    /// so the Crew loses its 2 heads once (8 → 6, never 4) and half the load rides on.
    #[tokio::test]
    async fn ops_crew_ambush_loss_applies_once_under_two_racers() {
        let mut state = session_state();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        state.demo_ops.latch_seam = Some(Arc::new(move || { let barrier = Arc::clone(&barrier); Box::pin(async move { barrier.wait().await; }) }));
        let state = Arc::new(state);
        let (c, crew) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 59).await;
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.get("crew_fight").is_none(), "no raid on the way: {body}");
        state.demo_ops.store.rewind_op(c, site_secs(&body, DIG));
        let raced = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(get(&state, Some("t"), c), get(&state, Some("t"), c))
        })
        .await;
        let ((a, body_a), (b, body_b)) = raced.expect("both readers reached the seam");
        assert_eq!(a, StatusCode::OK, "{body_a}");
        assert_eq!(b, StatusCode::OK, "{body_b}");
        assert_eq!(headcount(&state, crew).await, 6, "one loss");
        assert_eq!(state.launch_locks.len(), 0);
        let s = state.demo_ops.store.ops_state(c, Utc::now());
        assert_eq!(s.crew_fight.map(|f| (f.kind, f.result, f.heads_lost)), Some(("ambush", "retreat", 2)));
        assert_eq!(s.op.and_then(|op| op.dig).map(|d| d.carried), Some(30));
    }

    // ---- BP-18a-1a: ops keyed by slots.

    /// BP-18a-1a (the Brief's residual (ii)): Next from a Crew-alone dig is refused until a guard
    /// stands.
    #[tokio::test]
    async fn ops_next_waits_for_the_guard() {
        let state = Arc::new(session_state());
        let (c, _) = dig_campaign(&state, false).await;
        alone_on_site(&state, c, 2).await;
        let (status, body, _) = next(&state, c, SITE).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("the guard hasn't reached the dig")));
        assert_eq!(state.demo_ops.store.op_view(c, chrono::Utc::now()).expect("op").site_id, DIG);
    }

    /// Reviewer BP-17a F1 + F4. Seed 59 alone on site (two drills, no raid; a 60-ore haul
    /// ambushed). The guard is sent at t0 and stands at t0 + 12 s (`DIG`'s time); a second escort
    /// is `409 already guarded` (F4, before the pull, which would answer "already heading home"
    /// first). The pull at t1 ≈ t0 puts the ambush at t1 + 6 s; a rewind of 7 s (never the whole
    /// 12 s, which would put the guard's arrival before the trigger and make it the Section's
    /// fight) puts the ambush at t1 − 1 s and the guard's arrival at t0 + 5 s. So the clock window:
    /// the GET that reveals the ambush must come within 5 s of the escort. Born before the guard
    /// stands, the ambush is the Crew's: 2 heads lost, the Section's headcount unchanged, half the
    /// load rides on.
    #[tokio::test]
    async fn ops_ambush_before_the_guard_stands_is_the_crews() {
        let state = Arc::new(session_state());
        let (c, crew) = dig_campaign(&state, false).await;
        let section = member_section(&state, c).await;
        alone_on_site(&state, c, 59).await;
        let (status, body, _) = escort(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body, _) = escort(&state, c).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("already guarded")));
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        assert_eq!(drill(&state, c).await.0, StatusCode::OK);
        let (status, body, _) = pull(&state, c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        state.demo_ops.store.rewind_op(c, 7);
        let (status, body) = get(&state, Some("t"), c).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["crew_fight"], serde_json::json!({"kind": "ambush", "result": "retreat", "heads_lost": 2}), "{body}");
        assert_eq!(headcount(&state, crew).await, 6);
        assert_eq!(section_headcount(&state, section).await, 8);
        assert_eq!(body["active_op"]["dig"]["carried"], 30, "{body}");
    }
}
