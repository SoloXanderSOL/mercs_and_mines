//! The Section's fight losses (BP-11a-1; Breakpoint plan §4e). A raid or ambush fight costs
//! the Section that fought one soldier per KIA or MIA in the recomputed report, written here
//! and nowhere else: `SectionRepository` is keeper and stays untouched. Demo-only: the `dev`
//! port deletes this file, its `lib.rs` line, its `AppState` field and its `main.rs` line,
//! each marked with this brick's tag.
//!
//! - **Single-statement arithmetic** (code `CLAUDE.md` §4c): one `UPDATE` floors the
//!   headcount at 0, so two losses can't read the same headcount and the result never breaks
//!   the 0–8 range.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::repository::{RepositoryError, SectionRepository};

/// Takes soldiers from a Section.
#[async_trait]
pub trait SectionLoss: Send + Sync {
    /// Takes up to `soldiers` from the Section, never below 0, and returns the headcount left.
    /// `NotFound` when no Section has the id.
    async fn lose_soldiers(&self, section_id: Uuid, soldiers: i16) -> Result<i16, RepositoryError>;
}

pub struct PostgresSectionLoss {
    pool: PgPool,
}

impl PostgresSectionLoss {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SectionLoss for PostgresSectionLoss {
    async fn lose_soldiers(&self, section_id: Uuid, soldiers: i16) -> Result<i16, RepositoryError> {
        sqlx::query_scalar!(
            "UPDATE section_records SET headcount = GREATEST(headcount - $2, 0) WHERE section_id = $1 RETURNING headcount",
            section_id,
            soldiers,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?
        .ok_or(RepositoryError::NotFound)
    }
}

/// The in-memory mirror, over the same Section store as `AppState::section_repo`. A read then
/// a write, not one atomic step: tests drive one loss at a time.
pub struct InMemorySectionLoss {
    sections: Arc<dyn SectionRepository + Send + Sync>,
}

impl InMemorySectionLoss {
    pub fn new(sections: Arc<dyn SectionRepository + Send + Sync>) -> Self {
        Self { sections }
    }
}

#[async_trait]
impl SectionLoss for InMemorySectionLoss {
    async fn lose_soldiers(&self, section_id: Uuid, soldiers: i16) -> Result<i16, RepositoryError> {
        let section = self.sections.get_section(section_id).await?.ok_or(RepositoryError::NotFound)?;
        let left = (section.headcount - soldiers).max(0);
        self.sections.update_headcount(section_id, left).await?;
        Ok(left)
    }
}
