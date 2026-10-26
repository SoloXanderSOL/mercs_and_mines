use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::repository::{CampaignLifecycle, CommanderRecord, CrewRecord, ForemanRecord, SectionRecord};
use crate::state::AppState;

use super::{
    client_ip, conflict, forbidden, internal_error, not_found, too_many_requests, unauthorized, AppError,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/campaign/:campaign_id/roster", get(get_campaign_roster))
}

/// The caller's own roster in one campaign (BP-2b-2). Every view below is an allowlist: a
/// record field stays out of the response until it is added to its view. Values are the
/// stored display strings, pinned by each table's CHECKs.
#[derive(Serialize)]
pub struct RosterView {
    pub campaign_id: Uuid,
    pub commanders: Vec<CommanderView>,
    pub foremen: Vec<ForemanView>,
    pub sections: Vec<SectionView>,
    pub crews: Vec<CrewView>,
}

#[derive(Serialize)]
pub struct CommanderView {
    pub commander_id: Uuid,
    pub name: String,
    pub rank: i16,
    pub xp: i32,
    pub stress: i16,
    pub origin: String,
    pub specialization: String,
    pub fatal_flaw: String,
    pub veteran_trait: Option<String>,
    pub is_shattered: bool,
    pub is_kia: bool,
    pub commander_grade: Option<String>,
}

#[derive(Serialize)]
pub struct ForemanView {
    pub foreman_id: Uuid,
    pub name: String,
    pub rank: i16,
    pub xp: i32,
    pub stress: i16,
    pub origin: String,
    pub specialization: String,
    pub fatal_flaw: String,
    pub foreman_grade: String,
    pub is_shattered: bool,
    pub is_kia: bool,
}

#[derive(Serialize)]
pub struct SectionView {
    pub section_id: Uuid,
    pub name: String,
    pub headcount: i16,
    pub xp: i32,
}

#[derive(Serialize)]
pub struct CrewView {
    pub crew_id: Uuid,
    pub headcount: i16,
    pub xp: i32,
    pub morale: i16,
}

impl From<CommanderRecord> for CommanderView {
    fn from(r: CommanderRecord) -> Self {
        Self {
            commander_id: r.commander_id,
            name: r.name,
            rank: r.rank,
            xp: r.xp,
            stress: r.stress,
            origin: r.origin,
            specialization: r.specialization,
            fatal_flaw: r.fatal_flaw,
            veteran_trait: r.veteran_trait,
            is_shattered: r.is_shattered,
            is_kia: r.is_kia,
            commander_grade: r.commander_grade,
        }
    }
}

impl From<ForemanRecord> for ForemanView {
    fn from(r: ForemanRecord) -> Self {
        Self {
            foreman_id: r.foreman_id,
            name: r.name,
            rank: r.rank,
            xp: r.xp,
            stress: r.stress,
            origin: r.origin,
            specialization: r.specialization,
            fatal_flaw: r.fatal_flaw,
            foreman_grade: r.foreman_grade,
            is_shattered: r.is_shattered,
            is_kia: r.is_kia,
        }
    }
}

impl From<SectionRecord> for SectionView {
    fn from(r: SectionRecord) -> Self {
        Self { section_id: r.section_id, name: r.name, headcount: r.headcount, xp: r.xp }
    }
}

impl From<CrewRecord> for CrewView {
    fn from(r: CrewRecord) -> Self {
        Self { crew_id: r.crew_id, headcount: r.headcount, xp: r.xp, morale: r.morale }
    }
}

/// A member's own roster, `GET /api/campaign/:campaign_id/roster` (BP-2b-2, keeper). Order,
/// as the sector route: `401` (no session) → `429` per address (`roster_ip`) → `429` per
/// wallet (`roster_wallet`) → `404` (unknown campaign) → `403` (not a member; any
/// membership row) → `409` (not `Active` or `Ending`: the join writes the starters before
/// the launch, but the client keeps one rule) → the four reads. A failed read is `500`.
/// - Every read is scoped to the caller's wallet and ordered oldest first; no campaign-wide
///   list is read, so another member's records never reach this handler.
/// - KIA and Shattered Commanders are returned, with their flags.
/// - Read-only and not behind the join switch. Rate-limited (ODQ-57).
async fn get_campaign_roster(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Path(campaign_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.roster_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.roster_wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let wallet: Vec<u8> = bs58::decode(&token.wallet_address)
        .into_vec()
        .map_err(|_| unauthorized("session wallet is not valid base58"))?;

    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(not_found("campaign not found")),
        Err(e) => {
            return Err(internal_error(format!("get_campaign_roster: get_campaign failed for campaign {campaign_id}: {e}")));
        }
    };
    let membership = match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(m) => m,
        Err(e) => {
            return Err(internal_error(format!("get_campaign_roster: membership read failed for campaign {campaign_id}: {e}")));
        }
    };
    if membership.is_none() {
        return Err(forbidden("not a member of this campaign"));
    }
    let admitted = matches!(campaign.state, CampaignLifecycle::Active | CampaignLifecycle::Ending);
    if !admitted {
        return Err(conflict("campaign is not active"));
    }
    let commanders = match state.commander_repo.list_commanders_for_member(campaign_id, &wallet).await {
        Ok(v) => v,
        Err(e) => {
            return Err(internal_error(format!("get_campaign_roster: Commander read failed for campaign {campaign_id}: {e}")));
        }
    };
    let foremen = match state.foreman_repo.list_foremen_for_member(campaign_id, &wallet).await {
        Ok(v) => v,
        Err(e) => {
            return Err(internal_error(format!("get_campaign_roster: Foreman read failed for campaign {campaign_id}: {e}")));
        }
    };
    let sections = match state.section_repo.list_sections_for_member(campaign_id, &wallet).await {
        Ok(v) => v,
        Err(e) => {
            return Err(internal_error(format!("get_campaign_roster: Section read failed for campaign {campaign_id}: {e}")));
        }
    };
    let crews = match state.crew_repo.list_crews_for_member(campaign_id, &wallet).await {
        Ok(v) => v,
        Err(e) => {
            return Err(internal_error(format!("get_campaign_roster: Crew read failed for campaign {campaign_id}: {e}")));
        }
    };
    let view = RosterView {
        campaign_id,
        commanders: commanders.into_iter().map(CommanderView::from).collect(),
        foremen: foremen.into_iter().map(ForemanView::from).collect(),
        sections: sections.into_iter().map(SectionView::from).collect(),
        crews: crews.into_iter().map(CrewView::from).collect(),
    };
    Ok(Json(view).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::repository::{CampaignInstance, NewCampaignInstance};
    use crate::routes::{with_test_peer, TEST_PEER};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const WALLET: [u8; 32] = [86u8; 32];
    const OTHER: [u8; 32] = [87u8; 32];

    /// A signed-in session `"t"` for `WALLET`, not yet shared.
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

    fn signed_in() -> Arc<AppState> {
        Arc::new(session_state())
    }

    async fn get(state: &Arc<AppState>, token: Option<&str>, campaign: Uuid) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method("GET").uri(format!("/api/campaign/{campaign}/roster"));
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = with_test_peer(router().with_state(Arc::clone(state)), TEST_PEER).oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    /// `signed_in()` plus sessions `"w0"`…`"w{n-1}"` for wallets `[100 + i; 32]` (in memory
    /// only; `with_sessions(121)` reaches byte 220).
    fn with_sessions(n: u8) -> Arc<AppState> {
        let state = session_state();
        for i in 0..n {
            let wallet = bs58::encode([100 + i; 32]).into_string();
            let now = chrono::Utc::now().timestamp();
            state.sessions.insert(format!("w{i}"), shared::SessionToken {
                token_id: format!("w{i}"),
                account_id: wallet.clone(),
                wallet_address: wallet,
                issued_at: now,
                expires_at: now + 600,
            });
        }
        Arc::new(state)
    }

    /// A campaign with `wallet` as its member: `Pending` is left as created, anything else
    /// is activated.
    async fn campaign_with(state: &AppState, wallet: [u8; 32], lifecycle: CampaignLifecycle) -> CampaignInstance {
        let c = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 42 })
            .await
            .expect("create_campaign");
        state.membership_repo
            .add_member(&wallet, c.campaign_id, None, None)
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
        state.campaign_repo.get_campaign(c.campaign_id).await.expect("get_campaign").expect("the campaign exists")
    }

    /// One Commander, Foreman, Section and Crew for `wallet`, as a join writes them, through
    /// the generators (never `join_campaign`: its outcome enum differs on the demo). Returns
    /// their ids in that order.
    async fn seed_roster(state: &AppState, campaign: Uuid, wallet: [u8; 32], seed: u32) -> [Uuid; 4] {
        let commander = crate::commander_gen::generate_commander(wallet.to_vec(), campaign, seed);
        let foreman = crate::foreman_gen::generate_foreman(wallet.to_vec(), campaign, seed + 1);
        let section = crate::repository::section::starter_section(campaign, wallet.to_vec());
        let crew = CrewRecord::new_hire(campaign, wallet.to_vec());
        let ids = [commander.commander_id, foreman.foreman_id, section.section_id, crew.crew_id];
        state.commander_repo.create_commander(commander).await.expect("create_commander");
        state.foreman_repo.create_foreman(foreman).await.expect("create_foreman");
        state.section_repo.create_section(section).await.expect("create_section");
        state.crew_repo.create_crew(crew).await.expect("create_crew");
        ids
    }

    #[tokio::test]
    async fn roster_requires_a_session() {
        let state = signed_in();
        let (status, _) = get(&state, None, Uuid::new_v4()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn roster_unknown_campaign_is_404() {
        let state = signed_in();
        let (status, _) = get(&state, Some("t"), Uuid::new_v4()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `OTHER`'s roster is seeded, so a broken membership check would answer `200`.
    #[tokio::test]
    async fn roster_by_a_non_member_is_403() {
        let state = signed_in();
        let c = campaign_with(&state, OTHER, CampaignLifecycle::Active).await;
        seed_roster(&state, c.campaign_id, OTHER, 52).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    /// The starters exist before the launch, and the route still answers `409`.
    #[tokio::test]
    async fn roster_of_a_pending_campaign_is_409() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Pending).await;
        seed_roster(&state, c.campaign_id, WALLET, 42).await;
        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "campaign is not active");
    }

    /// Another member's roster in the same campaign never appears. The caller's Commanders
    /// come oldest first, a dead and Shattered one included with its flags.
    #[tokio::test]
    async fn roster_returns_only_the_callers_records() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Active).await;
        state.membership_repo.add_member(&OTHER, c.campaign_id, None, None).await.expect("add OTHER");
        let mine = seed_roster(&state, c.campaign_id, WALLET, 42).await;
        let theirs = seed_roster(&state, c.campaign_id, OTHER, 52).await;
        let mut fallen = crate::commander_gen::generate_commander(WALLET.to_vec(), c.campaign_id, 62);
        fallen.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        fallen.is_kia = true;
        fallen.is_shattered = true;
        let fallen_id = fallen.commander_id;
        state.commander_repo.create_commander(fallen).await.expect("create the fallen Commander");

        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["campaign_id"], c.campaign_id.to_string());
        let ids = |list: &str, key: &str| -> Vec<String> {
            body[list].as_array().expect("a list").iter().map(|v| v[key].as_str().expect("an id").to_string()).collect()
        };
        assert_eq!(ids("commanders", "commander_id"), vec![fallen_id.to_string(), mine[0].to_string()]);
        assert_eq!(ids("foremen", "foreman_id"), vec![mine[1].to_string()]);
        assert_eq!(ids("sections", "section_id"), vec![mine[2].to_string()]);
        assert_eq!(ids("crews", "crew_id"), vec![mine[3].to_string()]);
        assert_eq!(body["commanders"][0]["is_kia"], true);
        assert_eq!(body["commanders"][0]["is_shattered"], true);
        let text = body.to_string();
        for id in theirs {
            assert!(!text.contains(&id.to_string()), "OTHER's {id} leaked");
        }
    }

    /// The allowlist: exact key sets for the body and each view, and none of the dropped
    /// values anywhere (the hidden bias, the generator seeds, the equipment JSON, the
    /// wallet). Seeds `WALLET` only.
    #[tokio::test]
    async fn roster_views_have_exactly_the_allowlisted_keys() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Active).await;
        let [commander_id, foreman_id, section_id, _] = seed_roster(&state, c.campaign_id, WALLET, 8_686_868).await;
        let mut commander = state.commander_repo.get_commander(commander_id).await.unwrap().expect("seeded Commander");
        commander.faction_bias = "BIAS-MARKER-C".into();
        state.commander_repo.create_commander(commander).await.expect("overwrite the Commander");
        let mut foreman = state.foreman_repo.get_foreman(foreman_id).await.unwrap().expect("seeded Foreman");
        foreman.faction_bias = "BIAS-MARKER-F".into();
        state.foreman_repo.create_foreman(foreman).await.expect("overwrite the Foreman");
        let mut section = state.section_repo.get_section(section_id).await.unwrap().expect("seeded Section");
        section.loadout = serde_json::json!({"LOADOUT-MARKER": 1});
        state.section_repo.create_section(section).await.expect("overwrite the Section");

        // Control: the store holds every value the body must not show.
        let c_row = state.commander_repo.get_commander(commander_id).await.unwrap().expect("Commander");
        assert_eq!((c_row.faction_bias.as_str(), c_row.prng_seed_state), ("BIAS-MARKER-C", 8_686_868));
        let f_row = state.foreman_repo.get_foreman(foreman_id).await.unwrap().expect("Foreman");
        assert_eq!((f_row.faction_bias.as_str(), f_row.prng_seed_state), ("BIAS-MARKER-F", 8_686_869));
        let s_row = state.section_repo.get_section(section_id).await.unwrap().expect("Section");
        assert_eq!(s_row.loadout, serde_json::json!({"LOADOUT-MARKER": 1}));

        let (status, body) = get(&state, Some("t"), c.campaign_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let keys = |v: &serde_json::Value| -> std::collections::BTreeSet<String> {
            v.as_object().expect("an object").keys().cloned().collect()
        };
        let set = |ks: &[&str]| -> std::collections::BTreeSet<String> { ks.iter().map(|k| k.to_string()).collect() };
        assert_eq!(keys(&body), set(&["campaign_id", "commanders", "foremen", "sections", "crews"]));
        assert_eq!(keys(&body["commanders"][0]), set(&[
            "commander_id", "name", "rank", "xp", "stress", "origin", "specialization", "fatal_flaw",
            "veteran_trait", "is_shattered", "is_kia", "commander_grade",
        ]));
        assert_eq!(keys(&body["foremen"][0]), set(&[
            "foreman_id", "name", "rank", "xp", "stress", "origin", "specialization", "fatal_flaw",
            "foreman_grade", "is_shattered", "is_kia",
        ]));
        assert_eq!(keys(&body["sections"][0]), set(&["section_id", "name", "headcount", "xp"]));
        assert_eq!(keys(&body["crews"][0]), set(&["crew_id", "headcount", "xp", "morale"]));
        let text = body.to_string();
        for marker in ["BIAS-MARKER-C", "BIAS-MARKER-F", "LOADOUT-MARKER", "8686868", "8686869"] {
            assert!(!text.contains(marker), "{marker} leaked");
        }
        assert!(!text.contains(&bs58::encode(WALLET).into_string()), "the wallet (base58) leaked");
        assert!(!text.contains(&serde_json::to_string(&WALLET.to_vec()).unwrap()), "the wallet (byte array) leaked");
    }

    #[tokio::test]
    async fn roster_per_ip_limit() {
        let state = with_sessions(121);
        let mut statuses = Vec::new();
        for i in 0..121 {
            statuses.push(get(&state, Some(&format!("w{i}")), Uuid::new_v4()).await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 120];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    /// Reviewer BP-2b-1 F1: every call reaches a campaign the caller may read, so the 11th
    /// answering `429`, not `200`, shows the limit runs before the reads.
    #[tokio::test]
    async fn roster_per_wallet_limit() {
        let state = signed_in();
        let c = campaign_with(&state, WALLET, CampaignLifecycle::Active).await;
        seed_roster(&state, c.campaign_id, WALLET, 42).await;
        let mut statuses = Vec::new();
        for _ in 0..11 {
            statuses.push(get(&state, Some("t"), c.campaign_id).await.0);
        }
        let mut expected = vec![StatusCode::OK; 10];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }
}
