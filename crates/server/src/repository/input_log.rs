use async_trait::async_trait;
use dashmap::DashMap;
use uuid::Uuid;

use super::RepositoryError;

#[derive(Debug, sqlx::FromRow)]
pub struct InputLogRow {
    pub log_id:          Uuid,
    pub session_id:      Option<Uuid>,
    pub tick:            i64,
    pub seq:             i64,
    pub event_type:      String,
    pub player_id:       Option<String>,
    pub payload:         Option<serde_json::Value>,
    pub narrative_event: Option<String>,
    pub recorded_at:     chrono::DateTime<chrono::Utc>,
}

impl From<InputLogRow> for shared::InputLogEntry {
    fn from(row: InputLogRow) -> Self {
        shared::InputLogEntry {
            tick:            row.tick as u64,
            seq:             row.seq as u32,
            event_type:      row.event_type,
            player_id:       row.player_id,
            payload:         row.payload.unwrap_or_default(),
            narrative_event: row.narrative_event,
        }
    }
}

#[async_trait]
pub trait InputLogRepository: Send + Sync {
    async fn append_entry(
        &self,
        session_id: &Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError>;

    async fn get_entries_by_session(
        &self,
        session_id: &Uuid,
    ) -> Result<Vec<shared::InputLogEntry>, RepositoryError>;

    async fn save_session_config(
        &self,
        config: &shared::SessionConfig,
    ) -> Result<(), RepositoryError>;

    async fn get_session_config(
        &self,
        session_id: &Uuid,
    ) -> Result<Option<shared::SessionConfig>, RepositoryError>;

    async fn append_campaign_entry(
        &self,
        campaign_id: &Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError>;
    // Phase 2: add get_entries_by_campaign(&self, campaign_id: &Uuid)
    //   -> Result<Vec<InputLogEntry>, RepositoryError>
    // for replay and integrity audit of campaign-scoped log entries
    // (victory_ticker_delta, campaign_started, etc.).
    // These rows are write-only until that method exists.
}

// ── Postgres implementation ───────────────────────────────────────────────────

pub struct PostgresInputLogRepository {
    pool: sqlx::PgPool,
}

impl PostgresInputLogRepository {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl InputLogRepository for PostgresInputLogRepository {
    async fn append_entry(
        &self,
        session_id: &Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError> {
        sqlx::query!(
            r#"
            INSERT INTO input_logs (session_id, tick, seq, event_type, player_id, payload, narrative_event)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            session_id,
            entry.tick as i64,
            entry.seq as i64,
            entry.event_type,
            entry.player_id.as_deref(),
            entry.payload.clone() as serde_json::Value,
            entry.narrative_event.as_deref(),
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_entries_by_session(
        &self,
        session_id: &Uuid,
    ) -> Result<Vec<shared::InputLogEntry>, RepositoryError> {
        let rows = sqlx::query_as!(
            InputLogRow,
            r#"
            SELECT log_id, session_id, tick, seq, event_type, player_id,
                   payload as "payload: serde_json::Value",
                   narrative_event, recorded_at
            FROM input_logs
            WHERE session_id = $1
            ORDER BY tick ASC, seq ASC
            "#,
            session_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn save_session_config(
        &self,
        config: &shared::SessionConfig,
    ) -> Result<(), RepositoryError> {
        let session_uuid = Uuid::parse_str(&config.session_id)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        sqlx::query!(
            r#"
            INSERT INTO session_configs
                (session_id, seed, build_version, sector_id, campaign_id, sector_tier, ruleset)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (session_id) DO NOTHING
            "#,
            session_uuid,
            config.seed as i64,
            config.build_version,
            config.sector_id,
            config.campaign_id,
            config.sector_tier,
            config.ruleset,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_session_config(
        &self,
        session_id: &Uuid,
    ) -> Result<Option<shared::SessionConfig>, RepositoryError> {
        let row = sqlx::query!(
            r#"
            SELECT session_id, seed, build_version, sector_id, campaign_id, sector_tier, ruleset
            FROM session_configs
            WHERE session_id = $1
            "#,
            session_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| shared::SessionConfig {
            session_id:    r.session_id.to_string(),
            seed:          r.seed as u64,
            build_version: r.build_version,
            sector_id:     r.sector_id,   // UUID column → Uuid field
            campaign_id:   r.campaign_id, // UUID column → Uuid field
            sector_tier:   r.sector_tier,
            ruleset:       r.ruleset,
        }))
    }

    async fn append_campaign_entry(
        &self,
        campaign_id: &Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError> {
        sqlx::query!(
            r#"
            INSERT INTO input_logs (session_id, campaign_id, tick, seq, event_type, player_id, payload, narrative_event)
            VALUES (NULL, $1, $2, $3, $4, $5, $6, $7)
            "#,
            campaign_id,
            entry.tick as i64,
            entry.seq as i64,
            entry.event_type,
            entry.player_id.as_deref(),
            entry.payload.clone() as serde_json::Value,
            entry.narrative_event.as_deref(),
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }
}

// ── In-memory implementation ──────────────────────────────────────────────────

pub struct InMemoryInputLogRepository {
    entries:          DashMap<Uuid, Vec<shared::InputLogEntry>>,
    configs:          DashMap<Uuid, shared::SessionConfig>,
    campaign_entries: DashMap<Uuid, Vec<shared::InputLogEntry>>,
}

impl InMemoryInputLogRepository {
    pub fn new() -> Self {
        Self {
            entries:          DashMap::new(),
            configs:          DashMap::new(),
            campaign_entries: DashMap::new(),
        }
    }
}

#[async_trait]
impl InputLogRepository for InMemoryInputLogRepository {
    async fn append_entry(
        &self,
        session_id: &Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError> {
        self.entries.entry(*session_id).or_default().push(entry.clone());
        Ok(())
    }

    async fn get_entries_by_session(
        &self,
        session_id: &Uuid,
    ) -> Result<Vec<shared::InputLogEntry>, RepositoryError> {
        Ok(self.entries.get(session_id).map(|v| v.clone()).unwrap_or_default())
    }

    async fn save_session_config(
        &self,
        config: &shared::SessionConfig,
    ) -> Result<(), RepositoryError> {
        let session_uuid = Uuid::parse_str(&config.session_id)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        self.configs.insert(session_uuid, config.clone());
        Ok(())
    }

    async fn get_session_config(
        &self,
        session_id: &Uuid,
    ) -> Result<Option<shared::SessionConfig>, RepositoryError> {
        Ok(self.configs.get(session_id).map(|v| v.clone()))
    }

    async fn append_campaign_entry(
        &self,
        campaign_id: &Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError> {
        self.campaign_entries.entry(*campaign_id).or_default().push(entry.clone());
        Ok(())
    }
}
