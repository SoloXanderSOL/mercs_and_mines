#![allow(unused)]
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

// ── PlayerCampaignMembership ─────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PlayerCampaignMembership {
    pub membership_id: Uuid,
    pub wallet_address: Vec<u8>,
    pub campaign_id: Uuid,
    pub gateway_hex_q: Option<i32>,
    pub gateway_hex_r: Option<i32>,
    pub joined_at: DateTime<Utc>,
    pub is_active: bool,
}

// ── MembershipRepository trait ───────────────────────────────────────────────

#[async_trait]
pub trait MembershipRepository: Send + Sync {
    async fn add_member(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
        gateway_hex_q: Option<i32>,
        gateway_hex_r: Option<i32>,
    ) -> Result<PlayerCampaignMembership, sqlx::Error>;

    async fn get_membership(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
    ) -> Result<Option<PlayerCampaignMembership>, sqlx::Error>;

    async fn assign_gateway_hex(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
        q: i32,
        r: i32,
    ) -> Result<(), sqlx::Error>;

    async fn set_inactive(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
    ) -> Result<(), sqlx::Error>;

    async fn list_members_by_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Vec<PlayerCampaignMembership>, sqlx::Error>;

    async fn list_campaigns_by_player(
        &self,
        wallet_address: &[u8],
    ) -> Result<Vec<PlayerCampaignMembership>, sqlx::Error>;

    async fn delete_memberships_by_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<(), sqlx::Error>;
}

// ── PostgresMembershipRepository ─────────────────────────────────────────────

pub struct PostgresMembershipRepository {
    pool: PgPool,
}

impl PostgresMembershipRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MembershipRepository for PostgresMembershipRepository {
    async fn add_member(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
        gateway_hex_q: Option<i32>,
        gateway_hex_r: Option<i32>,
    ) -> Result<PlayerCampaignMembership, sqlx::Error> {
        let row = sqlx::query_as!(
            PlayerCampaignMembership,
            r#"INSERT INTO player_campaign_membership
                (wallet_address, campaign_id, gateway_hex_q, gateway_hex_r)
               VALUES ($1, $2, $3, $4)
               RETURNING
                membership_id, wallet_address, campaign_id,
                gateway_hex_q, gateway_hex_r, joined_at, is_active"#,
            wallet_address,
            campaign_id,
            gateway_hex_q,
            gateway_hex_r,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn get_membership(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
    ) -> Result<Option<PlayerCampaignMembership>, sqlx::Error> {
        let row = sqlx::query_as!(
            PlayerCampaignMembership,
            r#"SELECT
                membership_id, wallet_address, campaign_id,
                gateway_hex_q, gateway_hex_r, joined_at, is_active
               FROM player_campaign_membership
               WHERE wallet_address = $1 AND campaign_id = $2"#,
            wallet_address,
            campaign_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn assign_gateway_hex(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
        q: i32,
        r: i32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE player_campaign_membership
               SET gateway_hex_q = $3, gateway_hex_r = $4
               WHERE wallet_address = $1 AND campaign_id = $2"#,
            wallet_address,
            campaign_id,
            q,
            r,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_inactive(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE player_campaign_membership
               SET is_active = FALSE
               WHERE wallet_address = $1 AND campaign_id = $2"#,
            wallet_address,
            campaign_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_members_by_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Vec<PlayerCampaignMembership>, sqlx::Error> {
        let rows = sqlx::query_as!(
            PlayerCampaignMembership,
            r#"SELECT
                membership_id, wallet_address, campaign_id,
                gateway_hex_q, gateway_hex_r, joined_at, is_active
               FROM player_campaign_membership
               WHERE campaign_id = $1
               ORDER BY joined_at ASC"#,
            campaign_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn list_campaigns_by_player(
        &self,
        wallet_address: &[u8],
    ) -> Result<Vec<PlayerCampaignMembership>, sqlx::Error> {
        let rows = sqlx::query_as!(
            PlayerCampaignMembership,
            r#"SELECT
                membership_id, wallet_address, campaign_id,
                gateway_hex_q, gateway_hex_r, joined_at, is_active
               FROM player_campaign_membership
               WHERE wallet_address = $1
               ORDER BY joined_at ASC"#,
            wallet_address,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn delete_memberships_by_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM player_campaign_membership WHERE campaign_id = $1",
            campaign_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

// ── InMemoryMembershipRepository (unit-test stub) ────────────────────────────

// Keyed by (wallet_address, campaign_id) to naturally enforce the unique constraint.
pub struct InMemoryMembershipRepository {
    store: DashMap<(Vec<u8>, Uuid), PlayerCampaignMembership>,
}

impl InMemoryMembershipRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryMembershipRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MembershipRepository for InMemoryMembershipRepository {
    async fn add_member(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
        gateway_hex_q: Option<i32>,
        gateway_hex_r: Option<i32>,
    ) -> Result<PlayerCampaignMembership, sqlx::Error> {
        let key = (wallet_address.to_vec(), campaign_id);
        if self.store.contains_key(&key) {
            return Err(sqlx::Error::Protocol(
                "unique constraint violation: (wallet_address, campaign_id) already exists".into(),
            ));
        }
        let membership = PlayerCampaignMembership {
            membership_id:  Uuid::new_v4(),
            wallet_address: wallet_address.to_vec(),
            campaign_id,
            gateway_hex_q,
            gateway_hex_r,
            joined_at:      Utc::now(),
            is_active:      true,
        };
        self.store.insert(key, membership.clone());
        Ok(membership)
    }

    async fn get_membership(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
    ) -> Result<Option<PlayerCampaignMembership>, sqlx::Error> {
        let key = (wallet_address.to_vec(), campaign_id);
        Ok(self.store.get(&key).map(|r| r.clone()))
    }

    async fn assign_gateway_hex(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
        q: i32,
        r: i32,
    ) -> Result<(), sqlx::Error> {
        let key = (wallet_address.to_vec(), campaign_id);
        if let Some(mut m) = self.store.get_mut(&key) {
            m.gateway_hex_q = Some(q);
            m.gateway_hex_r = Some(r);
        }
        Ok(())
    }

    async fn set_inactive(
        &self,
        wallet_address: &[u8],
        campaign_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        let key = (wallet_address.to_vec(), campaign_id);
        if let Some(mut m) = self.store.get_mut(&key) {
            m.is_active = false;
        }
        Ok(())
    }

    async fn list_members_by_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<Vec<PlayerCampaignMembership>, sqlx::Error> {
        let mut rows: Vec<PlayerCampaignMembership> = self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.clone())
            .collect();
        rows.sort_by_key(|r| r.joined_at);
        Ok(rows)
    }

    async fn list_campaigns_by_player(
        &self,
        wallet_address: &[u8],
    ) -> Result<Vec<PlayerCampaignMembership>, sqlx::Error> {
        let mut rows: Vec<PlayerCampaignMembership> = self.store.iter()
            .filter(|r| r.wallet_address == wallet_address)
            .map(|r| r.clone())
            .collect();
        rows.sort_by_key(|r| r.joined_at);
        Ok(rows)
    }

    async fn delete_memberships_by_campaign(
        &self,
        campaign_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        let keys: Vec<(Vec<u8>, Uuid)> = self.store.iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.key().clone())
            .collect();
        for k in keys {
            self.store.remove(&k);
        }
        Ok(())
    }
}
