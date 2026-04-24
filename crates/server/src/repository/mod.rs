// Repository types are infrastructure for Batch 8. Concrete methods are called through
// Arc<dyn Trait> in AppState; the stub impls and enum variants will fill in future batches.
// This suppress is scoped to the repository module — remove once the types are fully wired up.
#![allow(unused)]
pub mod account;
pub mod sector;
pub mod timer;

/// Phase-0 stand-in for `solana_sdk::pubkey::Pubkey`.
/// Replace with the real type once the rustc-1.95 ICE (span-rendering bug triggered by
/// Solana SDK macros) is resolved.  Swap uses here and the Cargo.toml dep to cut over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WalletAddress(pub [u8; 32]);

impl WalletAddress {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

pub use account::{
    AccountRepository, GcnEventType, GcnLedgerEntry, InMemoryAccountRepository, PlayerAccount,
    PlayerProfile,
};
pub use sector::{
    InMemorySectorStateRepository, OccupationStatus, SectorId, SectorState, SectorStateRepository,
};
pub use timer::{DeploymentTimer, InMemoryTimerRepository, TimerRepository, TimerType};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("record not found")]
    NotFound,
    #[error("internal repository error: {0}")]
    Internal(String),
}
