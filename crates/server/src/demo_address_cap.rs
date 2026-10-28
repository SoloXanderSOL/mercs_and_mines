//! The per-address live-campaign cap (BP-3c-1b-2, ODQ-68 (c′), **demo-only**). One client
//! address may hold at most `DEMO_ADDRESS_LIVE_CAP` live demo campaigns that its joins
//! created, so filling the demo's 1,000 slots (ODQ-55) takes at least ten addresses, each
//! playing every campaign to beat the reaper (ODQ-67 (b)). A booth address holds about one
//! live campaign per phone, because New run archives the old one first. The `dev` port
//! deletes this file, its `lib.rs` line, its two `AppState` fields and its `main.rs` line.
//!
//! - **Where it runs:** the join route checks it before the join transaction, so at the cap it
//!   refuses every join from that address, a member's re-join included (the route can't know
//!   the outcome first). The client re-joins only when it holds no stored campaign, so that
//!   takes a phone that lost its storage on an address already at the cap.
//! - **The map holds ids, never states.** Below the cap a join reserves with no read. At the
//!   cap the ids are re-read (`live_among`) and the non-live ones dropped. A campaign never
//!   returns to a live state once it leaves one, so a stale id can only over-count. Every
//!   archiver (the reaper, the abandon, the join's failed-launch retire, the sector repair)
//!   is seen at the next read; none of them needs a hook here.
//! - **Reservations close the race:** a permit counts toward the cap from `reserve` until it
//!   is committed (`Joined`) or dropped (any other outcome, an error or a panic), so two
//!   joins from one address at `cap - 1` can't both pass.
//! - In memory only: a restart empties it, as it empties every limiter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::rate_limit::{ClientKey, RateWarn};
use crate::repository::{CampaignLifecycle, CampaignRepository, RepositoryError};

/// Live demo campaigns one client address may hold (ODQ-68 (c′)).
pub const DEMO_ADDRESS_LIVE_CAP: usize = 100;

/// Which of a set of campaigns are still live.
#[async_trait]
pub trait LiveCampaigns: Send + Sync {
    /// The ids among `ids` whose campaign is `Pending`, `Active` or `Ending`: the live set
    /// `DEMO_LIVE_CAMPAIGN_CAP` counts.
    async fn live_among(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, RepositoryError>;
}

pub struct PostgresLiveCampaigns {
    pool: PgPool,
}

impl PostgresLiveCampaigns {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl LiveCampaigns for PostgresLiveCampaigns {
    /// The state list is the text of the join's 1,000-cap count, so the two caps agree on
    /// what is live. Primary-key lookups: 0.3 ms for 100 ids at 100,000 rows.
    async fn live_among(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, RepositoryError> {
        sqlx::query_scalar!(
            r#"SELECT campaign_id AS "campaign_id!" FROM campaign_instances
               WHERE campaign_id = ANY($1) AND state IN ('Pending','Active','Ending')"#,
            ids,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Internal(e.to_string()))
    }
}

/// The in-memory mirror, over the same campaign store as `AppState::campaign_repo`.
pub struct InMemoryLiveCampaigns {
    campaigns: Arc<dyn CampaignRepository + Send + Sync>,
}

impl InMemoryLiveCampaigns {
    pub fn new(campaigns: Arc<dyn CampaignRepository + Send + Sync>) -> Self {
        Self { campaigns }
    }
}

#[async_trait]
impl LiveCampaigns for InMemoryLiveCampaigns {
    async fn live_among(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, RepositoryError> {
        let mut live = Vec::new();
        for &id in ids {
            if let Some(c) = self.campaigns.get_campaign(id).await? {
                if matches!(c.state, CampaignLifecycle::Pending | CampaignLifecycle::Active | CampaignLifecycle::Ending) {
                    live.push(id);
                }
            }
        }
        Ok(live)
    }
}

#[derive(Default)]
struct Held {
    ids: Vec<Uuid>,
    reserved: usize,
}

/// The campaigns each address created, plus its outstanding reservations. The `std` `Mutex`
/// is never held across an `.await`.
pub struct AddressCampaigns {
    cap: usize,
    held: Mutex<HashMap<ClientKey, Held>>,
    /// Journals refusals: the first, then at most hourly with a count.
    pub refused_warn: RateWarn,
}

impl AddressCampaigns {
    pub fn new(cap: usize) -> Self {
        Self { cap, held: Mutex::new(HashMap::new()), refused_warn: RateWarn::new() }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// A permit for one more campaign at `key`, or `None` if the address holds `cap` live
    /// campaigns and reservations. Below the cap, no read. At the cap, one `live_among` read,
    /// then the ids it did not return are dropped and the count is taken again. An `Err` is
    /// the read's: the caller refuses the join (fail closed).
    pub async fn reserve(&self, key: ClientKey, live: &dyn LiveCampaigns) -> Result<Option<AddressPermit<'_>>, RepositoryError> {
        let snapshot = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            let h = held.entry(key).or_default();
            if h.ids.len() + h.reserved < self.cap {
                h.reserved += 1;
                return Ok(Some(AddressPermit { caps: self, key, done: false }));
            }
            h.ids.clone()
        };
        let still = live.live_among(&snapshot).await?;
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let h = held.entry(key).or_default();
        // Only ids the read covered are dropped: one committed meanwhile is kept.
        h.ids.retain(|id| !snapshot.contains(id) || still.contains(id));
        if h.ids.len() + h.reserved >= self.cap {
            return Ok(None);
        }
        h.reserved += 1;
        Ok(Some(AddressPermit { caps: self, key, done: false }))
    }

    /// `(ids, reserved)` held for `key`; `None` once the key is removed.
    #[cfg(test)]
    fn held_by(&self, key: ClientKey) -> Option<(usize, usize)> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.get(&key).map(|h| (h.ids.len(), h.reserved))
    }
}

/// One reserved slot. `commit` turns it into a held campaign id; dropping it uncommitted
/// releases the slot, and removes the key if that leaves it empty.
pub struct AddressPermit<'a> {
    caps: &'a AddressCampaigns,
    key: ClientKey,
    done: bool,
}

impl AddressPermit<'_> {
    pub fn commit(mut self, campaign_id: Uuid) {
        {
            let mut held = self.caps.held.lock().unwrap_or_else(PoisonError::into_inner);
            let h = held.entry(self.key).or_default();
            h.reserved = h.reserved.saturating_sub(1);
            h.ids.push(campaign_id);
        }
        self.done = true;
    }
}

impl Drop for AddressPermit<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut held = self.caps.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(h) = held.get_mut(&self.key) {
            h.reserved = h.reserved.saturating_sub(1);
            if h.ids.is_empty() && h.reserved == 0 {
                held.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use crate::repository::{InMemoryCampaignRepository, NewCampaignInstance};

    const KEY: ClientKey = ClientKey::V4(Ipv4Addr::new(203, 0, 113, 7));

    /// A fixed live set that counts its reads.
    #[derive(Default)]
    struct Fixed {
        live: Mutex<HashSet<Uuid>>,
        reads: AtomicUsize,
    }

    impl Fixed {
        fn set_live(&self, id: Uuid, live: bool) {
            let mut s = self.live.lock().unwrap();
            if live { s.insert(id); } else { s.remove(&id); }
        }
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LiveCampaigns for Fixed {
        async fn live_among(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, RepositoryError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let s = self.live.lock().unwrap();
            Ok(ids.iter().copied().filter(|id| s.contains(id)).collect())
        }
    }

    /// Reserves and commits one live campaign at `KEY`; returns its id.
    async fn hold_one(caps: &AddressCampaigns, live: &Fixed) -> Uuid {
        let id = Uuid::new_v4();
        live.set_live(id, true);
        caps.reserve(KEY, live).await.unwrap().expect("a permit").commit(id);
        id
    }

    #[tokio::test]
    async fn under_the_cap_reserves_without_a_read() {
        let (caps, live) = (AddressCampaigns::new(3), Fixed::default());
        hold_one(&caps, &live).await;
        hold_one(&caps, &live).await;
        assert!(caps.reserve(KEY, &live).await.unwrap().is_some(), "2 of 3: admitted");
        assert_eq!(live.reads(), 0, "below the cap nothing is read");
    }

    #[tokio::test]
    async fn at_the_cap_refuses_while_every_id_is_live() {
        let (caps, live) = (AddressCampaigns::new(2), Fixed::default());
        hold_one(&caps, &live).await;
        hold_one(&caps, &live).await;
        assert!(caps.reserve(KEY, &live).await.unwrap().is_none(), "2 of 2 live: refused");
        assert_eq!(live.reads(), 1, "one read at the cap");
        assert_eq!(caps.held_by(KEY), Some((2, 0)), "a refusal holds nothing");
    }

    #[tokio::test]
    async fn the_prune_drops_archived_ids_then_reserves() {
        let (caps, live) = (AddressCampaigns::new(2), Fixed::default());
        let a = hold_one(&caps, &live).await;
        hold_one(&caps, &live).await;
        live.set_live(a, false);
        let permit = caps.reserve(KEY, &live).await.unwrap();
        assert!(permit.is_some(), "one archived: admitted");
        assert_eq!(caps.held_by(KEY), Some((1, 1)), "the archived id is dropped, one slot reserved");
    }

    #[tokio::test]
    async fn a_key_that_prunes_to_empty_is_removed() {
        let (caps, live) = (AddressCampaigns::new(2), Fixed::default());
        let a = hold_one(&caps, &live).await;
        let b = hold_one(&caps, &live).await;
        live.set_live(a, false);
        live.set_live(b, false);
        let permit = caps.reserve(KEY, &live).await.unwrap();
        assert!(permit.is_some(), "both archived: admitted");
        assert_eq!(caps.held_by(KEY), Some((0, 1)), "both ids dropped");
        drop(permit);
        assert_eq!(caps.held_by(KEY), None, "the emptied key is removed");
    }

    #[tokio::test]
    async fn outstanding_permits_count_toward_the_cap() {
        let (caps, live) = (AddressCampaigns::new(2), Fixed::default());
        let first = caps.reserve(KEY, &live).await.unwrap().expect("first");
        let _second = caps.reserve(KEY, &live).await.unwrap().expect("second");
        assert!(caps.reserve(KEY, &live).await.unwrap().is_none(), "two in flight at 2: refused");
        drop(first);
        assert!(caps.reserve(KEY, &live).await.unwrap().is_some(), "a dropped permit frees its slot");
    }

    #[tokio::test]
    async fn in_memory_live_among_keeps_pending_active_and_ending() {
        let repo = Arc::new(InMemoryCampaignRepository::new());
        let mut ids = Vec::new();
        for (i, s) in [CampaignLifecycle::Pending, CampaignLifecycle::Active, CampaignLifecycle::Ending,
                       CampaignLifecycle::Ended, CampaignLifecycle::Archived].into_iter().enumerate() {
            let c = repo.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: i as i64 }).await.unwrap();
            if s != CampaignLifecycle::Pending {
                repo.update_sector_state(c.campaign_id, s).await.unwrap();
            }
            ids.push(c.campaign_id);
        }
        let unknown = Uuid::new_v4();
        let mut probe = ids.clone();
        probe.push(unknown);
        let live = InMemoryLiveCampaigns::new(repo).live_among(&probe).await.unwrap();
        assert_eq!(live, ids[..3].to_vec(), "Pending, Active and Ending are live; Ended, Archived and unknown are not");
    }
}
