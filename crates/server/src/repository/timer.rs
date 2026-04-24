#![allow(unused)]
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use crate::repository::WalletAddress as Pubkey;
use uuid::Uuid;

use super::RepositoryError;

#[derive(Clone)]
pub struct DeploymentTimer {
    pub timer_id: Uuid,
    pub player_wallet: Pubkey,
    pub sector_id: Uuid,
    pub timer_type: TimerType,
    pub fires_at: DateTime<Utc>,
}

/// Duration constants are canon-locked — see FOB_Siege_and_Raid_Mechanics.md
#[derive(Clone)]
pub enum TimerType {
    ConvoyArrival,
    AnchorCampSiege,           // 4-hour flat
    SoloFobSiegeStaging,       // 8 hours
    SoloFobVulnerability,      // 4-hour window
    SyndicateFobSiegeStaging,  // 24-hour staging
    SyndicateFobVulnerability, // 6-hour window
    DeploymentExpiry,
}

#[async_trait]
pub trait TimerRepository: Send + Sync {
    async fn schedule_timer(&self, timer: DeploymentTimer) -> Result<(), RepositoryError>;
    async fn cancel_timer(&self, timer_id: Uuid) -> Result<(), RepositoryError>;
    async fn get_due_timers(&self, now: DateTime<Utc>) -> Vec<DeploymentTimer>;
}

pub struct InMemoryTimerRepository(pub Arc<DashMap<Uuid, DeploymentTimer>>);

impl InMemoryTimerRepository {
    pub fn new() -> Self {
        Self(Arc::new(DashMap::new()))
    }
}

impl Default for InMemoryTimerRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TimerRepository for InMemoryTimerRepository {
    async fn schedule_timer(&self, timer: DeploymentTimer) -> Result<(), RepositoryError> {
        self.0.insert(timer.timer_id, timer);
        Ok(())
    }

    async fn cancel_timer(&self, timer_id: Uuid) -> Result<(), RepositoryError> {
        self.0
            .remove(&timer_id)
            .map(|_| ())
            .ok_or(RepositoryError::NotFound)
    }

    async fn get_due_timers(&self, now: DateTime<Utc>) -> Vec<DeploymentTimer> {
        self.0
            .iter()
            .filter(|entry| entry.fires_at <= now)
            .map(|entry| entry.value().clone())
            .collect()
    }
}
