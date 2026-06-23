use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

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
    async fn create_convoy(&self, record: ConvoyDbRecord) -> Result<Uuid, RepositoryError>;
    async fn get_convoy(&self, convoy_id: Uuid) -> Result<Option<ConvoyDbRecord>, RepositoryError>;
    async fn mark_arrived(&self, convoy_id: Uuid) -> Result<(), RepositoryError>;
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
    async fn mark_arrived(&self, _: Uuid) -> Result<(), RepositoryError> {
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

#[async_trait]
impl ConvoyRepository for PostgresConvoyRepository {
    async fn create_convoy(&self, record: ConvoyDbRecord) -> Result<Uuid, RepositoryError> {
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
            record.route,
            record.arrival_time,
            record.vehicle_class,
            record.cargo,
            record.in_transit,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
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

    async fn mark_arrived(&self, convoy_id: Uuid) -> Result<(), RepositoryError> {
        sqlx::query!(
            "UPDATE convoy_records SET in_transit = false WHERE convoy_id = $1",
            convoy_id
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }
}
