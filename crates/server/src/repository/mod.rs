// Repository types are infrastructure for Batch 8. Concrete methods are called through
// Arc<dyn Trait> in AppState; the stub impls and enum variants will fill in future batches.
// This suppress is scoped to the repository module — remove once the types are fully wired up.
#![allow(unused)]
pub mod account;
pub mod campaign;
pub mod commander;
pub mod input_log;
pub mod membership;
pub mod postgres_account;
pub mod section;
pub mod sector;
pub mod session;
pub mod timer;

/// Phase-0 stand-in for `solana_sdk::pubkey::Pubkey`.
/// Replace with the real type once the rustc-1.95 ICE (span-rendering bug triggered by
/// Solana SDK macros) is resolved.  Swap uses here and the Cargo.toml dep to cut over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WalletAddress(pub [u8; 32]);

impl WalletAddress {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

pub use account::{
    AccountRepository, GcnLedgerEntry, InMemoryAccountRepository, PlayerAccount,
    PlayerProfile,
};
pub use campaign::{
    CampaignInstance, CampaignLifecycle, CampaignRepository,
    InMemoryCampaignRepository, NewCampaignInstance, PostgresCampaignRepository,
    VictoryTickerType,
};
pub use commander::{
    CommanderRecord, CommanderRepository,
    InMemoryCommanderRepository, PostgresCommanderRepository,
};
pub use postgres_account::PostgresAccountRepository;
pub use section::{
    InMemorySectionRepository, PostgresSectionRepository,
    SectionRecord, SectionRepository,
};
pub use sector::{
    HexOccupant, InMemorySectorStateRepository, OccupationStatus, RedisSectorStateRepository,
    SectorId, SectorState, SectorStateRepository,
};
pub use membership::{
    InMemoryMembershipRepository, MembershipRepository, PlayerCampaignMembership,
    PostgresMembershipRepository,
};
pub use input_log::{
    InMemoryInputLogRepository, InputLogRepository, InputLogRow,
    PostgresInputLogRepository,
};
pub use timer::{DeploymentTimer, InMemoryTimerRepository, RedisTimerRepository, TimerRepository, TimerType};
pub use session::{
    CombatSession, InMemorySessionStateRepository, RedisSessionStateRepository,
    SessionStateRepository,
};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("record not found")]
    NotFound,
    #[error("internal repository error: {0}")]
    Internal(String),
}
