use chrono::{Duration, Utc};
use uuid::Uuid;

use shared::InputLogEntry;
use sim_engine::rng::Rng;

use crate::map_gen::{
    assign_gateway_hexes, generate_sector_map,
    MAP_RADIUS_HEXES, SPAWN_SEED_NONCE,
    PlayerSpawnRequest, SpawnType,
};
use crate::repository::{
    CampaignJoinRepository, InputLogRepository, LaunchStart,
    MembershipRepository, OccupationStatus, RepositoryError,
    SectorState, SectorStateRepository,
    VictoryTickerType,
};

/// The most members a launch accepts. A multi-member launch would write one whole-blob
/// Sector state per member (GAP-58); 5b-2 fixes that and removes this guard.
pub const LAUNCH_MAX_MEMBERS: usize = 1;

#[derive(Debug)]
pub enum InitError {
    NotPending,
    CampaignNotFound,
    NoMembersFound,
    TooManyMembers(usize),
    GatewayAssignmentFailed,
    RepositoryError(RepositoryError),
}

pub async fn initialize_campaign(
    campaign_id: Uuid,
    join_repo:       &dyn CampaignJoinRepository,
    membership_repo: &dyn MembershipRepository,
    sector_repo:     &dyn SectorStateRepository,
    input_log_repo:  &(dyn InputLogRepository + Send + Sync),
) -> Result<(), InitError> {
    // 3.7 (values) — build initial tickers and ends_at; `begin_launch` writes them
    let initial_tickers: serde_json::Value = VictoryTickerType::variants()
        .iter()
        .map(|v| (v.as_json_key().to_string(), serde_json::Value::from(0.0_f64)))
        .collect::<serde_json::Map<_, _>>()
        .into();

    let ends_at = Utc::now() + Duration::days(90);

    // 3.1, 3.4, 3.7 — under the join lock: load and validate, list members, guard, activate.
    // Once Active, no join can enter, so everything below runs after the commit.
    let (campaign, members) = match join_repo
        .begin_launch(campaign_id, LAUNCH_MAX_MEMBERS, ends_at, initial_tickers)
        .await
        .map_err(InitError::RepositoryError)?
    {
        LaunchStart::Started { campaign, members } => (campaign, members),
        LaunchStart::NotFound => return Err(InitError::CampaignNotFound),
        LaunchStart::NotPending => return Err(InitError::NotPending),
        LaunchStart::NoMembers => return Err(InitError::NoMembersFound),
        LaunchStart::TooManyMembers(n) => return Err(InitError::TooManyMembers(n)),
    };

    // 3.2 — derive u32 PRNG root from i64 map_seed
    let seed_u32 = (campaign.map_seed as u32) ^ ((campaign.map_seed >> 32) as u32);

    // 3.3 — generate sector map
    let sector_map = generate_sector_map(seed_u32, MAP_RADIUS_HEXES);

    // 3.5 — build spawn requests (Phase 1: all SoloQueue)
    let requests: Vec<PlayerSpawnRequest> = members.iter()
        .map(|m| PlayerSpawnRequest {
            wallet_address: m.wallet_address.clone(),
            spawn_type: SpawnType::SoloQueue,
        })
        .collect();
    // TODO(phase-2): wire CorporateCharter once charter_id is added to
    // player_campaign_membership

    // 3.6 — assign gateway hexes (independent PRNG stream)
    let assignments = assign_gateway_hexes(
        &sector_map.safe_zone,
        &requests,
        &mut Rng::new(seed_u32.wrapping_add(SPAWN_SEED_NONCE)),
    );

    // 3.8 — write sector state to Redis
    sector_repo.upsert_sector(SectorState {
        sector_id: campaign.sector_id,
        campaign_id,
        terrain: sector_map.terrain,
        magma_veins: sector_map.magma_veins,
        occupation_status: OccupationStatus::Neutral,
        owner: None,
        deployed_unit_count: 0,
        hex_occupancy: std::collections::HashMap::new(),
    }).await.map_err(InitError::RepositoryError)?;

    // 3.9 — assign gateway hexes in DB
    if assignments.len() != members.len() {
        return Err(InitError::GatewayAssignmentFailed);
    }
    for assignment in &assignments {
        membership_repo.assign_gateway_hex(
            &assignment.wallet_address,
            campaign_id,
            assignment.q,
            assignment.r,
        ).await.map_err(InitError::RepositoryError)?;
    }

    // 3.10 — append campaign_started input log entry (the ends_at just written)
    let campaign_started = InputLogEntry {
        tick: 0,
        seq: 0,
        event_type: "campaign_started".to_string(),
        player_id: None,
        payload: serde_json::json!({
            "campaign_id": campaign_id.to_string(),
            "ends_at": ends_at.to_rfc3339(),
        }),
        narrative_event: None,
    };

    input_log_repo.append_campaign_entry(&campaign_id, &campaign_started).await
        .map_err(InitError::RepositoryError)?;

    Ok(())
}
