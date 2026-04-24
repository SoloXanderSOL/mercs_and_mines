use std::path::PathBuf;
use std::time::Instant;
use dashmap::DashMap;
use uuid::Uuid;
use crate::api_types::CombatResolveRequest;

pub struct CombatSession {
    pub params: CombatResolveRequest,
    pub created_at: Instant,
}

pub struct AppState {
    pub combat_sessions: DashMap<Uuid, CombatSession>,
    pub log_dir: PathBuf,
}

impl AppState {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            combat_sessions: DashMap::new(),
            log_dir,
        }
    }
}
