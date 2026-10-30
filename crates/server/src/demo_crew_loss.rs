//! The meltdown's Crew loss (BP-8b-1a, **demo-only**; Breakpoint plan §4d). A meltdown on a dig
//! costs the Crew 1–3 heads, written here and nowhere else: `CrewRepository` is keeper and stays
//! untouched. The `dev` port deletes this file, its `lib.rs` line, its `AppState` field and its
//! `main.rs` line, each marked "(BP-8b-1a, demo-only)".
//!
//! - **The ops layer's one durable effect.** No replay rebuilds it (the ops layer logs nothing),
//!   which is acceptable only because the layer is demo-only.
//! - **Single-statement arithmetic** (code `CLAUDE.md` §4c): `GREATEST(headcount - $2, 0)` in one
//!   `UPDATE`, so two losses can't read the same headcount, and the result never breaks
//!   `crew_records_headcount_range` (0–8).

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::repository::{CrewRepository, RepositoryError};

/// Takes heads from a Crew.
#[async_trait]
pub trait CrewLoss: Send + Sync {
    /// Takes up to `heads` from the Crew, never below 0, and returns the headcount left.
    /// `NotFound` when no Crew has the id.
    async fn lose_heads(&self, crew_id: Uuid, heads: i16) -> Result<i16, RepositoryError>;
}

pub struct PostgresCrewLoss {
    pool: PgPool,
}

impl PostgresCrewLoss {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CrewLoss for PostgresCrewLoss {
    async fn lose_heads(&self, crew_id: Uuid, heads: i16) -> Result<i16, RepositoryError> {
        sqlx::query_scalar!(
            "UPDATE crew_records SET headcount = GREATEST(headcount - $2, 0) WHERE crew_id = $1 RETURNING headcount",
            crew_id,
            heads,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?
        .ok_or(RepositoryError::NotFound)
    }
}

/// The in-memory mirror, over the same Crew store as `AppState::crew_repo`. A read then an
/// overwrite, not one atomic step: tests drive one meltdown at a time.
pub struct InMemoryCrewLoss {
    crews: Arc<dyn CrewRepository + Send + Sync>,
}

impl InMemoryCrewLoss {
    pub fn new(crews: Arc<dyn CrewRepository + Send + Sync>) -> Self {
        Self { crews }
    }
}

#[async_trait]
impl CrewLoss for InMemoryCrewLoss {
    async fn lose_heads(&self, crew_id: Uuid, heads: i16) -> Result<i16, RepositoryError> {
        let mut crew = self.crews.get_crew(crew_id).await?.ok_or(RepositoryError::NotFound)?;
        crew.headcount = (crew.headcount - heads).max(0);
        let left = crew.headcount;
        self.crews.create_crew(crew).await?;
        Ok(left)
    }
}
