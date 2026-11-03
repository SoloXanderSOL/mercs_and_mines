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
//!   A drill's rolls depend on the op's seed and the drill's number alone (`drill_roll`), and so
//!   does its heat, a seeded 10 to 15 + 5n from a stream of its own (`drill_heat`, BP-13).
//! - **The meltdown is the layer's one durable effect:** the route writes the Crew's heads
//!   through `demo_crew_loss::CrewLoss` after the store has committed the meltdown. No replay
//!   rebuilds it, which is acceptable only because the layer is demo-only.
//! - **The raid (BP-8b-1b):** a drill that does not melt down may bring raiders (`DEMO_RAID_PCT_PER_DRILL`
//!   × n %, on the drill's first roll). The raid is fought as an ordinary `GUARD_DUTY` mission by the
//!   dig's Commander, and the store reads the outcome from that Commander's deployment records
//!   (`raid_result`), never from the phone.
//! - **The haul (BP-8b-2):** a pull takes the ore on site home, where the arrival banks it for the
//!   campaign. The haul may be ambushed halfway (`ambush_pct`), fought and read like a raid.
//! - **The merc base (BP-10a, reworked by BP-16):** banked ore buys five buildings
//!   (`BaseBuilding`), one copy each, only while the team is home. What stands (`OpsBase`)
//!   changes the dig's numbers, what a lost ambush takes and the heal's price and time; every
//!   reader goes through `OpsBase`, so the panel never disagrees with the roll. The Gunsmith's
//!   +10 is read by the mission route (`DemoOpsStore::base_of`) and logged in `roll_inputs`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use sim_engine::game_types::{LootDrop, QualityGrade, UnitBattleResult, UnitStatus};
use sim_engine::missions::missions;
use sim_engine::rng::{generate_seed, Rng};
use uuid::Uuid;

use crate::map_gen::{hex_axial_distance, HexTerrain, MagmaVeinNode, SAFE_ZONE_RADIUS};
use crate::rate_limit::{ClientKey, FixedWindow, RateWarn, RATE_LIMIT_MAP_CAP, RATE_LIMIT_WINDOW};
use crate::repository::{DeploymentRecord, PauseHook};

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
/// The contract pins' missions, in the client's order (`app/play/battle.js` `ORDER`; BP-11b-1
/// brings `ORDER` in line).
pub const OPS_CONTRACT_MISSIONS: [&str; 4] = ["GUARD_DUTY", "CRASHED_SHIP_RAID", "SUPPLY_RAID", "BLACK_SITE_RAID"];
/// Cached campaigns before an insert evicts.
pub const DEMO_OPS_CAP: usize = 1_000;
/// Demo travel, seconds per hex before the destination's terrain modifier. Never canon:
/// canon is 20 minutes per hex (`Hex_Map_and_Travel` §2).
pub const DEMO_SECS_PER_HEX: u32 = 2;
/// A fought contract is off the board for 45 s, then returns at another hex of its ring
/// (BP-11a-3).
pub const DEMO_SITE_REFRESH_SECS: i64 = 45;
/// Credits per wounded soldier healed (BP-12a-1, §4e B3); `DEMO_BENCHES_HEAL_CREDITS` with the
/// Hobbyist Benches (BP-16). Read through `OpsBase::heal_credits`.
pub const DEMO_HEAL_CREDITS: u32 = 40;
/// Seconds until a healed batch is back on its feet (BP-12a-1); `DEMO_BENCHES_HEAL_SECS` with
/// the Hobbyist Benches (BP-16). Read through `OpsBase::heal_secs`.
pub const DEMO_HEAL_SECS: i64 = 15;
/// The Trophy Shelf keeps the newest this many items (BP-12a-1); the oldest is dropped.
pub const DEMO_TROPHY_CAP: usize = 20;
/// Credits per soldier replaced (BP-12a-2, §4e): a fresh merc for a Section, up to 8.
pub const DEMO_REPLACE_MERC_CREDITS: u32 = 120;
/// Credits per miner replaced (BP-12a-2, §4e): a fresh head for a Crew, up to 8.
pub const DEMO_REPLACE_MINER_CREDITS: u32 = 80;
/// Drill *n* adds a seeded heat from `DEMO_HEAT_MIN` to `DEMO_HEAT_TOP_BASE + DEMO_HEAT_TOP_PER_DRILL × n`
/// inclusive (§4e B1, BP-13: drill 1 10–20, drill 2 10–25, …); the site melts down at the threshold.
pub const DEMO_HEAT_MIN: u32 = 10;
pub const DEMO_HEAT_TOP_BASE: u32 = 15;
pub const DEMO_HEAT_TOP_PER_DRILL: u32 = 5;
pub const DEMO_HEAT_THRESHOLD: u32 = 100;
/// A Coolant-Jockey Foreman's threshold, in percent of `DEMO_HEAT_THRESHOLD` (×1.15 → 115).
pub const DEMO_COOLANT_THRESHOLD_PCT: u32 = 115;
/// Drill *n* yields `n ×` this on a tier-1 vein, and `DEMO_TIER3_ORE_MULTIPLIER` times that on tier 3.
pub const DEMO_ORE_PER_DRILL: u32 = 20;
pub const DEMO_TIER3_ORE_MULTIPLIER: u32 = 2;
/// Heads a meltdown costs the Crew, inclusive.
pub const DEMO_MELTDOWN_HEADS_MIN: u32 = 1;
pub const DEMO_MELTDOWN_HEADS_MAX: u32 = 3;
/// Morale a meltdown costs the Crew (BP-11a-1), in the same statement as its heads.
pub const DEMO_MELTDOWN_MORALE_LOSS: i16 = 10;
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
/// dig reads at most 24 there (12 drills × 2: heat 10 at least, threshold 115 at most).
pub const DEMO_AMBUSH_SEED_OFFSET: u32 = 0x8000_0000;
/// Each drill's heat rolls from `Rng::new(op_seed + this)` (wrapping), draw n − 1 for drill n: 2³⁰
/// draws from both the drill stream and the ambush stream (BP-13 spec check), and a dig reads at
/// most 12 here, so no drill's raid or heads roll moves.
pub const DEMO_HEAT_SEED_OFFSET: u32 = 0x4000_0000;
/// The mission an ambush is fought as (§4d: `GUARD_DUTY`'s numbers under ambush copy).
pub const DEMO_AMBUSH_MISSION: &str = "GUARD_DUTY";
/// The Crew's own fight (BP-17a, §4e B2): a battle no escort fights is an ops-layer d100 from
/// `Rng::new(op_seed + this)` (wrapping), with no Commander, deployment or log row. 2³⁰ draws
/// from both the drill stream and the ambush stream, 2³¹ from the heat stream (BP-17a spec
/// check); a dig reads at most 26 here: a raid on drill n reads draws 2(n − 1) and 2(n − 1) + 1,
/// the haul's ambush draws `DEMO_CREW_AMBUSH_DRAW` and the one after.
pub const DEMO_CREW_FIGHT_SEED_OFFSET: u32 = 0xC000_0000;
/// The first Crew-stream draw the haul's ambush reads: past every raid's (12 drills at most).
pub const DEMO_CREW_AMBUSH_DRAW: u32 = 24;
/// The Crew holds on a d100 at most this (50 %): no heads lost.
pub const DEMO_CREW_HELD_MAX: u32 = 50;
/// The Crew falls back on a d100 above `DEMO_CREW_HELD_MAX` and at most this (45 %); above it
/// (5 %) the Crew is wiped out.
pub const DEMO_CREW_RETREAT_MAX: u32 = 95;
/// Heads a Crew retreat costs, inclusive (drawn on every Crew fight, so each fight's reads stay fixed).
pub const DEMO_CREW_RETREAT_HEADS_MIN: u32 = 1;
pub const DEMO_CREW_RETREAT_HEADS_MAX: u32 = 2;
/// A Crew wipeout takes every head: `CrewLoss::meltdown_loss` floors at 0, so 8 means all.
pub const DEMO_CREW_WIPE_HEADS: i16 = 8;
/// Morale a Crew retreat or wipeout costs, as a meltdown's.
pub const DEMO_CREW_FIGHT_MORALE_LOSS: i16 = 10;
// The merc base (BP-10a, reworked by BP-16, §4e C1): the five buildings' effects and costs in
// banked ore. T1 names from `Base_Infrastructure_Tech_Tree`; the numbers are demo-only, never
// canon. Every effect is read through `OpsBase`'s methods, never these constants.

/// The Radio Shack's raid chance per drill, in place of `DEMO_RAID_PCT_PER_DRILL`.
pub const DEMO_SHACK_RAID_PCT_PER_DRILL: u32 = 3;
/// The Basic Smelter's ore per drill: `× NUM / DEN` (floor, per drill), after the tier
/// multiplier (the Gunsmith's rule until BP-16, moved whole).
pub const DEMO_SMELTER_ORE_NUM: u32 = 5;
/// The denominator of the Basic Smelter's ore multiplier.
pub const DEMO_SMELTER_ORE_DEN: u32 = 4;
/// With the Grease-Pit a lost haul ambush takes `carried / DIV` (floor) and the team keeps the
/// rest, the odd ore included. Without it the team keeps half (floor).
pub const DEMO_GREASE_PIT_LOSS_DIV: u32 = 4;
/// The Hobbyist Benches' price per wounded soldier healed, in place of `DEMO_HEAL_CREDITS`.
pub const DEMO_BENCHES_HEAL_CREDITS: u32 = 20;
/// The Hobbyist Benches' seconds until a healed batch is back, in place of `DEMO_HEAL_SECS`.
pub const DEMO_BENCHES_HEAL_SECS: i64 = 8;
/// The Radio Shack's cost in banked ore.
pub const DEMO_COST_RADIO_SHACK: u32 = 50;
/// The Gunsmith's cost in banked ore (its +10: `demo_roll::DEMO_GUNSMITH_ROLL_POINTS`).
pub const DEMO_COST_GUNSMITH: u32 = 100;
/// The Grease-Pit's cost in banked ore.
pub const DEMO_COST_GREASE_PIT: u32 = 80;
/// The Hobbyist Benches' cost in banked ore.
pub const DEMO_COST_HOBBYIST_BENCHES: u32 = 120;
/// The Basic Smelter's cost in banked ore.
pub const DEMO_COST_BASIC_SMELTER: u32 = 80;

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
    /// Every hex a contract may stand on, `(q, r, distance, travel_secs)` sorted by `(q, r)`
    /// (BP-11a-3): where a fought contract returns after 45 s. Never sent.
    #[serde(skip)]
    pub contract_pool: Vec<(i32, i32, u32, u32)>,
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
///   `Rng::new(ops_site_seed(campaign_id))`. Fewer candidates → fewer contracts. The sorted
///   candidates, before the shuffle, are the board's `contract_pool` (BP-11a-3).
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
    let contract_pool = candidates.iter().map(|&(q, r, d, t)| (q, r, d, travel_secs(d, t))).collect();
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
    OpsBoard { sites, contract_pool }
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
/// (BP-8b-1b) and the meltdown threshold. `section_id` (BP-11a-1) is the member's first Section,
/// whose headcount the at-0 check reads before a battle; `None` when the member has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigStart {
    pub crew_id: Uuid,
    pub commander_id: Uuid,
    pub threshold: u32,
    pub section_id: Option<Uuid>,
    /// The Section guards the dig from the start (BP-17a): `ops/start`'s `escort`, absent = true;
    /// `ops/next` always escorts.
    pub escort: bool,
}

/// What `ops/start` reads for a contract (BP-11a-2): the Commander who fights it and the
/// member's first Section, for the at-0 check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractStart {
    pub commander_id: Uuid,
    pub section_id: Option<Uuid>,
}

/// A contract in progress, private (BP-11a-2). `result` is set once, by the latch.
struct ContractState {
    mission_id: &'static str,
    commander_id: Uuid,
    section_id: Option<Uuid>,
    result: Option<RaidResult>,
}

/// Which battle a trigger waits for: raiders at the site (BP-8b-1b), an ambush on the haul
/// home (BP-8b-2) or a contract's fight (BP-11a-2). Only one is ever pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BattleKind {
    Raid,
    Ambush,
    Contract,
}

/// A battle waiting for its fight: when it began, the mission it is fought as, and its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaidTrigger {
    pub trigger_at: DateTime<Utc>,
    pub mission_id: &'static str,
    pub kind: BattleKind,
}

/// What a route needs to latch a raid: the Commander whose deployments to read, the trigger, and
/// the dig's Section for the at-0 check (BP-11a-1; `None` when the member had none at start).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingRaid {
    pub commander_id: Uuid,
    pub trigger: RaidTrigger,
    pub section_id: Option<Uuid>,
    /// The Crew's own fight (BP-17a): set for a haul's ambush revealed before the guard arrived,
    /// already rolled; the route latches it with `latch_crew` and reads no deployment.
    pub crew: Option<CrewRoll>,
}

/// A Crew's fight, rolled (BP-17a): the Crew, the result and the heads it takes (0 when Held).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrewRoll {
    pub crew_id: Uuid,
    pub result: RaidResult,
    pub heads: i16,
}

/// The last Crew fight as the client sees it (BP-17a), top level in the ops body (`crew_fight`),
/// because a wipeout clears the op: `kind` `"raid"` or `"ambush"`, `result` `"held"`,
/// `"retreat"` or `"wiped"`, and the heads the fight took (at most the Crew's headcount is
/// actually lost; the write floors at 0). Cleared by the next `ops/start` or `ops/next`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CrewFightView {
    pub kind: &'static str,
    pub result: &'static str,
    pub heads_lost: i16,
}

fn crew_fight_view(kind: &'static str, result: RaidResult, heads: i16) -> CrewFightView {
    let result = match result {
        RaidResult::Held => "held",
        RaidResult::Retreat => "retreat",
        RaidResult::Wiped => "wiped",
    };
    CrewFightView { kind, result, heads_lost: heads }
}

/// A Crew fight's result and heads (BP-17a), a function of `(op_seed, skip)` alone:
/// `Rng::new(op_seed + DEMO_CREW_FIGHT_SEED_OFFSET)` skips `skip` draws, then draws the d100 and
/// the retreat's heads (both always drawn).
fn crew_fight_roll(op_seed: u32, skip: u32) -> (RaidResult, i16) {
    let mut rng = Rng::new(op_seed.wrapping_add(DEMO_CREW_FIGHT_SEED_OFFSET));
    for _ in 0..skip {
        rng.next_u32();
    }
    let d100 = rng.roll_d100();
    let heads = rng.roll_int(DEMO_CREW_RETREAT_HEADS_MIN, DEMO_CREW_RETREAT_HEADS_MAX) as i16;
    if d100 <= DEMO_CREW_HELD_MAX {
        (RaidResult::Held, 0)
    } else if d100 <= DEMO_CREW_RETREAT_MAX {
        (RaidResult::Retreat, heads)
    } else {
        (RaidResult::Wiped, DEMO_CREW_WIPE_HEADS)
    }
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
    /// The member's first Section at `ops/start` (BP-11a-1), for the at-0 check.
    section_id: Option<Uuid>,
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
    /// When the Section stands guard (BP-17a): the start for an escorted dig, `ops/escort`'s
    /// arrival for a guard sent later, `None` while the Crew is alone.
    escort_at: Option<DateTime<Utc>>,
}

impl DigState {
    /// A battle born at `t` is the Section's (BP-17a) only once the guard stands: fixed when the
    /// trigger is made, so no read's timing changes who fights it.
    fn guarded(&self, t: DateTime<Utc>) -> bool {
        self.escort_at.is_some_and(|e| t >= e)
    }
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
    /// The next drill's heat range, inclusive (BP-13); 0 and 0 once the team is heading home.
    pub next_heat_min: u32,
    pub next_heat_max: u32,
    /// The top of the next drill's range would reach the threshold; false once heading home.
    pub might_overheat: bool,
    pub ended_by: Option<OpEnd>,
    /// The next drill's raid chance in percent; `null` once heading home, while a raid is
    /// pending, or when even the bottom of the next drill's range melts the site down (a
    /// meltdown rolls no raid; BP-13).
    pub next_raid_pct: Option<u32>,
    /// The raid waiting for its fight, or `null`.
    pub raid: Option<RaidView>,
    /// The ambush waiting for its fight on the way home, or `null`; `null` until it fires, so
    /// an ambushed haul and a clear one read the same until then (BP-8b-2).
    pub ambush: Option<RaidView>,
    /// The ore riding home (a pull's load, or half after a lost fight); 0 otherwise.
    pub carried: u32,
    /// When the Section stands guard (BP-17a): the start for an escorted dig, a later arrival
    /// for a guard sent by `ops/escort`, `null` while the Crew is alone.
    pub escort_at: Option<String>,
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

/// Drill `n`'s heat (1-based) within `(lo, hi)` inclusive: `Rng::new(op_seed + DEMO_HEAT_SEED_OFFSET)`
/// skips `n − 1` draws, then draws once. The range sets only the width, so a base that moves the
/// range never moves the draw (BP-13).
fn drill_heat(op_seed: u32, n: u32, (lo, hi): (u32, u32)) -> u32 {
    let mut rng = Rng::new(op_seed.wrapping_add(DEMO_HEAT_SEED_OFFSET));
    for _ in 0..n.saturating_sub(1) {
        rng.next_u32();
    }
    rng.roll_int(lo, hi)
}

/// A merc-base building (BP-10a; BP-16 adds the Basic Smelter): the body's `building` id, its
/// cost and its flag on `OpsBase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseBuilding {
    RadioShack,
    HobbyistBenches,
    GreasePit,
    Gunsmith,
    BasicSmelter,
}

impl BaseBuilding {
    /// The building a body's `building` id names; `None` for any other string.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "radio_shack" => Some(Self::RadioShack),
            "hobbyist_benches" => Some(Self::HobbyistBenches),
            "grease_pit" => Some(Self::GreasePit),
            "gunsmith" => Some(Self::Gunsmith),
            "basic_smelter" => Some(Self::BasicSmelter),
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
            Self::BasicSmelter => DEMO_COST_BASIC_SMELTER,
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
    pub basic_smelter: bool,
}

impl OpsBase {
    /// Drill `n`'s heat range, inclusive: 10 to 15 + 5n. No building changes it since BP-16
    /// (the Benches' cut is retired; B1's ranges and the Coolant-Jockey cover heat).
    fn heat_range(&self, n: u32) -> (u32, u32) {
        (DEMO_HEAT_MIN, DEMO_HEAT_TOP_BASE + DEMO_HEAT_TOP_PER_DRILL * n)
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

    /// The ore drill `n` yields on a vein of `tier`: × 5 / 4 (floor) with the Basic Smelter.
    fn ore(&self, n: u32, tier: u8) -> u32 {
        let plain = ore_for(n, tier);
        if self.basic_smelter {
            plain * DEMO_SMELTER_ORE_NUM / DEMO_SMELTER_ORE_DEN
        } else {
            plain
        }
    }

    /// The haul's ambush chance for a load of `load` ore. No building changes it since BP-16:
    /// the Grease-Pit cuts what a lost ambush takes instead (`ambush_kept`).
    fn haul_ambush_pct(&self, load: u32) -> u32 {
        ambush_pct(load)
    }

    /// What a lost haul ambush leaves of `carried` (BP-16): half (floor), or with the Grease-Pit
    /// all but a quarter, the quarter floored so the odd ore is kept (30 → 23, 120 → 90).
    fn ambush_kept(&self, carried: u32) -> u32 {
        if self.grease_pit {
            carried - carried / DEMO_GREASE_PIT_LOSS_DIV
        } else {
            carried / 2
        }
    }

    /// The price per wounded soldier healed: 20 with the Hobbyist Benches, else 40 (BP-16).
    fn heal_credits(&self) -> u32 {
        if self.hobbyist_benches {
            DEMO_BENCHES_HEAL_CREDITS
        } else {
            DEMO_HEAL_CREDITS
        }
    }

    /// Seconds until a healed batch is back: 8 with the Hobbyist Benches, else 15 (BP-16).
    fn heal_secs(&self) -> i64 {
        if self.hobbyist_benches {
            DEMO_BENCHES_HEAL_SECS
        } else {
            DEMO_HEAL_SECS
        }
    }

    fn has(&self, building: BaseBuilding) -> bool {
        match building {
            BaseBuilding::RadioShack => self.radio_shack,
            BaseBuilding::HobbyistBenches => self.hobbyist_benches,
            BaseBuilding::GreasePit => self.grease_pit,
            BaseBuilding::Gunsmith => self.gunsmith,
            BaseBuilding::BasicSmelter => self.basic_smelter,
        }
    }

    fn set(&mut self, building: BaseBuilding) {
        match building {
            BaseBuilding::RadioShack => self.radio_shack = true,
            BaseBuilding::HobbyistBenches => self.hobbyist_benches = true,
            BaseBuilding::GreasePit => self.grease_pit = true,
            BaseBuilding::Gunsmith => self.gunsmith = true,
            BaseBuilding::BasicSmelter => self.basic_smelter = true,
        }
    }
}

/// The raid's fight record among the Commander's deployments (BP-11a-1, extracted from
/// `raid_result`): the earliest one that is not `aborted`, is the trigger's mission and started
/// strictly after the trigger. `None` while there is none, whatever its status.
pub fn raid_fight<'a>(deps: &'a [DeploymentRecord], trigger: &RaidTrigger) -> Option<&'a DeploymentRecord> {
    deps.iter()
        .filter(|d| d.status != "aborted" && d.mission_id == trigger.mission_id && d.started_at > trigger.trigger_at)
        .min_by_key(|d| d.started_at)
}

/// A fight's losses in the demo (BP-11a-1): one soldier per `Kia` or `Mia` (MIA counts as lost);
/// `Wounded`, `Ready` and `OnMission` cost nothing.
pub fn fight_losses(units: &[UnitBattleResult]) -> i16 {
    units.iter().filter(|u| matches!(u.final_status, UnitStatus::Kia | UnitStatus::Mia)).count() as i16
}

/// How many of a recomputed fight's units came back wounded (BP-12a-1). They hold their bunk:
/// `fight_losses` counts only KIA and MIA, and the sim never makes one unit both.
pub fn fight_wounded(units: &[UnitBattleResult]) -> u8 {
    units.iter().filter(|u| matches!(u.final_status, UnitStatus::Wounded)).count() as u8
}

/// What an applied latch banks (BP-12a-1): the fight's pay; its wounded, into `section_id`'s
/// sickbay cut to `cap` survivors (`None`: no sickbay write); and its loot, onto the Trophy
/// Shelf. `latch_raid` applies all of it on its applied path only.
#[derive(Clone, Debug, Default)]
pub struct FightPay {
    pub credits: u32,
    pub section_id: Uuid,
    pub wounded: u8,
    pub cap: Option<u8>,
    pub loot: Option<LootDrop>,
}

impl FightPay {
    /// Pay only: no wounded, no loot (the at-0 Wipeout's shape, and the store tests').
    pub fn credits(credits: u32) -> Self {
        FightPay { credits, ..FightPay::default() }
    }
}

/// One Section's sickbay (BP-12a-1): soldiers `wounded` and not yet healed, and a batch
/// `healing` until `healed_at`. Both hold a bunk and neither deploys. An id the store has
/// never seen reads as the default, and a bay back at the default is dropped at the next settle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SickBay {
    wounded: u8,
    healing: u8,
    healed_at: Option<DateTime<Utc>>,
}

impl SickBay {
    /// A latched fight's wounded join the bay, and then the bay is cut to the Section's `cap`
    /// survivors, wounded first, then healing. The dead are never in the sickbay (Linux C1).
    fn take_wounded(&mut self, wounded: u8, cap: u8) {
        self.wounded = self.wounded.saturating_add(wounded);
        let mut over = self.wounded.saturating_add(self.healing).saturating_sub(cap);
        let w = over.min(self.wounded);
        self.wounded -= w;
        over -= w;
        self.healing -= over.min(self.healing);
        if self.healing == 0 {
            self.healed_at = None;
        }
    }
}

/// One item on the Trophy Shelf (BP-12a-1): the loot's grade and name, all the server knows
/// about it (Linux C4). The flavour text is the client's, keyed by `item_name` (BP-12b).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Trophy {
    pub quality_grade: QualityGrade,
    pub item_name: String,
}

/// Puts `loot` on the shelf, newest last, keeping the newest `DEMO_TROPHY_CAP`.
fn shelve(trophies: &mut Vec<Trophy>, loot: LootDrop) {
    trophies.push(Trophy { quality_grade: loot.quality_grade, item_name: loot.item_name });
    if trophies.len() > DEMO_TROPHY_CAP {
        trophies.remove(0);
    }
}

/// A Section's sickbay as the client sees it (BP-12a-1); `healed_at` in the format of the body's `now`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SickBayView {
    pub section_id: Uuid,
    pub wounded: u8,
    pub healing: u8,
    pub healed_at: Option<String>,
}

/// The store's half of the Company (BP-12a-1): the sickbays, sorted by `section_id`, and the
/// Trophy Shelf. Headcount, morale and XP stay on the roster read.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CompanyView {
    pub sections: Vec<SickBayView>,
    pub trophies: Vec<Trophy>,
}

/// Why `heal_op` refused (BP-12a-1), in the order it checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealRefusal {
    /// No wounded in the Section's bay (also an id the store has never seen, or no entry).
    NobodyToHeal,
    /// A batch is still healing: one batch per Section at a time.
    AlreadyHealing,
    /// Fewer banked credits than `DEMO_HEAL_CREDITS` × the wounded.
    NotEnoughCredits,
}

/// Why `begin_replace` refused (BP-12a-2), in the order it checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceRefusal {
    /// An op is out: replacements report at base.
    OpOut,
    /// Fewer banked credits than the replace costs (also a campaign with no entry).
    NotEnoughCredits,
}

/// A replace's debit, held until its write lands (BP-12a-2). Only `finish_replace` consumes it,
/// so the refund is always the amount debited.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct ReplaceHold {
    campaign_id: Uuid,
    credits: u32,
}

/// The raid's fight among the Commander's deployments: the earliest one that is not `aborted`,
/// is the trigger's mission and started strictly after the trigger (`started_at` is the app's
/// clock, as `trigger_at` is). `None` while there is none or it is still `active` (a later
/// record is never picked past it). Once `resolved`: Success → `Held`, Defeat or
/// TacticalRetreat → `Retreat`, Wipeout → `Wiped`. Any other outcome on a resolved record is
/// impossible by the schema's checks, so it is an error, never a raid left pending in silence.
pub fn raid_result(deps: &[DeploymentRecord], trigger: &RaidTrigger) -> Result<Option<RaidResult>, String> {
    let Some(d) = raid_fight(deps, trigger) else {
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
    /// The site's hex as the op started (BP-11a-3): where the team stands once on site, whatever
    /// the board shows for `site_id` later.
    at: (i32, i32),
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
    /// The contract (BP-11a-2): its mission, its lead and its fight's result; `None` on a dig.
    contract: Option<ContractState>,
}

impl ActiveOp {
    /// The Section is at base while the op is out (BP-17a): an unescorted dig whose guard has
    /// not been sent.
    fn section_home(&self) -> bool {
        self.dig.as_ref().is_some_and(|d| d.escort_at.is_none())
    }
}

/// The op as the client sees it. Times are RFC 3339 with milliseconds; `arrived` is
/// `now >= arrive_at` at the read, and `false` while a haul's ambush is still to be fought.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ActiveOpView {
    pub leg: OpLeg,
    pub site_id: String,
    /// The team's own site hex (BP-11a-3), so the client never maps the op through the board: a
    /// fought contract's `site_id` returns elsewhere while the team still stands here.
    pub at: OpHex,
    pub kind: &'static str,
    pub depart_at: String,
    pub arrive_at: String,
    pub arrived: bool,
    /// The dig's numbers; absent on a contract, so a contract op keeps its seven keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dig: Option<DigView>,
    /// A contract's fight once latched (BP-11a-2): `"won"` (Held) or `"lost"` (Retreat). Absent
    /// before the fight and on a dig, so an unfought contract keeps its seven keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fought: Option<&'static str>,
}

/// A hex as the ops routes send it: `{"q": …, "r": …}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OpHex {
    pub q: i32,
    pub r: i32,
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
    /// Raiders the Crew fought alone (BP-17a), already applied: the route writes `heads` (0 when Held).
    CrewFight { crew_id: Uuid, heads: i16 },
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

/// Why `next_op` refused (BP-11a-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextRefusal {
    /// No op, or its homeward leg has arrived and been cleared.
    NoOp,
    /// The team is already on its way home.
    AlreadyHomeward,
    /// The outbound leg has not arrived yet.
    NotArrived,
    /// Raiders are at the dig: the raid must be fought first.
    RaidPending,
    /// The target stands on the team's own hex.
    SameSite,
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

/// Why `escort_op` refused (BP-17a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscortRefusal {
    /// No op, or its homeward leg has arrived and been cleared.
    NoOp,
    /// The op is a contract.
    Contract,
    /// The team is on its way home.
    AlreadyHomeward,
    /// The Section already guards the dig, or is on its way.
    AlreadyEscorted,
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

/// A contract's own state on the board (BP-11a-3), kept apart from the cached board: when its
/// fight was latched (off the board until 45 s later), and the hex it returned at,
/// `(q, r, distance, travel_secs)` from the pool; `None` = the board's own pin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ContractSlot {
    finished_at: Option<DateTime<Utc>>,
    pin: Option<(i32, i32, u32, u32)>,
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
    /// The pay this campaign has banked (BP-11a-2): added by every latched fight, inside
    /// `latch_raid`. It leaves only with the entry, as the banked ore does.
    credits_banked: u32,
    /// The buildings that stand (BP-10a). They leave only with the entry, as the banked ore does.
    base: OpsBase,
    /// The contracts' own state, by mission id (BP-11a-3): finished by an applied latch, moved
    /// by the refresh, cleared with a new gateway.
    contracts: HashMap<&'static str, ContractSlot>,
    /// By Section id (BP-12a-1): written by an applied latch and by a heal; settled by
    /// `settle_sickbay`; leaves only with the entry.
    sickbay: HashMap<Uuid, SickBay>,
    /// The Trophy Shelf (BP-12a-1): an applied latch's loot, newest last, at most
    /// `DEMO_TROPHY_CAP`; leaves only with the entry.
    trophies: Vec<Trophy>,
    /// Credits a replace has debited and not yet finished (BP-12a-2): set by `begin_replace`,
    /// cleared by `finish_replace`. `make_room` keeps an entry while it is not 0.
    credits_in_flight: u32,
    /// The last Crew fight (BP-17a), written by the Crew's raid in `drill_op` and by
    /// `latch_crew`, cleared by the next `start_op` or `next_op`. Display only, not on the keep
    /// list: an evicted entry loses only the message.
    crew_fight: Option<CrewFightView>,
}

impl OpsEntry {
    /// Takes `cost` from the banked credits, or refuses (`false`) when short, never saturating
    /// (BP-12a-1, the first credit debit). Callers hold the store lock.
    fn debit_credits(&mut self, cost: u32) -> bool {
        if self.credits_banked < cost {
            return false;
        }
        self.credits_banked -= cost;
        true
    }

    /// Brings back every batch whose `healed_at` has come, then drops the bays back at the
    /// default (BP-12a-1). A clock test under the lock: it reads nothing outside the entry.
    fn settle_sickbay(&mut self, now: DateTime<Utc>) {
        for bay in self.sickbay.values_mut() {
            if bay.healed_at.is_some_and(|t| now >= t) {
                bay.healing = 0;
                bay.healed_at = None;
            }
        }
        self.sickbay.retain(|_, b| *b != SickBay::default());
    }

    /// The Company as the client sees it (BP-12a-1), sickbays sorted by `section_id`.
    fn company(&self) -> CompanyView {
        let mut sections: Vec<SickBayView> = self
            .sickbay
            .iter()
            .map(|(id, b)| SickBayView { section_id: *id, wounded: b.wounded, healing: b.healing, healed_at: b.healed_at.map(ops_time) })
            .collect();
        sections.sort_by_key(|s| s.section_id);
        CompanyView { sections, trophies: self.trophies.clone() }
    }

    /// First fires a haul's ambush once its time has come (BP-8b-2): the trigger is set at
    /// `now`, the reveal, so no fight started before the player could know of it is latched, and
    /// the team waits on the road. Then clears a homeward leg that has arrived with no battle
    /// waiting and none to come, banking what it carried and recording the arrival (not the
    /// read) as the last movement. Every read and write of the op calls it first, under the lock.
    /// It first runs the board refresh (`settle_contracts`, BP-11a-3), then the sickbay
    /// (`settle_sickbay`, BP-12a-1).
    fn settle_arrival(&mut self, now: DateTime<Utc>) {
        self.settle_contracts(now);
        self.settle_sickbay(now);
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

    /// The board refresh (BP-11a-3): a contract finished at least 45 s ago returns at another
    /// hex of the board's pool, one draw from a fresh seed (display only, never a roll's seed):
    /// never on another contract's hex, its own old hex or the op's hex. With no hex free it
    /// stays off the board and the next settle tries again. A clock test under the lock: it
    /// reads nothing outside the entry, writes no `moved_at`, and never touches the op or
    /// anything banked. Due slots are visited in board order (`OPS_CONTRACT_MISSIONS`,
    /// BP-11a-4), so which contract picks first never depends on `HashMap` order.
    fn settle_contracts(&mut self, now: DateTime<Utc>) {
        let wait = Duration::seconds(DEMO_SITE_REFRESH_SECS);
        let due: Vec<&'static str> = OPS_CONTRACT_MISSIONS
            .iter()
            .copied()
            .filter(|m| self.contracts.get(m).is_some_and(|s| s.finished_at.is_some_and(|t| now >= t + wait)))
            .collect();
        for mission in due {
            let own = self.contract_hex(mission);
            let shown: HashSet<(i32, i32)> = self
                .board
                .sites
                .iter()
                .filter_map(|s| s.mission_id)
                .filter(|m| *m != mission)
                .filter_map(|m| self.contract_hex(m))
                .collect();
            let at = self.op.as_ref().map(|op| op.at);
            let free: Vec<(i32, i32, u32, u32)> = self
                .board
                .contract_pool
                .iter()
                .copied()
                .filter(|&(q, r, _, _)| Some((q, r)) != own && !shown.contains(&(q, r)) && Some((q, r)) != at)
                .collect();
            if free.is_empty() {
                continue;
            }
            let pick = free[Rng::new(generate_seed()).roll_int(0, (free.len() - 1) as u32) as usize];
            if let Some(slot) = self.contracts.get_mut(mission) {
                slot.pin = Some(pick);
                slot.finished_at = None;
            }
        }
    }

    /// A contract's current hex (BP-11a-3): its returned pin, else the board's own.
    fn contract_hex(&self, mission: &str) -> Option<(i32, i32)> {
        if let Some((q, r, _, _)) = self.contracts.get(mission).and_then(|s| s.pin) {
            return Some((q, r));
        }
        self.board.sites.iter().find(|s| s.mission_id == Some(mission)).map(|s| (s.q, s.r))
    }

    /// The board as the client sees it (BP-11a-3): a finished contract is dropped, a returned one
    /// stands at its new pin with the same `site_id`, mission and name. A slot whose mission is
    /// not on the board hides nothing.
    fn sites(&self) -> Vec<OpsSite> {
        self.board
            .sites
            .iter()
            .filter_map(|s| {
                let Some(slot) = s.mission_id.and_then(|m| self.contracts.get(m)) else {
                    return Some(s.clone());
                };
                if slot.finished_at.is_some() {
                    return None;
                }
                Some(match slot.pin {
                    Some((q, r, distance, travel_secs)) => OpsSite { q, r, distance, travel_secs, ..s.clone() },
                    None => s.clone(),
                })
            })
            .collect()
    }

    fn view(&self, now: DateTime<Utc>) -> Option<ActiveOpView> {
        self.op.as_ref().map(|op| ActiveOpView {
            leg: op.leg,
            site_id: op.site_id.clone(),
            at: OpHex { q: op.at.0, r: op.at.1 },
            kind: op.kind,
            depart_at: ops_time(op.depart_at),
            arrive_at: ops_time(op.arrive_at),
            arrived: now >= op.arrive_at && op.dig.as_ref().is_none_or(|d| d.ambush_at.is_none()),
            dig: op.dig.as_ref().map(|d| {
                let out = op.leg == OpLeg::Outbound;
                let (lo, hi) = if out { self.base.heat_range(d.drills + 1) } else { (0, 0) };
                DigView {
                    drills: d.drills,
                    heat: d.heat,
                    threshold: d.threshold,
                    ore_on_site: d.ore_on_site,
                    next_ore: if op.leg == OpLeg::Homeward { 0 } else { self.base.ore(d.drills + 1, d.tier) },
                    next_heat_min: lo,
                    next_heat_max: hi,
                    might_overheat: out && d.heat + hi >= d.threshold,
                    ended_by: d.ended_by,
                    next_raid_pct: if out && d.raid.is_none() && d.heat + lo < d.threshold {
                        Some(self.base.raid_pct(d.drills + 1))
                    } else {
                        None
                    },
                    raid: d.raid.filter(|r| r.kind == BattleKind::Raid).map(|r| RaidView { mission_id: r.mission_id }),
                    ambush: d.raid.filter(|r| r.kind == BattleKind::Ambush).map(|r| RaidView { mission_id: r.mission_id }),
                    carried: d.carried,
                    escort_at: d.escort_at.map(ops_time),
                }
            }),
            fought: op.contract.as_ref().and_then(|c| match c.result {
                Some(RaidResult::Held) => Some("won"),
                Some(RaidResult::Retreat) => Some("lost"),
                Some(RaidResult::Wiped) | None => None,
            }),
        })
    }
}

/// The contract's fight as a battle trigger (BP-11a-2): once the team is on site and until the
/// contract is fought. Its fight is the first deployment of the contract's mission started
/// after the arrival, so a fight from the hub's list before the arrival is never the contract's.
fn contract_trigger(op: &ActiveOp, now: DateTime<Utc>) -> Option<RaidTrigger> {
    let c = op.contract.as_ref()?;
    if op.leg != OpLeg::Outbound || now < op.arrive_at || c.result.is_some() {
        return None;
    }
    Some(RaidTrigger { trigger_at: op.arrive_at, mission_id: c.mission_id, kind: BattleKind::Contract })
}

/// What every ops body shows from the store (BP-11a-2): the board's sites as they stand now
/// (BP-11a-3; `None` with no entry, when the route serves its own board), the op, the banked
/// ore and pay, and the base, read under one lock.
#[derive(Clone, Debug, PartialEq)]
pub struct OpsSnapshot {
    pub sites: Option<Vec<OpsSite>>,
    pub op: Option<ActiveOpView>,
    pub ore_banked: u32,
    pub credits_banked: u32,
    pub base: OpsBase,
    /// and the Company's store half (BP-12a-1).
    pub company: CompanyView,
    /// The last Crew fight (BP-17a).
    pub crew_fight: Option<CrewFightView>,
}

/// A new op (BP-11a-4), built for a start (`start_op`) and for a next (`next_op`): outbound to
/// `site`, standing at the site's hex (`at`) once there. The leg takes `leg_secs`; home from the
/// site takes its own `travel_secs`. A dig starts fresh and a contract gets a fresh
/// `ContractState` with no result, so nothing carries over from an earlier op.
fn new_op(site: &OpsSite, op_seed: u32, dig: Option<DigStart>, contract: Option<ContractStart>, leg_secs: u32, now: DateTime<Utc>) -> ActiveOp {
    ActiveOp {
        site_id: site.site_id.clone(),
        at: (site.q, site.r),
        kind: site.kind,
        leg: OpLeg::Outbound,
        depart_at: now,
        arrive_at: now + Duration::seconds(i64::from(leg_secs)),
        travel_secs: site.travel_secs,
        op_seed,
        dig: dig.map(|d| DigState {
            tier: site.tier.unwrap_or(1),
            threshold: d.threshold,
            crew_id: d.crew_id,
            commander_id: d.commander_id,
            section_id: d.section_id,
            drills: 0,
            heat: 0,
            ore_on_site: 0,
            ended_by: None,
            raid: None,
            carried: 0,
            ambush_at: None,
            escort_at: d.escort.then_some(now),
        }),
        contract: contract.and_then(|c| site.mission_id.map(|mission_id| ContractState { mission_id, commander_id: c.commander_id, section_id: c.section_id, result: None })),
    }
}

/// Every campaign's ops state, in memory, bounded by `cap`.
///
/// **Writes later bricks must keep:**
/// - the board write (`put_board`) updates `gateway`, `board` and `touched` in place and never
///   touches the op, `moved_at`, the banked ore or the base; a new gateway clears the contracts'
///   own state (BP-11a-3);
/// - **the board refresh (BP-11a-3):** only `latch_raid`'s applied contract path finishes a
///   contract (one line); `settle_contracts`, run first by `settle_arrival`, returns it 45 s
///   later at another hex. The body's `sites` come from the entry (`OpsEntry::sites`), and
///   `ops/start` and `ops/next` look their site up there (`site`), so a finished contract is
///   unknown;
/// - only `start_op`, `next_op`, `return_op`, `drill_op`, `pull_op`, `latch_raid` and
///   `settle_arrival` (the ambush's reveal and the arrival, run first by every op read and
///   write) write the op. The arrival adds to the banked ore and `build_op` debits it; only
///   `build_op` writes the base, and never while an op is out. Each takes the lock, calls
///   `settle_arrival` first and returns owned data; none is held across an `.await`;
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
/// - **the escort (BP-17a):** a battle born before `escort_at` is the Crew's
///   (`DigState::guarded`): an unguarded raid is fought inside `drill_op` from the Crew stream
///   and never pends; an unguarded ambush pends with its roll (`PendingRaid::crew`) and is
///   latched by `latch_crew`. Only `new_op` and `escort_op` write `escort_at`;
/// - **the keep list** (the reaper's `moved_since`): a departure (start, next, return, pull), a
///   homeward arrival, a drill, a latch, a build, a heal and a replace each write `moved_at`. Reading the board or the op
///   does not, nor does the ambush's reveal; a read that latches a battle does. While an
///   ambush waits, `moved_since` still counts the leg's passed `arrive_at`, at most
///   `travel_secs` (≤ 48 s) after the pull: an accepted overcount;
/// - the eviction takes only an entry with no op, no banked ore, no banked credits, no
///   building, no sickbay off the default, no trophy and no replace in flight (BP-12a-1,
///   BP-12a-2);
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
    /// touched entry that holds no op, no banked ore, no banked credits, no building, no sickbay
    /// off the default, no trophy and no replace in flight (BP-12a-1, BP-12a-2), and journals
    /// (first, then at most hourly). The sickbay is tested by value, never by presence: a latch
    /// can leave a bay at the default until the entry's next settle (Reviewer BP-12a-1 F1).
    /// `false` when every entry holds one of them. An entry whose only state is a finished contract can go
    /// (BP-11a-3, accepted): its next read recomputes the board and the contract is back early. A
    /// won contract banked its pay, so it is never the victim.
    fn make_room(&self, inner: &mut HashMap<Uuid, OpsEntry>) -> bool {
        if inner.len() < self.cap {
            return true;
        }
        let victim = inner
            .iter()
            .filter(|(_, e)| e.op.is_none() && e.ore_banked == 0 && e.credits_banked == 0 && e.base == OpsBase::default() && e.sickbay.values().all(|b| *b == SickBay::default()) && e.trophies.is_empty() && e.credits_in_flight == 0)
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
    /// place, keeping its op; a new gateway also clears the contracts' own state (BP-11a-3), so
    /// every contract stands at the new board's pin. A new entry at the cap first evicts
    /// (`make_room`). `false`: not cached, because every entry holds an op, banked ore or a
    /// building (the caller serves the board uncached).
    pub fn put_board(&self, campaign_id: Uuid, gateway: (i32, i32), board: Arc<OpsBoard>, now: DateTime<Utc>) -> bool {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            if e.gateway != gateway {
                e.contracts.clear();
            }
            e.gateway = gateway;
            e.board = board;
            e.touched = now;
            return true;
        }
        if !self.make_room(&mut inner) {
            return false;
        }
        inner.insert(campaign_id, OpsEntry { gateway, board, touched: now, op: None, moved_at: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), contracts: HashMap::new(), sickbay: HashMap::new(), trophies: Vec::new(), credits_in_flight: 0, crew_fight: None });
        true
    }

    /// Starts the outbound leg to `site`: `depart_at = now`, `arrive_at = now + travel_secs`.
    /// One op per campaign (`Busy`); a campaign with no entry is cached with `board` first
    /// (`Full` when every entry at the cap holds an op). A dig passes `dig` (its tier comes
    /// from the site); a contract passes `contract` (BP-11a-2; its mission comes from the site).
    pub fn start_op(
        &self,
        campaign_id: Uuid,
        gateway: (i32, i32),
        board: Arc<OpsBoard>,
        site: &OpsSite,
        op_seed: u32,
        dig: Option<DigStart>,
        contract: Option<ContractStart>,
        now: DateTime<Utc>,
    ) -> Result<(), StartRefusal> {
        let mut inner = self.lock();
        if !inner.contains_key(&campaign_id) {
            if !self.make_room(&mut inner) {
                return Err(StartRefusal::Full);
            }
            inner.insert(campaign_id, OpsEntry { gateway, board, touched: now, op: None, moved_at: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), contracts: HashMap::new(), sickbay: HashMap::new(), trophies: Vec::new(), credits_in_flight: 0, crew_fight: None });
        }
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(StartRefusal::Full);
        };
        e.settle_arrival(now);
        if e.op.is_some() {
            return Err(StartRefusal::Busy);
        }
        e.op = Some(new_op(site, op_seed, dig, contract, site.travel_secs, now));
        e.crew_fight = None;
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

    /// Moves the team from the site it stands on straight to `site` (BP-11a-4), as a new op
    /// (`new_op`). It accepts what `return_op` accepts. Refusals in order: `NoOp` →
    /// `AlreadyHomeward` → `NotArrived` → `RaidPending` (a dig's raid) → `SameSite` (the target's
    /// hex is the team's `at`; hexes, not site ids, since a refreshed contract keeps its id).
    /// - The leg runs from `op.at`, never from a board lookup: the hop's hexes at the target's
    ///   seconds per hex (`travel_secs / distance`, distance at least 1), at least 1 s. Home from
    ///   there is the target's own `travel_secs`.
    /// - The old op goes whole: the ore on a dig's site is walked away from, as `return_op` (the
    ///   body then shows no `ended_by`), and an unfought contract stays on the board unfinished.
    /// - CF2: a new contract's trigger is the new leg's arrival (`contract_trigger` unchanged),
    ///   so no fight started before it counts. Nothing banked is written.
    /// - Writes `moved_at` (the keep list).
    pub fn next_op(
        &self,
        campaign_id: Uuid,
        site: &OpsSite,
        op_seed: u32,
        dig: Option<DigStart>,
        contract: Option<ContractStart>,
        now: DateTime<Utc>,
    ) -> Result<(), NextRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(NextRefusal::NoOp);
        };
        e.settle_arrival(now);
        let Some(op) = e.op.as_ref() else {
            return Err(NextRefusal::NoOp);
        };
        if op.leg == OpLeg::Homeward {
            return Err(NextRefusal::AlreadyHomeward);
        }
        if now < op.arrive_at {
            return Err(NextRefusal::NotArrived);
        }
        if op.dig.as_ref().is_some_and(|d| d.raid.is_some()) {
            return Err(NextRefusal::RaidPending);
        }
        if (site.q, site.r) == op.at {
            return Err(NextRefusal::SameSite);
        }
        let hop = hex_axial_distance(op.at.0, op.at.1, site.q, site.r);
        let leg_secs = (site.travel_secs / site.distance.max(1) * hop).max(1);
        e.op = Some(new_op(site, op_seed, dig, contract, leg_secs, now));
        e.crew_fight = None;
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// One drill on an arrived dig: drill `n` adds a seeded heat in the base's range (`drill_heat` over `OpsBase::heat_range`); at
    /// the threshold the site melts down (the ore on site is lost, the homeward leg starts at once
    /// and the Crew's heads are returned for the route to write), otherwise it adds the base's ore
    /// (`OpsBase::ore`) and, when the first roll is at most the base's raid chance
    /// (`OpsBase::raid_pct`), sets the raid trigger (a meltdown rolls no raid). Writes `moved_at` (the keep list). Refusals in order: `NoOp`
    /// → `Contract` → `AlreadyHomeward` → `NotArrived` → `RaidPending`. The meltdown is committed
    /// here, before the route's write, so a second drill sees `AlreadyHomeward` and the heads
    /// can't be lost twice.
    /// BP-17a: raiders the guard is not there for (`DigState::guarded` at `now`) are the Crew's,
    /// fought here from the Crew stream and applied at once (no trigger): Held drills on,
    /// Retreat goes home with half the ore on site as a Section's retreat does, Wiped clears the
    /// op; `crew_fight` records it and the route writes the heads (`DrillOutcome::CrewFight`).
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
        dig.heat += drill_heat(op.op_seed, dig.drills, base.heat_range(dig.drills));
        e.moved_at = Some(now);
        e.touched = now;
        let melts = dig.heat >= dig.threshold;
        let mut crew = None;
        if !melts && raid_d100 <= base.raid_pct(dig.drills) {
            if dig.guarded(now) {
                dig.raid = Some(RaidTrigger { trigger_at: now, mission_id: DEMO_RAID_MISSION, kind: BattleKind::Raid });
            } else {
                crew = Some(crew_fight_roll(op.op_seed, DEMO_ROLLS_PER_DRILL * (dig.drills - 1)));
            }
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
        let Some((result, heads)) = crew else {
            return Ok(DrillOutcome::Drilled);
        };
        let crew_id = dig.crew_id;
        match result {
            RaidResult::Held => {}
            RaidResult::Retreat => {
                dig.carried = dig.ore_on_site / 2;
                dig.ore_on_site = 0;
                dig.ended_by = Some(OpEnd::Retreat);
                op.leg = OpLeg::Homeward;
                op.depart_at = now;
                op.arrive_at = now + Duration::seconds(i64::from(op.travel_secs));
            }
            RaidResult::Wiped => e.op = None,
        }
        e.crew_fight = Some(crew_fight_view("raid", result, heads));
        Ok(DrillOutcome::CrewFight { crew_id, heads })
    }

    /// Hauls the ore home from an arrived dig (BP-8b-2): the ore on site rides home as `carried`
    /// (`OpEnd::Pulled`) and the homeward leg starts at `now` with the site's time. The ambush is
    /// rolled once, here: on `ambush_d100 ≤` the base's chance (`OpsBase::haul_ambush_pct`) only
    /// its fire time is stored, the
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

    /// The battle waiting for its fight (a raid, or a haul's ambush once fired, or a contract's
    /// fight once the team is on site (BP-11a-2)), after settling the op: the op's Commander, the
    /// trigger and its Section. `None` when none is pending.
    pub fn pending_raid(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Option<PendingRaid> {
        let mut inner = self.lock();
        let e = inner.get_mut(&campaign_id)?;
        e.settle_arrival(now);
        let op = e.op.as_ref()?;
        if let Some(trigger) = contract_trigger(op, now) {
            let c = op.contract.as_ref()?;
            return Some(PendingRaid { commander_id: c.commander_id, trigger, section_id: c.section_id, crew: None });
        }
        let dig = op.dig.as_ref()?;
        // BP-17a: a battle born before the guard stands is the Crew's (only an ambush can be: an
        // unguarded raid is fought inside `drill_op`), rolled here from the Crew stream.
        dig.raid.map(|trigger| {
            let crew = (!dig.guarded(trigger.trigger_at)).then(|| {
                let (result, heads) = crew_fight_roll(op.op_seed, DEMO_CREW_AMBUSH_DRAW);
                CrewRoll { crew_id: dig.crew_id, result, heads }
            });
            PendingRaid { commander_id: dig.commander_id, trigger, section_id: dig.section_id, crew }
        })
    }

    /// Applies a fought battle, only if `trigger` is still the op's pending one (`false`, and no
    /// write, otherwise), and banks `credits`, the fight's pay, in the same step (BP-11a-2), so the
    /// pay lands exactly when the latch does. A raid: `Held`, the trigger clears and drilling
    /// resumes; `Retreat`, half the ore on site (floor) is `carried`, the rest is lost, and the
    /// homeward leg starts at `now` (`OpEnd::Retreat`). An ambush (BP-8b-2): `Held` resumes the
    /// leg, shifted by the wait so the team sets off from the midpoint, with the whole load;
    /// `Retreat` the same with `OpsBase::ambush_kept` of it (half, or three quarters with the
    /// Grease-Pit, BP-16; `OpEnd::Retreat`). A contract (BP-11a-2): `Held`
    /// or `Retreat` records the result and the team stays on site. `Wiped`, any kind: the op is
    /// cleared and its ore lost (the mission route has already marked the Commander KIA), or, at 0
    /// soldiers (BP-11a-1), with no fight and no death. Each writes `moved_at` (the keep list).
    /// An applied contract latch, whatever its result, also finishes the contract (BP-11a-3): it
    /// leaves the board, and the refresh brings it back 45 s later. A refused latch finishes
    /// nothing. On the applied path only, beside the pay (BP-12a-1, one writer): the fight's
    /// wounded join the Section's sickbay, cut to `pay.cap` survivors, and its loot goes on the
    /// Trophy Shelf. A refused latch writes neither.
    pub fn latch_raid(&self, campaign_id: Uuid, trigger: &RaidTrigger, result: RaidResult, pay: FightPay, now: DateTime<Utc>) -> bool {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return false;
        };
        e.settle_arrival(now);
        let Some(op) = e.op.as_mut() else {
            return false;
        };
        if trigger.kind == BattleKind::Contract {
            if contract_trigger(op, now) != Some(*trigger) {
                return false;
            }
            match result {
                RaidResult::Held | RaidResult::Retreat => {
                    if let Some(c) = op.contract.as_mut() {
                        c.result = Some(result);
                    }
                }
                RaidResult::Wiped => e.op = None,
            }
            e.contracts.entry(trigger.mission_id).or_default().finished_at = Some(now);
        } else {
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
                // `Contract` never reaches this match (it latches above); it is named only to
                // keep the match exhaustive without a catch-all.
                (BattleKind::Ambush | BattleKind::Contract, RaidResult::Held | RaidResult::Retreat) => {
                    if result == RaidResult::Retreat {
                        dig.carried = e.base.ambush_kept(dig.carried);
                        dig.ended_by = Some(OpEnd::Retreat);
                    }
                    let wait = now - dig.ambush_at.take().unwrap_or(now);
                    op.depart_at += wait;
                    op.arrive_at += wait;
                }
                (_, RaidResult::Wiped) => e.op = None,
            }
        }
        e.credits_banked = e.credits_banked.saturating_add(pay.credits);
        if let Some(cap) = pay.cap {
            e.sickbay.entry(pay.section_id).or_default().take_wounded(pay.wounded, cap);
        }
        if let Some(loot) = pay.loot {
            shelve(&mut e.trophies, loot);
        }
        e.moved_at = Some(now);
        e.touched = now;
        true
    }

    /// Applies the Crew's own fight on a haul's ambush (BP-17a), only if `trigger` is still the
    /// op's pending one (`false`, and no write, otherwise): `latch_raid`'s ambush arm with no pay,
    /// no sickbay and no loot (Held resumes the leg with the whole load, Retreat with
    /// `OpsBase::ambush_kept` of it, Wiped clears the op), and `crew_fight` recorded on the
    /// applied path only. Writes `moved_at` (the keep list).
    pub fn latch_crew(&self, campaign_id: Uuid, trigger: &RaidTrigger, crew: CrewRoll, now: DateTime<Utc>) -> bool {
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
        match crew.result {
            RaidResult::Held | RaidResult::Retreat => {
                if crew.result == RaidResult::Retreat {
                    dig.carried = e.base.ambush_kept(dig.carried);
                    dig.ended_by = Some(OpEnd::Retreat);
                }
                let wait = now - dig.ambush_at.take().unwrap_or(now);
                op.depart_at += wait;
                op.arrive_at += wait;
            }
            RaidResult::Wiped => e.op = None,
        }
        e.crew_fight = Some(crew_fight_view("ambush", crew.result, crew.heads));
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
        let s = self.ops_state(campaign_id, now);
        (s.op, s.ore_banked, s.base)
    }

    /// The board's sites as they stand (BP-11a-3), the op as the client sees it, the banked ore
    /// and pay (BP-11a-2) and the base, from one lock, after settling. A campaign with no entry
    /// has no sites of its own (`None`), and has banked and built nothing.
    pub fn ops_state(&self, campaign_id: Uuid, now: DateTime<Utc>) -> OpsSnapshot {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return OpsSnapshot { sites: None, op: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), company: CompanyView::default(), crew_fight: None };
        };
        e.settle_arrival(now);
        OpsSnapshot {
            sites: Some(e.sites()),
            op: e.view(now),
            ore_banked: e.ore_banked,
            credits_banked: e.credits_banked,
            base: e.base,
            company: e.company(),
            crew_fight: e.crew_fight,
        }
    }

    /// The site `site_id` as it stands, after settling (BP-11a-3): from the entry's sites, else
    /// from `board` (a campaign with no entry). `None` for an unknown or finished site.
    pub fn site(&self, campaign_id: Uuid, board: &OpsBoard, site_id: &str, now: DateTime<Utc>) -> Option<OpsSite> {
        let mut inner = self.lock();
        let sites = match inner.get_mut(&campaign_id) {
            Some(e) => {
                e.settle_arrival(now);
                e.sites()
            }
            None => board.sites.clone(),
        };
        sites.into_iter().find(|s| s.site_id == site_id)
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

    /// Sends the Section to guard an unescorted dig (BP-17a, "go guard the dig"): it stands
    /// guard from `now + travel_secs` (`escort_at`) and fights every battle born from then on;
    /// `commander_id` and `section_id` (read by the route) become the dig's. Allowed while
    /// outbound (no fight can come before the arrival). Writes `moved_at` (a departure).
    /// Refusals in order: `NoOp` → `Contract` → `AlreadyHomeward` → `AlreadyEscorted`.
    pub fn escort_op(&self, campaign_id: Uuid, commander_id: Uuid, section_id: Option<Uuid>, now: DateTime<Utc>) -> Result<(), EscortRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(EscortRefusal::NoOp);
        };
        e.settle_arrival(now);
        let Some(op) = e.op.as_mut() else {
            return Err(EscortRefusal::NoOp);
        };
        let travel_secs = op.travel_secs;
        let leg = op.leg;
        let Some(dig) = op.dig.as_mut() else {
            return Err(EscortRefusal::Contract);
        };
        if leg == OpLeg::Homeward {
            return Err(EscortRefusal::AlreadyHomeward);
        }
        if dig.escort_at.is_some() {
            return Err(EscortRefusal::AlreadyEscorted);
        }
        dig.escort_at = Some(now + Duration::seconds(i64::from(travel_secs)));
        dig.commander_id = commander_id;
        dig.section_id = section_id;
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// The campaign's base as it stands (BP-16), for the mission route's Gunsmith row: a copy
    /// taken under the lock, with no settle (settling never changes `base`); no entry reads as
    /// no building.
    pub fn base_of(&self, campaign_id: Uuid) -> OpsBase {
        self.lock().get(&campaign_id).map_or_else(OpsBase::default, |e| e.base)
    }

    /// Heals every wounded soldier of `section_id` at once (BP-12a-1, §4e B3), for
    /// `OpsBase::heal_credits` each (BP-16: 20 with the Hobbyist Benches, else 40): they move to
    /// `healing` and are back after `OpsBase::heal_secs` (8 s, else 15 s). A batch already
    /// healing keeps the price and time it was healed at.
    /// Allowed while the team is out (Linux C3): the sickbay feeds nothing the server rolls.
    /// Writes `moved_at` (the keep list). Refusals in order: `NobodyToHeal` (also no entry) →
    /// `AlreadyHealing` → `NotEnoughCredits`. Check and debit are one critical section, no await.
    pub fn heal_op(&self, campaign_id: Uuid, section_id: Uuid, now: DateTime<Utc>) -> Result<(), HealRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(HealRefusal::NobodyToHeal);
        };
        e.settle_arrival(now);
        let base = e.base;
        let bay = e.sickbay.get(&section_id).copied().unwrap_or_default();
        if bay.wounded == 0 {
            return Err(HealRefusal::NobodyToHeal);
        }
        if bay.healing > 0 {
            return Err(HealRefusal::AlreadyHealing);
        }
        if !e.debit_credits(base.heal_credits() * u32::from(bay.wounded)) {
            return Err(HealRefusal::NotEnoughCredits);
        }
        e.sickbay.insert(section_id, SickBay { wounded: 0, healing: bay.wounded, healed_at: Some(now + Duration::seconds(base.heal_secs())) });
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// Debits `cost` for a replace (BP-12a-2) and holds it in flight, so `make_room` keeps the
    /// entry until `finish_replace`. Settles first, so a team that has just come home can replace
    /// on the same call. Writes `moved_at` (the keep list). Refusals in order: `OpOut` →
    /// `NotEnoughCredits` (also no entry). Check and debit are one critical section, no await.
    pub fn begin_replace(&self, campaign_id: Uuid, cost: u32, now: DateTime<Utc>) -> Result<ReplaceHold, ReplaceRefusal> {
        self.begin_hold(campaign_id, cost, false, now)
    }

    /// `begin_replace` for a Section's Requisition (BP-17a): it also passes while the op is an
    /// unescorted dig whose guard has not been sent (`ActiveOp::section_home`), since the
    /// Section is at base. A Crew Hire never does: the Crew is out.
    pub fn begin_requisition(&self, campaign_id: Uuid, cost: u32, now: DateTime<Utc>) -> Result<ReplaceHold, ReplaceRefusal> {
        self.begin_hold(campaign_id, cost, true, now)
    }

    fn begin_hold(&self, campaign_id: Uuid, cost: u32, section: bool, now: DateTime<Utc>) -> Result<ReplaceHold, ReplaceRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(ReplaceRefusal::NotEnoughCredits);
        };
        e.settle_arrival(now);
        if e.op.as_ref().is_some_and(|op| !(section && op.section_home())) {
            return Err(ReplaceRefusal::OpOut);
        }
        if !e.debit_credits(cost) {
            return Err(ReplaceRefusal::NotEnoughCredits);
        }
        e.credits_in_flight = e.credits_in_flight.saturating_add(cost);
        e.moved_at = Some(now);
        e.touched = now;
        Ok(ReplaceHold { campaign_id, credits: cost })
    }

    /// Ends a replace (BP-12a-2): the hold leaves flight and, with `refund`, goes back to the
    /// bank. An entry gone by now went with its campaign (only a retire removes an entry a hold
    /// protects), so the credits leave with the campaign and nothing is recreated.
    pub fn finish_replace(&self, hold: ReplaceHold, refund: bool) {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&hold.campaign_id) else {
            return;
        };
        e.credits_in_flight = e.credits_in_flight.saturating_sub(hold.credits);
        if refund {
            e.credits_banked = e.credits_banked.saturating_add(hold.credits);
        }
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

    /// Moves the campaign's op (its leg and a haul's hidden ambush time), its last movement and
    /// its finished contracts' times (BP-11a-3) `secs` into the past, so a route test crosses an
    /// arrival or a refresh without sleeping. A trigger's time stays: a fight's `started_at` is
    /// the real clock.
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
                    d.escort_at = d.escort_at.map(|t| t - back);
                }
            }
            e.moved_at = e.moved_at.map(|t| t - back);
            for slot in e.contracts.values_mut() {
                if let Some(t) = slot.finished_at.as_mut() {
                    *t -= back;
                }
            }
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

    /// Sets the banked credits at the campaign's entry, so a test can replace without winning a
    /// fight first (BP-12a-2: a Section at 0 can't start the op that would earn them).
    #[cfg(test)]
    pub(crate) fn set_credits(&self, campaign_id: Uuid, credits: u32) {
        if let Some(e) = self.lock().get_mut(&campaign_id) {
            e.credits_banked = credits;
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
    /// A test seam (BP-11a-2, Reviewer BP-11a-1 F1): when set, `latch_pending_raid` awaits it
    /// between the fight's recompute and the store's latch, so a race test can park two readers
    /// there. `None` in production.
    pub latch_seam: Option<PauseHook>,
}

impl Default for DemoOps {
    fn default() -> Self {
        Self {
            store: DemoOpsStore::default(),
            ip: FixedWindow::new("ops_ip", OPS_PER_IP, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP),
            wallet: FixedWindow::new("ops_wallet", OPS_PER_WALLET, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP),
            latch_seam: None,
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
        Arc::new(OpsBoard { sites: vec![], contract_pool: vec![] })
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
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
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
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
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
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
        assert!(!store.put_board(b, (5, 0), board(), at(1)), "nothing evictable");
        assert_eq!(store.start_op(b, (5, 0), board(), &site(), 8, None, None, at(1)), Err(StartRefusal::Full));
        assert_eq!(store.len(), 1);
        assert!(store.op_view(a, at(2)).is_some());
    }

    #[test]
    fn ops_store_one_op_per_campaign() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
        assert_eq!(store.start_op(a, (5, 0), board(), &site(), 8, None, None, at(20)), Err(StartRefusal::Busy), "arrived outbound is still out");
        assert_eq!(store.op_view(a, at(20)).expect("op").depart_at, ops_time(at(0)), "the first op is untouched");
    }

    #[test]
    fn ops_store_return_needs_an_arrived_outbound() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(store.return_op(a, at(0)), Err(ReturnRefusal::NoOp));
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
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
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
        store.return_op(a, at(10)).expect("return");
        assert!(store.op_view(a, at(19)).is_some(), "still on the way home");
        assert!(store.op_view(a, at(500)).is_none(), "home: the op is cleared");
        assert!(store.moved_since(a, at(20), at(500)), "the arrival at 20 s is the last movement");
        assert!(!store.moved_since(a, at(21), at(500)));
        assert!(store.start_op(a, (5, 0), board(), &site(), 8, None, None, at(501)).is_ok(), "a new op can start");
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
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold, section_id: None, escort: true };
        store.start_op(a, (5, 0), board(), &dig(tier), seed, Some(crew), None, at(0)).expect("start");
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

    /// Drill n's heat against an independent Mulberry32 (Linux's Python oracle, BP-13 spec check),
    /// on literal ranges: seed 2 at 10 to 15 + 5n and at the Benches' 10 to 10 + 5n (the same draws,
    /// never hotter), and seed 57 at 10 to 15 + 5n.
    #[test]
    fn ops_drill_heat_matches_the_oracle() {
        let plain: Vec<u32> = (1..=8).map(|n| drill_heat(2, n, (10, 15 + 5 * n))).collect();
        assert_eq!(plain, vec![12, 17, 19, 21, 28, 19, 12, 43]);
        let benches: Vec<u32> = (1..=8).map(|n| drill_heat(2, n, (10, 10 + 5 * n))).collect();
        assert_eq!(benches, vec![11, 14, 17, 19, 25, 18, 12, 39]);
        let s57: Vec<u32> = (1..=8).map(|n| drill_heat(57, n, (10, 15 + 5 * n))).collect();
        assert_eq!(s57, vec![13, 11, 21, 13, 22, 20, 19, 54]);
    }

    /// BP-13: seed 2's seeded heat (12, 17, 19); drill n yields 20 × n on tier 1 and twice that on tier 3.
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
        assert_eq!(seen, vec![(1, 12, 20, 40, 40, 80), (2, 29, 60, 60, 120, 120), (3, 48, 120, 80, 240, 160)]);
    }

    /// Seed 57, threshold 100: drills 1–5 dig, the 6th reaches exactly 100 and melts down (the
    /// threshold is `>=`), costing 2 heads.
    #[test]
    fn ops_store_meltdown_on_the_6th_drill_at_exactly_100() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 57);
        for i in 1..=5 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(store.drill_op(a, at(20)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 2 }));
        let op = store.op_view(a, at(21)).expect("op");
        assert_eq!(op.leg, OpLeg::Homeward);
        assert_eq!((op.depart_at, op.arrive_at), (ops_time(at(20)), ops_time(at(30))));
        let d = op.dig.expect("a dig");
        assert_eq!((d.drills, d.heat, d.ore_on_site, d.next_ore, d.ended_by), (6, 100, 0, 0, Some(OpEnd::Meltdown)));
        assert_eq!(store.drill_op(a, at(22)), Err(DrillRefusal::AlreadyHomeward));
    }

    /// §4d: a Coolant-Jockey's threshold is 115, so seed 57's 6th drill (heat 100) digs and the
    /// 7th melts down at 119 (1 head).
    #[test]
    fn ops_store_coolant_threshold_is_115_and_melts_on_the_7th() {
        assert_eq!((dig_threshold(false), dig_threshold(true)), (100, 115));
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 115, 57);
        for i in 1..=6 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(dig_view(&store, a, at(17)).heat, 100);
        assert_eq!(store.drill_op(a, at(18)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 1 }));
        assert_eq!(dig_view(&store, a, at(19)).heat, 119);
    }

    #[test]
    fn ops_store_drill_refusals() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(store.drill_op(a, at(0)), Err(DrillRefusal::NoOp));
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
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
        assert_eq!(dig_view(&store, a, at(13)).ore_on_site, 60);
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
        let want = ["ambush", "carried", "drills", "ended_by", "escort_at", "heat", "might_overheat", "next_heat_max", "next_heat_min", "next_ore", "next_raid_pct", "ore_on_site", "raid", "threshold"];
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

    /// §4d: drill n raids on its d100 ≤ 5 × n. Seed 7's drill 1 (d100 2) raids at heat 10 and ore
    /// 20; seed 5's drills 1–2 (69, 22) don't and its drill 3 (9 ≤ 15) does, at heat 50 and ore 120.
    #[test]
    fn ops_store_raid_triggers_on_the_seeds_drill() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        on_site(&store, a, 1, 100, 7);
        assert_eq!(store.drill_op(a, at(11)), Ok(DrillOutcome::Drilled));
        let d = dig_view(&store, a, at(12));
        assert_eq!((d.heat, d.ore_on_site, d.next_raid_pct), (10, 20, None));
        assert_eq!(d.raid, Some(RaidView { mission_id: "GUARD_DUTY" }));
        assert_eq!(store.pending_raid(a, at(12)), Some(PendingRaid { commander_id: COMMANDER, trigger: trigger(11), section_id: None, crew: None }));
        on_site(&store, b, 1, 100, 5);
        store.drill_op(b, at(11)).expect("drill 1");
        store.drill_op(b, at(12)).expect("drill 2");
        assert_eq!(store.pending_raid(b, at(13)), None);
        store.drill_op(b, at(13)).expect("drill 3");
        let d = dig_view(&store, b, at(14));
        assert_eq!((d.drills, d.heat, d.ore_on_site, d.raid.is_some()), (3, 50, 120, true));
    }

    /// Seed 1: no raid on drills 1–6 (heat 97 after them), and drill 7 melts down at 121 (2 heads)
    /// although its d100 (14 ≤ 35) would raid: a meltdown rolls no raid.
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
        assert_eq!((d.drills, d.ore_on_site, d.raid.is_some()), (1, 20, true));
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
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Held, FightPay::credits(0), at(20)));
        let d = dig_view(&store, a, at(20));
        assert_eq!((d.raid, d.next_raid_pct, d.ore_on_site, d.carried, d.ended_by), (None, Some(10), 20, 0, None));
        assert_eq!(store.drill_op(a, at(21)), Ok(DrillOutcome::Drilled), "seed 7's drill 2 (d100 98) digs on");
        assert!(store.latch_raid(b, &trigger(11), RaidResult::Retreat, FightPay::credits(0), at(20)));
        let op = store.op_view(b, at(21)).expect("op");
        assert_eq!((op.leg, op.depart_at, op.arrive_at), (OpLeg::Homeward, ops_time(at(20)), ops_time(at(30))));
        let d = op.dig.expect("a dig");
        assert_eq!((d.carried, d.ore_on_site, d.ended_by, d.raid, d.next_raid_pct), (10, 0, Some(OpEnd::Retreat), None, None));
        assert!(store.latch_raid(c, &trigger(11), RaidResult::Wiped, FightPay::credits(0), at(20)));
        assert!(store.op_view(c, at(21)).is_none(), "the op is cleared");
    }

    /// BP-11a-1: KIA and MIA each cost one soldier; Wounded and Ready cost nothing. Nothing in
    /// the sim emits `Mia` today (`resolver.rs:1379`), so the slice is built by hand.
    #[test]
    fn fight_losses_counts_kia_and_mia_not_wounded() {
        let unit = |s: UnitStatus| UnitBattleResult {
            unit_id: "u".into(),
            unit_name: "u".into(),
            unit_type: "u".into(),
            emoji: String::new(),
            hp_lost: 0,
            hp_remaining: 10,
            final_status: s,
            status_note: String::new(),
        };
        let units = [unit(UnitStatus::Kia), unit(UnitStatus::Mia), unit(UnitStatus::Wounded), unit(UnitStatus::Ready)];
        assert_eq!(fight_losses(&units), 2);
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
        assert!(store.latch_raid(a, &trigger(12), RaidResult::Held, FightPay::credits(0), at(13)));
        store.drill_op(a, at(14)).expect("drill 3");
        assert!(!store.latch_raid(a, &trigger(12), RaidResult::Retreat, FightPay::credits(0), at(15)), "the trigger has changed");
        let op = store.op_view(a, at(15)).expect("op");
        assert_eq!(op.leg, OpLeg::Outbound);
        let d = op.dig.expect("a dig");
        assert_eq!((d.ore_on_site, d.carried, d.ended_by), (120, 0, None));
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
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Held, FightPay::credits(0), at(500)));
        assert!(store.moved_since(a, at(400), at(600)));
    }

    /// `(heat, next_heat_min, next_heat_max, might_overheat, next_raid_pct)`.
    fn next_drill(d: &DigView) -> (u32, u32, u32, bool, Option<u32>) {
        (d.heat, d.next_heat_min, d.next_heat_max, d.might_overheat, d.next_raid_pct)
    }

    /// BP-13: the next drill's range is 10 to 15 + 5 × (drills + 1); `might_overheat` once its top
    /// reaches the threshold, while the raid chance, 5 × (drills + 1) %, stays until even the
    /// bottom would melt. Seed 57 raids on none of drills 1–7. At 100 the 6th drill melts at
    /// exactly 100 (the homeward zeros); at 115 it digs, and the 7th melts at 119.
    #[test]
    fn ops_store_next_raid_pct() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        on_site(&store, a, 1, 100, 57);
        on_site(&store, b, 1, 115, 57);
        let mut seen = vec![next_drill(&dig_view(&store, a, at(11)))];
        for i in 1..=5 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
            assert_eq!(store.drill_op(b, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
            seen.push(next_drill(&dig_view(&store, a, at(20))));
        }
        let want = vec![
            (0, 10, 20, false, Some(5)),
            (13, 10, 25, false, Some(10)),
            (24, 10, 30, false, Some(15)),
            (45, 10, 35, false, Some(20)),
            (58, 10, 40, false, Some(25)),
            (80, 10, 45, true, Some(30)),
        ];
        assert_eq!(seen, want);
        assert_eq!(store.drill_op(a, at(21)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 2 }));
        assert_eq!(next_drill(&dig_view(&store, a, at(22))), (100, 0, 0, false, None));
        assert_eq!(store.drill_op(b, at(21)), Ok(DrillOutcome::Drilled), "drill 6 at 115");
        assert_eq!(next_drill(&dig_view(&store, b, at(22))), (100, 10, 50, true, Some(35)));
        assert_eq!(store.drill_op(b, at(23)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 1 }));
        assert_eq!(next_drill(&dig_view(&store, b, at(24))), (119, 0, 0, false, None));
    }

    /// Seed 8 at 100 (no raid on drills 1–4): heat 60 after four drills and the 5th's top 40 reach
    /// exactly 100, so the next drill might overheat (`>=`); the raid % stays (60 + 10 < 100).
    #[test]
    fn ops_view_might_overheat_at_exactly_the_top() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 8);
        for i in 1..=4 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(next_drill(&dig_view(&store, a, at(20))), (60, 10, 40, true, Some(25)));
    }

    /// Seed 3 at 100 (no raid on drills 1–5): heat 90 after five drills, so even the bottom of the
    /// 6th's range (10) melts the site: no raid % (a meltdown rolls no raid).
    #[test]
    fn ops_view_certain_meltdown_hides_the_raid_pct() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 3);
        for i in 1..=5 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(next_drill(&dig_view(&store, a, at(20))), (90, 10, 45, true, None));
    }

    /// Seed 54 at 100: heat 86 after four drills. The bottom of the 5th's range (86 + 10 < 100)
    /// keeps the raid %, where §4d's old fixed 15 (86 + 15 ≥ 100) would have hidden it.
    #[test]
    fn ops_view_raid_pct_reads_the_bottom_of_the_range() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 54);
        for i in 1..=4 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(next_drill(&dig_view(&store, a, at(20))), (86, 10, 40, true, Some(25)));
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

    /// Seed 2 (ambush d100 53 > 20 %): the pull loads all 120 ore and heads home, with no ambush
    /// at any time.
    #[test]
    fn ops_store_pull_loads_the_ore_and_heads_home() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 2, 3);
        let op = store.op_view(a, at(26)).expect("op");
        assert_eq!((op.leg, op.depart_at, op.arrive_at, op.arrived), (OpLeg::Homeward, ops_time(at(20)), ops_time(at(30)), false));
        let d = op.dig.expect("a dig");
        assert_eq!((d.carried, d.ore_on_site, d.next_ore, d.next_raid_pct, d.ended_by), (120, 0, 0, None, Some(OpEnd::Pulled)));
        assert_eq!((d.raid, d.ambush), (None, None));
        assert_eq!(store.pending_raid(a, at(29)), None);
    }

    /// The pull's refusals in order; none moves anything.
    #[test]
    fn ops_store_pull_refusals() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        assert_eq!(store.pull_op(a, at(0)), Err(PullRefusal::NoOp));
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
        assert_eq!(store.pull_op(a, at(20)), Err(PullRefusal::Contract));
        on_site(&store, b, 1, 100, 7);
        assert_eq!(store.pull_op(b, at(9)), Err(PullRefusal::NotArrived));
        assert_eq!(store.pull_op(b, at(10)), Err(PullRefusal::NothingToHaul));
        store.drill_op(b, at(11)).expect("drill 1: seed 7 raids");
        assert_eq!(store.pull_op(b, at(12)), Err(PullRefusal::RaidPending));
        let d = dig_view(&store, b, at(12));
        assert_eq!((d.ore_on_site, d.carried, d.ended_by, d.raid.is_some()), (20, 0, None, true));
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
        assert_eq!(store.ops_snapshot(a, at(30)), (None, 120, OpsBase::default()));
        assert_eq!(store.ops_snapshot(a, at(31)), (None, 120, OpsBase::default()), "banked once");
        assert!(store.moved_since(a, at(30), at(31)), "the arrival is the last movement");
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 100, section_id: None, escort: true };
        store.start_op(a, (5, 0), board(), &dig(1), 2, Some(crew), None, at(40)).expect("start");
        store.drill_op(a, at(51)).expect("drill");
        store.pull_op(a, at(52)).expect("pull");
        assert_eq!(store.ops_snapshot(a, at(62)), (None, 140, OpsBase::default()));
    }

    /// Reviewer BP-8b-1b F4: a retreat's `carried` (seed 7: 20 ore, half is 10) is banked on arrival.
    #[test]
    fn ops_store_retreat_banks_what_it_carried() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Retreat, FightPay::credits(0), at(20)));
        assert_eq!(store.ops_snapshot(a, at(29)).1, 0);
        assert_eq!(store.ops_snapshot(a, at(30)), (None, 10, OpsBase::default()));
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
        on_site(&store, b, 1, 100, 57);
        for i in 1..=5 {
            store.drill_op(b, at(10 + i)).expect("drill");
        }
        assert_eq!(store.drill_op(b, at(20)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 2 }));
        assert_eq!(store.ops_snapshot(b, at(30)), (None, 0, OpsBase::default()));
        store.start_op(c, (5, 0), board(), &site(), 7, None, None, at(0)).expect("start");
        store.return_op(c, at(10)).expect("return");
        assert_eq!(store.ops_snapshot(c, at(20)), (None, 0, OpsBase::default()));
    }

    /// Seed 14 (d100 4 ≤ 15 %) is ambushed and seed 2 (53) is not, on the same 60-ore haul. The two
    /// read the same until the midpoint (25 s); the first read from then on fires seed 14's ambush
    /// at that read's time, and its leg waits on the road while seed 2's comes home and banks.
    #[test]
    fn ops_store_ambush_is_hidden_until_its_midpoint() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        pulled(&store, a, 14, 2);
        pulled(&store, b, 2, 2);
        // The two seeds' heats differ (37 and 29, BP-13); everything else reads the same.
        let masked = |v: Option<ActiveOpView>| v.map(|mut o| { if let Some(d) = o.dig.as_mut() { d.heat = 0; } o });
        assert_eq!(masked(store.op_view(a, at(24))), masked(store.op_view(b, at(24))));
        assert_eq!(store.pending_raid(a, at(24)), None);
        assert_eq!(store.pending_raid(a, at(26)), Some(PendingRaid { commander_id: COMMANDER, trigger: ambush_trigger(26), section_id: None, crew: None }));
        assert_eq!(store.pending_raid(b, at(26)), None);
        let op = store.op_view(a, at(40)).expect("the ambushed team is still on the road");
        assert_eq!((op.leg, op.arrived), (OpLeg::Homeward, false));
        let d = op.dig.expect("a dig");
        assert_eq!((d.ambush, d.raid, d.carried), (Some(RaidView { mission_id: "GUARD_DUTY" }), None, 60));
        assert_eq!(store.ops_snapshot(a, at(40)).1, 0);
        assert_eq!(store.ops_snapshot(b, at(40)), (None, 60, OpsBase::default()));
    }

    /// Each result of seed 14's ambush (fired at 25 s, fought by 28 s): Held resumes the leg
    /// shifted by the 3 s wait with all 60 ore, Retreat with 30, Wiped clears the op; the
    /// arrivals at 33 s bank 60 and 30.
    #[test]
    fn ops_store_ambush_latch_applies_each_result() {
        let store = DemoOpsStore::new(4);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        for id in [a, b, c] {
            pulled(&store, id, 14, 2);
            assert_eq!(store.pending_raid(id, at(25)).map(|p| p.trigger), Some(ambush_trigger(25)));
        }
        assert!(store.latch_raid(a, &ambush_trigger(25), RaidResult::Held, FightPay::credits(0), at(28)));
        let op = store.op_view(a, at(32)).expect("op");
        assert_eq!((op.depart_at, op.arrive_at, op.arrived), (ops_time(at(23)), ops_time(at(33)), false));
        let d = op.dig.expect("a dig");
        assert_eq!((d.carried, d.ended_by, d.ambush), (60, Some(OpEnd::Pulled), None));
        assert!(store.latch_raid(b, &ambush_trigger(25), RaidResult::Retreat, FightPay::credits(0), at(28)));
        let d = dig_view(&store, b, at(32));
        assert_eq!((d.carried, d.ended_by, d.ambush), (30, Some(OpEnd::Retreat), None));
        assert!(store.latch_raid(c, &ambush_trigger(25), RaidResult::Wiped, FightPay::credits(0), at(28)));
        assert_eq!(store.ops_snapshot(c, at(28)), (None, 0, OpsBase::default()));
        assert_eq!(store.ops_snapshot(a, at(33)), (None, 60, OpsBase::default()));
        assert_eq!(store.ops_snapshot(b, at(33)), (None, 30, OpsBase::default()));
        assert_eq!(store.ops_snapshot(c, at(40)), (None, 0, OpsBase::default()));
    }

    /// Two reads that both saw the ambush latch it once: the second finds no trigger, so a
    /// Retreat halves the load once (60 → 30, never 15).
    #[test]
    fn ops_store_ambush_latches_once() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 14, 2);
        store.pending_raid(a, at(25)).expect("the ambush fires");
        assert!(store.latch_raid(a, &ambush_trigger(25), RaidResult::Retreat, FightPay::credits(0), at(28)));
        assert!(!store.latch_raid(a, &ambush_trigger(25), RaidResult::Retreat, FightPay::credits(0), at(29)), "already latched");
        assert_eq!(dig_view(&store, a, at(30)).carried, 30);
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
        assert!(store.latch_raid(a, &ambush_trigger(505), RaidResult::Held, FightPay::credits(0), at(507)));
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
        assert_eq!(store.ops_snapshot(a, at(22)), (None, 20, OpsBase::default()));
        assert!(store.put_board(b, (5, 0), board(), at(30)));
        assert!(store.put_board(c, (5, 0), board(), at(40)));
        assert_eq!(store.len(), 2);
        assert!(store.board(b, (5, 0), at(41)).is_none(), "the board-only entry is evicted");
        assert_eq!(store.ops_snapshot(a, at(41)).1, 20, "the banked ore is kept");
    }

    /// §4d's step per 50 ore on seed 57 (d100 13): 20 ore (10 %) is not ambushed, 60 ore (15 %) is.
    #[test]
    fn ops_store_ambush_chance_steps_per_50_ore() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        pulled(&store, a, 57, 1);
        pulled(&store, b, 57, 2);
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

    /// The five buildings' numbers (Linux's oracle, BP-10a spec check; BP-16's rework): each flag
    /// moves only its own number, and no building moves heat or the ambush chance any more. The
    /// Gunsmith alone leaves ore as it is (its +10 is a roll row, `demo_roll`). The ids and the
    /// costs are pinned as literals.
    #[test]
    fn ops_base_numbers_match_the_oracle() {
        let plain = OpsBase::default();
        let all = OpsBase { radio_shack: true, hobbyist_benches: true, grease_pit: true, gunsmith: true, basic_smelter: true };
        let gun = OpsBase { gunsmith: true, ..OpsBase::default() };
        assert_eq!((plain.heat_range(1), plain.heat_range(2), plain.heat_range(3)), ((10, 20), (10, 25), (10, 30)));
        assert_eq!((all.heat_range(1), all.heat_range(2), all.heat_range(3)), ((10, 20), (10, 25), (10, 30)));
        let raid: Vec<(u32, u32)> = (1..=3).map(|n| (plain.raid_pct(n), all.raid_pct(n))).collect();
        assert_eq!(raid, vec![(5, 3), (10, 6), (15, 9)]);
        let ore: Vec<(u32, u32, u32, u32)> = (1..=3).map(|n| (plain.ore(n, 1), all.ore(n, 1), plain.ore(n, 3), all.ore(n, 3))).collect();
        assert_eq!(ore, vec![(20, 25, 40, 50), (40, 50, 80, 100), (60, 75, 120, 150)]);
        assert_eq!((gun.ore(3, 1), gun.ore(3, 3)), (60, 120));
        let ambush: Vec<(u32, u32)> = [30, 50, 60, 120, 450].into_iter().map(|l| (plain.haul_ambush_pct(l), all.haul_ambush_pct(l))).collect();
        assert_eq!(ambush, vec![(10, 10), (15, 15), (15, 15), (20, 20), (50, 50)]);
        let kept: Vec<(u32, u32)> = [30, 50, 60, 120].into_iter().map(|c| (plain.ambush_kept(c), all.ambush_kept(c))).collect();
        assert_eq!(kept, vec![(15, 23), (25, 38), (30, 45), (60, 90)]);
        assert_eq!((plain.heal_credits(), plain.heal_secs(), all.heal_credits(), all.heal_secs()), (40, 15, 20, 8));
        let ids = ["radio_shack", "hobbyist_benches", "grease_pit", "gunsmith", "basic_smelter", "barracks", ""];
        let costs: Vec<Option<u32>> = ids.into_iter().map(|id| BaseBuilding::from_id(id).map(BaseBuilding::cost)).collect();
        assert_eq!(costs, vec![Some(50), Some(120), Some(80), Some(100), Some(80), None, None]);
    }

    /// Seed 2's three-drill haul (120 ore), banked by a read at 30 s.
    fn banked_120(store: &DemoOpsStore, a: Uuid) {
        pulled(store, a, 2, 3);
        assert_eq!(store.ops_snapshot(a, at(30)).1, 120);
    }

    /// A build debits its cost and stands. Already built comes before short of ore, and a
    /// refusal moves nothing.
    #[test]
    fn ops_store_build_debits_the_bank_and_stands() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(0)), Err(BuildRefusal::NotEnoughOre), "no entry: nothing banked");
        banked_120(&store, a);
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(31)), Ok(()));
        let shack = OpsBase { radio_shack: true, ..OpsBase::default() };
        assert_eq!(store.ops_snapshot(a, at(32)), (None, 70, shack));
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(33)), Err(BuildRefusal::AlreadyBuilt));
        assert_eq!(store.build_op(a, BaseBuilding::Gunsmith, at(34)), Err(BuildRefusal::NotEnoughOre));
        assert_eq!(store.ops_snapshot(a, at(35)), (None, 70, shack));
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
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(30)), Ok(()), "the arrival banks 120, then pays 50");
        assert_eq!(store.ops_snapshot(a, at(30)).1, 70);
        store.start_op(a, (5, 0), board(), &site(), 7, None, None, at(40)).expect("start");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(45)), Err(BuildRefusal::OpOut), "before AlreadyBuilt");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(50)), Err(BuildRefusal::OpOut), "arrived at the contract");
        store.return_op(a, at(50)).expect("return");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(55)), Err(BuildRefusal::OpOut), "heading home");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(60)), Err(BuildRefusal::AlreadyBuilt), "home");
    }

    /// The base feeds the drill and the view alike (sync 2026-10-31, claim 4). The Benches and
    /// the Shack at the Coolant-Jockey's 115 on seed 2 (no raid on drills 1–5): heats 12, 17, 19,
    /// 21, 28 (97 after five, plain: BP-16 retired the Benches' cut), the view's raid chance
    /// 3 × (drills + 1) while even the bottom of the range is safe, and the 6th drill melts (116,
    /// 1 head; Linux's oracle). With the old cut the Benches stood at 86 after five. The Basic
    /// Smelter on tier 3: 50, 100, 150 a drill, so 50, 150, 300 on site, then `next_ore` 200.
    #[test]
    fn ops_store_base_feeds_the_drill_and_the_view() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        store.put_board(a, (5, 0), board(), at(0));
        store.set_base(a, OpsBase { radio_shack: true, hobbyist_benches: true, ..OpsBase::default() });
        on_site(&store, a, 1, 115, 2);
        let mut seen = vec![dig_view(&store, a, at(11)).next_raid_pct];
        for i in 1..=5 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
            seen.push(dig_view(&store, a, at(30)).next_raid_pct);
        }
        assert_eq!(seen, vec![Some(3), Some(6), Some(9), Some(12), Some(15), Some(18)]);
        let d = dig_view(&store, a, at(30));
        assert_eq!((d.heat, d.threshold, d.raid), (97, 115, None));
        assert_eq!(store.drill_op(a, at(31)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 1 }));
        assert_eq!(dig_view(&store, a, at(32)).heat, 116);
        store.put_board(b, (5, 0), board(), at(0));
        store.set_base(b, OpsBase { basic_smelter: true, ..OpsBase::default() });
        on_site(&store, b, 3, 100, 2);
        let mut piles = vec![];
        for i in 1..=3 {
            store.drill_op(b, at(10 + i)).expect("drill");
            piles.push(dig_view(&store, b, at(20)).ore_on_site);
        }
        assert_eq!(piles, vec![50, 150, 300]);
        assert_eq!(dig_view(&store, b, at(20)).next_ore, 200);
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

    /// The Grease-Pit (BP-16) on seed 57 (ambush d100 13) and a 120-ore haul: ambushed at 20 % with
    /// or without it, and a lost ambush (Retreat at 28 s) leaves 60 without the Pit, 90 with it;
    /// the arrivals at 33 s bank those.
    #[test]
    fn ops_store_grease_pit_keeps_three_quarters_of_a_lost_haul() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let pit = OpsBase { grease_pit: true, ..OpsBase::default() };
        store.put_board(b, (5, 0), board(), at(0));
        store.set_base(b, pit);
        pulled(&store, a, 57, 3);
        pulled(&store, b, 57, 3);
        for id in [a, b] {
            assert_eq!(store.pending_raid(id, at(25)).map(|p| p.trigger), Some(ambush_trigger(25)));
            assert!(store.latch_raid(id, &ambush_trigger(25), RaidResult::Retreat, FightPay::credits(0), at(28)));
        }
        assert_eq!((dig_view(&store, a, at(32)).carried, dig_view(&store, b, at(32)).carried), (60, 90));
        assert_eq!(store.ops_snapshot(a, at(33)), (None, 60, OpsBase::default()));
        assert_eq!(store.ops_snapshot(b, at(33)), (None, 90, pit));
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
        banked_120(&store, a);
        assert!(!store.moved_since(a, at(400), at(450)), "home at 30 s, nothing since");
        assert_eq!(store.build_op(a, BaseBuilding::RadioShack, at(500)), Ok(()));
        assert!(store.moved_since(a, at(500), at(501)), "the build");
    }

    /// BP-11a-2: a contract's fight is pending only once the team is on site, as a `Contract`
    /// trigger at the arrival; one latch applies, banks its pay and records the result.
    #[test]
    fn ops_store_contract_is_pending_once_on_site() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let lead = ContractStart { commander_id: COMMANDER, section_id: None };
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(lead), at(0)).expect("start");
        assert_eq!(store.pending_raid(a, at(9)), None, "still travelling");
        assert_eq!(
            store.pending_raid(a, at(11)),
            Some(PendingRaid {
                commander_id: COMMANDER,
                trigger: RaidTrigger { trigger_at: at(10), mission_id: "GUARD_DUTY", kind: BattleKind::Contract },
                section_id: None,
                crew: None,
            })
        );
        let t = RaidTrigger { trigger_at: at(10), mission_id: "GUARD_DUTY", kind: BattleKind::Contract };
        assert!(store.latch_raid(a, &t, RaidResult::Held, FightPay::credits(400), at(12)));
        assert_eq!(store.pending_raid(a, at(13)), None, "fought");
        assert!(!store.latch_raid(a, &t, RaidResult::Held, FightPay::credits(400), at(14)), "already latched");
        assert_eq!(store.ops_state(a, at(15)).credits_banked, 400);
        assert_eq!(store.op_view(a, at(15)).expect("op").fought, Some("won"));
    }

    /// Reviewer BP-10a F2 (BP-11a-2): at the cap, an entry holding only banked credits (no op, no
    /// ore, no building) is never evicted, so a new campaign's board goes uncached.
    #[test]
    fn ops_store_eviction_skips_banked_credits() {
        let store = DemoOpsStore::new(1);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let lead = ContractStart { commander_id: COMMANDER, section_id: None };
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(lead), at(0)).expect("start");
        let t = RaidTrigger { trigger_at: at(10), mission_id: "GUARD_DUTY", kind: BattleKind::Contract };
        assert!(store.latch_raid(a, &t, RaidResult::Held, FightPay::credits(400), at(12)));
        store.return_op(a, at(13)).expect("return");
        assert_eq!(
            store.ops_state(a, at(30)),
            OpsSnapshot { sites: Some(vec![]), op: None, ore_banked: 0, credits_banked: 400, base: OpsBase::default(), company: CompanyView::default(), crew_fight: None }
        );
        assert!(!store.put_board(b, (5, 0), board(), at(31)), "not cached");
        assert_eq!(store.len(), 1);
        assert!(store.board(a, (5, 0), at(32)).is_some(), "the banked entry is kept");
        assert!(store.board(b, (5, 0), at(32)).is_none());
    }

    /// BP-12a-1: the Section the store tests' sickbays belong to.
    const SECTION: Uuid = Uuid::from_u128(0x5EC);

    /// BP-12a-1: `GUARD_DUTY` (`site()`) started at 0 and its fight latched at 12 with `result`
    /// and `pay`.
    fn contract_latched(store: &DemoOpsStore, a: Uuid, result: RaidResult, pay: FightPay) {
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(LEAD), at(0)).expect("start");
        assert!(store.latch_raid(a, &guard_duty_arrival(), result, pay, at(12)));
    }

    /// A pay of `credits` with `wounded` into `SECTION`'s bay, cut to 8, and no loot.
    fn wounded_pay(credits: u32, wounded: u8) -> FightPay {
        FightPay { credits, section_id: SECTION, wounded, cap: Some(8), loot: None }
    }

    fn bay(wounded: u8, healing: u8, healed_at: Option<DateTime<Utc>>) -> SickBayView {
        SickBayView { section_id: SECTION, wounded, healing, healed_at: healed_at.map(ops_time) }
    }

    /// BP-12a-1 (Linux C1): a latch's wounded join the bay, then the bay is cut to the
    /// survivors, wounded first, then healing; a bay with nobody healing has no `healed_at`.
    #[test]
    fn take_wounded_cuts_the_bay_to_the_survivors() {
        let t = Some(at(15));
        let cases = [
            (SickBay { wounded: 0, healing: 0, healed_at: None }, 2, 8, SickBay { wounded: 2, healing: 0, healed_at: None }),
            (SickBay { wounded: 6, healing: 0, healed_at: None }, 1, 3, SickBay { wounded: 3, healing: 0, healed_at: None }),
            (SickBay { wounded: 1, healing: 4, healed_at: t }, 2, 4, SickBay { wounded: 0, healing: 4, healed_at: t }),
            (SickBay { wounded: 0, healing: 4, healed_at: t }, 0, 2, SickBay { wounded: 0, healing: 2, healed_at: t }),
            (SickBay { wounded: 0, healing: 2, healed_at: t }, 0, 0, SickBay { wounded: 0, healing: 0, healed_at: None }),
        ];
        for (before, w, cap, after) in cases {
            let mut got = before;
            got.take_wounded(w, cap);
            assert_eq!(got, after, "{before:?}.take_wounded({w}, {cap})");
        }
    }

    /// BP-12a-1: one applied latch puts its wounded in the Section's bay and its loot on the
    /// shelf; a refused latch with the same pay writes neither.
    #[test]
    fn latch_records_wounded_and_shelves_loot() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let jammer = LootDrop { quality_grade: QualityGrade::Standard, item_name: "Signal Jammer Module".into() };
        let pay = FightPay { credits: 0, section_id: SECTION, wounded: 1, cap: Some(7), loot: Some(jammer) };
        contract_latched(&store, a, RaidResult::Retreat, pay.clone());
        let want = CompanyView {
            sections: vec![bay(1, 0, None)],
            trophies: vec![Trophy { quality_grade: QualityGrade::Standard, item_name: "Signal Jammer Module".into() }],
        };
        assert_eq!(store.ops_state(a, at(13)).company, want);
        assert!(!store.latch_raid(a, &guard_duty_arrival(), RaidResult::Retreat, pay, at(14)), "already latched");
        assert_eq!(store.ops_state(a, at(15)).company, want, "a refused latch writes neither");
    }

    /// BP-12a-1: a heal takes 40 a head from the bank and moves the wounded to healing until 15 s
    /// after the heal; with nobody left wounded, a second heal is refused.
    #[test]
    fn heal_debits_40_a_head() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        contract_latched(&store, a, RaidResult::Held, wounded_pay(400, 3));
        assert_eq!(store.heal_op(a, SECTION, at(12)), Ok(()));
        let s = store.ops_state(a, at(12));
        assert_eq!(s.credits_banked, 280);
        assert_eq!(s.company.sections, vec![bay(0, 3, Some(at(27)))]);
        assert_eq!(store.heal_op(a, SECTION, at(12)), Err(HealRefusal::NobodyToHeal));
    }

    /// BP-16: with the Hobbyist Benches a heal takes 20 a head and the batch is back after 8 s
    /// (healed at 12, back at 20).
    #[test]
    fn heal_with_the_benches_debits_20_a_head_for_8_s() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        contract_latched(&store, a, RaidResult::Held, wounded_pay(400, 3));
        store.set_base(a, OpsBase { hobbyist_benches: true, ..OpsBase::default() });
        assert_eq!(store.heal_op(a, SECTION, at(12)), Ok(()));
        let s = store.ops_state(a, at(12));
        assert_eq!(s.credits_banked, 340);
        assert_eq!(s.company.sections, vec![bay(0, 3, Some(at(20)))]);
    }

    /// BP-12a-1: short of 40 a head, the heal is refused and writes nothing.
    #[test]
    fn heal_refuses_when_credits_are_short() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        contract_latched(&store, a, RaidResult::Held, wounded_pay(80, 3));
        assert_eq!(store.heal_op(a, SECTION, at(12)), Err(HealRefusal::NotEnoughCredits));
        let s = store.ops_state(a, at(13));
        assert_eq!(s.credits_banked, 80);
        assert_eq!(s.company.sections, vec![bay(3, 0, None)]);
    }

    /// BP-12a-1: a batch healed at 12 is back at 27. A second contract's latch at 26 (the team
    /// sent on at 20, arriving at 26) wounds one more, who can't be healed while the batch is
    /// healing; at 26 the bay holds both, at 27 only the new wounded.
    #[test]
    fn heal_returns_after_15_s_and_takes_one_batch_at_a_time() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        contract_latched(&store, a, RaidResult::Held, wounded_pay(400, 2));
        assert_eq!(store.heal_op(a, SECTION, at(12)), Ok(()));
        assert_eq!(store.next_op(a, &supply_raid(), 9, None, Some(LEAD), at(20)), Ok(()));
        let second = RaidTrigger { trigger_at: at(26), mission_id: "SUPPLY_RAID", kind: BattleKind::Contract };
        assert!(store.latch_raid(a, &second, RaidResult::Held, wounded_pay(0, 1), at(26)));
        assert_eq!(store.heal_op(a, SECTION, at(26)), Err(HealRefusal::AlreadyHealing));
        assert_eq!(store.ops_state(a, at(26)).company.sections, vec![bay(1, 2, Some(at(27)))]);
        assert_eq!(store.ops_state(a, at(27)).company.sections, vec![bay(1, 0, None)]);
    }

    /// BP-12a-1: the shelf keeps the newest 20, newest last.
    #[test]
    fn trophy_shelf_keeps_the_newest_20() {
        let mut shelf = Vec::new();
        for i in 0..21 {
            shelve(&mut shelf, LootDrop { quality_grade: QualityGrade::Basic, item_name: format!("t{i}") });
        }
        assert_eq!(shelf.len(), 20);
        assert_eq!(shelf[0].item_name, "t1");
        assert_eq!(shelf[19].item_name, "t20");
    }

    /// BP-12a-1 (Linux Q5), mirroring `ops_store_eviction_skips_banked_credits`, and BP-12a-2
    /// (Reviewer BP-12a-1 F1): at the cap, each case's entry is kept or evicted by another
    /// campaign's first `put_board` at 30 s. Every op is cleared by a wiped contract, with no pay,
    /// ore or building. A, a trophy: kept. B, a bay `{1, 0}`: kept. C, a default bay reached by
    /// insert (wounded 0, cap 8): evictable. D, a bay written back to the default (`{0, 2}`
    /// trimmed by a cap-0 latch): evictable. No read that settles runs between a case's last
    /// latch and the `put_board`, since a settle would drop C's and D's bays and void them. D is
    /// the case that tells the value test from a guard on the insert: its bay existed before
    /// the trim.
    #[test]
    fn make_room_keeps_a_sickbay_or_a_shelf() {
        let (x, y) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let a = |store: &DemoOpsStore| {
            let visor = LootDrop { quality_grade: QualityGrade::Basic, item_name: "Cracked Visor Helmet".into() };
            contract_latched(store, x, RaidResult::Wiped, FightPay { loot: Some(visor), ..FightPay::default() });
        };
        let b = |store: &DemoOpsStore| contract_latched(store, x, RaidResult::Wiped, wounded_pay(0, 1));
        let c = |store: &DemoOpsStore| contract_latched(store, x, RaidResult::Wiped, wounded_pay(0, 0));
        let d = |store: &DemoOpsStore| {
            contract_latched(store, x, RaidResult::Held, wounded_pay(80, 2));
            assert_eq!(store.heal_op(x, SECTION, at(12)), Ok(()));
            assert_eq!(store.next_op(x, &supply_raid(), 9, None, Some(LEAD), at(20)), Ok(()));
            let second = RaidTrigger { trigger_at: at(26), mission_id: "SUPPLY_RAID", kind: BattleKind::Contract };
            let cap0 = FightPay { section_id: SECTION, cap: Some(0), ..FightPay::default() };
            assert!(store.latch_raid(x, &second, RaidResult::Wiped, cap0, at(26)));
        };
        let cases: [(&str, &dyn Fn(&DemoOpsStore), bool); 4] = [("A", &a, true), ("B", &b, true), ("C", &c, false), ("D", &d, false)];
        let mut wrong = Vec::new();
        for (name, setup, kept) in cases {
            let store = DemoOpsStore::new(1);
            setup(&store);
            store.put_board(y, (5, 0), board(), at(30));
            if store.board(x, (5, 0), at(31)).is_some() != kept {
                wrong.push(name);
            }
        }
        assert!(wrong.is_empty(), "wrong cases: {wrong:?}");
    }

    /// BP-12a-2: a replace is refused while the op is out (400 banked, credits untouched), then
    /// when the bank is short (80 < 120), and for a campaign with no entry.
    #[test]
    fn begin_replace_refuses_while_out_then_when_short() {
        let store = DemoOpsStore::new(3);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        contract_latched(&store, a, RaidResult::Held, wounded_pay(400, 0));
        assert_eq!(store.begin_replace(a, 120, at(13)), Err(ReplaceRefusal::OpOut));
        assert_eq!(store.ops_state(a, at(13)).credits_banked, 400);
        contract_latched(&store, b, RaidResult::Wiped, FightPay::credits(80));
        assert_eq!(store.begin_replace(b, 120, at(13)), Err(ReplaceRefusal::NotEnoughCredits));
        assert_eq!(store.ops_state(b, at(13)).credits_banked, 80);
        assert_eq!(store.begin_replace(Uuid::from_u128(9), 120, at(13)), Err(ReplaceRefusal::NotEnoughCredits));
    }

    /// BP-12a-2: a refunded hold goes back to the bank; a kept one stays spent.
    #[test]
    fn finish_replace_refunds_or_keeps_the_debit() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        contract_latched(&store, a, RaidResult::Wiped, FightPay::credits(300));
        let hold = store.begin_replace(a, 120, at(13)).expect("begin");
        assert_eq!(store.ops_state(a, at(13)).credits_banked, 180);
        store.finish_replace(hold, true);
        assert_eq!(store.ops_state(a, at(14)).credits_banked, 300);
        let hold = store.begin_replace(a, 240, at(15)).expect("begin");
        assert_eq!(store.ops_state(a, at(15)).credits_banked, 60);
        store.finish_replace(hold, false);
        assert_eq!(store.ops_state(a, at(16)).credits_banked, 60);
    }

    /// BP-12a-2 (Linux item 13): a replace that debits the bank to 0 holds its entry against
    /// eviction until it finishes; once it has, the empty entry is the victim.
    #[test]
    fn make_room_skips_a_replace_in_flight() {
        let store = DemoOpsStore::new(1);
        let (x, y) = (Uuid::from_u128(1), Uuid::from_u128(2));
        contract_latched(&store, x, RaidResult::Wiped, FightPay::credits(120));
        let hold = store.begin_replace(x, 120, at(13)).expect("begin");
        assert!(!store.put_board(y, (5, 0), board(), at(30)), "not cached: x holds a replace");
        assert!(store.board(x, (5, 0), at(30)).is_some(), "x is kept");
        store.finish_replace(hold, false);
        assert!(store.put_board(y, (5, 0), board(), at(31)), "cached: x is now empty");
        assert!(store.board(x, (5, 0), at(31)).is_none(), "x was the victim");
    }

    /// BP-11a-3: the four contracts in `OPS_CONTRACT_MISSIONS` order at (8, 0) … (11, 0), each
    /// 10 s out (the first is `site()`'s pin), with `pool` as the board's contract pool (each
    /// pool hex 4 hexes and 8 s out).
    fn contract_board(pool: &[(i32, i32)]) -> Arc<OpsBoard> {
        let sites = OPS_CONTRACT_MISSIONS
            .iter()
            .enumerate()
            .map(|(i, m)| OpsSite {
                site_id: format!("contract:{m}"),
                kind: "contract",
                q: 8 + i as i32,
                r: 0,
                distance: 3 + i as u32,
                travel_secs: 10,
                tier: None,
                mission_id: Some(*m),
                name: Some(format!("name of {m}")),
            })
            .collect();
        Arc::new(OpsBoard { sites, contract_pool: pool.iter().map(|&(q, r)| (q, r, 4, 8)).collect() })
    }

    /// The `GUARD_DUTY` contract's trigger once the team has arrived at 10 s.
    fn guard_duty_arrival() -> RaidTrigger {
        RaidTrigger { trigger_at: at(10), mission_id: "GUARD_DUTY", kind: BattleKind::Contract }
    }

    /// The team sent to `GUARD_DUTY` on `b` at 0 s and its fight latched (won, 400) at 12 s.
    fn fought_guard_duty(store: &DemoOpsStore, a: Uuid, b: &Arc<OpsBoard>) {
        let lead = ContractStart { commander_id: COMMANDER, section_id: None };
        store.start_op(a, (5, 0), Arc::clone(b), &b.sites[0], 7, None, Some(lead), at(0)).expect("start");
        assert!(store.latch_raid(a, &guard_duty_arrival(), RaidResult::Held, FightPay::credits(400), at(12)));
    }

    fn shown(store: &DemoOpsStore, a: Uuid, now: DateTime<Utc>) -> Vec<OpsSite> {
        store.ops_state(a, now).sites.expect("the campaign has an entry")
    }

    fn guard_duty(sites: &[OpsSite]) -> Option<&OpsSite> {
        sites.iter().find(|s| s.site_id == "contract:GUARD_DUTY")
    }

    /// BP-11a-3: a fought contract is off the board from its latch (12 s) until 44 s later, and
    /// 45 s later it is back at a pool hex, with the same site id, mission and name.
    #[test]
    fn ops_store_finished_contract_returns_after_45s_elsewhere() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(3, 4), (4, 4)]);
        fought_guard_duty(&store, a, &b);
        for t in [13, 56] {
            assert_eq!(guard_duty(&shown(&store, a, at(t))), None, "off the board at {t} s");
            assert_eq!(store.site(a, &b, "contract:GUARD_DUTY", at(t)), None, "unknown at {t} s");
        }
        let sites = shown(&store, a, at(57));
        let back = guard_duty(&sites).expect("back at 57 s").clone();
        assert_eq!((back.mission_id, back.name.as_deref(), back.kind), (Some("GUARD_DUTY"), Some("name of GUARD_DUTY"), "contract"));
        assert!([(3, 4), (4, 4)].contains(&(back.q, back.r)), "a pool hex: {back:?}");
        assert_ne!((back.q, back.r), (8, 0), "elsewhere");
        assert_eq!((back.distance, back.travel_secs), (4, 8), "the pool hex's numbers");
        assert_eq!(sites.len(), 4);
        assert_eq!(store.site(a, &b, "contract:GUARD_DUTY", at(58)), Some(back));
        assert_eq!(store.op_view(a, at(58)).expect("op").fought, Some("won"), "the refresh leaves the op");
        assert_eq!(store.ops_state(a, at(58)).credits_banked, 400);
    }

    /// BP-11a-3: with every pool hex taken (the other three contracts and the old hex, where
    /// the team stands), the contract stays off the board.
    #[test]
    fn ops_store_refresh_with_no_free_hex_keeps_the_contract_off() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(8, 0), (9, 0), (10, 0), (11, 0)]);
        fought_guard_duty(&store, a, &b);
        assert_eq!(guard_duty(&shown(&store, a, at(57))), None);
        assert_eq!(guard_duty(&shown(&store, a, at(102))), None);
        assert_eq!(shown(&store, a, at(102)).len(), 3);
    }

    /// BP-11a-3: a latch that is refused (the contract was already fought) finishes nothing, so
    /// it can't take a returned contract off the board again.
    #[test]
    fn ops_store_stale_contract_latch_finishes_nothing() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(3, 4)]);
        fought_guard_duty(&store, a, &b);
        assert_eq!(guard_duty(&shown(&store, a, at(13))), None);
        assert!(guard_duty(&shown(&store, a, at(57))).is_some(), "back at 57 s");
        assert!(!store.latch_raid(a, &guard_duty_arrival(), RaidResult::Held, FightPay::credits(400), at(58)), "already fought");
        let back = guard_duty(&shown(&store, a, at(58))).cloned().expect("still on the board");
        assert_eq!((back.q, back.r), (3, 4));
        assert_eq!(store.ops_state(a, at(58)).credits_banked, 400);
    }

    /// BP-11a-3: a board written for a new gateway clears the contracts' own state, so the fought
    /// contract stands at the new board's own pin at once.
    #[test]
    fn ops_store_new_gateway_clears_the_contract_slots() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(3, 4)]);
        fought_guard_duty(&store, a, &b);
        assert_eq!(guard_duty(&shown(&store, a, at(13))), None);
        assert!(store.put_board(a, (0, 5), contract_board(&[]), at(14)));
        let pin = guard_duty(&shown(&store, a, at(15))).cloned().expect("back with the new board");
        assert_eq!((pin.q, pin.r), (8, 0));
    }

    /// BP-11a-3: the relic contract replaces Ore Extraction, in the same place.
    #[test]
    fn ops_contract_missions_are_the_relic_list() {
        assert_eq!(OPS_CONTRACT_MISSIONS, ["GUARD_DUTY", "CRASHED_SHIP_RAID", "SUPPLY_RAID", "BLACK_SITE_RAID"]);
    }

    const LEAD: ContractStart = ContractStart { commander_id: COMMANDER, section_id: None };

    /// BP-11a-4: a `SUPPLY_RAID` contract at (11, 0), 6 hexes and 12 s out (2 s per hex), so 3
    /// hexes (6 s) from `site()`'s (8, 0).
    fn supply_raid() -> OpsSite {
        OpsSite {
            site_id: "contract:SUPPLY_RAID".into(),
            kind: "contract",
            q: 11,
            r: 0,
            distance: 6,
            travel_secs: 12,
            tier: None,
            mission_id: Some("SUPPLY_RAID"),
            name: None,
        }
    }

    /// `GUARD_DUTY` (`site()`) started at 0, fought and won (400) at 12, then the team sent on to
    /// `supply_raid()` at 20: a 6 s leg, arriving at 26.
    fn next_to_supply_raid(store: &DemoOpsStore, a: Uuid) {
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(LEAD), at(0)).expect("start");
        assert!(store.latch_raid(a, &guard_duty_arrival(), RaidResult::Held, FightPay::credits(400), at(12)));
        assert_eq!(store.next_op(a, &supply_raid(), 9, None, Some(LEAD), at(20)), Ok(()));
    }

    /// BP-11a-4: `next_op`'s refusals in its order, each from a fresh store, each writing
    /// nothing; and an arrived, unfought contract is walked away from (Wiki Q1 (a)) and stays
    /// on the board.
    #[test]
    fn ops_store_next_refusals() {
        let a = Uuid::from_u128(1);
        let refused = |store: &DemoOpsStore, now: DateTime<Utc>, target: &OpsSite, want: NextRefusal| {
            let before = store.op_view(a, now);
            assert_eq!(store.next_op(a, target, 9, None, Some(LEAD), now), Err(want));
            assert_eq!(store.op_view(a, now), before, "{want:?} writes nothing");
        };
        let store = DemoOpsStore::new(2);
        refused(&store, at(0), &supply_raid(), NextRefusal::NoOp);

        let store = DemoOpsStore::new(2);
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(LEAD), at(0)).expect("start");
        store.return_op(a, at(11)).expect("return");
        refused(&store, at(12), &supply_raid(), NextRefusal::AlreadyHomeward);

        let store = DemoOpsStore::new(2);
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(LEAD), at(0)).expect("start");
        refused(&store, at(5), &supply_raid(), NextRefusal::NotArrived);

        let store = DemoOpsStore::new(2);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        refused(&store, at(12), &supply_raid(), NextRefusal::RaidPending);

        let store = DemoOpsStore::new(2);
        store.start_op(a, (5, 0), board(), &site(), 7, None, Some(LEAD), at(0)).expect("start");
        refused(&store, at(11), &site(), NextRefusal::SameSite);

        let store = DemoOpsStore::new(2);
        let b = contract_board(&[]);
        store.start_op(a, (5, 0), Arc::clone(&b), &b.sites[0], 7, None, Some(LEAD), at(0)).expect("start");
        assert!(store.pending_raid(a, at(11)).is_some(), "on site, unfought");
        assert_eq!(store.next_op(a, &b.sites[2], 9, None, Some(LEAD), at(11)), Ok(()));
        let pin = guard_duty(&shown(&store, a, at(12))).cloned().expect("still on the board");
        assert_eq!((pin.q, pin.r), (8, 0));
        assert_eq!(store.op_view(a, at(12)).expect("op").site_id, "contract:SUPPLY_RAID");
    }

    /// BP-11a-4: the next leg is the hop (3 hexes at 2 s) from the team's hex, and home from
    /// the new site is its own 12 s; nothing from the fought contract carries over.
    #[test]
    fn ops_store_next_leg_is_the_hop() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        next_to_supply_raid(&store, a);
        let op = store.op_view(a, at(21)).expect("op");
        assert_eq!((op.depart_at, op.arrive_at), (ops_time(at(20)), ops_time(at(26))));
        assert_eq!((op.at, op.site_id.as_str(), op.fought), (OpHex { q: 11, r: 0 }, "contract:SUPPLY_RAID", None));
        assert_eq!(store.ops_state(a, at(21)).credits_banked, 400);
        store.return_op(a, at(27)).expect("return");
        let op = store.op_view(a, at(28)).expect("op");
        assert_eq!((op.leg, op.arrive_at), (OpLeg::Homeward, ops_time(at(39))));
    }

    /// BP-11a-4: a next is a departure for the reaper's keep list.
    #[test]
    fn ops_store_next_counts_as_moving() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[]);
        fought_guard_duty(&store, a, &b);
        assert_eq!(store.next_op(a, &supply_raid(), 9, None, Some(LEAD), at(500)), Ok(()));
        assert!(store.moved_since(a, at(400), at(500)));
    }

    /// BP-11a-4, Reviewer BP-11a-2 CF2: the new contract's trigger is the new leg's arrival
    /// (26), so neither its own mission fought between the two contracts (21) nor the old
    /// contract's mission after the arrival (27) is its fight; its own mission after it (28) is.
    #[test]
    fn ops_store_next_contract_takes_no_fight_before_its_arrival() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        next_to_supply_raid(&store, a);
        assert_eq!(store.ops_state(a, at(21)).credits_banked, 400);
        assert_eq!(store.op_view(a, at(21)).expect("op").fought, None);
        assert_eq!(store.pending_raid(a, at(25)), None, "not arrived");
        let t = store.pending_raid(a, at(27)).expect("on site").trigger;
        assert_eq!(t, RaidTrigger { trigger_at: at(26), mission_id: "SUPPLY_RAID", kind: BattleKind::Contract });
        let between = record(21, "SUPPLY_RAID", "resolved", Some("Success"));
        let old = record(27, "GUARD_DUTY", "resolved", Some("Success"));
        assert_eq!(raid_result(std::slice::from_ref(&between), &t), Ok(None));
        assert_eq!(raid_result(std::slice::from_ref(&old), &t), Ok(None));
        let own = record(28, "SUPPLY_RAID", "resolved", Some("Success"));
        assert_eq!(raid_result(&[between, old, own], &t), Ok(Some(RaidResult::Held)));
    }

    /// BP-11a-4 (W4): standing on the fought `GUARD_DUTY`'s old hex, the team may walk to the
    /// same contract once it has returned elsewhere: `SameSite` compares hexes, not site ids.
    #[test]
    fn ops_store_next_to_the_refreshed_contract() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(3, 4)]);
        fought_guard_duty(&store, a, &b);
        let back = store.site(a, &b, "contract:GUARD_DUTY", at(58)).expect("back at 57 s");
        assert_eq!((back.q, back.r), (3, 4));
        assert_eq!(store.next_op(a, &back, 9, None, Some(LEAD), at(58)), Ok(()));
        let op = store.op_view(a, at(59)).expect("op");
        assert_eq!((op.site_id.as_str(), op.at, op.fought), ("contract:GUARD_DUTY", OpHex { q: 3, r: 4 }, None));
    }

    fn mission_hex(sites: &[OpsSite], mission: &str) -> Option<(i32, i32)> {
        sites.iter().find(|s| s.mission_id == Some(mission)).map(|s| (s.q, s.r))
    }

    /// BP-11a-4, Reviewer BP-11a-3 F1 (the op's hex alone decides). B (`CRASHED_SHIP_RAID`,
    /// (9, 0)) is fought at 12 and the team comes home at 23; A (`GUARD_DUTY`, (8, 0)) is started
    /// at 24 and fought at 34, the team still on site; nothing reads in between. At 80 both are
    /// due: A, first in board order, takes the one free hex (3, 4); B's only candidate is A's old
    /// hex under the team, so B stays off until the team is home. The fixture runs over 32 fresh
    /// stores, so a settle in `HashMap` order (B first in about half of them) fails it except
    /// with probability ≈ 2⁻³²: that negative is probabilistic.
    #[test]
    fn ops_store_refresh_in_board_order_never_lands_under_the_team() {
        const ROUNDS: u32 = 32;
        for round in 0..ROUNDS {
            let store = DemoOpsStore::new(2);
            let a = Uuid::from_u128(1);
            let b = contract_board(&[(8, 0), (3, 4)]);
            store.start_op(a, (5, 0), Arc::clone(&b), &b.sites[1], 7, None, Some(LEAD), at(0)).expect("start B");
            let tb = RaidTrigger { trigger_at: at(10), mission_id: "CRASHED_SHIP_RAID", kind: BattleKind::Contract };
            assert!(store.latch_raid(a, &tb, RaidResult::Held, FightPay::credits(400), at(12)), "round {round}");
            store.return_op(a, at(13)).expect("return");
            store.start_op(a, (5, 0), Arc::clone(&b), &b.sites[0], 8, None, Some(LEAD), at(24)).expect("start A");
            let ta = RaidTrigger { trigger_at: at(34), mission_id: "GUARD_DUTY", kind: BattleKind::Contract };
            assert!(store.latch_raid(a, &ta, RaidResult::Held, FightPay::credits(400), at(34)), "round {round}");
            for t in [80, 200] {
                let sites = shown(&store, a, at(t));
                assert_eq!(mission_hex(&sites, "GUARD_DUTY"), Some((3, 4)), "round {round}, {t} s: A takes the free hex");
                assert_eq!(mission_hex(&sites, "CRASHED_SHIP_RAID"), None, "round {round}, {t} s: B stays off");
            }
            store.return_op(a, at(201)).expect("return");
            shown(&store, a, at(212));
            let sites = shown(&store, a, at(213));
            assert_eq!(mission_hex(&sites, "CRASHED_SHIP_RAID"), Some((8, 0)), "round {round}: the team home, B returns");
        }
    }

    /// BP-11a-4 (the own hex alone decides): the team is home and the pool holds only the
    /// contract's own old hex, so it stays off the board.
    #[test]
    fn ops_store_refresh_never_returns_to_its_own_hex() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(8, 0)]);
        fought_guard_duty(&store, a, &b);
        store.return_op(a, at(13)).expect("return");
        assert!(store.op_view(a, at(24)).is_none(), "home at 23");
        for t in [57, 200] {
            assert_eq!(guard_duty(&shown(&store, a, at(t))), None, "off at {t} s");
        }
        assert_eq!(shown(&store, a, at(200)).len(), 3);
    }

    /// BP-17a: the Crew stream's pins (independent Mulberry32 oracle, Wiki): a raid on drill 1
    /// reads draws 0 and 1, the haul's ambush draws 24 and 25. Seed 191 holds (d100 27), 45 falls
    /// back with 2 heads (77), 505 is wiped out (97); seed 59's ambush falls back with 2 (73).
    /// On the ambush stream's offset the same seeds read 71, 44 and 32 (bands R, H, H).
    #[test]
    fn crew_fight_roll_pins() {
        assert_eq!(crew_fight_roll(191, 0), (RaidResult::Held, 0));
        assert_eq!(crew_fight_roll(45, 0), (RaidResult::Retreat, 2));
        assert_eq!(crew_fight_roll(505, 0), (RaidResult::Wiped, 8));
        assert_eq!(crew_fight_roll(59, DEMO_CREW_AMBUSH_DRAW), (RaidResult::Retreat, 2));
    }
}
