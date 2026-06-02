#![allow(unused)]
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use crate::repository::WalletAddress as Pubkey;
use uuid::Uuid;

use super::RepositoryError;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DeploymentTimer {
    pub timer_id: Uuid,
    pub player_wallet: Pubkey,
    pub sector_id: Uuid,
    pub timer_type: TimerType,
    pub fires_at: DateTime<Utc>,
}

/// Duration constants are canon-locked — see FOB_Siege_and_Raid_Mechanics.md
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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

// ---------------------------------------------------------------------------
// Redis implementation
// ---------------------------------------------------------------------------
//
// Key schema:
//   timer:{timer_id}            String  — full DeploymentTimer JSON
//   timer:{timer_id}:sector_id  String  — sector UUID (reverse-lookup for cancel)
//   sector:{sector_id}:timers   ZSet    — timer_ids scored by fires_at unix timestamp
//   timers:global               ZSet    — all timer_ids scored by fires_at unix timestamp

pub struct RedisTimerRepository {
    conn: redis::aio::ConnectionManager,
}

impl RedisTimerRepository {
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }

    fn timer_key(timer_id: Uuid) -> String {
        format!("timer:{}", timer_id)
    }

    fn timer_sector_key(timer_id: Uuid) -> String {
        format!("timer:{}:sector_id", timer_id)
    }

    fn sector_timers_key(sector_id: Uuid) -> String {
        format!("sector:{}:timers", sector_id)
    }
}

#[async_trait]
impl TimerRepository for RedisTimerRepository {
    async fn schedule_timer(&self, timer: DeploymentTimer) -> Result<(), RepositoryError> {
        let mut conn = self.conn.clone();
        let timer_id    = timer.timer_id;
        let sector_id   = timer.sector_id;
        let score       = timer.fires_at.timestamp() as f64;
        let timer_id_str  = timer_id.to_string();
        let sector_id_str = sector_id.to_string();
        let json = serde_json::to_string(&timer)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        let mut pipe = redis::pipe();
        pipe.set(Self::timer_key(timer_id), &json).ignore()
            .set(Self::timer_sector_key(timer_id), &sector_id_str).ignore()
            .zadd(Self::sector_timers_key(sector_id), &timer_id_str, score).ignore()
            .zadd("timers:global", &timer_id_str, score).ignore();
        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    async fn cancel_timer(&self, timer_id: Uuid) -> Result<(), RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let timer_id_str = timer_id.to_string();

        let sector_str: Option<String> = conn
            .get(Self::timer_sector_key(timer_id))
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let sector_str = sector_str.ok_or(RepositoryError::NotFound)?;
        let sector_id  = sector_str.parse::<Uuid>()
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        let mut pipe = redis::pipe();
        pipe.zrem(Self::sector_timers_key(sector_id), &timer_id_str).ignore()
            .zrem("timers:global", &timer_id_str).ignore()
            .del(Self::timer_key(timer_id)).ignore()
            .del(Self::timer_sector_key(timer_id)).ignore();
        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    async fn get_due_timers(&self, now: DateTime<Utc>) -> Vec<DeploymentTimer> {
        use redis::AsyncCommands;
        let mut conn  = self.conn.clone();
        let now_score = now.timestamp() as f64;

        let ids: Vec<String> = match conn
            .zrangebyscore("timers:global", "-inf", now_score)
            .await
        {
            Ok(v)  => v,
            Err(_) => return vec![],
        };

        let mut out = Vec::with_capacity(ids.len());
        for id_str in ids {
            let Ok(tid) = id_str.parse::<Uuid>() else { continue };
            let raw: Option<String> = match conn.get(Self::timer_key(tid)).await {
                Ok(v)  => v,
                Err(_) => continue,
            };
            let Some(json) = raw else { continue };
            let Ok(timer)  = serde_json::from_str::<DeploymentTimer>(&json) else { continue };
            out.push(timer);
        }
        out
    }
}
