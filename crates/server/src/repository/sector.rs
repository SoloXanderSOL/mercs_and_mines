#![allow(unused)]
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::repository::WalletAddress;
use super::RepositoryError;

pub type SectorId = Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct SectorState {
    pub sector_id: SectorId,
    pub campaign_id: Uuid,
    pub occupation_status: OccupationStatus,
    pub owner: Option<WalletAddress>,
    pub deployed_unit_count: u32,
    pub active_timer_ids: Vec<Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
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
    async fn get_player_presence(&self, sector_id: SectorId) -> Result<Vec<WalletAddress>, RepositoryError>;
    async fn set_player_presence(&self, sector_id: SectorId, players: &[WalletAddress]) -> Result<(), RepositoryError>;
}

// ---------------------------------------------------------------------------
// In-memory implementation
// ---------------------------------------------------------------------------

pub struct InMemorySectorStateRepository {
    sectors:  Arc<DashMap<SectorId, SectorState>>,
    presence: Arc<DashMap<SectorId, Vec<WalletAddress>>>,
}

impl InMemorySectorStateRepository {
    pub fn new() -> Self {
        Self {
            sectors:  Arc::new(DashMap::new()),
            presence: Arc::new(DashMap::new()),
        }
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
        self.sectors.get(&sector_id).map(|e| e.value().clone())
    }

    async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError> {
        self.sectors.insert(state.sector_id, state);
        Ok(())
    }

    async fn list_sectors(&self) -> Vec<SectorState> {
        self.sectors.iter().map(|e| e.value().clone()).collect()
    }

    async fn get_player_presence(&self, sector_id: SectorId) -> Result<Vec<WalletAddress>, RepositoryError> {
        Ok(self.presence
            .get(&sector_id)
            .map(|e| e.value().clone())
            .unwrap_or_default())
    }

    async fn set_player_presence(&self, sector_id: SectorId, players: &[WalletAddress]) -> Result<(), RepositoryError> {
        self.presence.insert(sector_id, players.to_vec());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Redis implementation
// ---------------------------------------------------------------------------

pub struct RedisSectorStateRepository {
    conn: redis::aio::ConnectionManager,
}

impl RedisSectorStateRepository {
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }

    fn hex_state_key(sector_id: SectorId) -> String {
        format!("sector:{}:hex_state", sector_id)
    }

    fn presence_key(sector_id: SectorId) -> String {
        format!("sector:{}:player_presence", sector_id)
    }
}

#[async_trait]
impl SectorStateRepository for RedisSectorStateRepository {
    async fn get_sector(&self, sector_id: SectorId) -> Option<SectorState> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::hex_state_key(sector_id)).await.ok()?;
        let json = raw?;
        serde_json::from_str(&json).ok()
    }

    async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(&state)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let sector_id = state.sector_id;
        conn.set::<_, _, ()>(Self::hex_state_key(sector_id), json)
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        conn.sadd::<_, _, ()>("sectors:all", sector_id.to_string())
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn list_sectors(&self) -> Vec<SectorState> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let ids: Vec<String> = match conn.smembers("sectors:all").await {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let mut out = Vec::with_capacity(ids.len());
        for id_str in ids {
            let Ok(sector_id) = id_str.parse::<Uuid>() else { continue };
            if let Some(s) = self.get_sector(sector_id).await {
                out.push(s);
            }
        }
        out
    }

    async fn get_player_presence(&self, sector_id: SectorId) -> Result<Vec<WalletAddress>, RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::presence_key(sector_id))
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        match raw {
            None => Ok(vec![]),
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| RepositoryError::Internal(e.to_string())),
        }
    }

    async fn set_player_presence(&self, sector_id: SectorId, players: &[WalletAddress]) -> Result<(), RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(players)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        conn.set::<_, _, ()>(Self::presence_key(sector_id), json)
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}
