//! The ops board (BP-8a-1, **demo-only**), its travel legs (BP-8a-2), the dig (BP-8b-1a), its raid (BP-8b-1b) and the haul (BP-8b-2): the dig sites and
//! merc contracts a demo player can send their team to, and the in-memory store that holds
//! each campaign's op (Breakpoint plan §4d). The `dev` port deletes this file, `routes/ops.rs`,
//! the `lib.rs` line, the `AppState` field and the two `routes/mod.rs` lines, each marked
//! "(BP-8a-1, demo-only)". Phase 2's Strike Mining inherits none of it.
//!
//! - **The board is a pure function** of the sector, the player's gateway and the campaign
//!   id (`ops_board`). The sector never changes after launch (only `hex_occupancy` is
//!   written), so a site id names the same site on every call and after every deploy.
//! - **The board is cached per campaign** (`DemoOpsStore`), so a board read costs no sector
//!   read after the first: uncached, every read moved the ~86 KB sector out of Redis.
//! - **The contract seed is displayed provenance, never a roll seed.** It only places
//!   pins the player sees (code `CLAUDE.md` §4). The op's own seed is drawn from OS entropy
//!   at `ops/start`, stored on the private `ActiveOp` and never serialised.
//! - **Leg times are wall clock** (`DateTime<Utc>`): the ops layer is not the sim and logs
//!   nothing, so code `CLAUDE.md` §2's ban on wall clock inside `Step()` does not reach it.
//!   Every store method takes `now`, so tests pass any instant.
//! - One lock, never held across an `.await`: every method takes it, does map work, and
//!   returns owned data. In memory only: a restart empties it.
//! - **The dig (BP-8b-1a):** each drill adds heat and ore on site; at the threshold the site
//!   melts down, the ore on site is lost and the Crew loses heads (`drill_op`, §4d's numbers).
//!   A drill's rolls depend on the op's seed and the drill's number alone (`drill_roll`).
//! - **The meltdown is the layer's one durable effect:** the route writes the Crew's heads
//!   through `demo_crew_loss::CrewLoss` after the store has committed the meltdown. No replay
//!   rebuilds it, which is acceptable only because the layer is demo-only.
//! - **The raid (BP-8b-1b):** a drill that does not melt down may bring raiders (`DEMO_RAID_PCT_PER_DRILL`
//!   × n %, on the drill's first roll). The raid is fought as an ordinary `GUARD_DUTY` mission by the
//!   dig's Commander, and the store reads the outcome from that Commander's deployment records
//!   (`raid_result`), never from the phone.
//! - **The haul (BP-8b-2):** a pull takes the ore on site home, where the arrival banks it for the
//!   campaign. The haul may be ambushed halfway (`ambush_pct`), fought and read like a raid.
//! - **The merc base (BP-10a):** banked ore buys four buildings (`BaseBuilding`), one copy each,
//!   only while the team is home. What stands (`OpsBase`) changes the dig's numbers, and the drill,
//!   the view and the pull all read them from `OpsBase`, so the panel never disagrees with the roll.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use sim_engine::missions::missions;
use sim_engine::rng::Rng;
use uuid::Uuid;

use crate::map_gen::{hex_axial_distance, HexTerrain, MagmaVeinNode, SAFE_ZONE_RADIUS};
use crate::rate_limit::{ClientKey, FixedWindow, RateWarn, RATE_LIMIT_MAP_CAP, RATE_LIMIT_WINDOW};
use crate::repository::DeploymentRecord;

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
/// Heat each drill adds, and the meltdown threshold (§4d: the 7th drill melts down).
pub const DEMO_HEAT_PER_DRILL: u32 = 15;
pub const DEMO_HEAT_THRESHOLD: u32 = 100;
/// A Coolant-Jockey Foreman's threshold, in percent of `DEMO_HEAT_THRESHOLD` (×1.15 → 115).
pub const DEMO_COOLANT_THRESHOLD_PCT: u32 = 115;
/// Drill *n* yields `n ×` this on a tier-1 vein, and `DEMO_TIER3_ORE_MULTIPLIER` times that on tier 3.
pub const DEMO_ORE_PER_DRILL: u32 = 10;
pub const DEMO_TIER3_ORE_MULTIPLIER: u32 = 2;
/// Heads a meltdown costs the Crew, inclusive.
pub const DEMO_MELTDOWN_HEADS_MIN: u32 = 1;
pub const DEMO_MELTDOWN_HEADS_MAX: u32 = 3;
/// Draws each drill takes from the op's roll stream: the raid d100 (read from BP-8b-1b), then
/// the meltdown's heads. Both are drawn on every drill, so the raid changes no meltdown.
pub const DEMO_ROLLS_PER_DRILL: u32 = 2;
/// Drill *n* brings raiders when its d100 is at most `n ×` this (§4d: 5 × n %).
pub const DEMO_RAID_PCT_PER_DRILL: u32 = 5;
/// The mission a raid is fought as (§4d: `GUARD_DUTY`'s numbers under raid copy).
pub const DEMO_RAID_MISSION: &str = "GUARD_DUTY";
/// The haul's ambush chance (§4d): `DEMO_AMBUSH_BASE_PCT` plus `DEMO_AMBUSH_PCT_PER_50` per whole
/// 50 ore carried, at most `DEMO_AMBUSH_CAP_PCT`.
pub const DEMO_AMBUSH_BASE_PCT: u32 = 10;
pub const DEMO_AMBUSH_PCT_PER_50: u32 = 5;
pub const DEMO_AMBUSH_CAP_PCT: u32 = 50;
/// The ambush rolls from `Rng::new(op_seed + this)` (wrapping). Draw k of `Rng::new(s)` runs on
/// state `s + k × 0x6D2B_79F5`, so a drill's stream would reach this one only at its 2³¹st draw; a
/// dig reads at most 16.
pub const DEMO_AMBUSH_SEED_OFFSET: u32 = 0x8000_0000;
/// The mission an ambush is fought as (§4d: `GUARD_DUTY`'s numbers under ambush copy).
pub const DEMO_AMBUSH_MISSION: &str = "GUARD_DUTY";
/// The merc base (BP-10a): the four buildings' effects and costs in banked ore. T1 names from
/// `Base_Infrastructure_Tech_Tree`; the numbers are demo-only, never canon.
/// The Radio Shack's raid chance per drill, in place of `DEMO_RAID_PCT_PER_DRILL`.
pub const DEMO_SHACK_RAID_PCT_PER_DRILL: u32 = 3;
/// The Hobbyist Benches' heat per drill, in place of `DEMO_HEAT_PER_DRILL`.
pub const DEMO_BENCHES_HEAT_PER_DRILL: u32 = 10;
/// The Gunsmith's ore per drill: `× NUM / DEN` (floor, per drill), after the tier multiplier.
pub const DEMO_GUNSMITH_ORE_NUM: u32 = 5;
pub const DEMO_GUNSMITH_ORE_DEN: u32 = 4;
/// The Grease-Pit divides the haul's ambush chance by this (floor), after its cap.
pub const DEMO_GREASE_PIT_AMBUSH_DIV: u32 = 2;
pub const DEMO_COST_RADIO_SHACK: u32 = 50;
pub const DEMO_COST_GUNSMITH: u32 = 80;
pub const DEMO_COST_GREASE_PIT: u32 = 80;
pub const DEMO_COST_HOBBYIST_BENCHES: u32 = 120;

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

/// Which way the team is travelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpLeg {
    Outbound,
    Homeward,
}

/// Why a dig's team is heading home (BP-8b-1a; BP-8b-1b adds a raid's retreat, BP-8b-2 the pull).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpEnd {
    /// The heat reached the threshold: the ore on site is lost and the Crew lost heads.
    Meltdown,
    /// `ops/return` from a dig: the ore on site is left behind.
    WalkedAway,
    /// The raiders won (Defeat or TacticalRetreat, BP-8b-1b): half the ore on site rides home
    /// (`carried`), the rest is lost. A lost ambush on the haul home reads the same (BP-8b-2).
    Retreat,
    /// `ops/pull` (BP-8b-2): the ore on site rides home (`carried`) to be banked.
    Pulled,
}

/// What `ops/start` reads for a dig: the Crew that digs, the Commander who fights its raids
/// (BP-8b-1b) and the meltdown threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigStart {
    pub crew_id: Uuid,
    pub commander_id: Uuid,
    pub threshold: u32,
}

/// Which battle a trigger waits for: raiders at the site (BP-8b-1b) or an ambush on the haul
/// home (BP-8b-2). Only one is ever pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BattleKind {
    Raid,
    Ambush,
}

/// A battle waiting for its fight: when it began, the mission it is fought as, and its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaidTrigger {
    pub trigger_at: DateTime<Utc>,
    pub mission_id: &'static str,
    pub kind: BattleKind,
}

/// What a route needs to latch a raid: the Commander whose deployments to read, and the trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingRaid {
    pub commander_id: Uuid,
    pub trigger: RaidTrigger,
}

/// A fought raid: Success holds the site, Defeat or TacticalRetreat is a retreat, Wipeout ends the op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaidResult {
    Held,
    Retreat,
    Wiped,
}

/// A dig in progress, private. `tier` comes from the site; the rest from `DigStart` and the drills.
struct DigState {
    tier: u8,
    threshold: u32,
    crew_id: Uuid,
    commander_id: Uuid,
    drills: u32,
    heat: u32,
    ore_on_site: u32,
    ended_by: Option<OpEnd>,
    /// Set by a drill that brought raiders, or by the haul's ambush when it fires (BP-8b-2);
    /// cleared by `latch_raid`.
    raid: Option<RaidTrigger>,
    /// The ore riding home: a pull's whole load, or half the pile after a lost fight. The
    /// homeward arrival banks it.
    carried: u32,
    /// When the haul's ambush fires (the leg's midpoint), set by a pull that rolled one; never
    /// sent. Cleared when its fight is latched (BP-8b-2).
    ambush_at: Option<DateTime<Utc>>,
}

/// A pending raid as the client sees it: the mission to open, never the trigger or the Commander.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RaidView {
    pub mission_id: &'static str,
}

/// The dig as the client sees it: never the seed, the Crew's id, the Commander or the trigger.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DigView {
    pub drills: u32,
    pub heat: u32,
    pub threshold: u32,
    pub ore_on_site: u32,
    /// The ore the next drill would add; 0 once the team is heading home.
    pub next_ore: u32,
    pub ended_by: Option<OpEnd>,
    /// The next drill's raid chance in percent; `null` once heading home, while a raid is
    /// pending, or when the next drill melts down (a meltdown rolls no raid).
    pub next_raid_pct: Option<u32>,
    /// The raid waiting for its fight, or `null`.
    pub raid: Option<RaidView>,
    /// The ambush waiting for its fight on the way home, or `null`; `null` until it fires, so
    /// an ambushed haul and a clear one read the same until then (BP-8b-2).
    pub ambush: Option<RaidView>,
    /// The ore riding home (a pull's load, or half after a lost fight); 0 otherwise.
    pub carried: u32,
}

/// The meltdown threshold for a dig: `DEMO_HEAT_THRESHOLD`, or ×1.15 with a Coolant-Jockey Foreman.
pub fn dig_threshold(coolant_jockey: bool) -> u32 {
    if coolant_jockey {
        DEMO_HEAT_THRESHOLD * DEMO_COOLANT_THRESHOLD_PCT / 100
    } else {
        DEMO_HEAT_THRESHOLD
    }
}

/// The ore drill `n` (1-based) yields on a vein of `tier`.
fn ore_for(n: u32, tier: u8) -> u32 {
    let mult = if tier == 3 { DEMO_TIER3_ORE_MULTIPLIER } else { 1 };
    DEMO_ORE_PER_DRILL * n * mult
}

/// Drill `n`'s rolls (1-based), `(raid d100, meltdown heads)`, a function of `(op_seed, n)`
/// alone: `Rng::new(op_seed)` skips `DEMO_ROLLS_PER_DRILL × (n − 1)` draws, then draws both.
/// So reads between drills move nothing. The d100 is the raid's roll (BP-8b-1b).
fn drill_roll(op_seed: u32, n: u32) -> (u32, u32) {
    let mut rng = Rng::new(op_seed);
    for _ in 0..DEMO_ROLLS_PER_DRILL * n.saturating_sub(1) {
        rng.next_u32();
    }
    let raid_d100 = rng.roll_d100();
    let heads = rng.roll_int(DEMO_MELTDOWN_HEADS_MIN, DEMO_MELTDOWN_HEADS_MAX);
    (raid_d100, heads)
}

/// The haul's ambush chance in percent for a load of `load` ore (§4d).
fn ambush_pct(load: u32) -> u32 {
    (DEMO_AMBUSH_BASE_PCT + DEMO_AMBUSH_PCT_PER_50 * (load / 50)).min(DEMO_AMBUSH_CAP_PCT)
}

/// The haul's ambush d100: the first draw of `Rng::new(op_seed + DEMO_AMBUSH_SEED_OFFSET)`, a
/// function of the op's seed alone, on a stream no drill reads (so no drill's roll moves).
fn ambush_d100(op_seed: u32) -> u32 {
    Rng::new(op_seed.wrapping_add(DEMO_AMBUSH_SEED_OFFSET)).roll_d100()
}

/// A merc-base building (BP-10a): the body's `building` id, its cost and its flag on `OpsBase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseBuilding {
    RadioShack,
    HobbyistBenches,
    GreasePit,
    Gunsmith,
}

impl BaseBuilding {
    /// The building a body's `building` id names; `None` for any other string.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "radio_shack" => Some(Self::RadioShack),
            "hobbyist_benches" => Some(Self::HobbyistBenches),
            "grease_pit" => Some(Self::GreasePit),
            "gunsmith" => Some(Self::Gunsmith),
            _ => None,
        }
    }

    /// Its cost in banked ore.
    pub fn cost(self) -> u32 {
        match self {
            Self::RadioShack => DEMO_COST_RADIO_SHACK,
            Self::HobbyistBenches => DEMO_COST_HOBBYIST_BENCHES,
            Self::GreasePit => DEMO_COST_GREASE_PIT,
            Self::Gunsmith => DEMO_COST_GUNSMITH,
        }
    }
}

/// The campaign's merc base (BP-10a): which buildings stand, serialised as the body's `base`.
/// **The one reader of the dig's base numbers:** the drill, the view and the pull call these
/// methods and never the plain constants, so what the panel shows is what the roll uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OpsBase {
    pub radio_shack: bool,
    pub hobbyist_benches: bool,
    pub grease_pit: bool,
    pub gunsmith: bool,
}

impl OpsBase {
    /// Heat each drill adds: the Hobbyist Benches' 10, else §4d's 15.
    fn heat_per_drill(&self) -> u32 {
        if self.hobbyist_benches {
            DEMO_BENCHES_HEAT_PER_DRILL
        } else {
            DEMO_HEAT_PER_DRILL
        }
    }

    /// Drill `n`'s raid chance in percent: 3 × n with the Radio Shack, else §4d's 5 × n.
    fn raid_pct(&self, n: u32) -> u32 {
        let per = if self.radio_shack {
            DEMO_SHACK_RAID_PCT_PER_DRILL
        } else {
            DEMO_RAID_PCT_PER_DRILL
        };
        per * n
    }

    /// The ore drill `n` yields on a vein of `tier`: × 5 / 4 (floor) with the Gunsmith.
    fn ore(&self, n: u32, tier: u8) -> u32 {
        let plain = ore_for(n, tier);
        if self.gunsmith {
            plain * DEMO_GUNSMITH_ORE_NUM / DEMO_GUNSMITH_ORE_DEN
        } else {
            plain
        }
    }

    /// The haul's ambush chance for a load of `load` ore: halved (floor) with the Grease-Pit.
    fn haul_ambush_pct(&self, load: u32) -> u32 {
        let capped = ambush_pct(load);
        if self.grease_pit {
            capped / DEMO_GREASE_PIT_AMBUSH_DIV
        } else {
            capped
        }
    }

    fn has(&self, building: BaseBuilding) -> bool {
        match building {
            BaseBuilding::RadioShack => self.radio_shack,
            BaseBuilding::HobbyistBenches => self.hobbyist_benches,
            BaseBuilding::GreasePit => self.grease_pit,
            BaseBuilding::Gunsmith => self.gunsmith,
        }
    }

    fn set(&mut self, building: BaseBuilding) {
        match building {
            BaseBuilding::RadioShack => self.radio_shack = true,
            BaseBuilding::HobbyistBenches => self.hobbyist_benches = true,
            BaseBuilding::GreasePit => self.grease_pit = true,
            BaseBuilding::Gunsmith => self.gunsmith = true,
        }
    }
}

/// The raid's fight among the Commander's deployments: the earliest one that is not `aborted`,
/// is the trigger's mission and started strictly after the trigger (`started_at` is the app's
/// clock, as `trigger_at` is). `None` while there is none or it is still `active` (a later
/// record is never picked past it). Once `resolved`: Success → `Held`, Defeat or
/// TacticalRetreat → `Retreat`, Wipeout → `Wiped`. Any other outcome on a resolved record is
/// impossible by the schema's checks, so it is an error, never a raid left pending in silence.
pub fn raid_result(deps: &[DeploymentRecord], trigger: &RaidTrigger) -> Result<Option<RaidResult>, String> {
    let fight = deps
        .iter()
        .filter(|d| d.status != "aborted" && d.mission_id == trigger.mission_id && d.started_at > trigger.trigger_at)
        .min_by_key(|d| d.started_at);
    let Some(d) = fight else {
        return Ok(None);
    };
    if d.status != "resolved" {
        return Ok(None);
    }
    match d.outcome.as_deref() {
        Some("Success") => Ok(Some(RaidResult::Held)),
        Some("Defeat") | Some("TacticalRetreat") => Ok(Some(RaidResult::Retreat)),
        Some("Wipeout") => Ok(Some(RaidResult::Wiped)),
        other => Err(format!("deployment {} is resolved with outcome {other:?}", d.deployment_id)),
    }
}

/// The op in flight, private: never serialised (`ActiveOpView` is what the client sees).
/// BP-8b-1a adds the dig; BP-8b-1b the raid trigger; BP-8b-2 the haul and the ambush.
struct ActiveOp {
    site_id: String,
    kind: &'static str,
    leg: OpLeg,
    depart_at: DateTime<Utc>,
    arrive_at: DateTime<Utc>,
    /// The site's one-way time; both legs take it, so the trip is symmetric (§4d: "each way").
    travel_secs: u32,
    /// Drawn by `generate_seed()` at `ops/start`, never derived from the campaign id and never
    /// sent or logged. `drill_op` rolls from it (`drill_roll`), and `pull_op` (`ambush_d100`).
    op_seed: u32,
    /// The dig; `None` on a contract.
    dig: Option<DigState>,
}

/// The op as the client sees it. Times are RFC 3339 with milliseconds; `arrived` is
/// `now >= arrive_at` at the read, and `false` while a haul's ambush is still to be fought.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ActiveOpView {
    pub leg: OpLeg,
    pub site_id: String,
    pub kind: &'static str,
    pub depart_at: String,
    pub arrive_at: String,
    pub arrived: bool,
    /// The dig's numbers; absent on a contract, so a contract op keeps its six keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dig: Option<DigView>,
}

/// The wire format of every time the ops routes send.
pub fn ops_time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Why `start_op` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartRefusal {
    /// An op is still out (an arrived homeward leg is cleared first, so this one is not home).
    Busy,
    /// The campaign has no entry and every entry at the cap holds an op, banked ore or a building.
    Full,
}

/// Why `drill_op` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrillRefusal {
    /// No op, or its homeward leg has arrived and been cleared.
    NoOp,
    /// The op is a contract.
    Contract,
    /// The team is on its way home (after a meltdown or a return).
    AlreadyHomeward,
    /// The outbound leg has not arrived yet.
    NotArrived,
    /// Raiders are at the site: the raid must be fought first (BP-8b-1b).
    RaidPending,
}

/// What a drill did. A meltdown names the Crew and the heads it loses, for the route to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrillOutcome {
    Drilled,
    Meltdown { crew_id: Uuid, heads: i16 },
}

/// Why `return_op` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnRefusal {
    /// No op, or its homeward leg has arrived and been cleared.
    NoOp,
    /// The outbound leg has not arrived yet.
    NotArrived,
    /// The team is already on its way home.
    AlreadyHomeward,
    /// Raiders are at the site: the raid must be fought first (BP-8b-1b).
    RaidPending,
}

/// Why `pull_op` refused (BP-8b-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRefusal {
    /// No op, or its homeward leg has arrived and been cleared.
    NoOp,
    /// The op is a contract.
    Contract,
    /// The team is already on its way home.
    AlreadyHomeward,
    /// The outbound leg has not arrived yet.
    NotArrived,
    /// No ore on site: a return brings the team home.
    NothingToHaul,
    /// Raiders are at the site: the raid must be fought first.
    RaidPending,
}

/// Why `build_op` refused (BP-10a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildRefusal {
    /// An op is out (a dig, a contract, or a haul still on the road): the base is built from home.
    OpOut,
    /// The building already stands.
    AlreadyBuilt,
    /// The banked ore is short of the building's cost (also: the campaign has no entry).
    NotEnoughOre,
}

/// One campaign's ops state: the cached board, the op in flight, when the team last moved, the
/// banked ore and the merc base.
struct OpsEntry {
    /// The gateway the board was computed for; another gateway is a miss.
    gateway: (i32, i32),
    board: Arc<OpsBoard>,
    touched: DateTime<Utc>,
    op: Option<ActiveOp>,
    /// The latest departure, drill or homeward arrival. Outlives the op, so a player who has just
    /// come home still counts as moving for the reaper.
    moved_at: Option<DateTime<Utc>>,
    /// The ore this campaign has banked (BP-8b-2): added at each homeward arrival, debited by a
    /// build (BP-10a). It leaves only with the entry, so a New run (a new campaign) starts at 0.
    ore_banked: u32,
    /// The buildings that stand (BP-10a). They leave only with the entry, as the banked ore does.
    base: OpsBase,
}

impl OpsEntry {
    /// First fires a haul's ambush once its time has come (BP-8b-2): the trigger is set at
    /// `now`, the reveal, so no fight started before the player could know of it is latched, and
    /// the team waits on the road. Then clears a homeward leg that has arrived with no battle
    /// waiting and none to come, banking what it carried and recording the arrival (not the
    /// read) as the last movement. Every read and write of the op calls it first, under the lock.
    fn settle_arrival(&mut self, now: DateTime<Utc>) {
        let Some(op) = self.op.as_mut() else {
            return;
        };
        if let Some(d) = op.dig.as_mut() {
            if d.raid.is_none() && d.ambush_at.is_some_and(|t| now >= t) {
                d.raid = Some(RaidTrigger { trigger_at: now, mission_id: DEMO_AMBUSH_MISSION, kind: BattleKind::Ambush });
            }
        }
        let waiting = op.dig.as_ref().is_some_and(|d| d.raid.is_some() || d.ambush_at.is_some());
        if op.leg == OpLeg::Homeward && now >= op.arrive_at && !waiting {
            let (arrived_at, carried) = (op.arrive_at, op.dig.as_ref().map_or(0, |d| d.carried));
            self.ore_banked = self.ore_banked.saturating_add(carried);
            self.moved_at = Some(arrived_at);
            self.op = None;
        }
    }

    fn view(&self, now: DateTime<Utc>) -> Option<ActiveOpView> {
        self.op.as_ref().map(|op| ActiveOpView {
            leg: op.leg,
            site_id: op.site_id.clone(),
            kind: op.kind,
            depart_at: ops_time(op.depart_at),
            arrive_at: ops_time(op.arrive_at),
            arrived: now >= op.arrive_at && op.dig.as_ref().is_none_or(|d| d.ambush_at.is_none()),
            dig: op.dig.as_ref().map(|d| DigView {
                drills: d.drills,
                heat: d.heat,
                threshold: d.threshold,
                ore_on_site: d.ore_on_site,
                next_ore: if op.leg == OpLeg::Homeward { 0 } else { self.base.ore(d.drills + 1, d.tier) },
                ended_by: d.ended_by,
                next_raid_pct: if op.leg == OpLeg::Outbound && d.raid.is_none() && d.heat + self.base.heat_per_drill() < d.threshold {
                    Some(self.base.raid_pct(d.drills + 1))
                } else {
                    None
                },
                raid: d.raid.filter(|r| r.kind == BattleKind::Raid).map(|r| RaidView { mission_id: r.mission_id }),
                ambush: d.raid.filter(|r| r.kind == BattleKind::Ambush).map(|r| RaidView { mission_id: r.mission_id }),
                carried: d.carried,
            }),
        })
    }
}

/// Every campaign's ops state, in memory, bounded by `cap`.
///
/// **Writes later bricks must keep:**
/// - the board write (`put_board`) updates `gateway`, `board` and `touched` in place and never
///   touches the op, `moved_at`, the banked ore or the base;
/// - only `start_op`, `return_op`, `drill_op`, `pull_op`, `latch_raid` and `settle_arrival` (the
///   ambush's reveal and the arrival, run first by every op read and write) write the op. The
///   arrival adds to the banked ore and `build_op` debits it; only `build_op` writes the base,
///   and never while an op is out. Each takes the lock, calls `settle_arrival` first and returns
///   owned data; none is held across an `.await`;
/// - **the merc base (BP-10a):** the drill's heat, raid chance and ore, the view's `next_ore`,
///   melt check and `next_raid_pct`, and the pull's ambush chance all come from `OpsBase`'s
///   methods, never the plain constants;
/// - **the raid (BP-8b-1b):** `drill_op` writes the trigger under the lock from the drill's
///   first roll, after the drill's heat and ore and only if the drill did not melt down. While
///   a trigger is set, every ops route latches it in three steps: `pending_raid` (under the
///   lock) returns the Commander and the trigger; the route reads that Commander's deployments
///   with no lock held; `latch_raid` (under the lock again) applies the outcome only if the
///   stored trigger is still the one it read, so a stale read never applies an old fight to a
///   newer raid. `drill`, `return` and `pull` refuse while a raid is set (`RaidPending`);
/// - **the haul (BP-8b-2):** `pull_op` loads the ore on site into `carried`, rolls the ambush
///   once (`ambush_d100`, a stream no drill reads) and stores only its fire time (`ambush_at`,
///   the leg's midpoint), never sent. `settle_arrival` fires it first, at the first read at or
///   after `ambush_at`: the trigger (`BattleKind::Ambush`, `trigger_at` = that read's `now`) is
///   latched in the raid's three steps, and the leg waits on the road until then. Then it banks:
///   a homeward leg that has arrived with no trigger and no ambush to come adds `carried` to
///   `ore_banked`, before the op clears. `latch_raid` applies an ambush's fight like a raid's
///   (compare-and-set, `moved_at`): Held resumes the rest of the leg with the whole load,
///   Defeat or TacticalRetreat with half (floor), Wipeout clears the op and banks nothing;
/// - **the keep list** (the reaper's `moved_since`): a departure (start, return, pull), a
///   homeward arrival, a drill, a latch and a build each write `moved_at`. Reading the board or the op
///   does not, nor does the ambush's reveal; a read that latches a battle does. While an
///   ambush waits, `moved_since` still counts the leg's passed `arrive_at`, at most
///   `travel_secs` (≤ 48 s) after the pull: an accepted overcount;
/// - the eviction skips any entry holding an op, banked ore or a building;
/// - an entry is dropped only by `remove` (a read of a campaign that is no longer `Active`,
///   the reaper's retire) and the reaper's prune (`retain_campaigns`).
///
/// **Keyed per campaign with one gateway and one op: correct only while `DEMO_PLAYER_CAP = 1`**
/// (`routes/campaign.rs`). A second member per campaign needs a per-wallet key.
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

    /// Makes room for a new entry: below the cap, nothing; at it, evicts the least recently
    /// touched entry that holds no op, no banked ore and no building, and journals (first, then
    /// at most hourly). `false` when every entry holds an op, banked ore or a building.
    fn make_room(&self, inner: &mut HashMap<Uuid, OpsEntry>) -> bool {
        if inner.len() < self.cap {
            return true;
        }
        let victim = inner
            .iter()
            .filter(|(_, e)| e.op.is_none() && e.ore_banked == 0 && e.base == OpsBase::default())
            .min_by_key(|(_, e)| e.touched)
            .map(|(id, _)| *id);
        let Some(id) = victim else {
            return false;
        };
        inner.remove(&id);
        if let Some(n) = self.evict_warn.hit(Instant::now()) {
            tracing::warn!(evicted = n, cap = self.cap, "ops boards at their cap: evicting the least recently read");
        }
        true
    }

    /// The cached board for `campaign_id` at `gateway`, touched; `None` on a miss.
    pub fn board(&self, campaign_id: Uuid, gateway: (i32, i32), now: DateTime<Utc>) -> Option<Arc<OpsBoard>> {
        let mut inner = self.lock();
        let entry = inner.get_mut(&campaign_id).filter(|e| e.gateway == gateway)?;
        entry.touched = now;
        Some(Arc::clone(&entry.board))
    }

    /// Caches `board`. An existing entry gets the new `gateway`, `board` and `touched` in
    /// place, keeping its op. A new entry at the cap first evicts (`make_room`). `false`: not
    /// cached, because every entry holds an op, banked ore or a building (the caller serves the
    /// board uncached).
    pub fn put_board(&self, campaign_id: Uuid, gateway: (i32, i32), board: Arc<OpsBoard>, now: DateTime<Utc>) -> bool {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            e.gateway = gateway;
            e.board = board;
            e.touched = now;
            return true;
        }
        if !self.make_room(&mut inner) {
            return false;
        }
        inner.insert(campaign_id, OpsEntry { gateway, board, touched: now, op: None, moved_at: None, ore_banked: 0, base: OpsBase::default() });
        true
    }

    /// Starts the outbound leg to `site`: `depart_at = now`, `arrive_at = now + travel_secs`.
    /// One op per campaign (`Busy`); a campaign with no entry is cached with `board` first
    /// (`Full` when every entry at the cap holds an op). A dig passes `dig` (its tier comes
    /// from the site); a contract passes `None`.
    pub fn start_op(
        &self,
        campaign_id: Uuid,
        gateway: (i32, i32),
        board: Arc<OpsBoard>,
        site: &OpsSite,
        op_seed: u32,
        dig: Option<DigStart>,
        now: DateTime<Utc>,
    ) -> Result<(), StartRefusal> {
        let mut inner = self.lock();
        if !inner.contains_key(&campaign_id) {
            if !self.make_room(&mut inner) {
                return Err(StartRefusal::Full);
            }
            inner.insert(campaign_id, OpsEntry { gateway, board, touched: now, op: None, moved_at: None, ore_banked: 0, base: OpsBase::default() });
        }
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(StartRefusal::Full);
        };
        e.settle_arrival(now);
        if e.op.is_some() {
            return Err(StartRefusal::Busy);
        }
        e.op = Some(ActiveOp {
            site_id: site.site_id.clone(),
            kind: site.kind,
            leg: OpLeg::Outbound,
            depart_at: now,
            arrive_at: now + Duration::seconds(i64::from(site.travel_secs)),
            travel_secs: site.travel_secs,
            op_seed,
            dig: dig.map(|d| DigState {
                tier: site.tier.unwrap_or(1),
                threshold: d.threshold,
                crew_id: d.crew_id,
                commander_id: d.commander_id,
                drills: 0,
                heat: 0,
                ore_on_site: 0,
                ended_by: None,
                raid: None,
                carried: 0,
                ambush_at: None,
            }),
        });
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// Starts the homeward leg once the outbound one has arrived, taking the site's time again.
    /// From a dig the team walks away: the ore on site is left (`OpEnd::WalkedAway`, F6);
    /// BP-8b-2's pull is the loaded return. Refusals in order: `NoOp` → `AlreadyHomeward` →
    /// `NotArrived` → `RaidPending` (raiders can't be walked away from).
    pub fn return_op(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Result<(), ReturnRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(ReturnRefusal::NoOp);
        };
        e.settle_arrival(now);
        let Some(op) = e.op.as_mut() else {
            return Err(ReturnRefusal::NoOp);
        };
        if op.leg == OpLeg::Homeward {
            return Err(ReturnRefusal::AlreadyHomeward);
        }
        if now < op.arrive_at {
            return Err(ReturnRefusal::NotArrived);
        }
        if op.dig.as_ref().is_some_and(|d| d.raid.is_some()) {
            return Err(ReturnRefusal::RaidPending);
        }
        if let Some(d) = op.dig.as_mut() {
            d.ore_on_site = 0;
            d.ended_by = Some(OpEnd::WalkedAway);
        }
        op.leg = OpLeg::Homeward;
        op.depart_at = now;
        op.arrive_at = now + Duration::seconds(i64::from(op.travel_secs));
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// One drill on an arrived dig: drill `n` adds the base's heat (`OpsBase::heat_per_drill`); at
    /// the threshold the site melts down (the ore on site is lost, the homeward leg starts at once
    /// and the Crew's heads are returned for the route to write), otherwise it adds the base's ore
    /// (`OpsBase::ore`) and, when the first roll is at most the base's raid chance
    /// (`OpsBase::raid_pct`), sets the raid trigger (a meltdown rolls no raid). Writes `moved_at` (the keep list). Refusals in order: `NoOp`
    /// → `Contract` → `AlreadyHomeward` → `NotArrived` → `RaidPending`. The meltdown is committed
    /// here, before the route's write, so a second drill sees `AlreadyHomeward` and the heads
    /// can't be lost twice.
    pub fn drill_op(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Result<DrillOutcome, DrillRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(DrillRefusal::NoOp);
        };
        e.settle_arrival(now);
        let base = e.base;
        let Some(op) = e.op.as_mut() else {
            return Err(DrillRefusal::NoOp);
        };
        let Some(dig) = op.dig.as_mut() else {
            return Err(DrillRefusal::Contract);
        };
        if op.leg == OpLeg::Homeward {
            return Err(DrillRefusal::AlreadyHomeward);
        }
        if now < op.arrive_at {
            return Err(DrillRefusal::NotArrived);
        }
        if dig.raid.is_some() {
            return Err(DrillRefusal::RaidPending);
        }
        dig.drills += 1;
        let (raid_d100, heads) = drill_roll(op.op_seed, dig.drills);
        dig.heat += base.heat_per_drill();
        e.moved_at = Some(now);
        e.touched = now;
        let melts = dig.heat >= dig.threshold;
        if !melts && raid_d100 <= base.raid_pct(dig.drills) {
            dig.raid = Some(RaidTrigger { trigger_at: now, mission_id: DEMO_RAID_MISSION, kind: BattleKind::Raid });
        }
        if melts {
            dig.ore_on_site = 0;
            dig.ended_by = Some(OpEnd::Meltdown);
            op.leg = OpLeg::Homeward;
            op.depart_at = now;
            op.arrive_at = now + Duration::seconds(i64::from(op.travel_secs));
            return Ok(DrillOutcome::Meltdown { crew_id: dig.crew_id, heads: heads as i16 });
        }
        dig.ore_on_site += base.ore(dig.drills, dig.tier);
        Ok(DrillOutcome::Drilled)
    }

    /// Hauls the ore home from an arrived dig (BP-8b-2): the ore on site rides home as `carried`
    /// (`OpEnd::Pulled`) and the homeward leg starts at `now` with the site's time. The ambush is
    /// rolled once, here: on `ambush_d100 ≤` the base's chance (`OpsBase::haul_ambush_pct`, so a
    /// Grease-Pit counts only if it stood before the pull) only its fire time is stored, the
    /// leg's midpoint in milliseconds (an odd `travel_secs` is common), and nothing sends it.
    /// Writes `moved_at` (the keep list). Refusals in order: `NoOp` → `Contract` →
    /// `AlreadyHomeward` → `NotArrived` → `NothingToHaul` → `RaidPending`.
    pub fn pull_op(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Result<(), PullRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(PullRefusal::NoOp);
        };
        e.settle_arrival(now);
        let base = e.base;
        let Some(op) = e.op.as_mut() else {
            return Err(PullRefusal::NoOp);
        };
        let Some(dig) = op.dig.as_mut() else {
            return Err(PullRefusal::Contract);
        };
        if op.leg == OpLeg::Homeward {
            return Err(PullRefusal::AlreadyHomeward);
        }
        if now < op.arrive_at {
            return Err(PullRefusal::NotArrived);
        }
        if dig.ore_on_site == 0 {
            return Err(PullRefusal::NothingToHaul);
        }
        if dig.raid.is_some() {
            return Err(PullRefusal::RaidPending);
        }
        let load = dig.ore_on_site;
        dig.carried = load;
        dig.ore_on_site = 0;
        dig.ended_by = Some(OpEnd::Pulled);
        op.leg = OpLeg::Homeward;
        op.depart_at = now;
        op.arrive_at = now + Duration::seconds(i64::from(op.travel_secs));
        if ambush_d100(op.op_seed) <= base.haul_ambush_pct(load) {
            dig.ambush_at = Some(now + Duration::milliseconds(i64::from(op.travel_secs) * 500));
        }
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// The battle waiting for its fight (a raid, or a haul's ambush once fired), after settling
    /// the op: the dig's Commander and the trigger. `None` when none is pending.
    pub fn pending_raid(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Option<PendingRaid> {
        let mut inner = self.lock();
        let e = inner.get_mut(&campaign_id)?;
        e.settle_arrival(now);
        let dig = e.op.as_ref()?.dig.as_ref()?;
        dig.raid.map(|trigger| PendingRaid { commander_id: dig.commander_id, trigger })
    }

    /// Applies a fought battle, only if `trigger` is still the dig's pending one (`false`, and no
    /// write, otherwise). A raid: `Held`, the trigger clears and drilling resumes; `Retreat`, half
    /// the ore on site (floor) is `carried`, the rest is lost, and the homeward leg starts at
    /// `now` (`OpEnd::Retreat`). An ambush (BP-8b-2): `Held` resumes the leg, shifted by the
    /// wait so the team sets off from the midpoint, with the whole load; `Retreat` the same with
    /// half of it (floor, `OpEnd::Retreat`). `Wiped`, either kind: the op is cleared and its ore
    /// lost (the mission route has already marked the Commander KIA). Each writes `moved_at`
    /// (the keep list).
    pub fn latch_raid(&self, campaign_id: Uuid, trigger: &RaidTrigger, result: RaidResult, now: DateTime<Utc>) -> bool {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return false;
        };
        e.settle_arrival(now);
        let Some(op) = e.op.as_mut() else {
            return false;
        };
        let Some(dig) = op.dig.as_mut() else {
            return false;
        };
        if dig.raid != Some(*trigger) {
            return false;
        }
        dig.raid = None;
        match (trigger.kind, result) {
            (BattleKind::Raid, RaidResult::Held) => {}
            (BattleKind::Raid, RaidResult::Retreat) => {
                dig.carried = dig.ore_on_site / 2;
                dig.ore_on_site = 0;
                dig.ended_by = Some(OpEnd::Retreat);
                op.leg = OpLeg::Homeward;
                op.depart_at = now;
                op.arrive_at = now + Duration::seconds(i64::from(op.travel_secs));
            }
            (BattleKind::Ambush, RaidResult::Held | RaidResult::Retreat) => {
                if result == RaidResult::Retreat {
                    dig.carried /= 2;
                    dig.ended_by = Some(OpEnd::Retreat);
                }
                let wait = now - dig.ambush_at.take().unwrap_or(now);
                op.depart_at += wait;
                op.arrive_at += wait;
            }
            (_, RaidResult::Wiped) => e.op = None,
        }
        e.moved_at = Some(now);
        e.touched = now;
        true
    }

    /// The op as the client sees it, after clearing an arrived homeward leg.
    pub fn op_view(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Option<ActiveOpView> {
        let mut inner = self.lock();
        let e = inner.get_mut(&campaign_id)?;
        e.settle_arrival(now);
        e.view(now)
    }

    /// The op as the client sees it, the campaign's banked ore (BP-8b-2) and its base (BP-10a),
    /// from one lock. A campaign with no entry has banked and built nothing.
    pub fn ops_snapshot(&self, campaign_id: Uuid, now: DateTime<Utc>) -> (Option<ActiveOpView>, u32, OpsBase) {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return (None, 0, OpsBase::default());
        };
        e.settle_arrival(now);
        (e.view(now), e.ore_banked, e.base)
    }

    /// Builds `building` at the campaign's merc base (BP-10a): its cost is debited from the banked
    /// ore. Settles first, so a haul that has just come home banks before the check and can pay on
    /// the same call. Never while an op is out, so no base number changes under a live op and a
    /// haul's ambush stays as the pull rolled it. Writes `moved_at` (the keep list). Refusals in
    /// order: `OpOut` → `AlreadyBuilt` → `NotEnoughOre` (also for a campaign with no entry, which
    /// has banked nothing).
    pub fn build_op(&self, campaign_id: Uuid, building: BaseBuilding, now: DateTime<Utc>) -> Result<(), BuildRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(BuildRefusal::NotEnoughOre);
        };
        e.settle_arrival(now);
        if e.op.is_some() {
            return Err(BuildRefusal::OpOut);
        }
        if e.base.has(building) {
            return Err(BuildRefusal::AlreadyBuilt);
        }
        let cost = building.cost();
        if e.ore_banked < cost {
            return Err(BuildRefusal::NotEnoughOre);
        }
        e.ore_banked -= cost;
        e.base.set(building);
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// The reaper's keep check: the team departed, drilled, latched a battle or came home at or after `cutoff` (a leg
    /// that has arrived by `now` counts at its arrival, also while a haul's ambush waits: an
    /// accepted overcount of at most `travel_secs`). Reading the board is not moving.
    pub fn moved_since(&self, campaign_id: Uuid, cutoff: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        let inner = self.lock();
        let Some(e) = inner.get(&campaign_id) else {
            return false;
        };
        let arrived = e.op.as_ref().map(|op| op.arrive_at).filter(|t| *t <= now);
        e.moved_at.max(arrived).is_some_and(|t| t >= cutoff)
    }

    /// The reaper's prune: keeps the entries of `active` campaigns, and any entry touched at or
    /// after `listed_at` (a campaign launched after the list was read).
    pub fn retain_campaigns(&self, active: &HashSet<Uuid>, listed_at: DateTime<Utc>) {
        self.lock().retain(|id, e| active.contains(id) || e.touched >= listed_at);
    }

    /// Drops the campaign's entry (a read of a campaign that is no longer `Active`, the reaper's
    /// retire).
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

    /// Moves the campaign's op (its leg and a haul's hidden ambush time) and its last movement
    /// `secs` into the past, so a route test crosses an arrival without sleeping. A trigger's
    /// time stays: a fight's `started_at` is the real clock.
    #[cfg(test)]
    pub(crate) fn rewind_op(&self, campaign_id: Uuid, secs: i64) {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            let back = Duration::seconds(secs);
            if let Some(op) = e.op.as_mut() {
                op.depart_at -= back;
                op.arrive_at -= back;
                if let Some(d) = op.dig.as_mut() {
                    d.ambush_at = d.ambush_at.map(|t| t - back);
                }
            }
            e.moved_at = e.moved_at.map(|t| t - back);
        }
    }

    /// Stands `base` at the campaign's entry without spending ore or touching it, so a test sets
    /// the buildings up without banking for them first.
    #[cfg(test)]
    pub(crate) fn set_base(&self, campaign_id: Uuid, base: OpsBase) {
        if let Some(e) = self.lock().get_mut(&campaign_id) {
            e.base = base;
        }
    }

    /// Fixes the op's seed, so a test chooses its rolls instead of hoping for them.
    #[cfg(test)]
    pub(crate) fn set_op_seed(&self, campaign_id: Uuid, seed: u32) {
        if let Some(op) = self.lock().get_mut(&campaign_id).and_then(|e| e.op.as_mut()) {
            op.op_seed = seed;
        }
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

    /// A contract site 10 s out.
    fn site() -> OpsSite {
        OpsSite {
            site_id: "contract:GUARD_DUTY".into(),
            kind: "contract",
            q: 8,
            r: 0,
            distance: 3,
            travel_secs: 10,
            tier: None,
            mission_id: Some("GUARD_DUTY"),
            name: None,
        }
    }

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-15T10:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn at(secs: i64) -> DateTime<Utc> {
        t0() + Duration::seconds(secs)
    }

    #[test]
    fn ops_store_at_its_cap_evicts_the_least_recently_read() {
        let store = DemoOpsStore::new(2);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        assert!(store.put_board(b, (5, 0), board(), at(1)));
        assert!(store.board(a, (5, 0), at(2)).is_some(), "a is read last");
        assert!(store.put_board(c, (5, 0), board(), at(3)));
        assert_eq!(store.len(), 2);
        assert!(store.board(b, (5, 0), at(4)).is_none(), "b was evicted");
        assert!(store.board(a, (5, 0), at(4)).is_some());
        assert!(store.board(c, (5, 0), at(4)).is_some());
    }

    #[test]
    fn ops_store_misses_on_another_gateway_and_forgets_on_remove() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        store.put_board(a, (5, 0), board(), at(0));
        assert!(store.board(a, (0, 5), at(0)).is_none());
        store.remove(a);
        assert!(store.is_empty());
    }

    /// CF1: a board write on an entry that holds an op replaces the board and keeps the op.
    #[test]
    fn ops_store_board_write_keeps_the_op() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        assert!(store.put_board(a, (0, 5), board(), at(1)));
        assert!(store.board(a, (0, 5), at(2)).is_some(), "the new gateway's board is cached");
        let op = store.op_view(a, at(2)).expect("the op survives the board write");
        assert_eq!(op.site_id, "contract:GUARD_DUTY");
        assert_eq!(op.leg, OpLeg::Outbound);
    }

    /// Cap 2: `a` holds an op and is the least recently touched, `b` holds a board only. A new
    /// campaign evicts `b`, never `a`.
    #[test]
    fn ops_store_eviction_skips_entries_with_an_op() {
        let store = DemoOpsStore::new(2);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        store.put_board(b, (5, 0), board(), at(1));
        assert!(store.put_board(c, (5, 0), board(), at(2)));
        assert_eq!(store.len(), 2);
        assert!(store.op_view(a, at(3)).is_some(), "the op entry is kept");
        assert!(store.board(b, (5, 0), at(3)).is_none(), "the board-only entry is evicted");
    }

    /// Cap 1, held by an op: another campaign's start is refused and its board is not cached.
    #[test]
    fn ops_store_full_of_ops_refuses_a_start_and_caches_no_board() {
        let store = DemoOpsStore::new(1);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        assert!(!store.put_board(b, (5, 0), board(), at(1)), "nothing evictable");
        assert_eq!(store.start_op(b, (5, 0), board(), &site(), 8, None, at(1)), Err(StartRefusal::Full));
        assert_eq!(store.len(), 1);
        assert!(store.op_view(a, at(2)).is_some());
    }

    #[test]
    fn ops_store_one_op_per_campaign() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        assert_eq!(store.start_op(a, (5, 0), board(), &site(), 8, None, at(20)), Err(StartRefusal::Busy), "arrived outbound is still out");
        assert_eq!(store.op_view(a, at(20)).expect("op").depart_at, ops_time(at(0)), "the first op is untouched");
    }

    #[test]
    fn ops_store_return_needs_an_arrived_outbound() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(store.return_op(a, at(0)), Err(ReturnRefusal::NoOp));
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        assert_eq!(store.return_op(a, at(9)), Err(ReturnRefusal::NotArrived));
        assert_eq!(store.return_op(a, at(10)), Ok(()));
        let op = store.op_view(a, at(11)).expect("op");
        assert_eq!(op.leg, OpLeg::Homeward);
        assert_eq!((op.depart_at, op.arrive_at, op.arrived), (ops_time(at(10)), ops_time(at(20)), false));
        assert_eq!(store.return_op(a, at(12)), Err(ReturnRefusal::AlreadyHomeward));
    }

    #[test]
    fn ops_store_homeward_arrival_clears_the_op_and_keeps_moved_at() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        store.return_op(a, at(10)).expect("return");
        assert!(store.op_view(a, at(19)).is_some(), "still on the way home");
        assert!(store.op_view(a, at(500)).is_none(), "home: the op is cleared");
        assert!(store.moved_since(a, at(20), at(500)), "the arrival at 20 s is the last movement");
        assert!(!store.moved_since(a, at(21), at(500)));
        assert!(store.start_op(a, (5, 0), board(), &site(), 8, None, at(501)).is_ok(), "a new op can start");
    }

    /// A tier-`tier` dig site 10 s out.
    fn dig(tier: u8) -> OpsSite {
        OpsSite {
            site_id: format!("dig:9,{tier}"),
            kind: "dig",
            q: 9,
            r: i32::from(tier),
            distance: 4,
            travel_secs: 10,
            tier: Some(tier),
            mission_id: None,
            name: None,
        }
    }

    const CREW: Uuid = Uuid::from_u128(0xC0);
    const COMMANDER: Uuid = Uuid::from_u128(0xC1);

    /// A dig on a tier-`tier` vein, started at 0 s and on site from 10 s, its seed `seed`.
    fn on_site(store: &DemoOpsStore, a: Uuid, tier: u8, threshold: u32, seed: u32) {
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold };
        store.start_op(a, (5, 0), board(), &dig(tier), seed, Some(crew), at(0)).expect("start");
    }

    fn dig_view(store: &DemoOpsStore, a: Uuid, now: DateTime<Utc>) -> DigView {
        store.op_view(a, now).expect("op").dig.expect("a dig")
    }

    /// Drill n's rolls against an independent Mulberry32 (Linux's Python oracle, BP-8b-1 spec
    /// check): seed 2's raid d100 and meltdown heads for drills 1–8, and three single slots.
    #[test]
    fn ops_drill_roll_is_a_function_of_the_seed_and_the_drill() {
        let got: Vec<(u32, u32)> = (1..=8).map(|n| drill_roll(2, n)).collect();
        assert_eq!(got, vec![(74, 1), (29, 2), (88, 2), (50, 2), (79, 1), (79, 1), (43, 3), (59, 1)]);
        assert_eq!(drill_roll(7, 1).0, 2);
        assert_eq!(drill_roll(5, 3).0, 9);
        assert_eq!(drill_roll(8, 7).1, 1);
    }

    /// §4d: +15 heat per drill; drill n yields 10 × n on tier 1 and twice that on tier 3.
    #[test]
    fn ops_store_drill_adds_heat_and_ore_by_tier() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        on_site(&store, a, 1, 100, 2);
        on_site(&store, b, 3, 100, 2);
        let mut seen = Vec::new();
        for i in 1..=3 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled));
            assert_eq!(store.drill_op(b, at(10 + i)), Ok(DrillOutcome::Drilled));
            let (va, vb) = (dig_view(&store, a, at(20)), dig_view(&store, b, at(20)));
            seen.push((va.drills, va.heat, va.ore_on_site, va.next_ore, vb.ore_on_site, vb.next_ore));
        }
        assert_eq!(seen, vec![(1, 15, 10, 20, 20, 40), (2, 30, 30, 30, 60, 60), (3, 45, 60, 40, 120, 80)]);
    }

    /// Seed 2, threshold 100: drills 1–6 dig, the 7th (heat 105) melts down and costs 3 heads.
    #[test]
    fn ops_store_meltdown_on_the_7th_drill_at_100() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 2);
        for i in 1..=6 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(store.drill_op(a, at(20)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 3 }));
        let op = store.op_view(a, at(21)).expect("op");
        assert_eq!(op.leg, OpLeg::Homeward);
        assert_eq!((op.depart_at, op.arrive_at), (ops_time(at(20)), ops_time(at(30))));
        let d = op.dig.expect("a dig");
        assert_eq!((d.drills, d.heat, d.ore_on_site, d.next_ore, d.ended_by), (7, 105, 0, 0, Some(OpEnd::Meltdown)));
        assert_eq!(store.drill_op(a, at(22)), Err(DrillRefusal::AlreadyHomeward));
    }

    /// §4d: a Coolant-Jockey's threshold is 115, so seed 2's 7th drill digs and the 8th melts down (1 head).
    #[test]
    fn ops_store_coolant_threshold_is_115_and_melts_on_the_8th() {
        assert_eq!((dig_threshold(false), dig_threshold(true)), (100, 115));
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 115, 2);
        for i in 1..=7 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(dig_view(&store, a, at(18)).heat, 105);
        assert_eq!(store.drill_op(a, at(18)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 1 }));
        assert_eq!(dig_view(&store, a, at(19)).heat, 120);
    }

    #[test]
    fn ops_store_drill_refusals() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(store.drill_op(a, at(0)), Err(DrillRefusal::NoOp));
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        assert_eq!(store.drill_op(a, at(20)), Err(DrillRefusal::Contract));
        on_site(&store, b, 1, 100, 2);
        assert_eq!(store.drill_op(b, at(9)), Err(DrillRefusal::NotArrived));
        store.return_op(b, at(10)).expect("return");
        assert_eq!(store.drill_op(b, at(11)), Err(DrillRefusal::AlreadyHomeward));
        assert_eq!(dig_view(&store, b, at(11)).drills, 0, "no refusal drills");
    }

    /// CF1: a drill counts as movement for the reaper; a read does not.
    #[test]
    fn ops_store_drill_counts_as_moving() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 2);
        store.op_view(a, at(500));
        assert!(!store.moved_since(a, at(400), at(600)), "arrived at 10 s, nothing since");
        store.drill_op(a, at(500)).expect("drill");
        assert!(store.moved_since(a, at(400), at(600)));
        assert!(!store.moved_since(a, at(501), at(600)));
    }

    /// F6: a return from a dig leaves the ore on site behind.
    #[test]
    fn ops_store_return_from_a_dig_walks_away() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 2);
        store.drill_op(a, at(11)).expect("drill");
        store.drill_op(a, at(12)).expect("drill");
        assert_eq!(dig_view(&store, a, at(13)).ore_on_site, 30);
        assert_eq!(store.return_op(a, at(13)), Ok(()));
        let d = dig_view(&store, a, at(14));
        assert_eq!((d.drills, d.ore_on_site, d.next_ore, d.ended_by), (2, 0, 0, Some(OpEnd::WalkedAway)));
    }

    /// The dig's exact keys; nothing names the seed, the Crew, the Commander or the trigger,
    /// with a raid pending (seed 7 raids on drill 1).
    #[test]
    fn ops_dig_view_has_exactly_its_keys() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        let v = serde_json::to_value(store.op_view(a, at(12)).expect("op")).unwrap();
        let keys: std::collections::BTreeSet<&str> = v["dig"].as_object().expect("dig").keys().map(|k| k.as_str()).collect();
        let want = ["ambush", "carried", "drills", "ended_by", "heat", "next_ore", "next_raid_pct", "ore_on_site", "raid", "threshold"];
        assert_eq!(keys, want.into_iter().collect());
        assert_eq!(v["dig"]["raid"], serde_json::json!({"mission_id": "GUARD_DUTY"}));
        let text = v.to_string();
        assert!(!text.contains("seed") && !text.contains("crew") && !text.contains("commander") && !text.contains("trigger"), "{text}");
    }

    #[test]
    fn ops_raid_mission_is_a_server_mission() {
        assert!(missions().contains_key(DEMO_RAID_MISSION));
    }

    fn trigger(at_secs: i64) -> RaidTrigger {
        RaidTrigger { trigger_at: at(at_secs), mission_id: DEMO_RAID_MISSION, kind: BattleKind::Raid }
    }

    /// §4d: drill n raids on its d100 ≤ 5 × n. Seed 7's drill 1 (d100 2) raids at heat 15 and ore
    /// 10; seed 5's drills 1–2 (69, 22) don't and its drill 3 (9 ≤ 15) does, at heat 45 and ore 60.
    #[test]
    fn ops_store_raid_triggers_on_the_seeds_drill() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        on_site(&store, a, 1, 100, 7);
        assert_eq!(store.drill_op(a, at(11)), Ok(DrillOutcome::Drilled));
        let d = dig_view(&store, a, at(12));
        assert_eq!((d.heat, d.ore_on_site, d.next_raid_pct), (15, 10, None));
        assert_eq!(d.raid, Some(RaidView { mission_id: "GUARD_DUTY" }));
        assert_eq!(store.pending_raid(a, at(12)), Some(PendingRaid { commander_id: COMMANDER, trigger: trigger(11) }));
        on_site(&store, b, 1, 100, 5);
        store.drill_op(b, at(11)).expect("drill 1");
        store.drill_op(b, at(12)).expect("drill 2");
        assert_eq!(store.pending_raid(b, at(13)), None);
        store.drill_op(b, at(13)).expect("drill 3");
        let d = dig_view(&store, b, at(14));
        assert_eq!((d.drills, d.heat, d.ore_on_site, d.raid.is_some()), (3, 45, 60, true));
    }

    /// Seed 1: no raid on drills 1–6, and drill 7 melts down (2 heads) although its d100 (14 ≤ 35)
    /// would raid: a meltdown rolls no raid.
    #[test]
    fn ops_store_meltdown_rolls_no_raid() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 1);
        for i in 1..=6 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(store.pending_raid(a, at(17)), None);
        assert_eq!(store.drill_op(a, at(20)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 2 }));
        let d = dig_view(&store, a, at(21));
        assert_eq!((d.ended_by, d.raid, d.next_raid_pct), (Some(OpEnd::Meltdown), None, None));
    }

    /// While raiders are at the site, neither a drill nor a return goes through, and nothing moves.
    #[test]
    fn ops_store_raid_pending_refuses_drill_and_return() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        assert_eq!(store.drill_op(a, at(12)), Err(DrillRefusal::RaidPending));
        assert_eq!(store.return_op(a, at(12)), Err(ReturnRefusal::RaidPending));
        let op = store.op_view(a, at(13)).expect("op");
        assert_eq!(op.leg, OpLeg::Outbound);
        let d = op.dig.expect("a dig");
        assert_eq!((d.drills, d.ore_on_site, d.raid.is_some()), (1, 10, true));
    }

    /// Each result on seed 7's raid at 11 s: Held resumes drilling, Retreat takes half the ore
    /// home from `now`, Wiped clears the op.
    #[test]
    fn ops_store_latch_applies_each_result() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        for id in [a, b, c] {
            on_site(&store, id, 1, 100, 7);
            store.drill_op(id, at(11)).expect("drill");
        }
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Held, at(20)));
        let d = dig_view(&store, a, at(20));
        assert_eq!((d.raid, d.next_raid_pct, d.ore_on_site, d.carried, d.ended_by), (None, Some(10), 10, 0, None));
        assert_eq!(store.drill_op(a, at(21)), Ok(DrillOutcome::Drilled), "seed 7's drill 2 (d100 98) digs on");
        assert!(store.latch_raid(b, &trigger(11), RaidResult::Retreat, at(20)));
        let op = store.op_view(b, at(21)).expect("op");
        assert_eq!((op.leg, op.depart_at, op.arrive_at), (OpLeg::Homeward, ops_time(at(20)), ops_time(at(30))));
        let d = op.dig.expect("a dig");
        assert_eq!((d.carried, d.ore_on_site, d.ended_by, d.raid, d.next_raid_pct), (5, 0, Some(OpEnd::Retreat), None, None));
        assert!(store.latch_raid(c, &trigger(11), RaidResult::Wiped, at(20)));
        assert!(store.op_view(c, at(21)).is_none(), "the op is cleared");
    }

    fn record(started: i64, mission: &str, status: &str, outcome: Option<&str>) -> DeploymentRecord {
        DeploymentRecord {
            deployment_id: Uuid::new_v4(),
            commander_id: COMMANDER,
            session_id: Uuid::new_v4(),
            mission_id: mission.into(),
            status: status.into(),
            outcome: outcome.map(Into::into),
            started_at: at(started),
            closed_at: None,
        }
    }

    /// The fight is the earliest non-aborted record of the raid's mission that started strictly
    /// after the trigger (100 s); an active one holds the latch back.
    #[test]
    fn ops_raid_result_picks_the_first_fight_after_the_trigger() {
        let t = trigger(100);
        let g = DEMO_RAID_MISSION;
        let pick = |deps: &[DeploymentRecord]| raid_result(deps, &t);
        assert_eq!(pick(&[]), Ok(None));
        assert_eq!(pick(&[record(99, g, "resolved", Some("Success"))]), Ok(None), "before the trigger");
        assert_eq!(
            pick(&[record(100, g, "resolved", Some("Defeat")), record(101, g, "resolved", Some("Success"))]),
            Ok(Some(RaidResult::Held)),
            "a fight at the trigger's instant is not after it"
        );
        assert_eq!(pick(&[record(101, g, "aborted", None), record(102, g, "resolved", Some("Defeat"))]), Ok(Some(RaidResult::Retreat)));
        assert_eq!(pick(&[record(101, "SUPPLY_RAID", "resolved", Some("Wipeout")), record(102, g, "active", None)]), Ok(None));
        assert_eq!(pick(&[record(101, g, "active", None), record(102, g, "resolved", Some("Success"))]), Ok(None), "never past an active fight");
        assert_eq!(pick(&[record(101, g, "resolved", Some("TacticalRetreat"))]), Ok(Some(RaidResult::Retreat)));
        assert_eq!(pick(&[record(101, g, "resolved", Some("Wipeout"))]), Ok(Some(RaidResult::Wiped)));
        assert!(pick(&[record(101, g, "resolved", None)]).is_err(), "a resolved record always has an outcome");
    }

    /// Seed 13 raids on drill 2 (12 s) and again on drill 3 (14 s). A latch carrying the first
    /// trigger, arriving after the second raid, writes nothing.
    #[test]
    fn ops_store_stale_latch_is_a_no_op() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 13);
        store.drill_op(a, at(11)).expect("drill 1");
        store.drill_op(a, at(12)).expect("drill 2");
        assert_eq!(store.pending_raid(a, at(12)).map(|p| p.trigger), Some(trigger(12)));
        assert!(store.latch_raid(a, &trigger(12), RaidResult::Held, at(13)));
        store.drill_op(a, at(14)).expect("drill 3");
        assert!(!store.latch_raid(a, &trigger(12), RaidResult::Retreat, at(15)), "the trigger has changed");
        let op = store.op_view(a, at(15)).expect("op");
        assert_eq!(op.leg, OpLeg::Outbound);
        let d = op.dig.expect("a dig");
        assert_eq!((d.ore_on_site, d.carried, d.ended_by), (60, 0, None));
        assert_eq!(store.pending_raid(a, at(15)).map(|p| p.trigger), Some(trigger(14)));
    }

    /// A latch counts as movement for the reaper (a held raid keeps the team on site).
    #[test]
    fn ops_store_latch_counts_as_moving() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        assert!(!store.moved_since(a, at(400), at(600)), "drilled at 11 s, nothing since");
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Held, at(500)));
        assert!(store.moved_since(a, at(400), at(600)));
    }

    /// The next drill's raid chance: 5 × (drills + 1) %, `None` once the next drill melts down.
    /// Seed 2 raids on none of drills 1–7.
    #[test]
    fn ops_store_next_raid_pct() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        on_site(&store, a, 1, 100, 2);
        on_site(&store, b, 1, 115, 2);
        let mut at100 = vec![dig_view(&store, a, at(11)).next_raid_pct];
        let mut at115 = vec![dig_view(&store, b, at(11)).next_raid_pct];
        for i in 1..=6 {
            store.drill_op(a, at(10 + i)).expect("drill");
            store.drill_op(b, at(10 + i)).expect("drill");
            at100.push(dig_view(&store, a, at(20)).next_raid_pct);
            at115.push(dig_view(&store, b, at(20)).next_raid_pct);
        }
        assert_eq!(at100, vec![Some(5), Some(10), Some(15), Some(20), Some(25), Some(30), None]);
        assert_eq!(at115, vec![Some(5), Some(10), Some(15), Some(20), Some(25), Some(30), Some(35)]);
        store.drill_op(b, at(21)).expect("drill 7");
        assert_eq!(dig_view(&store, b, at(22)).next_raid_pct, None, "heat 105: the 8th drill melts down at 115");
    }

    /// The ambush's d100 against an independent Mulberry32 (Linux's Python oracle, BP-8b-2 spec
    /// check), and §4d's chance: 10 % + 5 % per whole 50 ore carried, capped at 50 %.
    #[test]
    fn ops_ambush_roll_matches_the_oracle() {
        let got: Vec<u32> = [7, 5, 14, 57, 2].into_iter().map(ambush_d100).collect();
        assert_eq!(got, vec![61, 59, 4, 13, 53]);
        let pct: Vec<u32> = [0, 49, 50, 99, 100, 399, 400, 1000].into_iter().map(ambush_pct).collect();
        assert_eq!(pct, vec![10, 10, 15, 15, 20, 45, 50, 50]);
    }

    fn ambush_trigger(at_secs: i64) -> RaidTrigger {
        RaidTrigger { trigger_at: at(at_secs), mission_id: DEMO_AMBUSH_MISSION, kind: BattleKind::Ambush }
    }

    /// A tier-1 dig on seed `seed`, `drills` drills at 11 s, 12 s, …, then a pull at 20 s: the
    /// homeward leg runs 20 s → 30 s and an ambush would fire at 25 s. No seed used here raids
    /// on its first three drills.
    fn pulled(store: &DemoOpsStore, a: Uuid, seed: u32, drills: i64) {
        on_site(store, a, 1, 100, seed);
        for i in 1..=drills {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(store.pull_op(a, at(20)), Ok(()));
    }

    /// Seed 2 (ambush d100 53 > 15 %): the pull loads all 60 ore and heads home, with no ambush
    /// at any time.
    #[test]
    fn ops_store_pull_loads_the_ore_and_heads_home() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 2, 3);
        let op = store.op_view(a, at(26)).expect("op");
        assert_eq!((op.leg, op.depart_at, op.arrive_at, op.arrived), (OpLeg::Homeward, ops_time(at(20)), ops_time(at(30)), false));
        let d = op.dig.expect("a dig");
        assert_eq!((d.carried, d.ore_on_site, d.next_ore, d.next_raid_pct, d.ended_by), (60, 0, 0, None, Some(OpEnd::Pulled)));
        assert_eq!((d.raid, d.ambush), (None, None));
        assert_eq!(store.pending_raid(a, at(29)), None);
    }

    /// The pull's refusals in order; none moves anything.
    #[test]
    fn ops_store_pull_refusals() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        assert_eq!(store.pull_op(a, at(0)), Err(PullRefusal::NoOp));
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        assert_eq!(store.pull_op(a, at(20)), Err(PullRefusal::Contract));
        on_site(&store, b, 1, 100, 7);
        assert_eq!(store.pull_op(b, at(9)), Err(PullRefusal::NotArrived));
        assert_eq!(store.pull_op(b, at(10)), Err(PullRefusal::NothingToHaul));
        store.drill_op(b, at(11)).expect("drill 1: seed 7 raids");
        assert_eq!(store.pull_op(b, at(12)), Err(PullRefusal::RaidPending));
        let d = dig_view(&store, b, at(12));
        assert_eq!((d.ore_on_site, d.carried, d.ended_by, d.raid.is_some()), (10, 0, None, true));
        on_site(&store, c, 1, 100, 2);
        store.drill_op(c, at(11)).expect("drill");
        assert_eq!(store.pull_op(c, at(12)), Ok(()));
        assert_eq!(store.pull_op(c, at(13)), Err(PullRefusal::AlreadyHomeward), "before NotArrived: the leg is still out");
    }

    /// The arrival banks the load once, and a second haul adds to it.
    #[test]
    fn ops_store_arrival_banks_the_load() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 2, 3);
        assert_eq!(store.ops_snapshot(a, at(29)).1, 0, "still on the road");
        assert_eq!(store.ops_snapshot(a, at(30)), (None, 60, OpsBase::default()));
        assert_eq!(store.ops_snapshot(a, at(31)), (None, 60, OpsBase::default()), "banked once");
        assert!(store.moved_since(a, at(30), at(31)), "the arrival is the last movement");
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 100 };
        store.start_op(a, (5, 0), board(), &dig(1), 2, Some(crew), at(40)).expect("start");
        store.drill_op(a, at(51)).expect("drill");
        store.pull_op(a, at(52)).expect("pull");
        assert_eq!(store.ops_snapshot(a, at(62)), (None, 70, OpsBase::default()));
    }

    /// Reviewer BP-8b-1b F4: a retreat's `carried` (seed 7: 10 ore, half is 5) is banked on arrival.
    #[test]
    fn ops_store_retreat_banks_what_it_carried() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Retreat, at(20)));
        assert_eq!(store.ops_snapshot(a, at(29)).1, 0);
        assert_eq!(store.ops_snapshot(a, at(30)), (None, 5, OpsBase::default()));
    }

    /// A walk-away, a meltdown and a contract carry nothing home, so they bank nothing.
    #[test]
    fn ops_store_walk_away_meltdown_and_contract_bank_nothing() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        on_site(&store, a, 1, 100, 2);
        store.drill_op(a, at(11)).expect("drill");
        store.drill_op(a, at(12)).expect("drill");
        store.return_op(a, at(13)).expect("return");
        assert_eq!(store.ops_snapshot(a, at(23)), (None, 0, OpsBase::default()));
        on_site(&store, b, 1, 100, 2);
        for i in 1..=6 {
            store.drill_op(b, at(10 + i)).expect("drill");
        }
        assert_eq!(store.drill_op(b, at(20)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 3 }));
        assert_eq!(store.ops_snapshot(b, at(30)), (None, 0, OpsBase::default()));
        store.start_op(c, (5, 0), board(), &site(), 7, None, at(0)).expect("start");
        store.return_op(c, at(10)).expect("return");
        assert_eq!(store.ops_snapshot(c, at(20)), (None, 0, OpsBase::default()));
    }

    /// Seed 14 (d100 4 ≤ 10 %) is ambushed and seed 2 (53) is not, on the same 30-ore haul. The two
    /// read the same until the midpoint (25 s); the first read from then on fires seed 14's ambush
    /// at that read's time, and its leg waits on the road while seed 2's comes home and banks.
    #[test]
    fn ops_store_ambush_is_hidden_until_its_midpoint() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        pulled(&store, a, 14, 2);
        pulled(&store, b, 2, 2);
        assert_eq!(store.op_view(a, at(24)), store.op_view(b, at(24)));
        assert_eq!(store.pending_raid(a, at(24)), None);
        assert_eq!(store.pending_raid(a, at(26)), Some(PendingRaid { commander_id: COMMANDER, trigger: ambush_trigger(26) }));
        assert_eq!(store.pending_raid(b, at(26)), None);
        let op = store.op_view(a, at(40)).expect("the ambushed team is still on the road");
        assert_eq!((op.leg, op.arrived), (OpLeg::Homeward, false));
        let d = op.dig.expect("a dig");
        assert_eq!((d.ambush, d.raid, d.carried), (Some(RaidView { mission_id: "GUARD_DUTY" }), None, 30));
        assert_eq!(store.ops_snapshot(a, at(40)).1, 0);
        assert_eq!(store.ops_snapshot(b, at(40)), (None, 30, OpsBase::default()));
    }

    /// Each result of seed 14's ambush (fired at 25 s, fought by 28 s): Held resumes the leg
    /// shifted by the 3 s wait with all 30 ore, Retreat with 15, Wiped clears the op; the
    /// arrivals at 33 s bank 30 and 15.
    #[test]
    fn ops_store_ambush_latch_applies_each_result() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        for id in [a, b, c] {
            pulled(&store, id, 14, 2);
            assert_eq!(store.pending_raid(id, at(25)).map(|p| p.trigger), Some(ambush_trigger(25)));
        }
        assert!(store.latch_raid(a, &ambush_trigger(25), RaidResult::Held, at(28)));
        let op = store.op_view(a, at(32)).expect("op");
        assert_eq!((op.depart_at, op.arrive_at, op.arrived), (ops_time(at(23)), ops_time(at(33)), false));
        let d = op.dig.expect("a dig");
        assert_eq!((d.carried, d.ended_by, d.ambush), (30, Some(OpEnd::Pulled), None));
        assert!(store.latch_raid(b, &ambush_trigger(25), RaidResult::Retreat, at(28)));
        let d = dig_view(&store, b, at(32));
        assert_eq!((d.carried, d.ended_by, d.ambush), (15, Some(OpEnd::Retreat), None));
        assert!(store.latch_raid(c, &ambush_trigger(25), RaidResult::Wiped, at(28)));
        assert_eq!(store.ops_snapshot(c, at(28)), (None, 0, OpsBase::default()));
        assert_eq!(store.ops_snapshot(a, at(33)), (None, 30, OpsBase::default()));
        assert_eq!(store.ops_snapshot(b, at(33)), (None, 15, OpsBase::default()));
        assert_eq!(store.ops_snapshot(c, at(40)), (None, 0, OpsBase::default()));
    }

    /// Two reads that both saw the ambush latch it once: the second finds no trigger, so a
    /// Retreat halves the load once (30 → 15, never 7).
    #[test]
    fn ops_store_ambush_latches_once() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 14, 2);
        store.pending_raid(a, at(25)).expect("the ambush fires");
        assert!(store.latch_raid(a, &ambush_trigger(25), RaidResult::Retreat, at(28)));
        assert!(!store.latch_raid(a, &ambush_trigger(25), RaidResult::Retreat, at(29)), "already latched");
        assert_eq!(dig_view(&store, a, at(30)).carried, 15);
    }

    /// A pull and an ambush latch each count as movement, read before the leg's arrival so the
    /// arrival term can't stand in for them (BP-8b-1b N7). The reveal does not count.
    #[test]
    fn ops_store_pull_and_ambush_latch_count_as_moving() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 14);
        store.drill_op(a, at(11)).expect("drill");
        store.drill_op(a, at(12)).expect("drill");
        assert!(!store.moved_since(a, at(400), at(450)), "drilled at 12 s, nothing since");
        store.pull_op(a, at(500)).expect("pull");
        assert!(store.moved_since(a, at(500), at(501)), "the pull");
        store.pending_raid(a, at(505)).expect("the ambush fires at 505 s");
        assert!(!store.moved_since(a, at(502), at(506)), "the reveal is not movement");
        assert!(store.latch_raid(a, &ambush_trigger(505), RaidResult::Held, at(507)));
        assert!(store.moved_since(a, at(507), at(508)), "the latch");
    }

    /// Reviewer BP-8a-2 CF4: cap 2, `a` holds banked ore and is the least recently touched, `b`
    /// holds a board only. A new campaign evicts `b`, never `a`.
    #[test]
    fn ops_store_eviction_skips_banked_ore() {
        let store = DemoOpsStore::new(2);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        on_site(&store, a, 1, 100, 2);
        store.drill_op(a, at(11)).expect("drill");
        store.pull_op(a, at(12)).expect("pull");
        assert_eq!(store.ops_snapshot(a, at(22)), (None, 10, OpsBase::default()));
        assert!(store.put_board(b, (5, 0), board(), at(30)));
        assert!(store.put_board(c, (5, 0), board(), at(40)));
        assert_eq!(store.len(), 2);
        assert!(store.board(b, (5, 0), at(41)).is_none(), "the board-only entry is evicted");
        assert_eq!(store.ops_snapshot(a, at(41)).1, 10, "the banked ore is kept");
    }

    /// §4d's step per 50 ore on seed 57 (d100 13): 30 ore (10 %) is not ambushed, 60 ore (15 %) is.
    #[test]
    fn ops_store_ambush_chance_steps_per_50_ore() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        pulled(&store, a, 57, 2);
        pulled(&store, b, 57, 3);
        assert_eq!(store.pending_raid(a, at(25)), None);
        assert_eq!(store.pending_raid(b, at(25)).map(|p| p.trigger), Some(ambush_trigger(25)));
    }

    /// `b` is not listed but was touched after the listing (a launch that raced the prune).
    #[test]
    fn ops_store_retain_drops_unlisted_but_keeps_entries_touched_after_the_listing() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        store.put_board(a, (5, 0), board(), at(0));
        store.put_board(b, (5, 0), board(), at(10));
        store.put_board(c, (5, 0), board(), at(0));
        store.retain_campaigns(&HashSet::from([a]), at(5));
        assert!(store.board(a, (5, 0), at(11)).is_some());
        assert!(store.board(b, (5, 0), at(11)).is_some());
        assert!(store.board(c, (5, 0), at(11)).is_none());
    }

    /// The four buildings' numbers (Linux's oracle, BP-10a spec check): each flag moves only its
    /// own number. The ids and the costs are pinned as literals.
    #[test]
    fn ops_base_numbers_match_the_oracle() {
        let plain = OpsBase::default();
        let all = OpsBase { radio_shack: true, hobbyist_benches: true, grease_pit: true, gunsmith: true };
        assert_eq!((plain.heat_per_drill(), all.heat_per_drill()), (15, 10));
        let raid: Vec<(u32, u32)> = (1..=3).map(|n| (plain.raid_pct(n), all.raid_pct(n))).collect();
        assert_eq!(raid, vec![(5, 3), (10, 6), (15, 9)]);
        let ore: Vec<(u32, u32, u32, u32)> = (1..=3).map(|n| (plain.ore(n, 1), all.ore(n, 1), plain.ore(n, 3), all.ore(n, 3))).collect();
        assert_eq!(ore, vec![(10, 12, 20, 25), (20, 25, 40, 50), (30, 37, 60, 75)]);
        let ambush: Vec<(u32, u32)> = [30, 50, 60, 120, 450].into_iter().map(|l| (plain.haul_ambush_pct(l), all.haul_ambush_pct(l))).collect();
        assert_eq!(ambush, vec![(10, 5), (15, 7), (15, 7), (20, 10), (50, 25)]);
        let ids = ["radio_shack", "hobbyist_benches", "grease_pit", "gunsmith", "barracks", ""];
        let costs: Vec<Option<u32>> = ids.into_iter().map(|id| BaseBuilding::from_id(id).map(BaseBuilding::cost)).collect();
        assert_eq!(costs, vec![Some(50), Some(120), Some(80), Some(80), None, None]);
    }

    /// Seed 2's three-drill haul (60 ore), banked by a read at 30 s.
    fn banked_60(store: &DemoOpsStore, a: Uuid) {
        pulled(store, a, 2, 3);
        assert_eq!(store.ops_snapshot(a, at(30)).1, 60);
    }

    /// A build debits its cost and stands. Already built comes before short of ore, and a
    /// refusal moves nothing.
    #[test]
    fn ops_store_build_debits_the_bank_and_stands() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(0)), Err(BuildRefusal::NotEnoughOre), "no entry: nothing banked");
        banked_60(&store, a);
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(31)), Ok(()));
        let shack = OpsBase { radio_shack: true, ..OpsBase::default() };
        assert_eq!(store.ops_snapshot(a, at(32)), (None, 10, shack));
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(33)), Err(BuildRefusal::AlreadyBuilt));
        assert_eq!(store.build_op(a, BaseBuilding::Gunsmith, at(34)), Err(BuildRefusal::NotEnoughOre));
        assert_eq!(store.ops_snapshot(a, at(35)), (None, 10, shack));
    }

    /// Never while an op is out, and "out" comes first: a haul on the road (nothing banked yet),
    /// then an outbound contract, its arrival and its homeward leg. The haul's arrival banks
    /// before the check, so the same call pays.
    #[test]
    fn ops_store_build_refuses_while_an_op_is_out() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 2, 3);
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(25)), Err(BuildRefusal::OpOut), "on the road, 0 banked");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(30)), Ok(()), "the arrival banks 60, then pays 50");
        assert_eq!(store.ops_snapshot(a, at(30)).1, 10);
        store.start_op(a, (5, 0), board(), &site(), 7, None, at(40)).expect("start");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(45)), Err(BuildRefusal::OpOut), "before AlreadyBuilt");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(50)), Err(BuildRefusal::OpOut), "arrived at the contract");
        store.return_op(a, at(50)).expect("return");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(55)), Err(BuildRefusal::OpOut), "heading home");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(60)), Err(BuildRefusal::AlreadyBuilt), "home");
    }

    /// The base feeds the drill and the view alike (sync 2026-10-31, claim 4). The Benches and
    /// the Shack at the Coolant-Jockey's 115 on seed 2 (no raid on drills 1–11): 10 heat a
    /// drill, the view's raid chance 3 × (drills + 1) while the next drill is safe, `None` at
    /// 110 heat, and the 12th drill melts (120). Only heat 100 tells 10 from 15 here: 110 < 115,
    /// 115 is not. The Gunsmith on tier 3: 25, 50, 75 on site, then `next_ore` 100.
    #[test]
    fn ops_store_base_feeds_the_drill_and_the_view() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        store.put_board(a, (5, 0), board(), at(0));
        store.set_base(a, OpsBase { radio_shack: true, hobbyist_benches: true, ..OpsBase::default() });
        on_site(&store, a, 1, 115, 2);
        let mut seen = vec![dig_view(&store, a, at(11)).next_raid_pct];
        for i in 1..=11 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
            seen.push(dig_view(&store, a, at(30)).next_raid_pct);
        }
        let want: Vec<Option<u32>> = (1..=11).map(|n| Some(3 * n)).chain([None]).collect();
        assert_eq!(seen, want);
        let d = dig_view(&store, a, at(30));
        assert_eq!((d.heat, d.threshold, d.raid), (110, 115, None));
        assert_eq!(store.drill_op(a, at(31)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 1 }));
        assert_eq!(dig_view(&store, a, at(32)).heat, 120);
        store.put_board(b, (5, 0), board(), at(0));
        store.set_base(b, OpsBase { gunsmith: true, ..OpsBase::default() });
        on_site(&store, b, 3, 100, 2);
        let mut piles = vec![];
        for i in 1..=3 {
            store.drill_op(b, at(10 + i)).expect("drill");
            piles.push(dig_view(&store, b, at(20)).ore_on_site);
        }
        assert_eq!(piles, vec![25, 75, 150]);
        assert_eq!(dig_view(&store, b, at(20)).next_ore, 100);
    }

    /// The Radio Shack's flip (Linux's oracle): seed 45's first drill rolls d100 4, a raid at 5 %
    /// and none at 3 %.
    #[test]
    fn ops_store_radio_shack_flips_seed_45s_first_raid() {
        assert_eq!(drill_roll(45, 1).0, 4);
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        store.put_board(b, (5, 0), board(), at(0));
        store.set_base(b, OpsBase { radio_shack: true, ..OpsBase::default() });
        on_site(&store, a, 1, 100, 45);
        on_site(&store, b, 1, 100, 45);
        store.drill_op(a, at(11)).expect("drill");
        store.drill_op(b, at(11)).expect("drill");
        assert!(dig_view(&store, a, at(12)).raid.is_some());
        assert_eq!(dig_view(&store, b, at(12)).raid, None);
    }

    /// The Grease-Pit's flip on seed 57 (ambush d100 13) and a 60-ore haul: ambushed at 15 %, not
    /// at 7 %, so the Pit's haul banks all 60.
    #[test]
    fn ops_store_grease_pit_halves_the_ambush() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let pit = OpsBase { grease_pit: true, ..OpsBase::default() };
        store.put_board(b, (5, 0), board(), at(0));
        store.set_base(b, pit);
        pulled(&store, a, 57, 3);
        pulled(&store, b, 57, 3);
        assert_eq!(store.pending_raid(a, at(25)).map(|p| p.trigger), Some(ambush_trigger(25)));
        assert_eq!(store.pending_raid(b, at(25)), None);
        assert_eq!(store.ops_snapshot(b, at(30)), (None, 60, pit));
    }

    /// Reviewer BP-8b-2 F5: cap 2, `a` holds only a building (no op, nothing banked) and is the
    /// least recently touched, `b` a board only. A new campaign evicts `b`, never `a`.
    #[test]
    fn ops_store_eviction_skips_a_base() {
        let store = DemoOpsStore::new(2);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let benches = OpsBase { hobbyist_benches: true, ..OpsBase::default() };
        store.put_board(a, (5, 0), board(), at(0));
        store.set_base(a, benches);
        assert!(store.put_board(b, (5, 0), board(), at(10)));
        assert!(store.put_board(c, (5, 0), board(), at(20)));
        assert_eq!(store.len(), 2);
        assert!(store.board(b, (5, 0), at(21)).is_none(), "the board-only entry is evicted");
        assert_eq!(store.ops_snapshot(a, at(21)).2, benches, "the base is kept");
    }

    /// A build counts as movement (the keep list). No op is out, so no leg's arrival can stand
    /// in for it.
    #[test]
    fn ops_store_build_counts_as_moving() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        banked_60(&store, a);
        assert!(!store.moved_since(a, at(400), at(450)), "home at 30 s, nothing since");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(500)), Ok(()));
        assert!(store.moved_since(a, at(500), at(501)), "the build");
    }
}
