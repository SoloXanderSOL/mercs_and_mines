#![allow(unused)]
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use crate::repository::WalletAddress as Pubkey;
use uuid::Uuid;

use super::RepositoryError;

pub type SectorId = Uuid;

#[derive(Clone)]
pub struct SectorState {
    pub sector_id: SectorId,
    pub campaign_id: Uuid,
    pub occupation_status: OccupationStatus,
    pub owner: Option<Pubkey>,
    pub deployed_unit_count: u32,
    pub active_timer_ids: Vec<Uuid>,
}

#[derive(Clone)]
pub enum OccupationStatus {
    Neutral,
    Contested,
    Controlled,
}

#[async_trait]
pub trait SectorStateRepository: Send + Sync {
    async fn get_sector(&self, sector_id: SectorId) -> Option<SectorState>;
    async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError>;
    async fn list_sectors(&self) -> Vec<SectorState>;
}

pub struct InMemorySectorStateRepository(pub Arc<DashMap<SectorId, SectorState>>);

impl InMemorySectorStateRepository {
    pub fn new() -> Self {
        Self(Arc::new(DashMap::new()))
    }
}

impl Default for InMemorySectorStateRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SectorStateRepository for InMemorySectorStateRepository {
    async fn get_sector(&self, sector_id: SectorId) -> Option<SectorState> {
        self.0.get(&sector_id).map(|entry| entry.value().clone())
    }

    async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError> {
        self.0.insert(state.sector_id, state);
        Ok(())
    }

    async fn list_sectors(&self) -> Vec<SectorState> {
        self.0.iter().map(|entry| entry.value().clone()).collect()
    }
}
