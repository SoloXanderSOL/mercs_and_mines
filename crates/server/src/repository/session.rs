use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api_types::CombatResolveRequest;
use super::RepositoryError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatSession {
    pub params: CombatResolveRequest,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait SessionStateRepository: Send + Sync {
    async fn save_session(
        &self,
        session_id: Uuid,
        session: CombatSession,
    ) -> Result<(), RepositoryError>;

    async fn get_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<CombatSession>, RepositoryError>;

    async fn consume_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<CombatSession>, RepositoryError>;
}

// ---------------------------------------------------------------------------
// In-memory implementation
// ---------------------------------------------------------------------------

pub struct InMemorySessionStateRepository {
    sessions: Arc<DashMap<Uuid, CombatSession>>,
}

impl InMemorySessionStateRepository {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
        }
    }
}

impl Default for InMemorySessionStateRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionStateRepository for InMemorySessionStateRepository {
    async fn save_session(
        &self,
        session_id: Uuid,
        session: CombatSession,
    ) -> Result<(), RepositoryError> {
        self.sessions.insert(session_id, session);
        Ok(())
    }

    async fn get_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<CombatSession>, RepositoryError> {
        Ok(self.sessions.get(&session_id).map(|e| e.value().clone()))
    }

    async fn consume_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<CombatSession>, RepositoryError> {
        Ok(self.sessions.remove(&session_id).map(|(_, v)| v))
    }
}

// ---------------------------------------------------------------------------
// Redis implementation
// ---------------------------------------------------------------------------

pub struct RedisSessionStateRepository {
    conn: redis::aio::ConnectionManager,
    /// Redis TTL = stale_secs * 2: app gate is the authoritative expiry;
    /// Redis keeps the key for a reconnect window beyond it before eviction.
    redis_ttl_secs: u64,
}

impl RedisSessionStateRepository {
    pub fn new(conn: redis::aio::ConnectionManager, stale_secs: u64) -> Self {
        Self { conn, redis_ttl_secs: stale_secs * 2 }
    }

    fn state_key(session_id: Uuid) -> String {
        format!("session:{}:state", session_id)
    }
}

#[async_trait]
impl SessionStateRepository for RedisSessionStateRepository {
    async fn save_session(
        &self,
        session_id: Uuid,
        session: CombatSession,
    ) -> Result<(), RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(&session)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        conn.set_ex::<_, _, ()>(Self::state_key(session_id), json, self.redis_ttl_secs)
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    async fn get_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<CombatSession>, RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::state_key(session_id))
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| RepositoryError::Internal(e.to_string())),
        }
    }

    async fn consume_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<CombatSession>, RepositoryError> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get_del(Self::state_key(session_id))
            .await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| RepositoryError::Internal(e.to_string())),
        }
    }
}
