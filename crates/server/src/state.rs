#![allow(unused)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use dashmap::DashMap;
use uuid::Uuid;
use crate::api_types::CombatResolveRequest;
use crate::config::Config;
use crate::repository::{
    AccountRepository, InMemoryAccountRepository,
    CampaignRepository, InMemoryCampaignRepository,
    CommanderRepository, InMemoryCommanderRepository,
    MembershipRepository, InMemoryMembershipRepository,
    SectionRepository, InMemorySectionRepository,
    SectorStateRepository, InMemorySectorStateRepository,
    TimerRepository, InMemoryTimerRepository,
};

pub struct CombatSession {
    pub params: CombatResolveRequest,
    pub created_at: Instant,
}

pub struct PendingChallenge {
    /// The full challenge string the client must sign.
    pub challenge_text: String,
    /// Unix timestamp — challenge expires after cfg.server.challenge_expiry_secs.
    pub expires_at: i64,
}

pub struct AppState {
    pub combat_sessions: DashMap<Uuid, CombatSession>,
    pub log_dir: PathBuf,
    /// Active 2-hour sessions, keyed by token_id.
    pub sessions: DashMap<String, shared::SessionToken>,
    /// Pending TEEPIN challenges, keyed by wallet_address.
    /// One-time use — removed on verify.
    pub pending_challenges: DashMap<String, PendingChallenge>,
    pub account_repo:    Arc<dyn AccountRepository>,
    pub campaign_repo:   Arc<dyn CampaignRepository>,
    pub commander_repo:  Arc<dyn CommanderRepository>,
    pub membership_repo: Arc<dyn MembershipRepository>,
    pub section_repo:    Arc<dyn SectionRepository>,
    pub sector_repo:     Arc<dyn SectorStateRepository>,
    pub timer_repo:      Arc<dyn TimerRepository>,
    pub config: Arc<Config>,
    /// Postgres connection pool. None only in unit-test contexts that use in-memory repos.
    /// Production startup panics if DATABASE_URL is unset; pool is always Some in prod.
    pub pool: Option<sqlx::PgPool>,
}

impl AppState {
    pub fn new(log_dir: PathBuf, config: Arc<Config>) -> Self {
        Self {
            combat_sessions:    DashMap::new(),
            log_dir,
            sessions:           DashMap::new(),
            pending_challenges: DashMap::new(),
            account_repo:        Arc::new(InMemoryAccountRepository::new()),
            campaign_repo:       Arc::new(InMemoryCampaignRepository::new()),
            commander_repo:      Arc::new(InMemoryCommanderRepository::new()),
            membership_repo:     Arc::new(InMemoryMembershipRepository::new()),
            section_repo:        Arc::new(InMemorySectionRepository::new()),
            sector_repo:         Arc::new(InMemorySectorStateRepository::new()),
            timer_repo:          Arc::new(InMemoryTimerRepository::new()),
            config,
            pool:               None,
        }
    }
}
