#![allow(unused)]
use std::path::PathBuf;
use std::sync::Arc;
use dashmap::DashMap;
use crate::config::Config;
use crate::repository::{
    AccountRepository, InMemoryAccountRepository,
    CampaignRepository, InMemoryCampaignRepository,
    ConvoyRepository, InMemoryConvoyRepository,
    CommanderRepository, InMemoryCommanderRepository,
    InputLogRepository, InMemoryInputLogRepository,
    MembershipRepository, InMemoryMembershipRepository,
    SectionRepository, InMemorySectionRepository,
    SectorStateRepository, InMemorySectorStateRepository,
    SessionStateRepository, InMemorySessionStateRepository,
    TimerRepository, InMemoryTimerRepository,
};

pub use crate::repository::CombatSession;

pub struct PendingChallenge {
    /// The full challenge string the client must sign.
    pub challenge_text: String,
    /// Unix timestamp — challenge expires after cfg.server.challenge_expiry_secs.
    pub expires_at: i64,
}

pub struct AppState {
    pub session_repo:    Arc<dyn SessionStateRepository + Send + Sync>,
    pub input_log_repo:  Arc<dyn InputLogRepository + Send + Sync>,
    pub log_dir: PathBuf,
    /// Active 2-hour sessions, keyed by token_id.
    pub sessions: DashMap<String, shared::SessionToken>,
    /// Pending TEEPIN challenges, keyed by wallet_address.
    /// One-time use — removed on verify.
    pub pending_challenges: DashMap<String, PendingChallenge>,
    pub account_repo:    Arc<dyn AccountRepository    + Send + Sync>,
    pub campaign_repo:   Arc<dyn CampaignRepository  + Send + Sync>,
    pub commander_repo:  Arc<dyn CommanderRepository  + Send + Sync>,
    pub membership_repo: Arc<dyn MembershipRepository + Send + Sync>,
    pub section_repo:    Arc<dyn SectionRepository    + Send + Sync>,
    pub sector_repo:     Arc<dyn SectorStateRepository>,
    pub timer_repo:      Arc<dyn TimerRepository>,
    pub convoy_repo:     Arc<dyn ConvoyRepository>,
    pub config: Arc<Config>,
    /// Postgres connection pool. None only in unit-test contexts that use in-memory repos.
    /// Production startup panics if DATABASE_URL is unset; pool is always Some in prod.
    pub pool: Option<sqlx::PgPool>,
    /// Redis connection manager. None only in unit-test contexts that use in-memory repos.
    /// Production startup panics if REDIS_URL is unset; always Some in prod.
    pub redis: Option<redis::aio::ConnectionManager>,
}

impl AppState {
    pub fn new(log_dir: PathBuf, config: Arc<Config>) -> Self {
        Self {
            session_repo:        Arc::new(InMemorySessionStateRepository::new()),
            input_log_repo:      Arc::new(InMemoryInputLogRepository::new()),
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
            convoy_repo:         Arc::new(InMemoryConvoyRepository),
            config,
            pool:               None,
            redis:              None,
        }
    }
}
