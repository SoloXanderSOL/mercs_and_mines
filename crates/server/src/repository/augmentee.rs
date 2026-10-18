use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

use sim_engine::roster::{Augment, AugmentCatalogue, CasualtyClass};

use super::RepositoryError;

// ── AugmenteeRecord (brick 7d-1) ─────────────────────────────────────────────

/// Who an augmentee belongs to: exactly one Section or one Mining Crew. Having exactly one
/// owner is unrepresentable otherwise in Rust; `augmentee_records_one_owner` enforces it for
/// raw SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AugmenteeOwner {
    Section(Uuid),
    Crew(Uuid),
}

impl AugmenteeOwner {
    fn columns(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            AugmenteeOwner::Section(id) => (Some(id), None),
            AugmenteeOwner::Crew(id) => (None, Some(id)),
        }
    }

    fn from_columns(section_id: Option<Uuid>, crew_id: Option<Uuid>) -> Result<Self, RepositoryError> {
        match (section_id, crew_id) {
            (Some(id), None) => Ok(AugmenteeOwner::Section(id)),
            (None, Some(id)) => Ok(AugmenteeOwner::Crew(id)),
            _ => Err(RepositoryError::Internal("augmentee row without exactly one owner".into())),
        }
    }

    /// The catalogue this owner is fitted from (Bespoke §2, division of labour).
    fn catalogue(self) -> AugmentCatalogue {
        match self {
            AugmenteeOwner::Section(_) => AugmentCatalogue::Combat,
            AugmenteeOwner::Crew(_) => AugmentCatalogue::Industrial,
        }
    }
}

/// A named augmented individual's row in `augmentee_records` (migration 0029). `augment`,
/// `fitting_source` and `casualty_class` hold `sim_engine::roster` display strings, pinned by
/// `0029`'s CHECKs. `casualty_class` is `None` while serving. The row is never deleted: a dead
/// augmentee's row holds the death notice's content (GAP-67). Origin is not stored (Q4).
#[derive(Debug, Clone, PartialEq)]
pub struct AugmenteeRecord {
    pub augmentee_id:    Uuid,
    pub campaign_id:     Uuid,
    pub player_wallet:   Vec<u8>,
    pub owner:           AugmenteeOwner,
    /// The Chrome Ceiling slot, 1 or 2.
    pub slot:            i16,
    pub name:            String,
    pub callsign:        String,
    pub augment:         String,
    pub fitting_source:  String,
    pub casualty_class:  Option<String>,
    pub fate_at:         Option<DateTime<Utc>>,
    pub prng_seed_state: i64,
    pub created_at:      DateTime<Utc>,
}

/// A class that still holds the slot: serving (`None`) or Wounded.
fn holds_slot(casualty_class: Option<&str>) -> bool {
    match casualty_class {
        None => true,
        Some(c) => c == CasualtyClass::Wounded.as_str(),
    }
}

// ── AugmenteeRepository trait ────────────────────────────────────────────────

/// No production caller yet, and no `AppState` field (8b-1 wires the casualty pipeline).
#[async_trait]
pub trait AugmenteeRepository: Send + Sync {
    /// The fitting. Refused while the owner's slot is held, or for an off-catalogue SKU.
    async fn create_augmentee(&self, record: AugmenteeRecord) -> Result<(), RepositoryError>;
    /// `NotFound` for an unknown id.
    async fn get_augmentee(&self, augmentee_id: Uuid) -> Result<AugmenteeRecord, RepositoryError>;
    /// Unordered (Reviewer 7a-1 F3): the first caller that shows or indexes the list adds an
    /// explicit order to both impls.
    async fn list_augmentees_by_owner(&self, owner: &AugmenteeOwner) -> Result<Vec<AugmenteeRecord>, RepositoryError>;
    /// Records a fate on a serving or Wounded augmentee, in one conditional statement, so a
    /// terminal augmentee (KIA, MIA, Deserted) is never re-fated (§4c rule). `NotFound` covers
    /// both an unknown id and an already-terminal augmentee. Never writes back to serving
    /// (GAP-68).
    async fn record_fate(&self, augmentee_id: Uuid, class: CasualtyClass) -> Result<(), RepositoryError>;
}

// ── PostgresAugmenteeRepository ──────────────────────────────────────────────

pub struct PostgresAugmenteeRepository {
    pool: PgPool,
}

impl PostgresAugmenteeRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AugmenteeRepository for PostgresAugmenteeRepository {
    async fn create_augmentee(&self, r: AugmenteeRecord) -> Result<(), RepositoryError> {
        let (section_id, crew_id) = r.owner.columns();
        sqlx::query!(
            r#"INSERT INTO augmentee_records (
                augmentee_id, campaign_id, player_wallet, section_id, crew_id, slot,
                name, callsign, augment, fitting_source, casualty_class, fate_at, prng_seed_state
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)"#,
            r.augmentee_id,
            r.campaign_id,
            r.player_wallet,
            section_id,
            crew_id,
            r.slot,
            r.name,
            r.callsign,
            r.augment,
            r.fitting_source,
            r.casualty_class,
            r.fate_at,
            r.prng_seed_state,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_augmentee(&self, augmentee_id: Uuid) -> Result<AugmenteeRecord, RepositoryError> {
        let r = sqlx::query!(
            r#"SELECT augmentee_id, campaign_id, player_wallet, section_id, crew_id, slot,
                      name, callsign, augment, fitting_source, casualty_class, fate_at,
                      prng_seed_state, created_at
               FROM augmentee_records WHERE augmentee_id = $1"#,
            augmentee_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?
        .ok_or(RepositoryError::NotFound)?;

        Ok(AugmenteeRecord {
            augmentee_id:    r.augmentee_id,
            campaign_id:     r.campaign_id,
            player_wallet:   r.player_wallet,
            owner:           AugmenteeOwner::from_columns(r.section_id, r.crew_id)?,
            slot:            r.slot,
            name:            r.name,
            callsign:        r.callsign,
            augment:         r.augment,
            fitting_source:  r.fitting_source,
            casualty_class:  r.casualty_class,
            fate_at:         r.fate_at,
            prng_seed_state: r.prng_seed_state,
            created_at:      r.created_at,
        })
    }

    async fn list_augmentees_by_owner(&self, owner: &AugmenteeOwner) -> Result<Vec<AugmenteeRecord>, RepositoryError> {
        // Two texts, one per owner column, so each uses its own index.
        macro_rules! fetch {
            ($sql:literal, $id:expr) => {
                sqlx::query!($sql, $id)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| RepositoryError::Internal(e.to_string()))?
                    .into_iter()
                    .map(|r| {
                        Ok(AugmenteeRecord {
                            augmentee_id:    r.augmentee_id,
                            campaign_id:     r.campaign_id,
                            player_wallet:   r.player_wallet,
                            owner:           AugmenteeOwner::from_columns(r.section_id, r.crew_id)?,
                            slot:            r.slot,
                            name:            r.name,
                            callsign:        r.callsign,
                            augment:         r.augment,
                            fitting_source:  r.fitting_source,
                            casualty_class:  r.casualty_class,
                            fate_at:         r.fate_at,
                            prng_seed_state: r.prng_seed_state,
                            created_at:      r.created_at,
                        })
                    })
                    .collect()
            };
        }
        match *owner {
            AugmenteeOwner::Section(id) => fetch!(
                r#"SELECT augmentee_id, campaign_id, player_wallet, section_id, crew_id, slot,
                          name, callsign, augment, fitting_source, casualty_class, fate_at,
                          prng_seed_state, created_at
                   FROM augmentee_records WHERE section_id = $1"#,
                id
            ),
            AugmenteeOwner::Crew(id) => fetch!(
                r#"SELECT augmentee_id, campaign_id, player_wallet, section_id, crew_id, slot,
                          name, callsign, augment, fitting_source, casualty_class, fate_at,
                          prng_seed_state, created_at
                   FROM augmentee_records WHERE crew_id = $1"#,
                id
            ),
        }
    }

    async fn record_fate(&self, augmentee_id: Uuid, class: CasualtyClass) -> Result<(), RepositoryError> {
        let result = sqlx::query!(
            "UPDATE augmentee_records SET casualty_class = $2, fate_at = now() WHERE augmentee_id = $1 AND (casualty_class IS NULL OR casualty_class = 'Wounded')",
            augmentee_id,
            class.as_str(),
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }
}

// ── InMemoryAugmenteeRepository ──────────────────────────────────────────────

/// Models the Chrome Ceiling (one serving or Wounded holder per owner and slot) and the
/// catalogue/owner match, refusing each with `Internal` as Postgres's violations map. The
/// owner FKs (the owner exists, with the same wallet and campaign), the slot range and the
/// string CHECKs are Postgres-only. A duplicate `augmentee_id` overwrites here, while Postgres
/// refuses it (Reviewer 7a-1 F2 parity); do not assert duplicate refusal against this impl.
pub struct InMemoryAugmenteeRepository {
    store: DashMap<Uuid, AugmenteeRecord>,
}

impl InMemoryAugmenteeRepository {
    pub fn new() -> Self {
        Self { store: DashMap::new() }
    }
}

impl Default for InMemoryAugmenteeRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AugmenteeRepository for InMemoryAugmenteeRepository {
    async fn create_augmentee(&self, record: AugmenteeRecord) -> Result<(), RepositoryError> {
        let catalogue = Augment::from_db_str(&record.augment).map(Augment::catalogue);
        if catalogue != Some(record.owner.catalogue()) {
            return Err(RepositoryError::Internal(format!(
                "create_augmentee: {:?} is not in the {:?} catalogue",
                record.augment,
                record.owner.catalogue()
            )));
        }
        if holds_slot(record.casualty_class.as_deref()) {
            let held = self.store.iter().any(|r| {
                r.augmentee_id != record.augmentee_id
                    && r.owner == record.owner
                    && r.slot == record.slot
                    && holds_slot(r.casualty_class.as_deref())
            });
            if held {
                return Err(RepositoryError::Internal(format!(
                    "create_augmentee: slot {} of {:?} is held",
                    record.slot, record.owner
                )));
            }
        }
        self.store.insert(record.augmentee_id, record);
        Ok(())
    }

    async fn get_augmentee(&self, augmentee_id: Uuid) -> Result<AugmenteeRecord, RepositoryError> {
        self.store.get(&augmentee_id).map(|r| r.clone()).ok_or(RepositoryError::NotFound)
    }

    async fn list_augmentees_by_owner(&self, owner: &AugmenteeOwner) -> Result<Vec<AugmenteeRecord>, RepositoryError> {
        Ok(self.store.iter().filter(|r| r.owner == *owner).map(|r| r.clone()).collect())
    }

    async fn record_fate(&self, augmentee_id: Uuid, class: CasualtyClass) -> Result<(), RepositoryError> {
        let mut r = self.store.get_mut(&augmentee_id).ok_or(RepositoryError::NotFound)?;
        if !holds_slot(r.casualty_class.as_deref()) {
            return Err(RepositoryError::NotFound);
        }
        r.casualty_class = Some(class.as_str().to_string());
        r.fate_at = Some(Utc::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::augmentee_gen::generate_augmentee;
    use sim_engine::roster::{CommanderOrigin, FittingSource};

    fn fitted(owner: AugmenteeOwner, slot: i16, seed: u32) -> AugmenteeRecord {
        generate_augmentee(
            vec![36u8; 32],
            Uuid::nil(),
            owner,
            slot,
            CommanderOrigin::HumanCorporate,
            Augment::IntegralGunArm,
            FittingSource::Secular,
            seed,
        )
    }

    #[tokio::test]
    async fn in_memory_augmentee_slot_frees_when_the_holder_dies() {
        let repo = InMemoryAugmenteeRepository::new();
        let owner = AugmenteeOwner::Section(Uuid::new_v4());
        let first = fitted(owner, 1, 1);
        repo.create_augmentee(first.clone()).await.expect("first holder");

        assert!(
            matches!(repo.create_augmentee(fitted(owner, 1, 2)).await, Err(RepositoryError::Internal(_))),
            "a second serving holder of slot 1 must be refused"
        );
        repo.record_fate(first.augmentee_id, CasualtyClass::Wounded).await.expect("Wounded");
        assert!(
            matches!(repo.create_augmentee(fitted(owner, 1, 3)).await, Err(RepositoryError::Internal(_))),
            "a Wounded holder keeps the slot"
        );
        repo.record_fate(first.augmentee_id, CasualtyClass::Kia).await.expect("KIA");
        repo.create_augmentee(fitted(owner, 1, 4)).await.expect("a KIA holder frees the slot");
    }

    #[tokio::test]
    async fn in_memory_record_fate_refuses_terminal_and_unknown() {
        let repo = InMemoryAugmenteeRepository::new();
        let owner = AugmenteeOwner::Section(Uuid::new_v4());
        let dead = fitted(owner, 1, 1);
        repo.create_augmentee(dead.clone()).await.unwrap();
        repo.record_fate(dead.augmentee_id, CasualtyClass::Kia).await.expect("first fate");
        assert!(matches!(repo.record_fate(dead.augmentee_id, CasualtyClass::Mia).await, Err(RepositoryError::NotFound)));
        assert_eq!(repo.get_augmentee(dead.augmentee_id).await.unwrap().casualty_class.as_deref(), Some("KIA"));

        assert!(matches!(repo.record_fate(Uuid::new_v4(), CasualtyClass::Kia).await, Err(RepositoryError::NotFound)));

        let hurt = fitted(owner, 2, 2);
        repo.create_augmentee(hurt.clone()).await.unwrap();
        repo.record_fate(hurt.augmentee_id, CasualtyClass::Wounded).await.expect("Wounded");
        repo.record_fate(hurt.augmentee_id, CasualtyClass::Kia).await.expect("Wounded then KIA");
        assert_eq!(repo.get_augmentee(hurt.augmentee_id).await.unwrap().casualty_class.as_deref(), Some("KIA"));
    }
}
