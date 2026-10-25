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
    CampaignJoinRepository, CampaignRepository, InputLogRepository, LaunchStart,
    MembershipRepository, OccupationStatus, RepositoryError,
    SectorState, SectorStateRepository,
    VictoryTickerType,
};

/// The most members a launch accepts. A multi-member launch would write one whole-blob
/// Sector state per member (GAP-58); 5b-2 fixes that and removes this guard.
pub const LAUNCH_MAX_MEMBERS: usize = 1;

/// What failed after the campaign went `Active`.
#[derive(Debug)]
pub enum PostActivation {
    Repository(RepositoryError),
    GatewayAssignmentFailed,
}

#[derive(Debug)]
pub enum InitError {
    NotPending,
    CampaignNotFound,
    NoMembersFound,
    TooManyMembers(usize),
    /// `begin_launch` failed; the campaign is still `Pending`.
    RepositoryError(RepositoryError),
    /// The campaign is `Active` and the failure came after that. `sector_id` is the
    /// campaign's sector, which may already be written.
    AfterActivation { sector_id: Uuid, cause: PostActivation },
}

#[derive(Debug)]
pub enum RetireError {
    Retire(RepositoryError),
    DeleteSector(RepositoryError),
}

/// Retires an `Active` campaign to `Archived` and deletes its sector. Returns `false`, and
/// deletes nothing, when the campaign was not `Active`. Shared by the join's failed launch
/// (BP-1) and the demo's abandon (BP-1b).
///
/// Retire first, then delete: deleting first and then failing the retire would leave an
/// `Active` campaign with no map.
pub async fn retire_campaign(
    campaign_repo: &dyn CampaignRepository,
    sector_repo:   &dyn SectorStateRepository,
    campaign_id:   Uuid,
    sector_id:     Uuid,
) -> Result<bool, RetireError> {
    let retired = campaign_repo.retire_if_active(campaign_id).await.map_err(RetireError::Retire)?;
    if retired {
        sector_repo.delete_sector(sector_id).await.map_err(RetireError::DeleteSector)?;
    }
    Ok(retired)
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

    // The campaign is Active from here: every failure below is post-activation.
    let sector_id = campaign.sector_id;
    let after = |cause: PostActivation| InitError::AfterActivation { sector_id, cause };

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
    }).await.map_err(|e| after(PostActivation::Repository(e)))?;

    // 3.9 — assign gateway hexes in DB
    if assignments.len() != members.len() {
        return Err(after(PostActivation::GatewayAssignmentFailed));
    }
    for assignment in &assignments {
        membership_repo.assign_gateway_hex(
            &assignment.wallet_address,
            campaign_id,
            assignment.q,
            assignment.r,
        ).await.map_err(|e| after(PostActivation::Repository(e)))?;
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
        .map_err(|e| after(PostActivation::Repository(e)))?;

    Ok(())
}
