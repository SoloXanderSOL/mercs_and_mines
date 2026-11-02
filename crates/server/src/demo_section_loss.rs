//! The Section's fight losses (BP-11a-1; Breakpoint plan §4e). A raid or ambush fight costs
//! the Section that fought one soldier per KIA or MIA in the recomputed report, the demo's
//! Section headcount writers, the latch's loss and replace's add (BP-12a-2), written here and
//! nowhere else: `SectionRepository` is keeper and stays untouched. Demo-only: the `dev`
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

/// Takes soldiers from a Section, and adds them back (BP-12a-2).
#[async_trait]
pub trait SectionLoss: Send + Sync {
    /// Takes up to `soldiers` from the Section, never below 0, and returns the headcount left.
    /// `NotFound` when no Section has the id.
    async fn lose_soldiers(&self, section_id: Uuid, soldiers: i16) -> Result<i16, RepositoryError>;

    /// Adds `soldiers` only when the Section is `campaign_id`'s and `player_wallet`'s and stays at
    /// most 8 (BP-12a-2, replace: the id comes from the client). `Ok(None)` when no row matches:
    /// no room, another owner's Section, or an unknown id. Never clamps. Under READ COMMITTED a
    /// concurrent add waits on the row and re-checks `headcount + $2 <= 8` against the committed
    /// row, so two adds can't pass 8 together.
    async fn add_soldiers(&self, section_id: Uuid, campaign_id: Uuid, player_wallet: &[u8], soldiers: i16) -> Result<Option<i16>, RepositoryError>;
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

    async fn add_soldiers(&self, section_id: Uuid, campaign_id: Uuid, player_wallet: &[u8], soldiers: i16) -> Result<Option<i16>, RepositoryError> {
        sqlx::query_scalar!(
            "UPDATE section_records SET headcount = headcount + $2 WHERE section_id = $1 AND campaign_id = $3 AND player_wallet = $4 AND headcount + $2 <= 8 RETURNING headcount",
            section_id,
            soldiers,
            campaign_id,
            player_wallet,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}

/// The in-memory mirror, over the same Section store as `AppState::section_repo`. A read then
/// a write, not one atomic step: tests drive one write at a time.
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

    async fn add_soldiers(&self, section_id: Uuid, campaign_id: Uuid, player_wallet: &[u8], soldiers: i16) -> Result<Option<i16>, RepositoryError> {
        let Some(mut s) = self.sections.get_section(section_id).await? else {
            return Ok(None);
        };
        if s.campaign_id != campaign_id || s.player_wallet != player_wallet || s.headcount + soldiers > 8 {
            return Ok(None);
        }
        s.headcount += soldiers;
        let left = s.headcount;
        self.sections.create_section(s).await?;
        Ok(Some(left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{InMemorySectionRepository, SectionRecord};

    /// BP-12a-2: the mirror refuses on exactly the Postgres statement's terms, each deciding
    /// alone: room (7 + 2 > 8), the campaign, the wallet, the id. In-memory bytes 254 (the owner)
    /// and 255 (another wallet) are reused inside this private store.
    #[tokio::test]
    async fn in_memory_add_soldiers_refuses_on_the_same_terms() {
        let sections: Arc<dyn SectionRepository + Send + Sync> = Arc::new(InMemorySectionRepository::new());
        let loss = InMemorySectionLoss::new(Arc::clone(&sections));
        let (a, b) = (Uuid::from_u128(0xA), Uuid::from_u128(0xB));
        let (owner, other) = ([254u8; 32], [255u8; 32]);
        let s = Uuid::from_u128(0x5EC);
        let now = chrono::Utc::now();
        sections
            .create_section(SectionRecord {
                section_id: s,
                campaign_id: a,
                player_wallet: owner.to_vec(),
                name: "1st Section".into(),
                headcount: 6,
                loadout: serde_json::json!({}),
                xp: 0,
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("create_section");
        let stored = || async { sections.get_section(s).await.expect("get_section").expect("the Section").headcount };
        assert_eq!(loss.add_soldiers(s, a, &owner, 1).await.expect("add"), Some(7));
        assert_eq!(stored().await, 7);
        assert_eq!(loss.add_soldiers(s, a, &owner, 2).await.expect("add"), None, "7 + 2 > 8");
        assert_eq!(stored().await, 7);
        assert_eq!(loss.add_soldiers(s, b, &owner, 1).await.expect("add"), None, "another campaign");
        assert_eq!(stored().await, 7);
        assert_eq!(loss.add_soldiers(s, a, &other, 1).await.expect("add"), None, "another wallet");
        assert_eq!(stored().await, 7);
        assert_eq!(loss.add_soldiers(Uuid::new_v4(), a, &owner, 1).await.expect("add"), None, "an unknown id");
        assert_eq!(stored().await, 7);
    }
}
