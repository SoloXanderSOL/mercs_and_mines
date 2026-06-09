#![allow(unused)]
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::map_gen::{HexTerrain, MagmaVeinNode};
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
    #[serde(default)]
    pub terrain: HashMap<String, HexTerrain>,
    #[serde(default)]
    pub magma_veins: Vec<MagmaVeinNode>,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum OccupationStatus {
    Neutral,
    Contested,
    Controlled,
}

#[async_trait]
pub trait SectorStateRepository: Send + Sync {
    async fn get_sector(&self, sector_id: SectorId) -> Result<Option<SectorState>, RepositoryError>;
    async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError>;
    async fn list_sectors(&self) -> Result<Vec<SectorState>, RepositoryError>;
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
    async fn get_sector(&self, sector_id: SectorId) -> Result<Option<SectorState>, RepositoryError> {
        Ok(self.sectors.get(&sector_id).map(|e| e.value().clone()))
    }

    async fn upsert_sector(&self, state: SectorState) -> Result<(), RepositoryError> {
        self.sectors.insert(state.sector_id, state);
        Ok(())
    }

    async fn list_sectors(&self) -> Result<Vec<SectorState>, RepositoryError> {
        Ok(self.sectors.iter().map(|e| e.value().clone()).collect())
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
    async fn get_sector(&self, sector_id: SectorId) -> Result<Option<SectorState>, RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::hex_state_key(sector_id))
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let Some(json) = raw else { return Ok(None) };
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| RepositoryError::Internal(e.to_string()))
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

    async fn list_sectors(&self) -> Result<Vec<SectorState>, RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let ids: Vec<String> = conn.smembers("sectors:all")
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let mut out = Vec::with_capacity(ids.len());
        for id_str in ids {
            let sector_id = id_str.parse::<Uuid>()
                .map_err(|e| RepositoryError::Internal(format!("invalid sector id {id_str}: {e}")))?;
            if let Some(s) = self.get_sector(sector_id).await? {
                out.push(s);
            }
        }
        Ok(out)
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
