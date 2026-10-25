#![allow(unused)]
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value as JsonValue;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::RepositoryError;

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

    /// Returns all 10 variants in enum-declaration order.
    pub fn variants() -> [Self; 10] {
        [
            Self::MilitaryDominance,
            Self::OneWorldGunvernment,
            Self::SqueakingProphets,
            Self::VoidCallers,
            Self::JumpLaneRestorers,
            Self::SingularitySeekers,
            Self::CapitalistDomination,
            Self::GrandSyndicate,
            Self::EmperorBobMovement,
            Self::TrashKhansHorde,
        ]
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
    ) -> Result<CampaignInstance, RepositoryError>;

    /// Look up a campaign by primary key.
    async fn get_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Option<CampaignInstance>, RepositoryError>;

    /// Transition the campaign's lifecycle state. Updates `updated_at`. Idempotent.
    /// Does not validate the transition. A missing id is a silent no-op (`Ok(())`).
    async fn update_sector_state(
        &self,
        campaign_id: Uuid,
        new_state: CampaignLifecycle,
    ) -> Result<(), RepositoryError>;

    /// Add `delta` to one victory ticker, clamping the result to [0.0, 100.0].
    /// Treats a missing key as 0.0 on first write.
    /// Returns the new ticker value after the update.
    async fn apply_ticker_delta(
        &self,
        campaign_id: Uuid,
        ticker: VictoryTickerType,
        delta: f64,
    ) -> Result<f64, RepositoryError>;

    /// All campaigns currently in the given lifecycle state.
    async fn list_campaigns_by_state(
        &self,
        state: CampaignLifecycle,
    ) -> Result<Vec<CampaignInstance>, RepositoryError>;

    /// Transition a Pending campaign to Active, setting ends_at and initializing victory_tickers.
    /// Returns true if the campaign was `Pending` and is now `Active` (1 row updated); false for
    /// any other state or a missing id (0 rows updated). Never errors on a 0-row update.
    async fn activate_campaign(
        &self,
        campaign_id: Uuid,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<bool, RepositoryError>;

    /// Moves an `Active` campaign to `Archived` (BP-1). Returns true if it was `Active` and
    /// is now `Archived` (1 row updated); false for any other state or a missing id. It never
    /// moves a campaign out of `Pending`, so it does not take `JOIN_ADVISORY_KEY`.
    async fn retire_if_active(&self, campaign_id: Uuid) -> Result<bool, RepositoryError>;
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

/// The campaign INSERT on a caller's connection (the join, 5a-2). The SQL literal is
/// `create_campaign`'s, byte for byte (its `.sqlx/` name hashes the text).
pub(crate) async fn insert_campaign(
    conn: &mut PgConnection,
    params: &NewCampaignInstance,
) -> Result<CampaignInstance, RepositoryError> {
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
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))
}

#[async_trait]
impl CampaignRepository for PostgresCampaignRepository {
    async fn create_campaign(
        &self,
        params: &NewCampaignInstance,
    ) -> Result<CampaignInstance, RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        insert_campaign(&mut conn, params).await
    }

    async fn get_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Option<CampaignInstance>, RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        campaign_row(&mut conn, campaign_id).await
    }

    async fn update_sector_state(
        &self,
        campaign_id: Uuid,
        new_state: CampaignLifecycle,
    ) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE campaign_instances SET state = $1, updated_at = now() WHERE campaign_id = $2",
            new_state as CampaignLifecycle,
            campaign_id
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn apply_ticker_delta(
        &self,
        campaign_id: Uuid,
        ticker: VictoryTickerType,
        delta: f64,
    ) -> Result<f64, RepositoryError> {
        let key = ticker.as_json_key();

        // Read the current tickers value.
        let row = sqlx::query!(
            "SELECT victory_tickers FROM campaign_instances WHERE campaign_id = $1",
            campaign_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?
        .ok_or(RepositoryError::NotFound)?;

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
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(new_value)
    }

    async fn list_campaigns_by_state(
        &self,
        state: CampaignLifecycle,
    ) -> Result<Vec<CampaignInstance>, RepositoryError> {
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
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    async fn activate_campaign(
        &self,
        campaign_id: Uuid,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<bool, RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        activate_row(&mut conn, campaign_id, ends_at, victory_tickers).await
    }

    async fn retire_if_active(&self, campaign_id: Uuid) -> Result<bool, RepositoryError> {
        let result = sqlx::query!(
            "UPDATE campaign_instances SET state = $1, updated_at = now() WHERE campaign_id = $2 AND state = $3",
            CampaignLifecycle::Archived as CampaignLifecycle,
            campaign_id,
            CampaignLifecycle::Active   as CampaignLifecycle,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected() == 1)
    }
}

/// `get_campaign`'s SELECT on a caller's connection (the launch, 5a-3a). The SQL literal is
/// the trait method's, byte for byte (its `.sqlx/` name hashes the text).
pub(crate) async fn campaign_row(
    conn: &mut PgConnection,
    campaign_id: Uuid,
) -> Result<Option<CampaignInstance>, RepositoryError> {
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
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
}

/// `activate_campaign`'s UPDATE on a caller's connection (the launch, 5a-3a). The SQL literal
/// is the trait method's, byte for byte. `true` iff the row was `Pending` and is now `Active`.
pub(crate) async fn activate_row(
    conn: &mut PgConnection,
    campaign_id: Uuid,
    ends_at: DateTime<Utc>,
    victory_tickers: serde_json::Value,
) -> Result<bool, RepositoryError> {
        let result = sqlx::query!(
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
        .execute(&mut *conn)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(result.rows_affected() == 1)
}

// ── In-memory implementation ────────────────────────────────────────────────

/// In-memory campaign store. A real store, not a stub: 5a-3's in-memory route tests
/// and 5a-2b's in-memory join read and write it. Share one instance as
/// `Arc<InMemoryCampaignRepository>` (as the sibling in-memory stores are shared).
///
/// Mirrors `PostgresCampaignRepository` method by method, miss paths included. It
/// cannot mirror:
/// - `now()` (transaction start) vs `Utc::now()` (wall clock); one `now` per call,
///   so `created_at == updated_at` on create, as in Postgres;
/// - JSONB number normalisation (`to_jsonb(0.0::float8)` stores `0`; serde_json keeps
///   `0.0`), so compare tickers with `.as_f64()`, never `Value ==` across backends;
/// - `apply_ticker_delta`'s two autocommit statements: here it is atomic under the
///   shard lock, so it is strictly stronger than Postgres and can't reveal GAP-72;
/// - timestamp precision: in-memory timestamps keep nanoseconds, while `timestamptz` keeps
///   microseconds, so an exact timestamp equality across backends (or a JSON snapshot)
///   can differ.
pub struct InMemoryCampaignRepository {
    store: DashMap<Uuid, CampaignInstance>,
}

impl InMemoryCampaignRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
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
        params: &NewCampaignInstance,
    ) -> Result<CampaignInstance, RepositoryError> {
        let now = Utc::now();
        let campaign = CampaignInstance {
            campaign_id:     Uuid::new_v4(),
            sector_id:       params.sector_id,
            map_seed:        params.map_seed,
            state:           CampaignLifecycle::Pending,
            victory_tickers: serde_json::json!({}),
            started_at:      None,
            ends_at:         None,
            created_at:      now,
            updated_at:      now,
        };
        self.store.insert(campaign.campaign_id, campaign.clone());
        Ok(campaign)
    }

    async fn get_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Option<CampaignInstance>, RepositoryError> {
        Ok(self.store.get(&campaign_id).map(|c| c.clone()))
    }

    /// As in Postgres: no transition validation, and a missing id is `Ok(())`.
    async fn update_sector_state(
        &self,
        campaign_id: Uuid,
        new_state: CampaignLifecycle,
    ) -> Result<(), RepositoryError> {
        if let Some(mut c) = self.store.get_mut(&campaign_id) {
            c.state = new_state;
            c.updated_at = Utc::now();
        }
        Ok(())
    }

    async fn apply_ticker_delta(
        &self,
        campaign_id: Uuid,
        ticker: VictoryTickerType,
        delta: f64,
    ) -> Result<f64, RepositoryError> {
        let mut c = self.store.get_mut(&campaign_id).ok_or(RepositoryError::NotFound)?;
        let key = ticker.as_json_key();
        let current = c
            .victory_tickers
            .as_object()
            .and_then(|m| m.get(key))
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.0);
        let new_value = (current + delta).clamp(0.0, 100.0);
        // No writer stores a non-object; Postgres' jsonb_set on one is an unreachable edge.
        if !c.victory_tickers.is_object() {
            c.victory_tickers = serde_json::json!({});
        }
        if let Some(m) = c.victory_tickers.as_object_mut() {
            m.insert(key.to_string(), JsonValue::from(new_value));
        }
        c.updated_at = Utc::now();
        Ok(new_value)
    }

    /// Unordered, as the Postgres SQL has no `ORDER BY`.
    async fn list_campaigns_by_state(
        &self,
        state: CampaignLifecycle,
    ) -> Result<Vec<CampaignInstance>, RepositoryError> {
        Ok(self.store.iter()
            .filter(|c| c.state == state)
            .map(|c| c.clone())
            .collect())
    }

    async fn activate_campaign(
        &self,
        campaign_id: Uuid,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<bool, RepositoryError> {
        let Some(mut c) = self.store.get_mut(&campaign_id) else {
            return Ok(false);
        };
        if c.state != CampaignLifecycle::Pending {
            return Ok(false);
        }
        let now = Utc::now();
        c.state = CampaignLifecycle::Active;
        c.ends_at = Some(ends_at);
        c.victory_tickers = victory_tickers;
        c.started_at = Some(now);
        c.updated_at = now;
        Ok(true)
    }

    async fn retire_if_active(&self, campaign_id: Uuid) -> Result<bool, RepositoryError> {
        match self.store.get_mut(&campaign_id) {
            Some(mut c) if c.state == CampaignLifecycle::Active => {
                c.state = CampaignLifecycle::Archived;
                c.updated_at = Utc::now();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn victory_ticker_variants_covers_all_ten() {
        let variants = VictoryTickerType::variants();
        assert_eq!(variants.len(), 10);
        for v in &variants {
            assert!(!v.as_json_key().is_empty());
        }
        let keys: Vec<_> = variants.iter().map(|v| v.as_json_key()).collect();
        let mut deduped = keys.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(keys.len(), deduped.len(), "duplicate keys found");
    }

    // ── InMemoryCampaignRepository (5a-2a) ──────────────────────────────────

    use std::collections::HashSet;
    use std::time::Duration;

    async fn create(repo: &InMemoryCampaignRepository, map_seed: i64) -> CampaignInstance {
        repo.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed })
            .await
            .expect("create_campaign")
    }

    async fn tick() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    fn assert_same(a: &CampaignInstance, b: &CampaignInstance) {
        assert_eq!(a.campaign_id, b.campaign_id);
        assert_eq!(a.sector_id, b.sector_id);
        assert_eq!(a.map_seed, b.map_seed);
        assert_eq!(a.state, b.state);
        assert_eq!(a.victory_tickers, b.victory_tickers);
        assert_eq!(a.started_at, b.started_at);
        assert_eq!(a.ends_at, b.ends_at);
        assert_eq!(a.created_at, b.created_at);
        assert_eq!(a.updated_at, b.updated_at);
    }

    #[tokio::test]
    async fn in_memory_create_campaign_sets_the_defaults() {
        let repo = InMemoryCampaignRepository::new();
        let sector_id = Uuid::new_v4();
        let c = repo.create_campaign(&NewCampaignInstance { sector_id, map_seed: 777 }).await.unwrap();
        assert_eq!(c.sector_id, sector_id);
        assert_eq!(c.map_seed, 777);
        assert_eq!(c.state, CampaignLifecycle::Pending);
        assert_eq!(c.victory_tickers, serde_json::json!({}));
        assert!(c.started_at.is_none());
        assert!(c.ends_at.is_none());
        assert_eq!(c.created_at, c.updated_at);
        let d = create(&repo, 777).await;
        assert_ne!(c.campaign_id, d.campaign_id, "each create gets a fresh id");
    }

    #[tokio::test]
    async fn in_memory_get_campaign_returns_the_record_or_none() {
        let repo = InMemoryCampaignRepository::new();
        let c = create(&repo, 1).await;
        let got = repo.get_campaign(c.campaign_id).await.unwrap().expect("hit");
        assert_same(&got, &c);
        assert!(repo.get_campaign(Uuid::new_v4()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_update_sector_state_sets_any_state_and_ignores_a_miss() {
        let repo = InMemoryCampaignRepository::new();
        let c = create(&repo, 1).await;
        tick().await;
        repo.update_sector_state(c.campaign_id, CampaignLifecycle::Archived).await.unwrap();
        let got = repo.get_campaign(c.campaign_id).await.unwrap().unwrap();
        assert_eq!(got.state, CampaignLifecycle::Archived, "no transition validation");
        assert!(got.updated_at > c.updated_at);
        repo.update_sector_state(Uuid::new_v4(), CampaignLifecycle::Active).await
            .expect("a miss is a silent no-op");
        assert_eq!(repo.store.len(), 1, "a miss creates nothing");
    }

    #[tokio::test]
    async fn in_memory_apply_ticker_delta_clamps_and_misses_with_not_found() {
        let repo = InMemoryCampaignRepository::new();
        let c = create(&repo, 1).await;
        let t = VictoryTickerType::GrandSyndicate;
        let stored = |repo: &InMemoryCampaignRepository| {
            repo.store.get(&c.campaign_id).unwrap()
                .victory_tickers.get("GRAND_SYNDICATE").and_then(JsonValue::as_f64)
        };
        tick().await;
        assert_eq!(repo.apply_ticker_delta(c.campaign_id, t.clone(), 12.5).await.unwrap(), 12.5);
        assert_eq!(stored(&repo), Some(12.5));
        assert!(repo.get_campaign(c.campaign_id).await.unwrap().unwrap().updated_at > c.updated_at);
        assert_eq!(repo.apply_ticker_delta(c.campaign_id, t.clone(), 200.0).await.unwrap(), 100.0);
        assert_eq!(stored(&repo), Some(100.0));
        assert_eq!(repo.apply_ticker_delta(c.campaign_id, t.clone(), -500.0).await.unwrap(), 0.0);
        assert_eq!(stored(&repo), Some(0.0));
        assert!(matches!(
            repo.apply_ticker_delta(Uuid::new_v4(), t, 1.0).await,
            Err(RepositoryError::NotFound)
        ));
    }

    #[tokio::test]
    async fn in_memory_list_campaigns_by_state_filters_unordered() {
        let repo = InMemoryCampaignRepository::new();
        let a = create(&repo, 1).await;
        let b = create(&repo, 2).await;
        let c = create(&repo, 3).await;
        repo.update_sector_state(b.campaign_id, CampaignLifecycle::Active).await.unwrap();
        let ids = |v: Vec<CampaignInstance>| v.into_iter().map(|c| c.campaign_id).collect::<HashSet<_>>();
        assert_eq!(
            ids(repo.list_campaigns_by_state(CampaignLifecycle::Pending).await.unwrap()),
            HashSet::from([a.campaign_id, c.campaign_id])
        );
        assert_eq!(
            ids(repo.list_campaigns_by_state(CampaignLifecycle::Active).await.unwrap()),
            HashSet::from([b.campaign_id])
        );
        assert!(repo.list_campaigns_by_state(CampaignLifecycle::Ended).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn in_memory_activate_campaign_only_moves_pending() {
        let repo = InMemoryCampaignRepository::new();
        let c = create(&repo, 1).await;
        let ends_at = DateTime::parse_from_rfc3339("2027-01-16T00:00:00Z").unwrap().with_timezone(&Utc);
        let tickers = serde_json::json!({ "MILITARY_DOMINANCE": 0.0 });
        tick().await;
        assert!(repo.activate_campaign(c.campaign_id, ends_at, tickers.clone()).await.unwrap());
        let got = repo.get_campaign(c.campaign_id).await.unwrap().unwrap();
        assert_eq!(got.state, CampaignLifecycle::Active);
        assert_eq!(got.ends_at, Some(ends_at));
        assert_eq!(got.victory_tickers, tickers);
        assert!(got.started_at.is_some());
        assert!(got.updated_at > c.updated_at);

        let other_end = DateTime::parse_from_rfc3339("2030-06-01T00:00:00Z").unwrap().with_timezone(&Utc);
        assert!(!repo.activate_campaign(c.campaign_id, other_end, serde_json::json!({ "X": 1.0 })).await.unwrap());
        let again = repo.get_campaign(c.campaign_id).await.unwrap().unwrap();
        assert_same(&again, &got);

        assert!(!repo.activate_campaign(Uuid::new_v4(), ends_at, tickers.clone()).await.unwrap());

        let e = create(&repo, 2).await;
        repo.update_sector_state(e.campaign_id, CampaignLifecycle::Ended).await.unwrap();
        assert!(!repo.activate_campaign(e.campaign_id, ends_at, tickers).await.unwrap());
        assert_eq!(repo.get_campaign(e.campaign_id).await.unwrap().unwrap().state, CampaignLifecycle::Ended);
    }

    /// BP-1: only an `Active` campaign is archived; every other state, and a miss, is `false`.
    #[tokio::test]
    async fn in_memory_retire_if_active_moves_only_active() {
        let repo = InMemoryCampaignRepository::new();
        let cases = [
            (CampaignLifecycle::Active,  true,  CampaignLifecycle::Archived),
            (CampaignLifecycle::Pending, false, CampaignLifecycle::Pending),
            (CampaignLifecycle::Ending,  false, CampaignLifecycle::Ending),
        ];
        for (i, (from, retired, to)) in cases.into_iter().enumerate() {
            let c = create(&repo, i as i64).await;
            repo.update_sector_state(c.campaign_id, from.clone()).await.unwrap();
            assert_eq!(repo.retire_if_active(c.campaign_id).await.unwrap(), retired, "{from:?}");
            assert_eq!(repo.get_campaign(c.campaign_id).await.unwrap().unwrap().state, to, "{from:?}");
        }
        assert!(!repo.retire_if_active(Uuid::new_v4()).await.unwrap(), "a missing id is false");
    }
}
