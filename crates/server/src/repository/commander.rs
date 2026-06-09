#![allow(unused)]
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

use super::RepositoryError;

// ── CommanderRecord ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CommanderRecord {
    pub commander_id:    Uuid,
    pub campaign_id:     Uuid,
    pub player_wallet:   Vec<u8>,
    pub name:            String,
    pub rank:            i16,
    pub xp:              i32,
    pub stress:          i16,
    pub origin:          String,
    pub faction_bias:    String,
    pub specialization:  String,
    pub fatal_flaw:      String,
    pub veteran_trait:   Option<String>,
    pub is_shattered:    bool,
    pub is_kia:          bool,
    pub is_nft:          bool,
    pub prng_seed_state: i64,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

// ── CommanderRepository trait ────────────────────────────────────────────────

#[async_trait]
pub trait CommanderRepository: Send + Sync {
    async fn create_commander(&self, record: CommanderRecord) -> Result<(), RepositoryError>;
    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError>;
    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError>;
    async fn update_rank_and_xp(&self, commander_id: Uuid, rank: i16, xp: i32) -> Result<(), RepositoryError>;
    async fn set_veteran_trait(&self, commander_id: Uuid, veteran_trait: String) -> Result<(), RepositoryError>;
    async fn set_shattered(&self, commander_id: Uuid) -> Result<(), RepositoryError>;
    async fn set_kia(&self, commander_id: Uuid) -> Result<(), RepositoryError>;
    async fn list_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CommanderRecord>, RepositoryError>;
    async fn delete_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError>;
}

// ── PostgresCommanderRepository ──────────────────────────────────────────────

pub struct PostgresCommanderRepository {
    pool: PgPool,
}

impl PostgresCommanderRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CommanderRepository for PostgresCommanderRepository {
    async fn create_commander(&self, r: CommanderRecord) -> Result<(), RepositoryError> {
        sqlx::query!(
            r#"INSERT INTO commander_records (
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, is_nft, prng_seed_state
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)"#,
            r.commander_id,
            r.campaign_id,
            r.player_wallet,
            r.name,
            r.rank,
            r.xp,
            r.stress,
            r.origin,
            r.faction_bias,
            r.specialization,
            r.fatal_flaw,
            r.veteran_trait,
            r.is_shattered,
            r.is_kia,
            r.is_nft,
            r.prng_seed_state,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, is_nft, prng_seed_state, created_at, updated_at
               FROM commander_records WHERE commander_id = $1"#,
            commander_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| CommanderRecord {
            commander_id:    r.commander_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            name:            r.name,
            rank:            r.rank,
            xp:              r.xp,
            stress:          r.stress,
            origin:          r.origin,
            faction_bias:    r.faction_bias,
            specialization:  r.specialization,
            fatal_flaw:      r.fatal_flaw,
            veteran_trait:   r.veteran_trait,
            is_shattered:    r.is_shattered,
            is_kia:          r.is_kia,
            is_nft:          r.is_nft,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }))
    }

    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError> {
        let clamped = i16::max(0, i16::min(100, stress));
        sqlx::query!(
            "UPDATE commander_records SET stress = $1, updated_at = now() WHERE commander_id = $2",
            clamped,
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn update_rank_and_xp(&self, commander_id: Uuid, rank: i16, xp: i32) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE commander_records SET rank = $1, xp = $2, updated_at = now() WHERE commander_id = $3",
            rank,
            xp,
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn set_veteran_trait(&self, commander_id: Uuid, veteran_trait: String) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE commander_records SET veteran_trait = $1, updated_at = now() WHERE commander_id = $2",
            veteran_trait,
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn set_shattered(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE commander_records SET is_shattered = TRUE, updated_at = now() WHERE commander_id = $1",
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn set_kia(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE commander_records SET is_kia = TRUE, updated_at = now() WHERE commander_id = $1",
            commander_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn list_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CommanderRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT
                commander_id, campaign_id, player_wallet, name, rank, xp, stress,
                origin, faction_bias, specialization, fatal_flaw, veteran_trait,
                is_shattered, is_kia, is_nft, prng_seed_state, created_at, updated_at
               FROM commander_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| CommanderRecord {
            commander_id:    r.commander_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            name:            r.name,
            rank:            r.rank,
            xp:              r.xp,
            stress:          r.stress,
            origin:          r.origin,
            faction_bias:    r.faction_bias,
            specialization:  r.specialization,
            fatal_flaw:      r.fatal_flaw,
            veteran_trait:   r.veteran_trait,
            is_shattered:    r.is_shattered,
            is_kia:          r.is_kia,
            is_nft:          r.is_nft,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
        }).collect())
    }

    async fn delete_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError> {
        let result = sqlx::query!(
            "DELETE FROM commander_records WHERE campaign_id = $1",
            campaign_id,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected())
    }
}

// ── InMemoryCommanderRepository (unit-test stub) ─────────────────────────────

pub struct InMemoryCommanderRepository {
    store: DashMap<Uuid, CommanderRecord>,
}

impl InMemoryCommanderRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryCommanderRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CommanderRepository for InMemoryCommanderRepository {
    async fn create_commander(&self, record: CommanderRecord) -> Result<(), RepositoryError> {
        self.store.insert(record.commander_id, record);
        Ok(())
    }

    async fn get_commander(&self, commander_id: Uuid) -> Result<Option<CommanderRecord>, RepositoryError> {
        Ok(self.store.get(&commander_id).map(|r| r.clone()))
    }

    async fn update_stress(&self, commander_id: Uuid, stress: i16) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&commander_id) {
            let clamped = i16::max(0, i16::min(100, stress));
            r.stress = clamped;
        }
        Ok(())
    }

    async fn update_rank_and_xp(&self, commander_id: Uuid, rank: i16, xp: i32) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&commander_id) {
            r.rank = rank;
            r.xp = xp;
        }
        Ok(())
    }

    async fn set_veteran_trait(&self, commander_id: Uuid, veteran_trait: String) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&commander_id) {
            r.veteran_trait = Some(veteran_trait);
        }
        Ok(())
    }

    async fn set_shattered(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&commander_id) {
            r.is_shattered = true;
        }
        Ok(())
    }

    async fn set_kia(&self, commander_id: Uuid) -> Result<(), RepositoryError> {
        if let Some(mut r) = self.store.get_mut(&commander_id) {
            r.is_kia = true;
        }
        Ok(())
    }

    async fn list_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CommanderRecord>, RepositoryError> {
        Ok(self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.clone())
            .collect())
    }

    async fn delete_commanders_by_campaign(&self, campaign_id: Uuid) -> Result<u64, RepositoryError> {
        let keys: Vec<Uuid> = self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.commander_id)
            .collect();
        let count = keys.len() as u64;
        for k in keys {
            self.store.remove(&k);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_commander(commander_id: Uuid) -> CommanderRecord {
        CommanderRecord {
            commander_id,
            campaign_id:     Uuid::new_v4(),
            player_wallet:   vec![0u8; 32],
            name:            "Test".into(),
            rank:            1,
            xp:              0,
            stress:          50,
            origin:          "unknown".into(),
            faction_bias:    "none".into(),
            specialization:  "none".into(),
            fatal_flaw:      "none".into(),
            veteran_trait:   None,
            is_shattered:    false,
            is_kia:          false,
            is_nft:          false,
            prng_seed_state: 0,
            created_at:      chrono::Utc::now(),
            updated_at:      chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn update_stress_clamps_to_valid_range() {
        let repo = InMemoryCommanderRepository::new();
        let id = Uuid::new_v4();
        repo.create_commander(stub_commander(id)).await.unwrap();

        // over-max clamps to 100
        repo.update_stress(id, 150).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 100, "stress 150 must clamp to 100");

        // under-min clamps to 0
        repo.update_stress(id, -5).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 0, "stress -5 must clamp to 0");

        // in-range value stored as-is
        repo.update_stress(id, 73).await.unwrap();
        let r = repo.get_commander(id).await.unwrap().unwrap();
        assert_eq!(r.stress, 73, "stress 73 must be stored unchanged");
    }
}
