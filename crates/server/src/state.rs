#![allow(unused)]
use std::sync::Arc;
use dashmap::DashMap;
use tokio::sync::mpsc;
use uuid::Uuid;
use crate::auth::{ChallengeCap, SessionIndex, CHALLENGE_CAP, SESSION_CAP, SESSION_QUOTA};
use crate::config::Config;
use crate::convoy_expiry::ConvoyEvent;
use crate::rate_limit::RateLimits;
use crate::repository::{
    CampaignRepository, InMemoryCampaignRepository,
    CampaignJoinRepository, InMemoryCampaignJoinRepository,
    ConvoyRepository, InMemoryConvoyRepository,
    CommanderRepository, InMemoryCommanderRepository,
    CrewRepository, InMemoryCrewRepository, ForemanRepository, InMemoryForemanRepository,
    InputLogRepository, InMemoryInputLogRepository,
    MembershipRepository, InMemoryMembershipRepository,
    SectionRepository, InMemorySectionRepository,
    SectorStateRepository, InMemorySectorStateRepository,
    SessionStateRepository, InMemorySessionStateRepository,
    TimerRepository, InMemoryTimerRepository,
};

pub use crate::repository::CombatSession;

/// Capacity of each convoy events socket's channel. A full channel means the
/// socket has stopped reading, and the producer drops that socket's sender.
pub const CONVOY_EVENT_CHANNEL: usize = 64;

pub struct PendingChallenge {
    /// The validated wallet this challenge was issued to; verify requires the request to name the same one.
    pub wallet_address: String,
    /// The full challenge string the client must sign.
    pub challenge_text: String,
    /// Unix timestamp — challenge expires after cfg.server.challenge_expiry_secs.
    pub expires_at: i64,
}

/// Per-campaign launch locks (BP-1c, demo-only). See `AppState::launch_locks`.
#[derive(Default)]
pub struct LaunchLocks(DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>);

impl LaunchLocks {
    /// Waits for the campaign's lock, creating it on first use. The `entry` guard is dropped
    /// before the `.await` (never hold a shard lock across an await).
    pub async fn lock(&self, campaign_id: Uuid) -> LaunchGuard<'_> {
        let mutex = self.0.entry(campaign_id).or_default().clone();
        let guard = mutex.lock_owned().await;
        LaunchGuard { locks: self, campaign_id, guard: Some(guard) }
    }

    /// In-flight launches (tests assert it drains to 0).
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Holds a campaign's launch lock. On drop it releases the lock, then removes the map entry
/// if nobody else holds or awaits it (`strong_count == 1`: only the map's own `Arc`).
pub struct LaunchGuard<'a> {
    locks: &'a LaunchLocks,
    campaign_id: Uuid,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for LaunchGuard<'_> {
    fn drop(&mut self) {
        self.guard.take();
        self.locks.0.remove_if(&self.campaign_id, |_, m| Arc::strong_count(m) == 1);
    }
}

pub struct AppState {
    pub session_repo:    Arc<dyn SessionStateRepository + Send + Sync>,
    pub input_log_repo:  Arc<dyn InputLogRepository + Send + Sync>,
    /// Active 2-hour sessions, keyed by token_id. The auth sweep removes expired ones.
    pub sessions: DashMap<String, shared::SessionToken>,
    /// The per-address session quota and the global backstop over `sessions` (ODQ-48).
    pub session_index: SessionIndex,
    /// Pending sign-in challenges, keyed by challenge_key of the challenge text. One-time use: verify removes the entry whatever the outcome; the auth sweep removes expired ones.
    pub pending_challenges: DashMap<[u8; 32], PendingChallenge>,
    /// The pending-challenge cap (ODQ-48). Production inserts go through it.
    pub challenge_cap: ChallengeCap,
    pub campaign_repo:   Arc<dyn CampaignRepository  + Send + Sync>,
    pub commander_repo:  Arc<dyn CommanderRepository  + Send + Sync>,
    pub membership_repo: Arc<dyn MembershipRepository + Send + Sync>,
    pub section_repo:    Arc<dyn SectionRepository    + Send + Sync>,
    pub foreman_repo:    Arc<dyn ForemanRepository    + Send + Sync>,
    pub crew_repo:       Arc<dyn CrewRepository       + Send + Sync>,
    /// The join and the launch (5a-3a). In memory it shares the campaign, membership,
    /// Commander, Foreman, Section and Crew stores above, so a test that replaces any of
    /// those six fields replaces this one alongside.
    pub campaign_join_repo: Arc<dyn CampaignJoinRepository>,
    pub sector_repo:    Arc<dyn SectorStateRepository>,
    pub timer_repo:      Arc<dyn TimerRepository>,
    pub convoy_repo:     Arc<dyn ConvoyRepository>,
    /// Sender map for convoy event WebSocket notifications, keyed by campaign_id.
    pub convoy_event_senders: DashMap<Uuid, mpsc::Sender<ConvoyEvent>>,
    pub config: Arc<Config>,
    /// The Genesis Token lookup client (BP-4). `None` when no RPC URL is configured (tests,
    /// CI, production), and `GET /api/genesis` then answers `503`.
    pub genesis: Option<Arc<crate::genesis::GenesisRpc>>,
    /// The checkout rail (BP-5). `None` when no devnet RPC URL is configured (tests, CI,
    /// production), and the checkout routes then answer `503`.
    pub checkout_rail: Option<Arc<dyn crate::payment::PaymentRail>>,
    /// The checkout intents and receipts (BP-5): in memory, bounded.
    pub checkout: crate::checkout_store::CheckoutStore,
    /// Postgres connection pool. None only in unit-test contexts that use in-memory repos.
    /// Production startup panics if DATABASE_URL is unset; pool is always Some in prod.
    pub pool: Option<sqlx::PgPool>,
    /// Redis connection manager. None only in unit-test contexts that use in-memory repos.
    /// Production startup panics if REDIS_URL is unset; always Some in prod.
    pub redis: Option<redis::aio::ConnectionManager>,
    /// The sign-in, join and replacement rate limits (INF-6c-1). Fresh per `AppState`.
    pub rate_limits: RateLimits,
    /// Per-campaign launch locks (BP-1c, demo-only): a demo join holds its campaign's lock
    /// across the launch and the sectorless repair, so a repair never retires a campaign
    /// whose launch is still writing its sector. In-process: a restart empties it, which is
    /// right, because a restart also ends every launch.
    pub launch_locks: LaunchLocks,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        // Each join-shared store is built once; the `dyn` fields and the one in-memory join
        // hold the same instances (Reviewer 5a-2b CF2).
        let campaigns   = Arc::new(InMemoryCampaignRepository::new());
        let memberships = Arc::new(InMemoryMembershipRepository::new());
        let commanders  = Arc::new(InMemoryCommanderRepository::new());
        let foremen     = Arc::new(InMemoryForemanRepository::new());
        let sections    = Arc::new(InMemorySectionRepository::new());
        let crews       = Arc::new(InMemoryCrewRepository::new());
        Self {
            session_repo:        Arc::new(InMemorySessionStateRepository::new()),
            input_log_repo:      Arc::new(InMemoryInputLogRepository::new()),
            sessions:           DashMap::new(),
            session_index:      SessionIndex::new(SESSION_QUOTA, SESSION_CAP),
            pending_challenges: DashMap::new(),
            challenge_cap:      ChallengeCap::new(CHALLENGE_CAP),
            campaign_repo:       campaigns.clone(),
            commander_repo:      commanders.clone(),
            membership_repo:     memberships.clone(),
            section_repo:        sections.clone(),
            foreman_repo:        foremen.clone(),
            crew_repo:           crews.clone(),
            campaign_join_repo:  Arc::new(InMemoryCampaignJoinRepository::new(
                campaigns, memberships, commanders, foremen, sections, crews,
            )),
            sector_repo:         Arc::new(InMemorySectorStateRepository::new()),
            timer_repo:          Arc::new(InMemoryTimerRepository::new()),
            convoy_repo:         Arc::new(InMemoryConvoyRepository),
            convoy_event_senders: DashMap::new(),
            genesis:            crate::genesis::from_config(config.server.solana_mainnet_rpc_url.as_ref()),
            checkout_rail:      crate::demo_checkout::rail(config.server.solana_devnet_rpc_url.as_ref()),
            checkout:           crate::checkout_store::CheckoutStore::default(),
            config,
            pool:               None,
            redis:              None,
            rate_limits:        RateLimits::new(),
            launch_locks:       LaunchLocks::default(),
        }
    }
}
