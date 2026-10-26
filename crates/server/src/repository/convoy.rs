use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::input_log::insert_campaign_entry;
use super::RepositoryError;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConvoyDbRecord {
    pub convoy_id:     Uuid,
    pub sector_id:     Uuid,
    pub owner_wallet:  Vec<u8>,
    pub origin_q:      i32,
    pub origin_r:      i32,
    pub destination_q: i32,
    pub destination_r: i32,
    pub route:         serde_json::Value,
    pub arrival_time:  DateTime<Utc>,
    pub vehicle_class: String,
    pub cargo:         serde_json::Value,
    pub in_transit:    bool,
    pub created_at:    DateTime<Utc>,
}

#[async_trait]
pub trait ConvoyRepository: Send + Sync {
    /// Inserts the convoy row alone. A test fixture: production dispatch goes through
    /// [`ConvoyRepository::dispatch_convoy`], which logs the dispatch with it.
    async fn create_convoy(&self, record: ConvoyDbRecord) -> Result<Uuid, RepositoryError>;
    async fn get_convoy(&self, convoy_id: Uuid) -> Result<Option<ConvoyDbRecord>, RepositoryError>;
    /// Writes the convoy and its `convoy_dispatched` log row together, or neither (GAP-44).
    async fn dispatch_convoy(
        &self,
        record: ConvoyDbRecord,
        campaign_id: Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError>;
    /// Lands an in-transit convoy and writes its `convoy_arrived` log row together (GAP-44).
    /// `Some` only on the call that landed it; `None` if it had already landed or does not
    /// exist, and then nothing is written. The `in_transit` guard makes the entry
    /// exactly-once however often the arrival is retried.
    async fn land_convoy(
        &self,
        convoy_id: Uuid,
        campaign_id: Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<Option<ConvoyDbRecord>, RepositoryError>;
    /// True while `owner_wallet` has a convoy in `sector_id` that is in transit and can still
    /// land: one more than five minutes overdue no longer counts. BP-1b: the demo's abandon
    /// refuses while this is true, because the expiry poll never cancels a timer whose sector
    /// is gone. A convoy in a campaign whose sector was never written never lands.
    async fn has_convoy_in_transit(&self, sector_id: Uuid, owner_wallet: &[u8]) -> Result<bool, RepositoryError>;
}

// ---------------------------------------------------------------------------
// InMemory stub — all methods unimplemented (convoy ops require Postgres)
// ---------------------------------------------------------------------------

pub struct InMemoryConvoyRepository;

#[async_trait]
impl ConvoyRepository for InMemoryConvoyRepository {
    async fn create_convoy(&self, _: ConvoyDbRecord) -> Result<Uuid, RepositoryError> {
        unimplemented!()
    }
    async fn get_convoy(&self, _: Uuid) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
        unimplemented!()
    }
    async fn dispatch_convoy(&self, _: ConvoyDbRecord, _: Uuid, _: &shared::InputLogEntry) -> Result<(), RepositoryError> {
        unimplemented!()
    }
    async fn land_convoy(&self, _: Uuid, _: Uuid, _: &shared::InputLogEntry) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
        unimplemented!()
    }
    async fn has_convoy_in_transit(&self, _: Uuid, _: &[u8]) -> Result<bool, RepositoryError> {
        unimplemented!()
    }
}

// ---------------------------------------------------------------------------
// Postgres implementation
// ---------------------------------------------------------------------------

pub struct PostgresConvoyRepository {
    pool: PgPool,
}

impl PostgresConvoyRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The one convoy `INSERT`, on the pool (fixture) or inside the dispatch transaction.
async fn insert_convoy<'e, E: sqlx::PgExecutor<'e>>(exec: E, record: &ConvoyDbRecord) -> Result<(), RepositoryError> {
    sqlx::query!(
            r#"INSERT INTO convoy_records (
                convoy_id, sector_id, owner_wallet,
                origin_q, origin_r, destination_q, destination_r,
                route, arrival_time, vehicle_class, cargo, in_transit
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
        record.convoy_id,
        record.sector_id,
        record.owner_wallet,
        record.origin_q,
        record.origin_r,
        record.destination_q,
        record.destination_r,
        record.route.clone(),
        record.arrival_time,
        record.vehicle_class,
        record.cargo.clone(),
        record.in_transit,
    )
    .execute(exec)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(())
}

#[async_trait]
impl ConvoyRepository for PostgresConvoyRepository {
    async fn create_convoy(&self, record: ConvoyDbRecord) -> Result<Uuid, RepositoryError> {
        insert_convoy(&self.pool, &record).await?;
        Ok(record.convoy_id)
    }

    async fn get_convoy(&self, convoy_id: Uuid) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
        sqlx::query_as!(
            ConvoyDbRecord,
            r#"SELECT convoy_id, sector_id, owner_wallet,
                      origin_q, origin_r, destination_q, destination_r,
                      route, arrival_time, vehicle_class, cargo, in_transit, created_at
               FROM convoy_records WHERE convoy_id = $1"#,
            convoy_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    async fn dispatch_convoy(
        &self,
        record: ConvoyDbRecord,
        campaign_id: Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<(), RepositoryError> {
        // Dropping `tx` without commit rolls back: no convoy without its log row, or the reverse.
        let mut tx = self.pool.begin().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        insert_convoy(&mut *tx, &record).await?;
        insert_campaign_entry(&mut *tx, &campaign_id, entry).await?;
        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))
    }

    async fn land_convoy(
        &self,
        convoy_id: Uuid,
        campaign_id: Uuid,
        entry: &shared::InputLogEntry,
    ) -> Result<Option<ConvoyDbRecord>, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        let landed = sqlx::query_as!(
            ConvoyDbRecord,
            r#"UPDATE convoy_records SET in_transit = false
               WHERE convoy_id = $1 AND in_transit
               RETURNING convoy_id, sector_id, owner_wallet,
                         origin_q, origin_r, destination_q, destination_r,
                         route, arrival_time, vehicle_class, cargo, in_transit, created_at"#,
            convoy_id
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        // Already landed (or no such convoy): write nothing, so a retry never duplicates the entry.
        let Some(convoy) = landed else { return Ok(None) };
        insert_campaign_entry(&mut *tx, &campaign_id, entry).await?;
        tx.commit().await.map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(Some(convoy))
    }

    async fn has_convoy_in_transit(&self, sector_id: Uuid, owner_wallet: &[u8]) -> Result<bool, RepositoryError> {
        sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM convoy_records
                   WHERE sector_id = $1 AND owner_wallet = $2 AND in_transit
                     AND arrival_time > now() - interval '5 minutes'
               ) AS "in_transit!""#,
            sector_id,
            owner_wallet
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}
