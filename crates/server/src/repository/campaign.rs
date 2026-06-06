#![allow(unused)]
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

// ── Campaign lifecycle state ─────────────────────────────────────────────────

/// Campaign lifecycle state machine — maps to the `sector_state` Postgres ENUM.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(type_name = "sector_state", rename_all = "PascalCase")]
pub enum CampaignLifecycle {
    Pending,
    Active,
    Ending,
    Ended,
    Archived,
}

// ── Victory ticker types ─────────────────────────────────────────────────────

/// One variant per server-wide victory condition (all ten from Victory_Conditions.md).
/// Serializes to SCREAMING_SNAKE_CASE for use as JSONB keys in `victory_tickers`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VictoryTickerType {
    MilitaryDominance,
    OneWorldGunvernment,
    SqueakingProphets,
    VoidCallers,
    JumpLaneRestorers,
    SingularitySeekers,
    CapitalistDomination,
    GrandSyndicate,
    EmperorBobMovement,
    TrashKhansHorde,
}

impl VictoryTickerType {
    /// Returns the SCREAMING_SNAKE_CASE key used in the `victory_tickers` JSONB column.
    pub fn as_json_key(&self) -> &'static str {
        match self {
            Self::MilitaryDominance    => "MILITARY_DOMINANCE",
            Self::OneWorldGunvernment  => "ONE_WORLD_GUNVERNMENT",
            Self::SqueakingProphets    => "SQUEAKING_PROPHETS",
            Self::VoidCallers          => "VOID_CALLERS",
            Self::JumpLaneRestorers    => "JUMP_LANE_RESTORERS",
            Self::SingularitySeekers   => "SINGULARITY_SEEKERS",
            Self::CapitalistDomination => "CAPITALIST_DOMINATION",
            Self::GrandSyndicate       => "GRAND_SYNDICATE",
            Self::EmperorBobMovement   => "EMPEROR_BOB_MOVEMENT",
            Self::TrashKhansHorde      => "TRASH_KHANS_HORDE",
        }
    }
}

// ── Campaign instance record ─────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CampaignInstance {
    pub campaign_id:     Uuid,
    pub sector_id:       Uuid,
    pub map_seed:        i64,
    pub state:           CampaignLifecycle,
    /// JSONB column — keys are `VictoryTickerType::as_json_key()`, values are f64 in [0.0, 100.0].
    /// Deserialize to `HashMap<VictoryTickerType, f64>` at the call site when needed.
    pub victory_tickers: JsonValue,
    pub started_at:      Option<DateTime<Utc>>,
    pub ends_at:         Option<DateTime<Utc>>,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}

pub struct NewCampaignInstance {
    pub sector_id: Uuid,
    pub map_seed:  i64,
}

// ── Repository trait ─────────────────────────────────────────────────────────

#[async_trait]
pub trait CampaignRepository: Send + Sync {
    /// Insert a new campaign in Pending state. Returns the created record.
    async fn create_campaign(
        &self,
        params: &NewCampaignInstance,
    ) -> Result<CampaignInstance, sqlx::Error>;

    /// Look up a campaign by primary key.
    async fn get_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Option<CampaignInstance>, sqlx::Error>;

    /// Transition the campaign's lifecycle state. Updates `updated_at`. Idempotent.
    async fn update_sector_state(
        &self,
        campaign_id: Uuid,
        new_state: CampaignLifecycle,
    ) -> Result<(), sqlx::Error>;

    /// Add `delta` to one victory ticker, clamping the result to [0.0, 100.0].
    /// Treats a missing key as 0.0 on first write.
    /// Returns the new ticker value after the update.
    async fn apply_ticker_delta(
        &self,
        campaign_id: Uuid,
        ticker: VictoryTickerType,
        delta: f64,
    ) -> Result<f64, sqlx::Error>;

    /// All campaigns currently in the given lifecycle state.
    async fn list_campaigns_by_state(
        &self,
        state: CampaignLifecycle,
    ) -> Result<Vec<CampaignInstance>, sqlx::Error>;

    /// Transition a Pending campaign to Active, setting ends_at and initializing victory_tickers.
    /// Idempotent: if the campaign is already Active the UPDATE matches 0 rows and returns Ok(()).
    async fn activate_campaign(
        &self,
        campaign_id: Uuid,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<(), sqlx::Error>;
}

// ── Postgres implementation ──────────────────────────────────────────────────

pub struct PostgresCampaignRepository {
    pool: PgPool,
}

impl PostgresCampaignRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CampaignRepository for PostgresCampaignRepository {
    async fn create_campaign(
        &self,
        params: &NewCampaignInstance,
    ) -> Result<CampaignInstance, sqlx::Error> {
        sqlx::query_as!(
            CampaignInstance,
            r#"INSERT INTO campaign_instances (sector_id, map_seed)
               VALUES ($1, $2)
               RETURNING
                   campaign_id,
                   sector_id,
                   map_seed,
                   state AS "state: CampaignLifecycle",
                   victory_tickers AS "victory_tickers: JsonValue",
                   started_at,
                   ends_at,
                   created_at,
                   updated_at"#,
            params.sector_id,
            params.map_seed
        )
        .fetch_one(&self.pool)
        .await
    }

    async fn get_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Option<CampaignInstance>, sqlx::Error> {
        sqlx::query_as!(
            CampaignInstance,
            r#"SELECT
                   campaign_id,
                   sector_id,
                   map_seed,
                   state AS "state: CampaignLifecycle",
                   victory_tickers AS "victory_tickers: JsonValue",
                   started_at,
                   ends_at,
                   created_at,
                   updated_at
               FROM campaign_instances
               WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_optional(&self.pool)
        .await
    }

    async fn update_sector_state(
        &self,
        campaign_id: Uuid,
        new_state: CampaignLifecycle,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE campaign_instances SET state = $1, updated_at = now() WHERE campaign_id = $2",
            new_state as CampaignLifecycle,
            campaign_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn apply_ticker_delta(
        &self,
        campaign_id: Uuid,
        ticker: VictoryTickerType,
        delta: f64,
    ) -> Result<f64, sqlx::Error> {
        let key = ticker.as_json_key();

        // Read the current tickers value.
        let row = sqlx::query!(
            "SELECT victory_tickers FROM campaign_instances WHERE campaign_id = $1",
            campaign_id
        )
        .fetch_one(&self.pool)
        .await?;

        let current = row
            .victory_tickers
            .as_object()
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        let new_value = (current + delta).clamp(0.0, 100.0);

        // Write the updated tickers back using jsonb_set.
        // ARRAY[key] is the JSON path; to_jsonb casts the float to a JSON number.
        sqlx::query!(
            r#"UPDATE campaign_instances
               SET victory_tickers = jsonb_set(victory_tickers, ARRAY[$1::text], to_jsonb($2::float8), true),
                   updated_at = now()
               WHERE campaign_id = $3"#,
            key,
            new_value,
            campaign_id
        )
        .execute(&self.pool)
        .await?;

        Ok(new_value)
    }

    async fn list_campaigns_by_state(
        &self,
        state: CampaignLifecycle,
    ) -> Result<Vec<CampaignInstance>, sqlx::Error> {
        sqlx::query_as!(
            CampaignInstance,
            r#"SELECT
                   campaign_id,
                   sector_id,
                   map_seed,
                   state AS "state: CampaignLifecycle",
                   victory_tickers AS "victory_tickers: JsonValue",
                   started_at,
                   ends_at,
                   created_at,
                   updated_at
               FROM campaign_instances
               WHERE state = $1"#,
            state as CampaignLifecycle
        )
        .fetch_all(&self.pool)
        .await
    }

    async fn activate_campaign(
        &self,
        campaign_id: Uuid,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE campaign_instances
               SET state            = $1,
                   ends_at          = $2,
                   victory_tickers  = $3,
                   started_at       = now(),
                   updated_at       = now()
               WHERE campaign_id = $4
                 AND state       = $5"#,
            CampaignLifecycle::Active  as CampaignLifecycle,
            ends_at,
            victory_tickers            as serde_json::Value,
            campaign_id,
            CampaignLifecycle::Pending as CampaignLifecycle,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

// ── In-memory stub (unit-test contexts) ─────────────────────────────────────

pub struct InMemoryCampaignRepository;

impl InMemoryCampaignRepository {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InMemoryCampaignRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CampaignRepository for InMemoryCampaignRepository {
    async fn create_campaign(
        &self,
        _params: &NewCampaignInstance,
    ) -> Result<CampaignInstance, sqlx::Error> {
        unimplemented!("InMemoryCampaignRepository is a unit-test stub only")
    }

    async fn get_campaign(
        &self,
        _campaign_id: Uuid,
    ) -> Result<Option<CampaignInstance>, sqlx::Error> {
        unimplemented!("InMemoryCampaignRepository is a unit-test stub only")
    }

    async fn update_sector_state(
        &self,
        _campaign_id: Uuid,
        _new_state: CampaignLifecycle,
    ) -> Result<(), sqlx::Error> {
        unimplemented!("InMemoryCampaignRepository is a unit-test stub only")
    }

    async fn apply_ticker_delta(
        &self,
        _campaign_id: Uuid,
        _ticker: VictoryTickerType,
        _delta: f64,
    ) -> Result<f64, sqlx::Error> {
        unimplemented!("InMemoryCampaignRepository is a unit-test stub only")
    }

    async fn list_campaigns_by_state(
        &self,
        _state: CampaignLifecycle,
    ) -> Result<Vec<CampaignInstance>, sqlx::Error> {
        unimplemented!("InMemoryCampaignRepository is a unit-test stub only")
    }

    async fn activate_campaign(
        &self,
        _campaign_id: Uuid,
        _ends_at: DateTime<Utc>,
        _victory_tickers: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        unimplemented!("InMemoryCampaignRepository is a unit-test stub only")
    }
}
