use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::RepositoryError;

// ── CrewRecord (brick 7c-1) ──────────────────────────────────────────────────

/// A Mining Crew's row in `crew_records` (migration 0024): the Mining equivalent of a
/// Section. A wipe is `headcount == 0`. Rank is derived from `xp`, never stored. `morale` is
/// Crew Morale (0–100, Crew_Morale_System §1); its bands are Phase 2 §13. No Foreman link:
/// Foremen lead operations, not crews (ODQ-40 (b)).
#[derive(Debug, Clone)]
pub struct CrewRecord {
    pub crew_id:       Uuid,
    pub campaign_id:   Uuid,
    pub player_wallet: Vec<u8>,
    pub headcount:     i16,
    pub xp:            i32,
    pub morale:        i16,
    pub created_at:    DateTime<Utc>,
}

impl CrewRecord {
    /// A newly hired crew with the canon starting values: 8 heads, 0 XP, morale 60 (ODQ-40).
    /// Draws no RNG, so it is not a generator.
    pub fn new_hire(campaign_id: Uuid, player_wallet: Vec<u8>) -> CrewRecord {
        CrewRecord {
            crew_id: Uuid::new_v4(),
            campaign_id,
            player_wallet,
            headcount: 8,
            xp: 0,
            morale: 60,
            created_at: Utc::now(),
        }
    }
}

/// `create_crew` stores morale in 0..=100. Headcount is not clamped: the CHECK guards it.
pub(super) fn clamp_new_record(mut r: CrewRecord) -> CrewRecord {
    r.morale = r.morale.clamp(0, 100);
    r
}

// ── CrewRepository trait ─────────────────────────────────────────────────────

/// No production caller yet, and no `AppState` field.
#[async_trait]
pub trait CrewRepository: Send + Sync {
    /// The only write. Clamps morale to 0..=100 before storing.
    async fn create_crew(&self, record: CrewRecord) -> Result<(), RepositoryError>;
    async fn get_crew(&self, crew_id: Uuid) -> Result<Option<CrewRecord>, RepositoryError>;
    /// Unordered (Reviewer 7a-1 F3): the first caller that shows or indexes the list adds an
    /// explicit order to both impls.
    async fn list_crews_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CrewRecord>, RepositoryError>;
}

// ── PostgresCrewRepository ───────────────────────────────────────────────────

pub struct PostgresCrewRepository {
    pool: PgPool,
}

impl PostgresCrewRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The Crew INSERT on a caller's connection (the join, 5a-2), with the morale clamp inside it.
/// The SQL literal is `create_crew`'s, byte for byte (its `.sqlx/` name hashes the text).
pub(crate) async fn insert_crew(
    conn: &mut PgConnection,
    record: CrewRecord,
) -> Result<(), RepositoryError> {
    let record = clamp_new_record(record);
    sqlx::query!(
            r#"INSERT INTO crew_records (
                crew_id, campaign_id, player_wallet, headcount, xp, morale
            ) VALUES ($1,$2,$3,$4,$5,$6)"#,
        record.crew_id,
        record.campaign_id,
        record.player_wallet,
        record.headcount,
        record.xp,
        record.morale,
    )
    .execute(&mut *conn)
    .await
    .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    Ok(())
}

#[async_trait]
impl CrewRepository for PostgresCrewRepository {
    async fn create_crew(&self, record: CrewRecord) -> Result<(), RepositoryError> {
        let mut conn = self.pool.acquire().await
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        insert_crew(&mut conn, record).await
    }

    async fn get_crew(&self, crew_id: Uuid) -> Result<Option<CrewRecord>, RepositoryError> {
        let row = sqlx::query!(
            r#"SELECT crew_id, campaign_id, player_wallet, headcount, xp, morale, created_at
               FROM crew_records WHERE crew_id = $1"#,
            crew_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(row.map(|r| CrewRecord {
            crew_id:       r.crew_id,
            campaign_id:   r.campaign_id,
            player_wallet: r.player_wallet,
            headcount:     r.headcount,
            xp:            r.xp,
            morale:        r.morale,
            created_at:    r.created_at,
        }))
    }

    async fn list_crews_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CrewRecord>, RepositoryError> {
        let rows = sqlx::query!(
            r#"SELECT crew_id, campaign_id, player_wallet, headcount, xp, morale, created_at
               FROM crew_records WHERE campaign_id = $1"#,
            campaign_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;

        Ok(rows.into_iter().map(|r| CrewRecord {
            crew_id:       r.crew_id,
            campaign_id:   r.campaign_id,
            player_wallet: r.player_wallet,
            headcount:     r.headcount,
            xp:            r.xp,
            morale:        r.morale,
            created_at:    r.created_at,
        }).collect())
    }
}

// ── InMemoryCrewRepository ───────────────────────────────────────────────────

/// Parity gaps with Postgres, kept as in the Commander, Foreman and Advisor in-memory repos
/// (Reviewer 7a-1 F2; 7c-1 C2): a duplicate `crew_id` overwrites here, and a headcount
/// outside 0..=8 is stored here, while Postgres refuses both (PK and
/// `crew_records_headcount_range`, as `Internal`). Do not assert headcount refusal or
/// duplicate refusal against this impl. Morale is clamped in both.
pub struct InMemoryCrewRepository {
    store: DashMap<Uuid, CrewRecord>,
}

impl InMemoryCrewRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryCrewRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CrewRepository for InMemoryCrewRepository {
    async fn create_crew(&self, record: CrewRecord) -> Result<(), RepositoryError> {
        let record = clamp_new_record(record);
        self.store.insert(record.crew_id, record);
        Ok(())
    }

    async fn get_crew(&self, crew_id: Uuid) -> Result<Option<CrewRecord>, RepositoryError> {
        Ok(self.store.get(&crew_id).map(|r| r.clone()))
    }

    async fn list_crews_by_campaign(&self, campaign_id: Uuid) -> Result<Vec<CrewRecord>, RepositoryError> {
        Ok(self
            .store
            .iter()
            .filter(|r| r.campaign_id == campaign_id)
            .map(|r| r.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_hire_sets_the_canon_starting_values() {
        let campaign_id = Uuid::new_v4();
        let r = CrewRecord::new_hire(campaign_id, vec![7u8; 32]);
        // Canon literals (Crew_Morale_System §1, ODQ-40): 8 heads, 0 XP, morale 60.
        assert_eq!(r.headcount, 8);
        assert_eq!(r.xp, 0);
        assert_eq!(r.morale, 60);
        assert_eq!(r.campaign_id, campaign_id);
        assert_eq!(r.player_wallet, vec![7u8; 32]);
    }

    #[tokio::test]
    async fn in_memory_crew_repository_round_trips() {
        let repo = InMemoryCrewRepository::new();
        let campaign_id = Uuid::new_v4();
        let a = CrewRecord::new_hire(campaign_id, vec![7u8; 32]);
        let b = CrewRecord::new_hire(campaign_id, vec![7u8; 32]);
        let other = CrewRecord::new_hire(Uuid::new_v4(), vec![7u8; 32]);
        repo.create_crew(a.clone()).await.unwrap();
        repo.create_crew(b.clone()).await.unwrap();
        repo.create_crew(other).await.unwrap();

        let got = repo.get_crew(a.crew_id).await.unwrap().expect("stored crew");
        assert_eq!(got.campaign_id, campaign_id);
        assert_eq!(got.player_wallet, vec![7u8; 32]);
        assert_eq!(got.headcount, 8);
        assert_eq!(got.xp, 0);
        assert_eq!(got.morale, 60);

        assert!(repo.get_crew(Uuid::new_v4()).await.unwrap().is_none());

        let mut listed: Vec<Uuid> = repo.list_crews_by_campaign(campaign_id).await.unwrap().iter().map(|r| r.crew_id).collect();
        listed.sort();
        let mut want = vec![a.crew_id, b.crew_id];
        want.sort();
        assert_eq!(listed, want, "list returns only this campaign's crews");
    }

    #[tokio::test]
    async fn in_memory_create_crew_clamps_morale_at_both_bounds() {
        let repo = InMemoryCrewRepository::new();
        let campaign_id = Uuid::new_v4();
        for (given, want) in [(-5i16, 0i16), (150, 100), (0, 0), (100, 100)] {
            let mut r = CrewRecord::new_hire(campaign_id, vec![7u8; 32]);
            r.morale = given;
            repo.create_crew(r.clone()).await.unwrap();
            let got = repo.get_crew(r.crew_id).await.unwrap().expect("stored crew");
            assert_eq!(got.morale, want, "morale {given} must be stored as {want}");
        }
    }
}
