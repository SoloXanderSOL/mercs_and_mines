//! The ops board (BP-8a-1, **demo-only**): the dig sites and merc contracts a demo player
//! can send their team to, and the in-memory store that will hold each campaign's op
//! (Breakpoint plan §4d). The `dev` port deletes this file, `routes/ops.rs`, the `lib.rs`
//! line, the `AppState` field and the two `routes/mod.rs` lines, each marked
//! "(BP-8a-1, demo-only)". Phase 2's Strike Mining inherits none of it.
//!
//! - **The board is a pure function** of the sector, the player's gateway and the campaign
//!   id (`ops_board`). The sector never changes after launch (only `hex_occupancy` is
//!   written), so a site id names the same site on every call and after every deploy.
//! - **The board is cached per campaign** (`DemoOpsStore`), so a board read costs no sector
//!   read after the first: uncached, every read moved the ~86 KB sector out of Redis.
//! - **The contract seed is displayed provenance, never a roll seed.** It only places
//!   pins the player sees (code `CLAUDE.md` §4); the secret `op_seed` is BP-8a-2's.
//! - One lock, never held across an `.await`: every method takes it, does map work, and
//!   returns owned data. In memory only: a restart empties it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use sim_engine::missions::missions;
use sim_engine::rng::Rng;
use uuid::Uuid;

use crate::map_gen::{hex_axial_distance, HexTerrain, MagmaVeinNode, SAFE_ZONE_RADIUS};
use crate::rate_limit::{ClientKey, FixedWindow, RateWarn, RATE_LIMIT_MAP_CAP, RATE_LIMIT_WINDOW};

/// Ops requests per client address per minute, every `…/ops*` route together.
pub const OPS_PER_IP: u32 = 400;
/// Ops requests per wallet per minute, every `…/ops*` route together.
pub const OPS_PER_WALLET: u32 = 40;
/// Tier-1 dig sites lie this many hexes from the gateway, inclusive ("close, poor").
pub const OPS_DIG_TIER1_MIN_HEXES: u32 = 2;
pub const OPS_DIG_TIER1_MAX_HEXES: u32 = 6;
/// The tier-3 dig site is the nearest tier-3 vein within this many hexes ("far, rich").
pub const OPS_DIG_TIER3_MAX_HEXES: u32 = 12;
/// Contracts lie this many hexes from the gateway, inclusive, outside the Safe Zone.
pub const OPS_CONTRACT_MIN_HEXES: u32 = 3;
pub const OPS_CONTRACT_MAX_HEXES: u32 = 6;
/// At most this many tier-1 dig sites, nearest first.
pub const OPS_TIER1_SITES: usize = 2;
/// The contract pins' missions, in the client's order (`app/play/battle.js` `ORDER`).
pub const OPS_CONTRACT_MISSIONS: [&str; 4] = ["GUARD_DUTY", "ORE_EXTRACTION", "SUPPLY_RAID", "BLACK_SITE_RAID"];
/// Cached campaigns before an insert evicts.
pub const DEMO_OPS_CAP: usize = 1_000;
/// Demo travel, seconds per hex before the destination's terrain modifier. Never canon:
/// canon is 20 minutes per hex (`Hex_Map_and_Travel` §2).
pub const DEMO_SECS_PER_HEX: u32 = 2;

/// One pin on the board. A dig carries `tier`; a contract carries `mission_id` and `name`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpsSite {
    /// `"dig:{q},{r}"` or `"contract:{mission_id}"`.
    pub site_id: String,
    /// `"dig"` or `"contract"`.
    pub kind: &'static str,
    pub q: i32,
    pub r: i32,
    /// Hexes from the gateway.
    pub distance: u32,
    /// One way, from the gateway: `distance × DEMO_SECS_PER_HEX × the site's terrain modifier`.
    pub travel_secs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The board: the tier-1 digs nearest first, then the tier-3 dig, then the contracts in
/// `OPS_CONTRACT_MISSIONS` order. A map with no qualifying vein has no dig site.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpsBoard {
    pub sites: Vec<OpsSite>,
}

/// The hex's terrain if the hex is on the map and passable. A hex missing from the map is
/// impassable, as in `map_gen`.
fn passable(terrain: &HashMap<String, HexTerrain>, q: i32, r: i32) -> Option<&HexTerrain> {
    terrain.get(&format!("{q},{r}")).filter(|t| !t.is_impassable())
}

/// One-way travel time to a passable hex. Every passable modifier is 0.5, 1, 1.5 or 2, so
/// with an even `DEMO_SECS_PER_HEX` the product is a whole number of seconds.
pub fn travel_secs(distance: u32, terrain: &HexTerrain) -> u32 {
    (f64::from(distance * DEMO_SECS_PER_HEX) * terrain.travel_time_modifier()).round() as u32
}

/// The contract pins' seed: the XOR of the campaign id's four little-endian `u32` words.
/// Stable across builds and platforms (no hasher). Display provenance only.
pub fn ops_site_seed(campaign_id: Uuid) -> u32 {
    campaign_id
        .as_bytes()
        .chunks_exact(4)
        .fold(0, |acc, w| acc ^ u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
}

fn dig_site(v: &MagmaVeinNode, distance: u32, terrain: &HexTerrain) -> OpsSite {
    OpsSite {
        site_id: format!("dig:{},{}", v.q, v.r),
        kind: "dig",
        q: v.q,
        r: v.r,
        distance,
        travel_secs: travel_secs(distance, terrain),
        tier: Some(v.tier),
        mission_id: None,
        name: None,
    }
}

/// The board for a player at `gateway`.
/// - **Tier-1:** veins with `tier == 1`, `OPS_DIG_TIER1_MIN_HEXES..=OPS_DIG_TIER1_MAX_HEXES`
///   from the gateway, on a passable hex; the first `OPS_TIER1_SITES` by `(distance, q, r)`.
/// - **Tier-3:** the nearest vein with `tier == 3` (tectonic anchors included: their tier
///   comes from the vein record) within `OPS_DIG_TIER3_MAX_HEXES`, passable; ties by `(q, r)`.
/// - **Contracts:** hexes `OPS_CONTRACT_MIN_HEXES..=OPS_CONTRACT_MAX_HEXES` from the gateway
///   that are passable, hold no vein and lie outside the Safe Zone, sorted by `(q, r)`, then
///   a partial Fisher–Yates for the first `OPS_CONTRACT_MISSIONS.len()` with
///   `Rng::new(ops_site_seed(campaign_id))`. Fewer candidates → fewer contracts.
pub fn ops_board(
    terrain: &HashMap<String, HexTerrain>,
    veins: &[MagmaVeinNode],
    gateway: (i32, i32),
    campaign_id: Uuid,
) -> OpsBoard {
    let (gq, gr) = gateway;
    let dist = |q: i32, r: i32| hex_axial_distance(gq, gr, q, r);

    let mut tier1: Vec<(u32, &MagmaVeinNode, &HexTerrain)> = veins
        .iter()
        .filter(|v| v.tier == 1)
        .filter_map(|v| {
            let d = dist(v.q, v.r);
            if d < OPS_DIG_TIER1_MIN_HEXES || d > OPS_DIG_TIER1_MAX_HEXES {
                return None;
            }
            passable(terrain, v.q, v.r).map(|t| (d, v, t))
        })
        .collect();
    tier1.sort_by_key(|(d, v, _)| (*d, v.q, v.r));

    let tier3 = veins
        .iter()
        .filter(|v| v.tier == 3)
        .filter_map(|v| {
            let d = dist(v.q, v.r);
            if d > OPS_DIG_TIER3_MAX_HEXES {
                return None;
            }
            passable(terrain, v.q, v.r).map(|t| (d, v, t))
        })
        .min_by_key(|(d, v, _)| (*d, v.q, v.r));

    let mut sites: Vec<OpsSite> = tier1
        .iter()
        .take(OPS_TIER1_SITES)
        .chain(tier3.iter())
        .map(|(d, v, t)| dig_site(v, *d, t))
        .collect();

    let vein_hexes: HashSet<(i32, i32)> = veins.iter().map(|v| (v.q, v.r)).collect();
    let reach = OPS_CONTRACT_MAX_HEXES as i32;
    let mut candidates: Vec<(i32, i32, u32, &HexTerrain)> = Vec::new();
    for q in gq - reach..=gq + reach {
        for r in gr - reach..=gr + reach {
            let d = dist(q, r);
            if d < OPS_CONTRACT_MIN_HEXES || d > OPS_CONTRACT_MAX_HEXES {
                continue;
            }
            if hex_axial_distance(q, r, 0, 0) <= SAFE_ZONE_RADIUS || vein_hexes.contains(&(q, r)) {
                continue;
            }
            if let Some(t) = passable(terrain, q, r) {
                candidates.push((q, r, d, t));
            }
        }
    }
    candidates.sort_by_key(|(q, r, _, _)| (*q, *r));
    let n = candidates.len();
    let picks = OPS_CONTRACT_MISSIONS.len().min(n);
    let mut rng = Rng::new(ops_site_seed(campaign_id));
    for i in 0..picks {
        let j = i + rng.roll_int(0, (n - 1 - i) as u32) as usize;
        candidates.swap(i, j);
    }
    for (&(q, r, d, t), &mission_id) in candidates.iter().take(picks).zip(OPS_CONTRACT_MISSIONS.iter()) {
        sites.push(OpsSite {
            site_id: format!("contract:{mission_id}"),
            kind: "contract",
            q,
            r,
            distance: d,
            travel_secs: travel_secs(d, t),
            tier: None,
            mission_id: Some(mission_id),
            name: missions().get(mission_id).map(|m| m.name.clone()),
        });
    }
    OpsBoard { sites }
}

/// One campaign's ops state. BP-8a-1 holds the cached board only; BP-8a-2 adds the op and
/// BP-8b-2 the banked ore (an absent entry reads as no op and 0 ore).
struct OpsEntry {
    /// The gateway the board was computed for; another gateway is a miss.
    gateway: (i32, i32),
    board: Arc<OpsBoard>,
    touched: Instant,
}

/// Every campaign's ops state, in memory and bounded by `cap`.
pub struct DemoOpsStore {
    inner: Mutex<HashMap<Uuid, OpsEntry>>,
    cap: usize,
    evict_warn: RateWarn,
}

impl Default for DemoOpsStore {
    fn default() -> Self {
        Self::new(DEMO_OPS_CAP)
    }
}

impl DemoOpsStore {
    pub fn new(cap: usize) -> Self {
        Self { inner: Mutex::new(HashMap::new()), cap, evict_warn: RateWarn::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, OpsEntry>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The cached board for `campaign_id` at `gateway`, touched; `None` on a miss.
    pub fn board(&self, campaign_id: Uuid, gateway: (i32, i32), now: Instant) -> Option<Arc<OpsBoard>> {
        let mut inner = self.lock();
        let entry = inner.get_mut(&campaign_id).filter(|e| e.gateway == gateway)?;
        entry.touched = now;
        Some(Arc::clone(&entry.board))
    }

    /// Caches `board`, replacing the campaign's entry. A new campaign at the cap first evicts
    /// the least recently touched entry and journals (first, then at most hourly). Every
    /// BP-8a-1 entry is a recomputable board; BP-8a-2 evicts only entries with no op.
    pub fn insert(&self, campaign_id: Uuid, gateway: (i32, i32), board: Arc<OpsBoard>, now: Instant) {
        let mut inner = self.lock();
        if !inner.contains_key(&campaign_id) && inner.len() >= self.cap {
            let victim = inner.iter().min_by_key(|(_, e)| e.touched).map(|(id, _)| *id);
            if let Some(id) = victim {
                inner.remove(&id);
            }
            if let Some(n) = self.evict_warn.hit(now) {
                tracing::warn!(evicted = n, cap = self.cap, "ops boards at their cap: evicting the least recently read");
            }
        }
        inner.insert(campaign_id, OpsEntry { gateway, board, touched: now });
    }

    /// Drops the campaign's entry (a read of a campaign that is no longer `Active`).
    pub fn remove(&self, campaign_id: Uuid) {
        self.lock().remove(&campaign_id);
    }

    /// Cached campaigns (tests assert it).
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The ops layer's state and its two limiters, one `AppState` field. The fields are public
/// so a test can swap a limiter before the state is shared.
pub struct DemoOps {
    pub store: DemoOpsStore,
    pub ip: FixedWindow<ClientKey>,
    pub wallet: FixedWindow<String>,
}

impl Default for DemoOps {
    fn default() -> Self {
        Self {
            store: DemoOpsStore::default(),
            ip: FixedWindow::new("ops_ip", OPS_PER_IP, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP),
            wallet: FixedWindow::new("ops_wallet", OPS_PER_WALLET, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_gen::{assign_gateway_hexes, generate_sector_map, PlayerSpawnRequest, SpawnType, MAP_RADIUS_HEXES, SPAWN_SEED_NONCE};

    fn map(hexes: &[((i32, i32), HexTerrain)]) -> HashMap<String, HexTerrain> {
        hexes.iter().map(|((q, r), t)| (format!("{q},{r}"), t.clone())).collect()
    }

    fn vein(q: i32, r: i32, tier: u8) -> MagmaVeinNode {
        MagmaVeinNode { q, r, tier, is_tectonic_anchor: false }
    }

    fn digs(b: &OpsBoard) -> Vec<(i32, i32, u8)> {
        b.sites.iter().filter(|s| s.kind == "dig").map(|s| (s.q, s.r, s.tier.unwrap())).collect()
    }

    fn contracts(b: &OpsBoard) -> Vec<(i32, i32)> {
        b.sites.iter().filter(|s| s.kind == "contract").map(|s| (s.q, s.r)).collect()
    }

    /// A launch-shaped board: the map for `seed`, the gateway the launch would give one solo
    /// player, and a campaign id.
    fn launched(seed: u32, campaign: u128) -> (crate::map_gen::SectorMap, (i32, i32), Uuid) {
        let m = generate_sector_map(seed, MAP_RADIUS_HEXES);
        let req = [PlayerSpawnRequest { wallet_address: vec![0u8; 32], spawn_type: SpawnType::SoloQueue }];
        let g = assign_gateway_hexes(&m.safe_zone, &req, &mut Rng::new(seed.wrapping_add(SPAWN_SEED_NONCE)));
        let gw = (g[0].q, g[0].r);
        (m, gw, Uuid::from_u128(campaign))
    }

    const A: HexTerrain = HexTerrain::AshenPlains;

    #[test]
    fn ops_board_is_deterministic_and_the_campaign_moves_the_contracts() {
        let (m, gw, id) = launched(42, 1);
        let one = ops_board(&m.terrain, &m.magma_veins, gw, id);
        let two = ops_board(&m.terrain, &m.magma_veins, gw, id);
        assert_eq!(one, two);
        let other = ops_board(&m.terrain, &m.magma_veins, gw, Uuid::from_u128(2));
        assert_ne!(contracts(&one), contracts(&other), "another campaign id places other contract pins");
        assert_eq!(digs(&one), digs(&other), "dig sites don't depend on the campaign id");
    }

    /// Exactly one tier-1 vein in the 2–6 ring (d = 6), one inside it (d = 1), one outside (d = 7).
    #[test]
    fn ops_board_tier1_ring_is_2_to_6() {
        let t = map(&[((6, 0), A), ((11, 0), A), ((12, 0), A)]);
        let veins = [vein(6, 0, 1), vein(11, 0, 1), vein(12, 0, 1)];
        let b = ops_board(&t, &veins, (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(11, 0, 1)]);
    }

    #[test]
    fn ops_board_takes_the_two_nearest_tier1_veins() {
        let t = map(&[((8, 0), A), ((5, 3), A), ((9, 0), A), ((10, 0), A)]);
        let veins = [vein(10, 0, 1), vein(9, 0, 1), vein(8, 0, 1), vein(5, 3, 1)];
        let b = ops_board(&t, &veins, (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(5, 3, 1), (8, 0, 1)], "d = 3 twice, ties by (q, r)");
    }

    #[test]
    fn ops_board_tier3_is_the_nearest_within_12() {
        let t = map(&[((14, 0), A), ((15, 0), A), ((18, 0), A)]);
        let b = ops_board(&t, &[vein(15, 0, 3), vein(14, 0, 3), vein(18, 0, 3)], (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(14, 0, 3)]);
        let far = ops_board(&t, &[vein(18, 0, 3)], (5, 0), Uuid::nil());
        assert_eq!(digs(&far), vec![], "d = 13 is out of range");
    }

    /// The ring around gateway (5, 0) holds a Mountain vein (d = 2), a passable vein (d = 3),
    /// a Mountain hex (d = 3), a Safe Zone hex (d = 3), and exactly four other passable hexes.
    #[test]
    fn ops_board_skips_impassable_vein_and_safe_zone_hexes() {
        let t = map(&[
            ((7, 0), HexTerrain::Mountain),
            ((5, 3), A),
            ((5, -3), HexTerrain::Mountain),
            ((2, 0), A),
            ((8, 0), A),
            ((9, 0), A),
            ((10, 0), A),
            ((11, 0), A),
        ]);
        let b = ops_board(&t, &[vein(7, 0, 1), vein(5, 3, 1)], (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(5, 3, 1)], "the Mountain vein is no dig site");
        let mut got = contracts(&b);
        got.sort();
        assert_eq!(got, vec![(8, 0), (9, 0), (10, 0), (11, 0)]);
    }

    #[test]
    fn ops_board_on_launched_maps_keeps_every_rule() {
        let mut rng = Rng::new(0xB8A1_0001);
        for i in 0..50u128 {
            let (m, gw, id) = launched(rng.next_u32(), i);
            let b = ops_board(&m.terrain, &m.magma_veins, gw, id);
            assert_eq!(hex_axial_distance(gw.0, gw.1, 0, 0), SAFE_ZONE_RADIUS);
            let veins: HashSet<(i32, i32)> = m.magma_veins.iter().map(|v| (v.q, v.r)).collect();
            let t1: Vec<&OpsSite> = b.sites.iter().filter(|s| s.tier == Some(1)).collect();
            let t3: Vec<&OpsSite> = b.sites.iter().filter(|s| s.tier == Some(3)).collect();
            let c: Vec<&OpsSite> = b.sites.iter().filter(|s| s.kind == "contract").collect();
            assert!(t1.len() <= 2 && t3.len() <= 1, "map {i}");
            assert_eq!(b.sites.len(), t1.len() + t3.len() + c.len(), "map {i}: no other kind");
            assert!(t1.iter().all(|s| (2..=6).contains(&s.distance)), "map {i}");
            assert!(t3.iter().all(|s| s.distance <= 12), "map {i}");
            assert_eq!(c.len(), 4, "map {i}");
            let ids: Vec<Option<&str>> = c.iter().map(|s| s.mission_id).collect();
            assert_eq!(ids, OPS_CONTRACT_MISSIONS.iter().map(|m| Some(*m)).collect::<Vec<_>>(), "map {i}");
            let hexes: HashSet<(i32, i32)> = c.iter().map(|s| (s.q, s.r)).collect();
            assert_eq!(hexes.len(), 4, "map {i}: distinct");
            for s in &c {
                assert!((3..=6).contains(&s.distance), "map {i}");
                assert!(hex_axial_distance(s.q, s.r, 0, 0) > SAFE_ZONE_RADIUS, "map {i}");
                assert!(!veins.contains(&(s.q, s.r)), "map {i}");
            }
            for s in &b.sites {
                assert_eq!(s.distance, hex_axial_distance(gw.0, gw.1, s.q, s.r), "map {i}");
                let t = m.terrain.get(&format!("{},{}", s.q, s.r)).expect("on the map");
                assert!(!t.is_impassable(), "map {i}: {} on {t:?}", s.site_id);
            }
        }
    }

    #[test]
    fn ops_board_with_no_vein_still_has_four_contracts() {
        let (m, gw, id) = launched(42, 1);
        let b = ops_board(&m.terrain, &[], gw, id);
        assert_eq!(digs(&b), vec![]);
        assert_eq!(contracts(&b).len(), 4);
    }

    #[test]
    fn ops_contract_missions_are_server_missions() {
        for id in OPS_CONTRACT_MISSIONS {
            assert!(missions().contains_key(id), "{id}");
        }
    }

    #[test]
    fn ops_site_seed_is_the_xor_of_the_ids_words() {
        let id = Uuid::from_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(ops_site_seed(id), 0x1000_0000);
    }

    #[test]
    fn ops_travel_secs_uses_the_destination_terrain() {
        let t = map(&[((17, 0), HexTerrain::CooledMagma), ((7, 0), HexTerrain::CityState)]);
        let b = ops_board(&t, &[vein(17, 0, 3), vein(7, 0, 1)], (5, 0), Uuid::nil());
        let secs: Vec<(String, u32)> = b.sites.iter().map(|s| (s.site_id.clone(), s.travel_secs)).collect();
        assert_eq!(secs, vec![("dig:7,0".to_string(), 2), ("dig:17,0".to_string(), 48)]);
    }

    fn board() -> Arc<OpsBoard> {
        Arc::new(OpsBoard { sites: vec![] })
    }

    #[test]
    fn ops_store_at_its_cap_evicts_the_least_recently_read() {
        let store = DemoOpsStore::new(2);
        let t0 = Instant::now();
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        store.insert(a, (5, 0), board(), t0);
        store.insert(b, (5, 0), board(), t0 + std::time::Duration::from_secs(1));
        assert!(store.board(a, (5, 0), t0 + std::time::Duration::from_secs(2)).is_some(), "a is read last");
        store.insert(c, (5, 0), board(), t0 + std::time::Duration::from_secs(3));
        assert_eq!(store.len(), 2);
        assert!(store.board(b, (5, 0), t0).is_none(), "b was evicted");
        assert!(store.board(a, (5, 0), t0).is_some());
        assert!(store.board(c, (5, 0), t0).is_some());
    }

    #[test]
    fn ops_store_misses_on_another_gateway_and_forgets_on_remove() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let now = Instant::now();
        store.insert(a, (5, 0), board(), now);
        assert!(store.board(a, (0, 5), now).is_none());
        store.remove(a);
        assert!(store.is_empty());
    }
}
