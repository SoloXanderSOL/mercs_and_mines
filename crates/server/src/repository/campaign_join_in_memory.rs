//! The in-memory campaign join (5a-2b), over 5a-2a's shared in-memory stores. The trait,
//! the shared types and the Postgres impl live in `campaign_join.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::campaign::{CampaignLifecycle, CampaignRepository, InMemoryCampaignRepository, NewCampaignInstance};
use super::campaign_join::{
    CampaignJoinRepository, JoinOutcome, JoinSeeds, LaunchStart, ReplacementOutcome, StarterIds, NO_COMMANDERS,
    NO_STARTER_SET,
};
use super::commander::{CommanderRepository, InMemoryCommanderRepository};
use super::crew::{CrewRecord, CrewRepository, InMemoryCrewRepository};
use super::foreman::{ForemanRepository, InMemoryForemanRepository};
use super::membership::{InMemoryMembershipRepository, MembershipRepository};
use super::section::{starter_section, InMemorySectionRepository, SectionRepository};
use super::RepositoryError;
use crate::commander_gen::generate_commander;
use crate::foreman_gen::generate_foreman;

/// The join over the six in-memory stores, writing through their trait methods only
/// (Reviewer 5a-2a CF2): it holds no `DashMap` `Ref`/`RefMut`, no `std` guard across an
/// `.await`, and never holds its lock while calling code that waits on it. The lock is a
/// `tokio::sync::Mutex` because its guard is held across those `.await`s (a `std` guard is
/// `!Send`). Lock order: join lock → one `DashMap` shard per call.
///
/// - **Not atomic.** A failure part-way leaves the earlier writes. It is sequentially correct
///   and serialised against other joins through this instance only.
/// - **The membership FK invariant holds in memory by construction, not by enforcement:** the
///   membership is written first, and no membership check is added to any in-memory
///   `create_*` (Reviewer 5a-1 CF3).
/// - The in-memory stores overwrite a duplicate id rather than refuse it (Reviewer 7a-1 F2),
///   so no in-memory test asserts duplicate refusal.
/// - The roster stores hold no `acquired_via`, so the starter ids live in this impl's map. A
///   membership written outside the join has no entry: the in-memory face of "no starter set".
/// - It never names `clamp_new_record`: each in-memory `create_*` clamps itself (Reviewer
///   5a-0 CF5 is satisfied vacuously).
pub struct InMemoryCampaignJoinRepository {
    campaigns:   Arc<InMemoryCampaignRepository>,
    memberships: Arc<InMemoryMembershipRepository>,
    commanders:  Arc<InMemoryCommanderRepository>,
    foremen:     Arc<InMemoryForemanRepository>,
    sections:    Arc<InMemorySectionRepository>,
    crews:       Arc<InMemoryCrewRepository>,
    /// The join lock, and the starter ids of every join made through this instance.
    starters:    tokio::sync::Mutex<HashMap<(Vec<u8>, Uuid), StarterIds>>,
    /// The replacement-claim lock (5a-4), never `starters`: the Postgres claim never takes the
    /// join key. It serialises claims through this instance, so this impl does not model
    /// `commander_records_one_live_replacement` (0032: at most one usable replacement per
    /// campaign and wallet); the stores hold no `acquired_via`.
    ///
    /// It is independent of `starters` (Reviewer 5a-4 F6): a claim that interleaves with a
    /// first join's non-atomic writes (membership, then Commander) can see a member with zero
    /// Commanders and answer `500`. Test-only: Postgres can't reach it, because an uncommitted
    /// membership is invisible there and the claim answers `NotMember`.
    claims:      tokio::sync::Mutex<()>,
}

impl InMemoryCampaignJoinRepository {
    pub fn new(
        campaigns:   Arc<InMemoryCampaignRepository>,
        memberships: Arc<InMemoryMembershipRepository>,
        commanders:  Arc<InMemoryCommanderRepository>,
        foremen:     Arc<InMemoryForemanRepository>,
        sections:    Arc<InMemorySectionRepository>,
        crews:       Arc<InMemoryCrewRepository>,
    ) -> Self {
        Self {
            campaigns,
            memberships,
            commanders,
            foremen,
            sections,
            crews,
            starters: tokio::sync::Mutex::new(HashMap::new()),
            claims:   tokio::sync::Mutex::new(()),
        }
    }
}

#[async_trait]
impl CampaignJoinRepository for InMemoryCampaignJoinRepository {
    async fn join_campaign(
        &self,
        wallet: &[u8],
        cap: i64,
        seeds: JoinSeeds,
    ) -> Result<JoinOutcome, RepositoryError> {
        let mut starters = self.starters.lock().await;

        // S1/S7: any membership row in a live campaign, oldest first.
        let mut rows = self.memberships.list_campaigns_by_player(wallet).await?;
        rows.sort_by_key(|m| (m.joined_at, m.campaign_id));
        for m in rows {
            let live = matches!(
                self.campaigns.get_campaign(m.campaign_id).await?.map(|c| c.state),
                Some(CampaignLifecycle::Pending | CampaignLifecycle::Active | CampaignLifecycle::Ending)
            );
            if live {
                return match starters.get(&(wallet.to_vec(), m.campaign_id)) {
                    Some(ids) => Ok(JoinOutcome::AlreadyMember(*ids)),
                    None => Err(RepositoryError::Internal(NO_STARTER_SET.into())),
                };
            }
        }

        // The pick: the store is unordered (Reviewer 7a-1 F3), so sort here.
        let mut pending = self.campaigns.list_campaigns_by_state(CampaignLifecycle::Pending).await?;
        pending.sort_by_key(|c| (c.created_at, c.campaign_id));
        let mut picked = None;
        for c in pending {
            let members = self.memberships.list_members_by_campaign(c.campaign_id).await?.len();
            if (members as i64) < cap {
                picked = Some(c.campaign_id);
                break;
            }
        }
        let campaign_id = match picked {
            Some(id) => id,
            None => {
                self.campaigns
                    .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: seeds.map })
                    .await?
                    .campaign_id
            }
        };

        // Membership first, then the roster, in the Postgres order (Reviewer 5a-1 CF3).
        self.memberships.add_member(wallet, campaign_id, None, None).await?;

        let commander = generate_commander(wallet.to_vec(), campaign_id, seeds.commander);
        let commander_id = commander.commander_id;
        self.commanders.create_commander(commander).await?;

        let foreman = generate_foreman(wallet.to_vec(), campaign_id, seeds.foreman);
        let foreman_id = foreman.foreman_id;
        self.foremen.create_foreman(foreman).await?;

        let section = starter_section(campaign_id, wallet.to_vec());
        let section_id = section.section_id;
        self.sections.create_section(section).await?;

        let crew = CrewRecord::new_hire(campaign_id, wallet.to_vec());
        let crew_id = crew.crew_id;
        self.crews.create_crew(crew).await?;

        let ids = StarterIds { campaign_id, commander_id, foreman_id, section_id, crew_id };
        starters.insert((wallet.to_vec(), campaign_id), ids);
        Ok(JoinOutcome::Joined(ids))
    }

    /// The Postgres order under the join Mutex, so it is serialised against in-memory joins
    /// through this instance. Every store call returns an owned value.
    async fn begin_launch(
        &self,
        campaign_id: Uuid,
        max_members: usize,
        ends_at: DateTime<Utc>,
        victory_tickers: serde_json::Value,
    ) -> Result<LaunchStart, RepositoryError> {
        let _join_lock = self.starters.lock().await;

        let Some(campaign) = self.campaigns.get_campaign(campaign_id).await? else {
            return Ok(LaunchStart::NotFound);
        };
        if campaign.state != CampaignLifecycle::Pending {
            return Ok(LaunchStart::NotPending);
        }

        // Already sorted `joined_at` by the store.
        let members = self.memberships.list_members_by_campaign(campaign_id).await?;
        if members.is_empty() {
            return Ok(LaunchStart::NoMembers);
        }
        if members.len() > max_members {
            return Ok(LaunchStart::TooManyMembers(members.len()));
        }

        if !self.campaigns.activate_campaign(campaign_id, ends_at, victory_tickers).await? {
            // Unreachable from the join under the lock; only an out-of-band state write
            // reaches it (backstop, the `uq_player_campaign` class).
            return Ok(LaunchStart::NotPending);
        }
        Ok(LaunchStart::Started { campaign, members })
    }

    /// The Postgres order under `claims`. Eligibility is read from the Commander store, so
    /// there is no replacement map.
    async fn claim_replacement(
        &self,
        campaign_id: Uuid,
        wallet: &[u8],
        seed: u32,
    ) -> Result<ReplacementOutcome, RepositoryError> {
        let _claim_lock = self.claims.lock().await;

        if self.memberships.get_membership(wallet, campaign_id).await?.is_none() {
            return Ok(ReplacementOutcome::NotMember);
        }
        let mine: Vec<_> = self.commanders.list_commanders_by_campaign(campaign_id).await?
            .into_iter()
            .filter(|c| c.player_wallet == wallet)
            .collect();
        if mine.is_empty() {
            return Err(RepositoryError::Internal(NO_COMMANDERS.into()));
        }
        // TODO(phase-2-§14): Shattered clause retires with Rehabilitation (ODQ-46).
        if mine.iter().any(|c| !c.is_kia && !c.is_shattered) {
            return Ok(ReplacementOutcome::HasUsable);
        }

        let record = generate_commander(wallet.to_vec(), campaign_id, seed);
        let commander_id = record.commander_id;
        self.commanders.create_commander(record).await?;
        Ok(ReplacementOutcome::Granted(commander_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SEEDS: JoinSeeds = JoinSeeds { commander: 0x1111_1111, foreman: 0x2222_2222, map: 0x3333_3333 };

    struct Stores {
        campaigns:   Arc<InMemoryCampaignRepository>,
        memberships: Arc<InMemoryMembershipRepository>,
        commanders:  Arc<InMemoryCommanderRepository>,
        foremen:     Arc<InMemoryForemanRepository>,
        sections:    Arc<InMemorySectionRepository>,
        crews:       Arc<InMemoryCrewRepository>,
        join:        InMemoryCampaignJoinRepository,
    }

    fn stores() -> Stores {
        let campaigns = Arc::new(InMemoryCampaignRepository::new());
        let memberships = Arc::new(InMemoryMembershipRepository::new());
        let commanders = Arc::new(InMemoryCommanderRepository::new());
        let foremen = Arc::new(InMemoryForemanRepository::new());
        let sections = Arc::new(InMemorySectionRepository::new());
        let crews = Arc::new(InMemoryCrewRepository::new());
        let join = InMemoryCampaignJoinRepository::new(
            campaigns.clone(), memberships.clone(), commanders.clone(),
            foremen.clone(), sections.clone(), crews.clone(),
        );
        Stores { campaigns, memberships, commanders, foremen, sections, crews, join }
    }

    fn joined(o: JoinOutcome) -> StarterIds {
        match o {
            JoinOutcome::Joined(ids) => ids,
            other => panic!("expected Joined, got {other:?}"),
        }
    }

    /// Row counts for `wallet` in `campaign` across the four roster stores.
    async fn roster_counts(s: &Stores, campaign: Uuid, wallet: &[u8]) -> [usize; 4] {
        [
            s.commanders.list_commanders_by_campaign(campaign).await.unwrap().iter().filter(|r| r.player_wallet == wallet).count(),
            s.foremen.list_foremen_by_campaign(campaign).await.unwrap().iter().filter(|r| r.player_wallet == wallet).count(),
            s.sections.list_sections_by_campaign(campaign).await.unwrap().iter().filter(|r| r.player_wallet == wallet).count(),
            s.crews.list_crews_by_campaign(campaign).await.unwrap().iter().filter(|r| r.player_wallet == wallet).count(),
        ]
    }

    async fn pending_count(s: &Stores) -> usize {
        s.campaigns.list_campaigns_by_state(CampaignLifecycle::Pending).await.unwrap().len()
    }

    /// M1 — one of each starter, owned by the joiner, with the canon starting values.
    #[tokio::test]
    async fn in_memory_join_writes_one_of_each_starter() {
        let s = stores();
        let wallet = vec![42u8; 32];
        let ids = joined(s.join.join_campaign(&wallet, 2, SEEDS).await.unwrap());

        assert_eq!(roster_counts(&s, ids.campaign_id, &wallet).await, [1, 1, 1, 1]);

        let c = s.commanders.get_commander(ids.commander_id).await.unwrap().expect("commander");
        assert_eq!((c.rank, c.xp, c.stress), (1, 0, 0));
        assert!(c.commander_grade.is_some());
        assert_eq!(c.prng_seed_state, 0x1111_1111);
        assert_eq!(c.player_wallet, wallet);

        let f = s.foremen.get_foreman(ids.foreman_id).await.unwrap().expect("foreman");
        assert_eq!(f.prng_seed_state, 0x2222_2222);
        assert_ne!(c.prng_seed_state, f.prng_seed_state, "the two seeds differ");
        assert_eq!(f.player_wallet, wallet);

        let sec = s.sections.get_section(ids.section_id).await.unwrap().expect("section");
        assert_eq!(sec.name, "1st Section");
        assert_eq!(sec.headcount, 8);
        assert_eq!(sec.loadout, serde_json::json!({}));
        assert_eq!(sec.player_wallet, wallet);

        let crew = s.crews.get_crew(ids.crew_id).await.unwrap().expect("crew");
        assert_eq!((crew.headcount, crew.xp, crew.morale), (8, 0, 60));
        assert_eq!(crew.player_wallet, wallet);

        let m = s.memberships.get_membership(&wallet, ids.campaign_id).await.unwrap().expect("membership");
        assert_eq!((m.gateway_hex_q, m.gateway_hex_r), (None, None));

        let camp = s.campaigns.get_campaign(ids.campaign_id).await.unwrap().expect("campaign");
        assert_eq!(camp.state, CampaignLifecycle::Pending);
        assert_eq!(camp.map_seed, 0x3333_3333);
    }

    /// M2 — a repeat join returns the same ids and writes nothing.
    #[tokio::test]
    async fn in_memory_repeat_join_returns_already_member() {
        let s = stores();
        let wallet = vec![43u8; 32];
        let ids = joined(s.join.join_campaign(&wallet, 2, SEEDS).await.unwrap());
        let again = s.join.join_campaign(&wallet, 2, SEEDS).await.unwrap();
        assert_eq!(again, JoinOutcome::AlreadyMember(ids));
        assert_eq!(roster_counts(&s, ids.campaign_id, &wallet).await, [1, 1, 1, 1]);
        assert_eq!(s.memberships.list_campaigns_by_player(&wallet).await.unwrap().len(), 1);
        assert_eq!(pending_count(&s).await, 1);
    }

    /// M3 — three wallets at cap 2 fill two campaigns.
    #[tokio::test]
    async fn in_memory_join_fills_campaigns_up_to_cap() {
        let s = stores();
        let a = joined(s.join.join_campaign(&[42u8; 32], 2, SEEDS).await.unwrap());
        let b = joined(s.join.join_campaign(&[43u8; 32], 2, SEEDS).await.unwrap());
        let c = joined(s.join.join_campaign(&[44u8; 32], 2, SEEDS).await.unwrap());
        assert_eq!(a.campaign_id, b.campaign_id, "the first two share a campaign");
        assert_ne!(c.campaign_id, a.campaign_id, "the third gets a new campaign");
        assert_eq!(pending_count(&s).await, 2);
    }

    /// M4 — at cap 1, a join lands in an existing empty Pending campaign.
    #[tokio::test]
    async fn in_memory_join_picks_an_empty_pending_campaign() {
        let s = stores();
        let e = s.campaigns.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 7 }).await.unwrap();
        let ids = joined(s.join.join_campaign(&[42u8; 32], 1, SEEDS).await.unwrap());
        assert_eq!(ids.campaign_id, e.campaign_id, "joined the existing empty campaign");
        assert_eq!(pending_count(&s).await, 1);
    }

    /// M5 — the older of two Pending campaigns is picked.
    #[tokio::test]
    async fn in_memory_join_picks_the_oldest_pending_campaign() {
        let s = stores();
        let older = s.campaigns.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 1 }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let newer = s.campaigns.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 2 }).await.unwrap();
        assert!(older.created_at < newer.created_at, "precondition: distinct creation times");
        let ids = joined(s.join.join_campaign(&[42u8; 32], 1, SEEDS).await.unwrap());
        assert_eq!(ids.campaign_id, older.campaign_id, "joined the older campaign");
    }

    // ── begin_launch (5a-3a) ────────────────────────────────────────────────

    fn launch_ends_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2027-01-17T00:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn launch_tickers() -> serde_json::Value {
        serde_json::json!({ "MILITARY_DOMINANCE": 0.0 })
    }

    async fn pending_campaign(s: &Stores) -> Uuid {
        s.campaigns.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 9 }).await.unwrap().campaign_id
    }

    async fn state_of(s: &Stores, c: Uuid) -> CampaignLifecycle {
        s.campaigns.get_campaign(c).await.unwrap().expect("campaign").state
    }

    /// T3 — a one-member Pending campaign starts, and the store holds what was passed in.
    #[tokio::test]
    async fn begin_launch_in_memory_starts() {
        let s = stores();
        let c = pending_campaign(&s).await;
        s.memberships.add_member(&[42u8; 32], c, None, None).await.unwrap();
        match s.join.begin_launch(c, 1, launch_ends_at(), launch_tickers()).await.unwrap() {
            LaunchStart::Started { campaign, members } => {
                assert_eq!(campaign.campaign_id, c);
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].wallet_address, vec![42u8; 32]);
            }
            other => panic!("expected Started, got {other:?}"),
        }
        let got = s.campaigns.get_campaign(c).await.unwrap().unwrap();
        assert_eq!(got.state, CampaignLifecycle::Active);
        assert_eq!(got.ends_at, Some(launch_ends_at()));
        assert_eq!(got.victory_tickers, launch_tickers());
    }

    /// T4 — an unknown id.
    #[tokio::test]
    async fn begin_launch_in_memory_not_found() {
        let s = stores();
        let r = s.join.begin_launch(Uuid::new_v4(), 1, launch_ends_at(), launch_tickers()).await.unwrap();
        assert!(matches!(r, LaunchStart::NotFound), "got {r:?}");
    }

    /// T5 — a second launch of the same campaign.
    #[tokio::test]
    async fn begin_launch_in_memory_not_pending() {
        let s = stores();
        let c = pending_campaign(&s).await;
        s.memberships.add_member(&[42u8; 32], c, None, None).await.unwrap();
        let first = s.join.begin_launch(c, 1, launch_ends_at(), launch_tickers()).await.unwrap();
        assert!(matches!(first, LaunchStart::Started { .. }), "got {first:?}");
        let again = s.join.begin_launch(c, 1, launch_ends_at(), launch_tickers()).await.unwrap();
        assert!(matches!(again, LaunchStart::NotPending), "got {again:?}");
    }

    /// T6 — an empty campaign is refused and stays Pending.
    #[tokio::test]
    async fn begin_launch_in_memory_no_members() {
        let s = stores();
        let c = pending_campaign(&s).await;
        let r = s.join.begin_launch(c, 1, launch_ends_at(), launch_tickers()).await.unwrap();
        assert!(matches!(r, LaunchStart::NoMembers), "got {r:?}");
        assert_eq!(state_of(&s, c).await, CampaignLifecycle::Pending);
    }

    /// T7 — two members over a one-member cap are refused, and the campaign stays Pending.
    #[tokio::test]
    async fn begin_launch_in_memory_too_many_members() {
        let s = stores();
        let c = pending_campaign(&s).await;
        s.memberships.add_member(&[42u8; 32], c, None, None).await.unwrap();
        s.memberships.add_member(&[43u8; 32], c, None, None).await.unwrap();
        let r = s.join.begin_launch(c, 1, launch_ends_at(), launch_tickers()).await.unwrap();
        assert!(matches!(r, LaunchStart::TooManyMembers(2)), "got {r:?}");
        assert_eq!(state_of(&s, c).await, CampaignLifecycle::Pending);
    }

    /// T8 — the launch takes the join Mutex: while it is held, the launch cannot finish.
    #[tokio::test]
    async fn begin_launch_in_memory_takes_the_join_lock() {
        let campaigns = Arc::new(InMemoryCampaignRepository::new());
        let memberships = Arc::new(InMemoryMembershipRepository::new());
        let join = Arc::new(InMemoryCampaignJoinRepository::new(
            campaigns.clone(), memberships.clone(),
            Arc::new(InMemoryCommanderRepository::new()), Arc::new(InMemoryForemanRepository::new()),
            Arc::new(InMemorySectionRepository::new()), Arc::new(InMemoryCrewRepository::new()),
        ));
        let c = campaigns.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 9 }).await.unwrap().campaign_id;
        memberships.add_member(&[44u8; 32], c, None, None).await.unwrap();

        let guard = join.starters.lock().await;
        let j = join.clone();
        let mut handle = tokio::spawn(async move { j.begin_launch(c, 1, launch_ends_at(), launch_tickers()).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut handle).await.is_err(),
            "begin_launch finished while the join lock was held"
        );
        drop(guard);
        let r = handle.await.expect("launch panicked").unwrap();
        assert!(matches!(r, LaunchStart::Started { .. }), "got {r:?}");
    }

    /// M6 — a membership written outside the join has no starter set.
    #[tokio::test]
    async fn in_memory_member_without_starter_set_is_internal() {
        let s = stores();
        let wallet = [44u8; 32];
        let c = s.campaigns.create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 3 }).await.unwrap();
        s.memberships.add_member(&wallet, c.campaign_id, None, None).await.unwrap();
        match s.join.join_campaign(&wallet, 2, SEEDS).await {
            Err(RepositoryError::Internal(m)) => assert!(m.contains("no starter set"), "unexpected text: {m}"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}
