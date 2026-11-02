//! The meltdown's Crew loss (BP-8b-1a, **demo-only**; Breakpoint plan §4d). A meltdown on a dig
//! costs the Crew 1–3 heads and, since BP-11a-1, 10 morale in the same statement, the demo's Crew headcount writers, the meltdown's loss and replace's add (BP-12a-2), written here and nowhere else: `CrewRepository` is keeper and stays
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

/// Takes a meltdown's heads and morale from a Crew, and adds heads back (BP-12a-2).
#[async_trait]
pub trait CrewLoss: Send + Sync {
    /// Takes up to `heads` and up to `morale` from the Crew, each never below 0, and returns
    /// `(headcount left, morale left)`. `NotFound` when no Crew has the id.
    async fn meltdown_loss(&self, crew_id: Uuid, heads: i16, morale: i16) -> Result<(i16, i16), RepositoryError>;

    /// Adds `heads` only when the Crew is `campaign_id`'s and `player_wallet`'s and stays at
    /// most 8 (BP-12a-2, replace: the id comes from the client). `Ok(None)` when no row matches:
    /// no room, another owner's Crew, or an unknown id. Never clamps. Under READ COMMITTED a
    /// concurrent add waits on the row and re-checks `headcount + $2 <= 8` against the committed
    /// row, so two adds can't pass 8 together. Morale is untouched (Wiki Q3).
    async fn add_heads(&self, crew_id: Uuid, campaign_id: Uuid, player_wallet: &[u8], heads: i16) -> Result<Option<i16>, RepositoryError>;
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

    async fn add_heads(&self, crew_id: Uuid, campaign_id: Uuid, player_wallet: &[u8], heads: i16) -> Result<Option<i16>, RepositoryError> {
        sqlx::query_scalar!(
            "UPDATE crew_records SET headcount = headcount + $2 WHERE crew_id = $1 AND campaign_id = $3 AND player_wallet = $4 AND headcount + $2 <= 8 RETURNING headcount",
            crew_id,
            heads,
            campaign_id,
            player_wallet,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}

/// The in-memory mirror, over the same Crew store as `AppState::crew_repo`. A read then an
/// overwrite, not one atomic step: tests drive one write at a time.
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

    async fn add_heads(&self, crew_id: Uuid, campaign_id: Uuid, player_wallet: &[u8], heads: i16) -> Result<Option<i16>, RepositoryError> {
        let Some(mut c) = self.crews.get_crew(crew_id).await? else {
            return Ok(None);
        };
        if c.campaign_id != campaign_id || c.player_wallet != player_wallet || c.headcount + heads > 8 {
            return Ok(None);
        }
        c.headcount += heads;
        let left = c.headcount;
        self.crews.create_crew(c).await?;
        Ok(Some(left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{CrewRecord, InMemoryCrewRepository};

    /// BP-12a-2: the mirror refuses on exactly the Postgres statement's terms, each deciding
    /// alone: room (7 + 2 > 8), the campaign, the wallet, the id; morale never moves. In-memory
    /// bytes 254 (the owner) and 255 (another wallet) are reused inside this private store.
    #[tokio::test]
    async fn in_memory_add_heads_refuses_on_the_same_terms() {
        let crews: Arc<dyn CrewRepository + Send + Sync> = Arc::new(InMemoryCrewRepository::new());
        let loss = InMemoryCrewLoss::new(Arc::clone(&crews));
        let (a, b) = (Uuid::from_u128(0xA), Uuid::from_u128(0xB));
        let (owner, other) = ([254u8; 32], [255u8; 32]);
        let mut crew = CrewRecord::new_hire(a, owner.to_vec());
        crew.headcount = 6;
        let id = crew.crew_id;
        let morale = crew.morale;
        crews.create_crew(crew).await.expect("create_crew");
        let stored = || async {
            let c = crews.get_crew(id).await.expect("get_crew").expect("the Crew");
            (c.headcount, c.morale)
        };
        assert_eq!(loss.add_heads(id, a, &owner, 1).await.expect("add"), Some(7));
        assert_eq!(stored().await, (7, morale));
        assert_eq!(loss.add_heads(id, a, &owner, 2).await.expect("add"), None, "7 + 2 > 8");
        assert_eq!(stored().await, (7, morale));
        assert_eq!(loss.add_heads(id, b, &owner, 1).await.expect("add"), None, "another campaign");
        assert_eq!(stored().await, (7, morale));
        assert_eq!(loss.add_heads(id, a, &other, 1).await.expect("add"), None, "another wallet");
        assert_eq!(stored().await, (7, morale));
        assert_eq!(loss.add_heads(Uuid::new_v4(), a, &owner, 1).await.expect("add"), None, "an unknown id");
        assert_eq!(stored().await, (7, morale));
    }
}
