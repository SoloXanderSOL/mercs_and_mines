//! The meltdown's Crew loss (BP-8b-1a, **demo-only**; Breakpoint plan §4d). A meltdown on a dig
//! costs the Crew 1–3 heads and, since BP-11a-1, 10 morale in the same statement, written here and nowhere else: `CrewRepository` is keeper and stays
//! untouched. The `dev` port deletes this file, its `lib.rs` line, its `AppState` field and its
//! `main.rs` line, each marked "(BP-8b-1a, demo-only)".
//!
//! - **The ops layer's one durable effect.** No replay rebuilds it (the ops layer logs nothing),
//!   which is acceptable only because the layer is demo-only.
//! - **Single-statement arithmetic** (code `CLAUDE.md` §4c): `GREATEST(headcount - $2, 0)` and
//!   `GREATEST(morale - $3, 0)` in one `UPDATE`, so two losses can't read the same values, and the
//!   result never breaks `crew_records_headcount_range` (0–8) or `crew_records_morale_range` (0–100).

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::repository::{CrewRepository, RepositoryError};

/// Takes a meltdown's heads and morale from a Crew.
#[async_trait]
pub trait CrewLoss: Send + Sync {
    /// Takes up to `heads` and up to `morale` from the Crew, each never below 0, and returns
    /// `(headcount left, morale left)`. `NotFound` when no Crew has the id.
    async fn meltdown_loss(&self, crew_id: Uuid, heads: i16, morale: i16) -> Result<(i16, i16), RepositoryError>;
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
    async fn meltdown_loss(&self, crew_id: Uuid, heads: i16, morale: i16) -> Result<(i16, i16), RepositoryError> {
        sqlx::query!(
            "UPDATE crew_records SET headcount = GREATEST(headcount - $2, 0), morale = GREATEST(morale - $3, 0) WHERE crew_id = $1 RETURNING headcount, morale",
            crew_id,
            heads,
            morale,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?
        .map(|r| (r.headcount, r.morale))
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
    async fn meltdown_loss(&self, crew_id: Uuid, heads: i16, morale: i16) -> Result<(i16, i16), RepositoryError> {
        let mut crew = self.crews.get_crew(crew_id).await?.ok_or(RepositoryError::NotFound)?;
        crew.headcount = (crew.headcount - heads).max(0);
        crew.morale = (crew.morale - morale).max(0);
        let left = (crew.headcount, crew.morale);
        self.crews.create_crew(crew).await?;
        Ok(left)
    }
}
