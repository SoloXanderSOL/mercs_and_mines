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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;
use sim_engine::constants::{MAX_STRESS, STRESS_BREAKING_POINT_MIN, STRESS_STRAINED_MIN};
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
/// A squad hire's price (BP-18a-2, §4e B6): a Section and a Commander together.
pub const DEMO_HIRE_CREDITS: u32 = 600;
/// The most squads (Sections) a member holds (BP-18a-2): the starter and one hire.
pub const DEMO_HIRE_CAP: usize = 2;
/// Wages (BP-18a-2, §4e B6): credits per squad per whole minute, settled on read, floor 0.
pub const DEMO_WAGE_CREDITS_PER_MIN: u32 = 5;
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
/// BP-20a-1 (D6): haul-ambush points per second the chosen way takes over the Fast one, inside
/// the same cap; a Fast trip's odds are unchanged.
pub const DEMO_AMBUSH_PCT_PER_DETOUR_SEC: u32 = 2;
/// BP-20a-2 (§4e C3): the chance, in percent, that a hazard hex hits a trip, per hex per leg.
/// Every store starts at 0 (`DemoOpsStore::set_hazard_pct`), so no test rolls unless it asks;
/// `main.rs` sets this one in production.
pub const DEMO_HAZARD_PCT: u32 = 35;
/// Stress a gas hit puts on the trip's leader (a contract's Commander, a dig's Foreman).
pub const DEMO_HAZARD_GAS_STRESS: u32 = 10;
/// Stress a ruins hit puts on a dig's Foreman (on a contract, ruins are a wound).
pub const DEMO_HAZARD_RUINS_STRESS: u32 = 5;
/// Morale a gas or ruins hit costs a Crew (it has no sickbay, so a road takes no heads).
pub const DEMO_HAZARD_MORALE: u32 = 5;
/// The hazard rolls come from `Rng::new(op_seed + this)` (wrapping), draw k for the k-th hex
/// rolled (out, then home). Draw k of `Rng::new(s)` runs on state `s + k × 0x6D2B_79F5`, so the
/// nearest other streams (the Crew fight's ahead, the ambush's behind) are 2²⁹ draws away; a
/// trip reads twice its way's hazards here, every other stream at most 26.
pub const DEMO_HAZARD_SEED_OFFSET: u32 = 0x2000_0000;
/// BP-20a-2: the leg a hazard hit came on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HazardLeg {
    Out,
    Home,
}

/// One hazard that hit (BP-20a-2) and what it cost: `stress` is the trip's leader's (the
/// Commander's on a contract, the Foreman's on a dig); `ore` came off the cart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HazardHit {
    pub hazard: Hazard,
    pub leg: HazardLeg,
    pub ore: u32,
    pub stress: u32,
    pub wounded: u32,
    pub morale: u32,
}

/// A landed trip that crossed a hazard (BP-20a-2): its hits in travel order, none when it went
/// clean. `seq` counts the entry's reports from 1, so the client shows each once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TripReport {
    pub seq: u32,
    pub site_id: String,
    pub route: RouteKind,
    pub hits: Vec<HazardHit>,
}

/// BP-20a-2 (W1): who a trip's hazards hit. A contract's fall on its Section and Commander; a
/// dig's, guarded or not, on its Crew and Foreman (a dig is the Crew's trip; the escort walks free).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HazardPayer {
    Section,
    Crew,
}

/// BP-20a-2 (W1, F-C, F-D): what one hit costs. A Section has no morale and a Crew no sickbay, so
/// a Section's hits are wounds (gas also stresses its Commander) and a Crew's are its Foreman's
/// stress and its morale; a crust that gives tips a quarter of the cart, on the way home only
/// (nothing rides out).
fn hazard_cost(hazard: Hazard, payer: HazardPayer, leg: HazardLeg, carried: u32) -> HazardHit {
    let mut hit = HazardHit { hazard, leg, ore: 0, stress: 0, wounded: 0, morale: 0 };
    match (payer, hazard) {
        (HazardPayer::Section, Hazard::Gas) => (hit.stress, hit.wounded) = (DEMO_HAZARD_GAS_STRESS, 1),
        (HazardPayer::Section, Hazard::Crust | Hazard::Ruins) => hit.wounded = 1,
        (HazardPayer::Crew, Hazard::Gas) => (hit.stress, hit.morale) = (DEMO_HAZARD_GAS_STRESS, DEMO_HAZARD_MORALE),
        (HazardPayer::Crew, Hazard::Ruins) => (hit.stress, hit.morale) = (DEMO_HAZARD_RUINS_STRESS, DEMO_HAZARD_MORALE),
        (HazardPayer::Crew, Hazard::Crust) => {
            if leg == HazardLeg::Home {
                hit.ore = carried / 4;
            }
        }
    }
    hit
}

/// BP-20a-2 (W3, F-J): a landed trip's hits. One d100 per hazard hex, out in path order then home
/// in travel order (the way reversed), on its own stream (`DEMO_HAZARD_SEED_OFFSET`); a roll at
/// most `pct` hits. `None` (no report) when the knob is 0 or the trip crossed no hazard; an empty
/// list is a clean trip. `carried` is what the cart holds at the landing; each crust hit on the
/// way home takes a quarter of what is still on it.
fn hazard_hits(op_seed: u32, pct: u32, payer: HazardPayer, out: &[Hazard], home: &[Hazard], carried: u32) -> Option<Vec<HazardHit>> {
    if pct == 0 || (out.is_empty() && home.is_empty()) {
        return None;
    }
    let mut rng = Rng::new(op_seed.wrapping_add(DEMO_HAZARD_SEED_OFFSET));
    let mut left = carried;
    let mut hits = Vec::new();
    let legs = out.iter().map(|h| (HazardLeg::Out, *h)).chain(home.iter().rev().map(|h| (HazardLeg::Home, *h)));
    for (leg, hazard) in legs {
        if rng.roll_d100() <= pct {
            let hit = hazard_cost(hazard, payer, leg, left);
            left -= hit.ore;
            hits.push(hit);
        }
    }
    Some(hits)
}

/// BP-20a-2 (W5): the vague warning on a way before setting off: each kind it crosses, once, in
/// the order met; never a count, odds or a price. `None` on a clean way.
pub fn hazard_warning(kinds: &[Hazard]) -> Option<String> {
    let mut words: Vec<&str> = Vec::new();
    for k in kinds {
        let w = match k {
            Hazard::Gas => "gas",
            Hazard::Crust => "soft crust",
            Hazard::Ruins => "ruins",
        };
        if !words.contains(&w) {
            words.push(w);
        }
    }
    (!words.is_empty()).then(|| words.join(", "))
}

/// BP-20a-2: the newest landed hazard trips an entry keeps (`OpsEntry::trip_reports`).
const TRIP_REPORTS_KEPT: usize = 3;
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
    /// One way, from the gateway: the Fast way's seconds (BP-20a-1, `board_routes`).
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
    /// BP-20a-1: the ways to every site hex on the board and every pool hex (so a returned
    /// contract finds its way), by hex. Sent through the route's `SiteView`, never from here.
    #[serde(skip)]
    pub routes: HashMap<(i32, i32), SiteRoutes>,
}

/// The hex's terrain if the hex is on the map and passable. A hex missing from the map is
/// impassable, as in `map_gen`.
fn passable(terrain: &HashMap<String, HexTerrain>, q: i32, r: i32) -> Option<&HexTerrain> {
    terrain.get(&format!("{q},{r}")).filter(|t| !t.is_impassable())
}

/// BP-20a-2 (§4e C3): a hazard hex by the word the player reads: toxic gas (Director
/// 2026-11-04), a crust that gives, ruins that fall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Hazard {
    Gas,
    Crust,
    Ruins,
}

/// BP-20a-2: a terrain's hazard, the one classifier (`is_hazard` reads it). Exhaustive on purpose
/// (no `_` arm): a new `HexTerrain` variant stops this compiling until someone says whether it is one.
pub fn hazard_of(t: &HexTerrain) -> Option<Hazard> {
    match t {
        HexTerrain::ToxicZone => Some(Hazard::Gas),
        HexTerrain::CooledMagma => Some(Hazard::Crust),
        HexTerrain::RuinedCity => Some(Hazard::Ruins),
        HexTerrain::AshenPlains
        | HexTerrain::ScorchedDesert
        | HexTerrain::CityState
        | HexTerrain::GunOutpost
        | HexTerrain::MagmaFlow
        | HexTerrain::VolcanicCaldera
        | HexTerrain::Mountain
        | HexTerrain::Ravine => None,
    }
}

/// BP-20a-1 (§4e C3): whether a route counts the hex as a hazard; `hazard_of` decides.
pub fn is_hazard(t: &HexTerrain) -> bool {
    hazard_of(t).is_some()
}

/// BP-20a-1: which way a trip goes. Fast orders paths by (seconds, hazards), Safe by (hazards,
/// seconds), so Fast is clean wherever a clean path ties (H3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteKind {
    Fast,
    Safe,
}

/// The order a route search keeps (H3).
fn route_key(kind: RouteKind, secs: u32, hazards: u32) -> (u32, u32) {
    match kind {
        RouteKind::Fast => (secs, hazards),
        RouteKind::Safe => (hazards, secs),
    }
}

/// One way to a site (BP-20a-1): its one-way seconds (each hex entered costs `travel_secs(1, …)`
/// of its terrain, the gateway excluded, the site included), the hazards crossed strictly
/// between the gateway and the site (the site's own terrain never counts, H1; never sent), and
/// the hexes entered, the site last.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RoutePath {
    pub secs: u32,
    #[serde(skip)]
    pub hazards: u32,
    pub path: Vec<(i32, i32)>,
}

/// A site's ways (BP-20a-1): Fast always; Safe only where it crosses fewer hazards.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteRoutes {
    pub fast: RoutePath,
    pub safe: Option<RoutePath>,
    /// BP-20a-2: the hazards each way crosses, in path order (`way_hazards`); `safe_kinds` is
    /// empty with no Safe way.
    pub fast_kinds: Vec<Hazard>,
    pub safe_kinds: Vec<Hazard>,
}

/// The six axial neighbour offsets (BP-20a-1; private, so no keeper file is touched).
const HEX_NEIGHBOURS: [(i32, i32); 6] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, -1), (-1, 1)];

/// One route search from `gateway` (BP-20a-1): a Dijkstra over `passable` hexes keyed by
/// `route_key(kind, …)`, the heap ordered by `(key, q, r)` ascending and a hex relaxed only by a
/// strictly lower key, so a tie goes to the predecessor popped first. It stops once every
/// target is settled. Returns each reachable target's path; the predecessor map is dropped.
fn route_search(terrain: &HashMap<String, HexTerrain>, gateway: (i32, i32), targets: &HashSet<(i32, i32)>, kind: RouteKind) -> HashMap<(i32, i32), RoutePath> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    // hex → (secs, hazards entered, predecessor); hazards include the hex's own terrain.
    let mut best: HashMap<(i32, i32), (u32, u32, (i32, i32))> = HashMap::new();
    let mut heap = BinaryHeap::new();
    best.insert(gateway, (0, 0, gateway));
    heap.push(Reverse((route_key(kind, 0, 0), gateway.0, gateway.1)));
    let mut left = targets.len();
    let mut settled: HashSet<(i32, i32)> = HashSet::new();
    while let Some(Reverse((key, q, r))) = heap.pop() {
        if left == 0 {
            break;
        }
        let Some(&(secs, hazards, _)) = best.get(&(q, r)) else { continue };
        if route_key(kind, secs, hazards) != key || !settled.insert((q, r)) {
            continue;
        }
        if targets.contains(&(q, r)) {
            left -= 1;
        }
        for (dq, dr) in HEX_NEIGHBOURS {
            let n = (q + dq, r + dr);
            let Some(t) = passable(terrain, n.0, n.1) else { continue };
            let (ns, nh) = (secs + travel_secs(1, t), hazards + u32::from(is_hazard(t)));
            let nk = route_key(kind, ns, nh);
            if best.get(&n).is_none_or(|&(s, h, _)| nk < route_key(kind, s, h)) {
                best.insert(n, (ns, nh, (q, r)));
                heap.push(Reverse((nk, n.0, n.1)));
            }
        }
    }
    let mut out = HashMap::new();
    for &target in targets {
        if !settled.contains(&target) {
            continue;
        }
        let Some(&(secs, hazards, _)) = best.get(&target) else { continue };
        let own = passable(terrain, target.0, target.1).is_some_and(is_hazard);
        let mut path = Vec::new();
        let mut at = target;
        while at != gateway {
            path.push(at);
            at = best[&at].2;
        }
        path.reverse();
        // Saturating: a target on the gateway itself entered nothing, its own hex included.
        out.insert(target, RoutePath { secs, hazards: hazards.saturating_sub(u32::from(own)), path });
    }
    out
}

/// BP-20a-2 (F-A): the hazards a way crosses, in path order: every hex entered but the site (the
/// last), the gateway's neighbour (`path[0]`) included. Its length is the way's `hazards` (H1).
fn way_hazards(terrain: &HashMap<String, HexTerrain>, way: &RoutePath) -> Vec<Hazard> {
    let entered = way.path.len().saturating_sub(1);
    way.path[..entered].iter().filter_map(|&(q, r)| passable(terrain, q, r).and_then(hazard_of)).collect()
}

/// BP-20a-1 (§4e C3): a Fast and a Safe way from `gateway` to each of `targets`, one bounded
/// search per `RouteKind`. A target with no path is absent; Safe is kept only where it crosses
/// fewer hazards than Fast.
pub fn board_routes(terrain: &HashMap<String, HexTerrain>, gateway: (i32, i32), targets: &[(i32, i32)]) -> HashMap<(i32, i32), SiteRoutes> {
    let wanted: HashSet<(i32, i32)> = targets.iter().copied().collect();
    let mut fast_ways = route_search(terrain, gateway, &wanted, RouteKind::Fast);
    let mut safe_ways = route_search(terrain, gateway, &wanted, RouteKind::Safe);
    let mut out = HashMap::new();
    for hex in wanted {
        let Some(fast) = fast_ways.remove(&hex) else { continue };
        let safe = safe_ways.remove(&hex);
        let safe = safe.filter(|s| s.hazards < fast.hazards);
        let (fast_kinds, safe_kinds) = (way_hazards(terrain, &fast), safe.as_ref().map_or_else(Vec::new, |s| way_hazards(terrain, s)));
        out.insert(hex, SiteRoutes { fast, safe, fast_kinds, safe_kinds });
    }
    out
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

fn dig_site(v: &MagmaVeinNode, distance: u32, travel_secs: u32) -> OpsSite {
    OpsSite {
        site_id: format!("dig:{},{}", v.q, v.r),
        kind: "dig",
        q: v.q,
        r: v.r,
        distance,
        travel_secs,
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
/// - **Routes (BP-20a-1, Variant B):** `board_routes` runs once over every candidate above (the
///   veins in range and the contract hexes), and a candidate with no path from the gateway
///   leaves before the tier picks and the shuffle. A site's `travel_secs` (and the pool's fourth
///   field) is its Fast way's seconds; `distance` stays the straight line. The board keeps the
///   ways of its site hexes and its pool hexes only.
pub fn ops_board(
    terrain: &HashMap<String, HexTerrain>,
    veins: &[MagmaVeinNode],
    gateway: (i32, i32),
    campaign_id: Uuid,
) -> OpsBoard {
    let (gq, gr) = gateway;
    let dist = |q: i32, r: i32| hex_axial_distance(gq, gr, q, r);

    let mut tier1: Vec<(u32, &MagmaVeinNode)> = veins
        .iter()
        .filter(|v| v.tier == 1)
        .filter_map(|v| {
            let d = dist(v.q, v.r);
            if d < OPS_DIG_TIER1_MIN_HEXES || d > OPS_DIG_TIER1_MAX_HEXES {
                return None;
            }
            passable(terrain, v.q, v.r).map(|_| (d, v))
        })
        .collect();

    let mut tier3: Vec<(u32, &MagmaVeinNode)> = veins
        .iter()
        .filter(|v| v.tier == 3)
        .filter_map(|v| {
            let d = dist(v.q, v.r);
            if d > OPS_DIG_TIER3_MAX_HEXES {
                return None;
            }
            passable(terrain, v.q, v.r).map(|_| (d, v))
        })
        .collect();

    let vein_hexes: HashSet<(i32, i32)> = veins.iter().map(|v| (v.q, v.r)).collect();
    let reach = OPS_CONTRACT_MAX_HEXES as i32;
    let mut candidates: Vec<(i32, i32, u32)> = Vec::new();
    for q in gq - reach..=gq + reach {
        for r in gr - reach..=gr + reach {
            let d = dist(q, r);
            if d < OPS_CONTRACT_MIN_HEXES || d > OPS_CONTRACT_MAX_HEXES {
                continue;
            }
            if hex_axial_distance(q, r, 0, 0) <= SAFE_ZONE_RADIUS || vein_hexes.contains(&(q, r)) {
                continue;
            }
            if passable(terrain, q, r).is_some() {
                candidates.push((q, r, d));
            }
        }
    }

    // BP-20a-1 (Variant B): one route search over every candidate; an unreachable one leaves
    // here, beside `passable`, before the tier picks and the contract shuffle.
    let targets: Vec<(i32, i32)> = tier1
        .iter()
        .chain(tier3.iter())
        .map(|(_, v)| (v.q, v.r))
        .chain(candidates.iter().map(|&(q, r, _)| (q, r)))
        .collect();
    let mut routes = board_routes(terrain, gateway, &targets);
    tier1.retain(|(_, v)| routes.contains_key(&(v.q, v.r)));
    tier3.retain(|(_, v)| routes.contains_key(&(v.q, v.r)));
    candidates.retain(|&(q, r, _)| routes.contains_key(&(q, r)));
    let fast_secs = |q: i32, r: i32| routes.get(&(q, r)).map_or(0, |w| w.fast.secs);

    tier1.sort_by_key(|(d, v)| (*d, v.q, v.r));
    let tier3 = tier3.into_iter().min_by_key(|(d, v)| (*d, v.q, v.r));

    let mut sites: Vec<OpsSite> = tier1
        .iter()
        .take(OPS_TIER1_SITES)
        .chain(tier3.iter())
        .map(|(d, v)| dig_site(v, *d, fast_secs(v.q, v.r)))
        .collect();

    candidates.sort_by_key(|(q, r, _)| (*q, *r));
    let contract_pool: Vec<(i32, i32, u32, u32)> = candidates.iter().map(|&(q, r, d)| (q, r, d, fast_secs(q, r))).collect();
    let n = candidates.len();
    let picks = OPS_CONTRACT_MISSIONS.len().min(n);
    let mut rng = Rng::new(ops_site_seed(campaign_id));
    for i in 0..picks {
        let j = i + rng.roll_int(0, (n - 1 - i) as u32) as usize;
        candidates.swap(i, j);
    }
    for (&(q, r, d), &mission_id) in candidates.iter().take(picks).zip(OPS_CONTRACT_MISSIONS.iter()) {
        sites.push(OpsSite {
            site_id: format!("contract:{mission_id}"),
            kind: "contract",
            q,
            r,
            distance: d,
            travel_secs: fast_secs(q, r),
            tier: None,
            mission_id: Some(mission_id),
            name: missions().get(mission_id).map(|m| m.name.clone()),
        });
    }
    let kept: HashSet<(i32, i32)> = sites.iter().map(|s| (s.q, s.r)).chain(contract_pool.iter().map(|p| (p.0, p.1))).collect();
    routes.retain(|hex, _| kept.contains(hex));
    OpsBoard { sites, contract_pool, routes }
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
    /// The nominal heads the Crew's rolls took on this dig (BP-19a), for its Foreman's stress.
    crew_heads_lost: u32,
}

impl DigState {
    /// What this dig owes its Foreman as it leaves its slot by `close` (BP-19a), with `carried`
    /// the ore that lands (0 when none does).
    fn settlement(&self, close: DigClose, carried: u32) -> ForemanDue {
        dig_settlement(self.drills, self.crew_heads_lost, self.ended_by, carried, close)
    }

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

/// A Foreman's heat-threshold cut while Strained (BP-19a): Command Presence at half.
pub const DEMO_STRAINED_THRESHOLD_CUT: u32 = 5;
/// A Foreman's heat-threshold cut at Breaking Point and when Shattered (BP-19a): Command Presence inverted.
pub const DEMO_BREAKING_THRESHOLD_CUT: u32 = 10;
/// Heat threshold per Foreman rank above 1 (BP-19a), as the Commander's +2 a rank on the roll.
pub const DEMO_FOREMAN_RANK_THRESHOLD: u32 = 2;
/// Foreman stress for a dig, once (BP-19a): canon's deployment penalty.
pub const DEMO_FOREMAN_DIG_STRESS: u32 = 10;
/// Foreman stress per drill (BP-19a): pushing the Crew costs nerves.
pub const DEMO_FOREMAN_DRILL_STRESS: u32 = 3;
/// Foreman stress per miner lost (BP-19a): canon's +5 a casualty, on the nominal heads a roll took.
pub const DEMO_FOREMAN_HEAD_STRESS: u32 = 5;
/// Foreman XP for a dig whose ore lands at base (BP-19a; canon Tier 2 Success).
pub const DEMO_FOREMAN_XP_LANDED: u32 = 25;
/// Foreman XP for a dig driven off: a meltdown, a lost fight (BP-19a; canon Tier 2 Defeat).
pub const DEMO_FOREMAN_XP_DRIVEN_OFF: u32 = 15;
/// Foreman XP for a dig left by choice with nothing landed (BP-19a).
pub const DEMO_FOREMAN_XP_LEFT: u32 = 8;
/// Stress a benched leader sheds per whole minute (BP-19b; canon's −5% a day, demo-sized to a minute).
pub const DEMO_BENCH_STRESS_PER_MIN: u32 = 5;

/// A leader's stress band (BP-19a; `Commander_Stress_System` §2): 0–30, 31–70, 71–99, and 100 or
/// the flag.
/// The cuts are `sim-engine`'s (`constants.rs`), the one copy; the Foreman bench's clear binds `STRESS_STRAINED_MAX` (BP-19b).
/// The roster sends it (BP-19c, demo-only) as `rested`, `strained`, `breaking_point` or `shattered`; the client
/// maps it to a word and never computes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StressBand {
    Rested,
    Strained,
    BreakingPoint,
    Shattered,
}

pub fn stress_band(stress: i16, is_shattered: bool) -> StressBand {
    if is_shattered || stress >= i16::from(MAX_STRESS) {
        StressBand::Shattered
    } else if stress >= i16::from(STRESS_BREAKING_POINT_MIN) {
        StressBand::BreakingPoint
    } else if stress >= i16::from(STRESS_STRAINED_MIN) {
        StressBand::Strained
    } else {
        StressBand::Rested
    }
}

/// What a dig's threshold reads of the member's first Foreman (BP-19a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForemanRead {
    pub stress: i16,
    pub is_shattered: bool,
    pub rank: i16,
    pub coolant_jockey: bool,
}

/// The meltdown threshold a Foreman gives a dig (BP-19a): 100, plus the Coolant-Jockey's 15
/// (half while Strained, none from Breaking Point), plus 2 a rank above 1 (none when Shattered),
/// less 5 while Strained and 10 from Breaking Point.
pub fn foreman_threshold(f: ForemanRead) -> u32 {
    let doctrine = DEMO_HEAT_THRESHOLD * DEMO_COOLANT_THRESHOLD_PCT / 100 - DEMO_HEAT_THRESHOLD;
    let rank = DEMO_FOREMAN_RANK_THRESHOLD * (f.rank.max(1) - 1) as u32;
    let (spec, bonus, cut) = match stress_band(f.stress, f.is_shattered) {
        StressBand::Rested => (doctrine, rank, 0),
        StressBand::Strained => (doctrine / 2, rank, DEMO_STRAINED_THRESHOLD_CUT),
        StressBand::BreakingPoint => (0, rank, DEMO_BREAKING_THRESHOLD_CUT),
        StressBand::Shattered => (0, 0, DEMO_BREAKING_THRESHOLD_CUT),
    };
    let spec = if f.coolant_jockey { spec } else { 0 };
    DEMO_HEAT_THRESHOLD + spec + bonus - cut
}

/// The meltdown threshold for a dig with a Rested rank-1 Foreman: `DEMO_HEAT_THRESHOLD`, or
/// ×1.15 with a Coolant-Jockey. The route reads the Foreman through `foreman_threshold`.
pub fn dig_threshold(coolant_jockey: bool) -> u32 {
    foreman_threshold(ForemanRead { stress: 0, is_shattered: false, rank: 1, coolant_jockey })
}

/// What finished digs owe the Foreman (BP-19a), summed in the entry until a route drains it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForemanDue {
    pub stress: u32,
    pub xp: u32,
}

/// What one ops read claims for the leaders (BP-19b), under one lock: the Foreman's drained
/// tally, the whole bench minutes since `bench_at`, whether the dig slot holds the Foreman, the
/// Commanders the store holds busy, and the drained hazard tally (BP-20a-2, `hazards`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeaderClaim {
    pub due: ForemanDue,
    pub bench_mins: u32,
    pub foreman_out: bool,
    pub busy: Vec<Uuid>,
    pub hazards: HazardDue,
}

impl ForemanDue {
    fn add(&mut self, other: ForemanDue) {
        self.stress = self.stress.saturating_add(other.stress);
        self.xp = self.xp.saturating_add(other.xp);
    }
}

/// What landed trips' hazards owe the roster (BP-20a-2), summed in the entry until a read drains
/// it (`take_leader_claim`): stress by Commander, wounded by Section, morale by Crew. A zero is
/// never inserted, so the tally is at its default exactly when nothing is owed (the keep list
/// tests it by value).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HazardDue {
    pub stress: BTreeMap<Uuid, u32>,
    pub wounded: BTreeMap<Uuid, u32>,
    pub morale: BTreeMap<Uuid, u32>,
}

impl HazardDue {
    /// Adds `other`, skipping zeros, saturating.
    fn add(&mut self, other: &HazardDue) {
        for (mine, theirs) in [(&mut self.stress, &other.stress), (&mut self.wounded, &other.wounded), (&mut self.morale, &other.morale)] {
            for (id, n) in theirs {
                if *n > 0 {
                    let v = mine.entry(*id).or_insert(0);
                    *v = v.saturating_add(*n);
                }
            }
        }
    }
}

/// How a dig left its slot (BP-19a): its homeward leg arrived, the Crew was wiped on site or on
/// the haul, the escorting Section was wiped, or `ops/next` moved the team on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigClose {
    Home,
    CrewWiped,
    GuardWiped,
    LeftForNext,
}

pub fn dig_settlement(drills: u32, crew_heads_lost: u32, ended_by: Option<OpEnd>, carried: u32, close: DigClose) -> ForemanDue {
    let stress = DEMO_FOREMAN_DIG_STRESS + DEMO_FOREMAN_DRILL_STRESS * drills + DEMO_FOREMAN_HEAD_STRESS * crew_heads_lost;
    let xp = match close {
        DigClose::CrewWiped => 0,
        DigClose::GuardWiped => DEMO_FOREMAN_XP_DRIVEN_OFF,
        DigClose::LeftForNext => DEMO_FOREMAN_XP_LEFT,
        DigClose::Home => match ended_by {
            Some(OpEnd::Pulled) if carried > 0 => DEMO_FOREMAN_XP_LANDED,
            Some(OpEnd::Meltdown | OpEnd::Retreat) => DEMO_FOREMAN_XP_DRIVEN_OFF,
            _ => DEMO_FOREMAN_XP_LEFT,
        },
    };
    ForemanDue { stress, xp }
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
    /// Fewer banked credits than `OpsBase::heal_credits()` × the wounded.
    NotEnoughCredits,
}

/// Why `begin_replace` refused (BP-12a-2), in the order it checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceRefusal {
    /// An op is out that holds the team: for a Crew, any op; for a Section, while that Section
    /// is busy (it leads a contract or guards the dig, BP-18a-1b-1). Replacements report at base.
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

/// Why `begin_hire` refused (BP-18a-2), in the order it checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HireRefusal {
    /// The member's Sections, plus a hire in flight, are already `DEMO_HIRE_CAP`.
    Cap,
    /// Fewer banked credits than `DEMO_HIRE_CREDITS` (also a campaign with no entry).
    NotEnoughCredits,
}

/// A squad hire's debit, held until its writes land (BP-18a-2). Only `finish_hire` consumes
/// it, so the refund is always the amount debited.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct HireHold {
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
    /// The chosen way's one-way time (BP-20a-1: the Fast way's, or the Safe way's on a Safe
    /// start); both legs take it, so the trip retraces its way (§4d: "each way").
    travel_secs: u32,
    /// The way the op went (BP-20a-1), fixed at the start; a next's op is always `Fast`.
    route: RouteKind,
    /// Seconds the chosen way takes over the Fast one (BP-20a-1): 0 on Fast. `pull_op` raises
    /// the haul's ambush by each of them, inside the cap (D6).
    detour_secs: u32,
    /// Drawn by `generate_seed()` at `ops/start`, never derived from the campaign id and never
    /// sent or logged. `drill_op` rolls from it (`drill_roll`), and `pull_op` (`ambush_d100`).
    op_seed: u32,
    /// The dig; `None` on a contract.
    dig: Option<DigState>,
    /// The contract (BP-11a-2): its mission, its lead and its fight's result; `None` on a dig.
    contract: Option<ContractState>,
    /// BP-20a-2: fixed at the start; `rewind_op` needs no new time field.
    hazards: OpHazards,
}

/// BP-20a-2 (W4): the hazards an op's legs cross, each in path order (gateway to site), and the
/// odds it rolls at, fixed at the start. A start's legs both cross its way; a next's hop crosses
/// nothing (a straight hop with no path), and its way home is the target's Fast way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct OpHazards {
    out: Vec<Hazard>,
    home: Vec<Hazard>,
    pct: u32,
}

/// BP-20a-2: who pays the op's hazards (block C's `HazardPayer`).
fn hazard_payer(op: &ActiveOp) -> HazardPayer {
    if op.contract.is_some() { HazardPayer::Section } else { HazardPayer::Crew }
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
    /// The squad the op sends into its fights (BP-18b-1): a contract's Section and Commander, or a dig's guard once one is named (`escort_at` set). Absent on a Crew-alone dig, so its keys stand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_id: Option<Uuid>,
    /// See section_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commander_id: Option<Uuid>,
    /// BP-20a-1: `"safe"` when the op went the Safe way; absent on Fast, so the op's keys stand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteKind>,
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

/// Which squad a return or a next acts for (BP-18a-1b-1): the contract slot a Section leads, or
/// the dig it guards; or the dig slot itself (the Crew's, guarded or not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpSquad {
    Section(Uuid),
    Dig,
}

/// Why `start_op` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartRefusal {
    /// An op is still out (an arrived homeward leg is cleared first, so this one is not home):
    /// a dig start while the dig slot is held, or an unnamed start while any slot is held,
    /// except a Crew-alone dig start, which needs only the dig slot empty (BP-18a-1b-1).
    Busy,
    /// The campaign has no entry and every entry at the cap holds an op, banked ore or a building.
    Full,
    /// A named start's Section is busy: it leads a contract or guards the dig (BP-18a-1b-1).
    SectionBusy,
    /// Another slot's contract has the target's mission, or a contract slot already holds the
    /// target's own key (BP-18a-1b-1; Reviewer BP-18a-1a F1).
    Taken,
    /// A named start's Commander is busy: it leads a contract or a guarded dig (BP-18a-1b-1).
    CommanderBusy,
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
    /// No squad was named and two or more ops are out (BP-18a-1b-1).
    SayWhich,
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
    /// The team is at a dig whose guard does not stand yet (BP-18a-1a): the Crew is alone
    /// (`escort_at` null) or the guard is still walking. Next moves the whole team, guard and
    /// Crew, so it waits for the guard; BP-17b's client offers Next only once the guard stands.
    GuardNotHere,
    /// The target stands on the team's own hex.
    SameSite,
    /// No squad was named and two or more ops are out (BP-18a-1b-1).
    SayWhich,
    /// The target is a dig and another slot holds the dig (BP-18a-1b-1).
    DigHeld,
    /// Another slot's contract has the target's mission, or another contract slot already holds
    /// the new op's key (BP-18a-1b-1; Reviewer BP-18a-1a F1).
    Taken,
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
    /// The Section sent is busy: it leads a contract (BP-18a-1b-1).
    SectionBusy,
    /// The Commander sent is busy: it leads a contract (BP-18a-1b-1).
    CommanderBusy,
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

/// One campaign's ops state: the cached board, the ops in flight (one slot each), when the team
/// last moved, the banked ore and the merc base.
struct OpsEntry {
    /// The gateway the board was computed for; another gateway is a miss.
    gateway: (i32, i32),
    board: Arc<OpsBoard>,
    touched: DateTime<Utc>,
    /// The dig slot (BP-18a-1a): the Crew's op, guarded or not. There is one Crew.
    dig: Option<ActiveOp>,
    /// The contract slots (BP-18a-1a), by the leading Section's id: `None` for a member with no
    /// Section, and for a bare op (no dig, no contract). A cleared op removes its key, so a key
    /// is present exactly while it holds an op (never a placeholder).
    contracts_out: BTreeMap<Option<Uuid>, ActiveOp>,
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
    /// Wages (BP-18a-2): the instant up to which this entry's squads are paid. Starts at the
    /// entry's creation and moves by whole minutes only (`settle_wages`), so a partial minute
    /// carries. Not on the keep list (it is never at a default), and `rewind_op` leaves it, so
    /// a route test that rewinds an op pays no wages.
    wages_at: DateTime<Utc>,
    /// The bench (BP-19b): the instant up to which this entry's idle leaders have shed stress.
    /// Starts at the entry's creation and moves by whole minutes only (`take_leader_claim`). Not
    /// on the keep list (it is never at a default), and `rewind_op` leaves it.
    bench_at: DateTime<Utc>,
    /// The squads (Sections) the wages count (BP-18a-2): 1 at creation (every demo member joins
    /// with one Section), then the route's recount under the launch lock (`finish_hire`).
    squads: u8,
    /// A squad hire is between `begin_hire` and `finish_hire` (BP-18a-2). Its credits ride in
    /// `credits_in_flight`, so `make_room` keeps the entry meanwhile.
    hire_in_flight: bool,
    /// The last Crew fight (BP-17a), written by the Crew's raid in `drill_op` and by
    /// `latch_crew`, cleared by the next `start_op` or `next_op`. Display only, not on the keep
    /// list: an evicted entry loses only the message.
    crew_fight: Option<CrewFightView>,
    /// What finished digs owe the member's first Foreman (BP-19a): written where a dig leaves
    /// its slot (`DigState::settlement`), drained by `take_foreman_due`, put back by
    /// `restore_foreman_due` after a failed write. On the keep list, tested by value.
    foreman_due: ForemanDue,
    /// What landed trips' hazards owe the roster (BP-20a-2): on the keep list, tested by value;
    /// drained by `take_leader_claim`, put back by `restore_hazard_due`.
    hazard_due: HazardDue,
    /// The landed hazard trips (BP-20a-2): newest last, at most 3; display only, off the keep
    /// list, never cleared by a start.
    trip_reports: Vec<TripReport>,
    /// The last report's `seq` (BP-20a-2): 0 until the first.
    trip_seq: u32,
}

impl OpsEntry {
    /// Every op out, in slot order: the dig, then the contract slots by key.
    fn ops(&self) -> impl Iterator<Item = &ActiveOp> {
        self.dig.iter().chain(self.contracts_out.values())
    }

    /// Whether any slot holds an op.
    fn any_op(&self) -> bool {
        self.dig.is_some() || !self.contracts_out.is_empty()
    }

    /// Every held slot's key, in slot order (the dig, then the contracts by key).
    fn slot_keys(&self) -> Vec<SlotKey> {
        self.dig.iter().map(|_| SlotKey::Dig).chain(self.contracts_out.keys().map(|k| SlotKey::Contract(*k))).collect()
    }

    /// The primary op's slot (BP-18a-1a): the dig, else the first contract by key. It is the op
    /// the body's `op` shows (`view`), the one definition of "primary" (Reviewer BP-18a-1a F3).
    fn primary_key(&self) -> Option<SlotKey> {
        self.slot_keys().into_iter().next()
    }

    fn slot(&self, key: SlotKey) -> Option<&ActiveOp> {
        match key {
            SlotKey::Dig => self.dig.as_ref(),
            SlotKey::Contract(k) => self.contracts_out.get(&k),
        }
    }

    fn slot_mut(&mut self, key: SlotKey) -> Option<&mut ActiveOp> {
        match key {
            SlotKey::Dig => self.dig.as_mut(),
            SlotKey::Contract(k) => self.contracts_out.get_mut(&k),
        }
    }

    /// The dig's state while a Section guards it, walking or standing (`escort_at` set). A
    /// Crew-alone dig records the member's home Section and a Commander for the run-alive check
    /// only, so neither is busy on it (BP-18a-1b-1, spec check A4).
    fn guarded_dig(&self) -> Option<&DigState> {
        self.dig.as_ref().and_then(|op| op.dig.as_ref()).filter(|d| d.escort_at.is_some())
    }

    /// A Section is busy (BP-18a-1b-1) while a contract slot is keyed by it, or while it guards
    /// the dig. Defined once; every site calls it.
    fn section_busy(&self, s: Uuid) -> bool {
        self.contracts_out.contains_key(&Some(s)) || self.guarded_dig().is_some_and(|d| d.section_id == Some(s))
    }

    /// The Commanders the store holds busy (BP-19b, extracted from `commander_busy`): every
    /// contract's lead, then the guarded dig's. A Crew-alone dig's Commander is not busy (A4).
    fn busy_commander_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.contracts_out.values().filter_map(|op| op.contract.as_ref().map(|k| k.commander_id)).collect();
        ids.extend(self.guarded_dig().map(|d| d.commander_id));
        ids
    }

    /// A Commander is busy (BP-18a-1b-1) while it leads a contract, or a guarded dig. Defined
    /// once (`busy_commander_ids`, BP-19b); every site calls it.
    fn commander_busy(&self, c: Uuid) -> bool {
        self.busy_commander_ids().contains(&c)
    }

    /// Whether a slot other than `except` holds a contract of `mission` (BP-18a-1b-1).
    fn mission_held(&self, mission: &str, except: Option<SlotKey>) -> bool {
        self.slot_keys()
            .into_iter()
            .filter(|k| Some(*k) != except)
            .filter_map(|k| self.slot(k))
            .any(|op| op.contract.as_ref().is_some_and(|c| c.mission_id == mission))
    }

    /// The slot a return or a next acts on (BP-18a-1b-1). `None`: no op → `NoOp`, one op → that
    /// op, two or more → `SayWhich`. `Dig`: the dig slot, else `NoOp`. `Section(s)`: the contract
    /// slot keyed by `s`, else the dig `s` guards, else `NoOp`. A `None`-keyed contract (a member
    /// with no Section) is selected only by `None` with one op out; production can't hold one
    /// beside another op (every demo member has a starter Section).
    fn select(&self, squad: Option<OpSquad>) -> Result<SlotKey, Select> {
        match squad {
            None => {
                let keys = self.slot_keys();
                if keys.len() > 1 {
                    return Err(Select::SayWhich);
                }
                keys.into_iter().next().ok_or(Select::NoOp)
            }
            Some(OpSquad::Dig) => self.dig.as_ref().map(|_| SlotKey::Dig).ok_or(Select::NoOp),
            Some(OpSquad::Section(s)) => {
                if self.contracts_out.contains_key(&Some(s)) {
                    Ok(SlotKey::Contract(Some(s)))
                } else if self.guarded_dig().is_some_and(|d| d.section_id == Some(s)) {
                    Ok(SlotKey::Dig)
                } else {
                    Err(Select::NoOp)
                }
            }
        }
    }

    /// Clears a slot: the dig's goes to `None`, a contract's key is removed.
    fn clear_slot(&mut self, key: SlotKey) {
        match key {
            SlotKey::Dig => self.dig = None,
            SlotKey::Contract(k) => {
                self.contracts_out.remove(&k);
            }
        }
    }

    /// Puts `op` in its own slot (`slot_of`). The slot must be empty (Reviewer BP-18a-1a F1):
    /// every caller's busy and taken checks refuse an occupied target first, so the assertion
    /// only catches a missed check in a test build; release relies on those checks.
    fn insert_op(&mut self, op: ActiveOp) {
        debug_assert!(self.slot(slot_of(&op)).is_none(), "insert_op: slot already held");
        match slot_of(&op) {
            SlotKey::Dig => self.dig = Some(op),
            SlotKey::Contract(k) => {
                self.contracts_out.insert(k, op);
            }
        }
    }

    /// Takes `cost` from the banked credits, or refuses (`false`) when short, never saturating
    /// (BP-12a-1, the first credit debit). Callers hold the store lock.
    fn debit_credits(&mut self, cost: u32) -> bool {
        if self.credits_banked < cost {
            return false;
        }
        self.credits_banked -= cost;
        true
    }

    /// Pays the wages due since `wages_at` (BP-18a-2): `DEMO_WAGE_CREDITS_PER_MIN` per squad per
    /// whole minute, saturating at 0 (a wage the bank can't cover is forgiven, never owed). The
    /// anchor moves by whole minutes only, so a partial minute carries; a clock at or behind the
    /// anchor pays nothing. A clock test under the lock: it reads nothing outside the entry.
    fn settle_wages(&mut self, now: DateTime<Utc>) {
        let mins = (now - self.wages_at).num_minutes();
        if mins <= 0 {
            return;
        }
        self.wages_at += Duration::minutes(mins);
        let owed = u32::try_from(mins).unwrap_or(u32::MAX).saturating_mul(DEMO_WAGE_CREDITS_PER_MIN).saturating_mul(u32::from(self.squads));
        self.credits_banked = self.credits_banked.saturating_sub(owed);
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
    /// (`settle_sickbay`, BP-12a-1). BP-18a-1a: every slot is settled (the dig, then the
    /// contracts by key), a cleared one removes its key, and an arrival stamps
    /// `moved_at = max(moved_at, arrived_at)`, so an older arrival settled after a newer
    /// movement never moves the stamp back (the same as today's write while one op is out).
    /// BP-18a-2: the wages (`settle_wages`) first of all, so pay a latch adds after this settle
    /// is never taken for minutes that passed before it.
    fn settle_arrival(&mut self, now: DateTime<Utc>) {
        self.settle_wages(now);
        self.settle_contracts(now);
        self.settle_sickbay(now);
        let mut arrivals: Vec<(DateTime<Utc>, u32)> = Vec::new();
        if let Some((arrived_at, carried)) = self.dig.as_mut().and_then(|op| settle_op(op, now)) {
            if let Some(op) = self.dig.take() {
                // BP-20a-2: the trip's hazards first, so the cart is cut before it banks.
                let carried = self.land_hazards(&op, carried);
                // BP-19a: the dig's homeward leg arrived (`DigClose::Home`), with what it carried.
                if let Some(d) = op.dig.as_ref() {
                    self.foreman_due.add(d.settlement(DigClose::Home, carried));
                }
                arrivals.push((arrived_at, carried));
            }
        }
        let mut landed: Vec<(Option<Uuid>, DateTime<Utc>, u32)> = Vec::new();
        for (key, op) in self.contracts_out.iter_mut() {
            if let Some((arrived_at, carried)) = settle_op(op, now) {
                landed.push((*key, arrived_at, carried));
            }
        }
        for (key, arrived_at, carried) in landed {
            if let Some(op) = self.contracts_out.remove(&key) {
                let carried = self.land_hazards(&op, carried);
                arrivals.push((arrived_at, carried));
            }
        }
        for (arrived_at, carried) in arrivals {
            self.ore_banked = self.ore_banked.saturating_add(carried);
            self.moved_at = self.moved_at.max(Some(arrived_at));
        }
    }

    /// BP-20a-2 (W3): a landing op's hazards, rolled at the landing (`hazard_hits`). A dig's
    /// Foreman stress goes into `foreman_due`; the rest of the trip's costs into `hazard_due`
    /// (morale by the Crew; a contract's stress by its Commander and wounds by its Section, the
    /// ids taken as `next_op_for`'s `moving` takes them); the report is kept, newest last.
    /// Returns what still rides on the cart. No hazard crossed, or the knob at 0: no report.
    fn land_hazards(&mut self, op: &ActiveOp, carried: u32) -> u32 {
        let payer = hazard_payer(op);
        let Some(hits) = hazard_hits(op.op_seed, op.hazards.pct, payer, &op.hazards.out, &op.hazards.home, carried) else {
            return carried;
        };
        let sum = |f: fn(&HazardHit) -> u32| hits.iter().fold(0u32, |acc, h| acc.saturating_add(f(h)));
        let (ore, stress, wounded, morale) = (sum(|h| h.ore), sum(|h| h.stress), sum(|h| h.wounded), sum(|h| h.morale));
        let mut trip = HazardDue::default();
        match payer {
            HazardPayer::Crew => {
                self.foreman_due.add(ForemanDue { stress, xp: 0 });
                if let Some(d) = op.dig.as_ref() {
                    trip.morale.insert(d.crew_id, morale);
                }
            }
            HazardPayer::Section => {
                let lead = op.contract.as_ref().map(|c| (c.commander_id, c.section_id)).or_else(|| op.dig.as_ref().map(|d| (d.commander_id, d.section_id)));
                if let Some((commander_id, section_id)) = lead {
                    trip.stress.insert(commander_id, stress);
                    if let Some(s) = section_id {
                        trip.wounded.insert(s, wounded);
                    }
                }
            }
        }
        self.hazard_due.add(&trip);
        self.trip_seq += 1;
        self.trip_reports.push(TripReport { seq: self.trip_seq, site_id: op.site_id.clone(), route: op.route, hits });
        if self.trip_reports.len() > TRIP_REPORTS_KEPT {
            self.trip_reports.remove(0);
        }
        carried.saturating_sub(ore)
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
            // BP-18a-1a: never on any slot's hex (with one op out, the op's hex as before).
            let at: HashSet<(i32, i32)> = self.ops().map(|op| op.at).collect();
            let free: Vec<(i32, i32, u32, u32)> = self
                .board
                .contract_pool
                .iter()
                .copied()
                .filter(|&(q, r, _, _)| Some((q, r)) != own && !shown.contains(&(q, r)) && !at.contains(&(q, r)))
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

    /// The primary op (`primary_key`, the one definition: the dig, else the first contract by
    /// key) as the client sees it.
    fn view(&self, now: DateTime<Utc>) -> Option<ActiveOpView> {
        self.primary_key().and_then(|k| self.view_of(k, now))
    }

    /// One slot's op as the client sees it (BP-18a-1b-2): `view` shows the primary's, the body's
    /// `more_ops` every other slot's. One builder, so the two shapes can't drift.
    fn view_of(&self, key: SlotKey, now: DateTime<Utc>) -> Option<ActiveOpView> {
        self.slot(key).map(|op| {
            let (section_id, commander_id) = match (&op.contract, &op.dig) {
                (Some(c), _) => (c.section_id, Some(c.commander_id)),
                (None, Some(d)) if d.escort_at.is_some() => (d.section_id, Some(d.commander_id)),
                _ => (None, None),
            };
            ActiveOpView {
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
                section_id,
                commander_id,
                route: (op.route == RouteKind::Safe).then_some(RouteKind::Safe),
            }
        })
    }
}

/// A slot of `OpsEntry` (BP-18a-1a): the dig's, or a contract's by the leading Section's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotKey {
    Dig,
    Contract(Option<Uuid>),
}

/// Why `OpsEntry::select` found no slot (BP-18a-1b-1).
enum Select {
    NoOp,
    SayWhich,
}

/// The squad an op takes (BP-18a-1b-1): `(commander, section)` of a contract, or of an
/// escorted dig; `None` for a Crew-alone dig or a bare op.
fn lead_of(dig: Option<&DigStart>, contract: Option<&ContractStart>) -> Option<(Uuid, Option<Uuid>)> {
    contract.map(|c| (c.commander_id, c.section_id)).or_else(|| dig.filter(|d| d.escort).map(|d| (d.commander_id, d.section_id)))
}

/// The slot an op lives in (BP-18a-1a): the dig slot for a dig, otherwise the contract slot of
/// its leading Section (`None` for a member with no Section, and for a bare op).
fn slot_of(op: &ActiveOp) -> SlotKey {
    if op.dig.is_some() {
        SlotKey::Dig
    } else {
        SlotKey::Contract(op.contract.as_ref().and_then(|c| c.section_id))
    }
}

/// One slot's settle (BP-18a-1a, `settle_arrival`'s body per op): first fires a haul's ambush
/// once its time has come, then reports a homeward leg that has arrived with no battle waiting
/// and none to come as `(arrived_at, carried)`, for the caller to bank and clear.
fn settle_op(op: &mut ActiveOp, now: DateTime<Utc>) -> Option<(DateTime<Utc>, u32)> {
    if let Some(d) = op.dig.as_mut() {
        if d.raid.is_none() && d.ambush_at.is_some_and(|t| now >= t) {
            d.raid = Some(RaidTrigger { trigger_at: now, mission_id: DEMO_AMBUSH_MISSION, kind: BattleKind::Ambush });
        }
    }
    let waiting = op.dig.as_ref().is_some_and(|d| d.raid.is_some() || d.ambush_at.is_some());
    (op.leg == OpLeg::Homeward && now >= op.arrive_at && !waiting).then(|| (op.arrive_at, op.dig.as_ref().map_or(0, |d| d.carried)))
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

/// One op beside the primary, in the body's `more_ops` (BP-18a-1b-2): the Section whose contract
/// slot it is (`null` for a `None` key, which production can't hold beside another op) and the op
/// as the client sees it, the same shape as `active_op`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MoreOp {
    pub section_id: Option<Uuid>,
    pub op: ActiveOpView,
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
/// site takes the chosen way's seconds, the site's `travel_secs` (its Fast way) plus
/// `detour_secs` (BP-20a-1: 0 on Fast), crossing `hazards` (BP-20a-2). A dig starts fresh and a contract gets a fresh
/// `ContractState` with no result, so nothing carries over from an earlier op.
#[allow(clippy::too_many_arguments)]
fn new_op(site: &OpsSite, op_seed: u32, dig: Option<DigStart>, contract: Option<ContractStart>, route: RouteKind, detour_secs: u32, hazards: OpHazards, leg_secs: u32, now: DateTime<Utc>) -> ActiveOp {
    ActiveOp {
        site_id: site.site_id.clone(),
        at: (site.q, site.r),
        kind: site.kind,
        leg: OpLeg::Outbound,
        depart_at: now,
        arrive_at: now + Duration::seconds(i64::from(leg_secs)),
        travel_secs: site.travel_secs + detour_secs,
        route,
        detour_secs,
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
            crew_heads_lost: 0,
        }),
        contract: contract.and_then(|c| site.mission_id.map(|mission_id| ContractState { mission_id, commander_id: c.commander_id, section_id: c.section_id, result: None })),
        hazards,
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
///   write) write a slot. The arrival adds to the banked ore and `build_op` debits it; only
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
///   `travel_secs` after the pull: an accepted overcount;
/// - the eviction takes only an entry with no op in any slot, no banked ore, no banked credits, no
///   building, no sickbay off the default, no trophy, no replace in flight (BP-12a-1,
///   BP-12a-2), no undrained Foreman tally (BP-19a, `foreman_due` off the default) and no
///   undrained hazard tally (BP-20a-2, tested by value);
/// - **the Foreman's tally (BP-19a):** every place a dig leaves its slot (the homeward arrival in
///   `settle_arrival`, a Crew wipe in `drill_op` or `latch_crew`, a guard wipe in `latch_raid`,
///   a dig left by `next_op_for`) adds `DigState::settlement` into `foreman_due` in the same
///   critical section, before the slot clears; only `take_foreman_due` drains it;
/// - an entry is dropped only by `remove` (a read of a campaign that is no longer `Active`,
///   the reaper's retire) and the reaper's prune (`retain_campaigns`).
///
/// **Keyed per campaign with one gateway, one dig slot (`dig`, the Crew's) and the contract
/// slots (`contracts_out`, by the leading Section's id; `None` for a member with no Section, or a
/// bare op).** A cleared op removes its key. The primary op (the dig, else the first contract by
/// key) is the one the body's `op` shows. **BP-18a-1b-1: an unnamed start lands only with no op
/// out, or as a Crew-alone dig in the empty dig slot; a named start (`start_op_for`) lands beside
/// other ops when its Section and Commander are not busy and its contract is not taken. Return
/// and next act on the op their squad selects (`OpsEntry::select`): unnamed, the one op out, or
/// `SayWhich` with two. Every busy and taken check is the store's, under the lock.** Two
/// battles' losses always fall on different rows (a Section is busy on one op, and the Crew
/// only on the dig). Correct only while `DEMO_PLAYER_CAP = 1`
/// (`routes/campaign.rs`); a second member per campaign needs a per-wallet key.
pub struct DemoOpsStore {
    inner: Mutex<HashMap<Uuid, OpsEntry>>,
    cap: usize,
    evict_warn: RateWarn,
    /// The hazard chance in percent per hex per leg (BP-20a-2), copied onto each op at its start.
    /// Every store starts at 0, so no test rolls unless it asks; production sets its own once,
    /// in `main.rs`, before the state is shared.
    hazard_pct: u32,
}

impl Default for DemoOpsStore {
    fn default() -> Self {
        Self::new(DEMO_OPS_CAP)
    }
}

impl DemoOpsStore {
    pub fn new(cap: usize) -> Self {
        Self { inner: Mutex::new(HashMap::new()), cap, evict_warn: RateWarn::new(), hazard_pct: 0 }
    }

    /// Sets the hazard chance (BP-20a-2) for the ops started from now on.
    pub fn set_hazard_pct(&mut self, pct: u32) {
        self.hazard_pct = pct;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, OpsEntry>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Makes room for a new entry: below the cap, nothing; at it, evicts the least recently
    /// touched entry that holds no op in any slot (BP-18a-1a), no banked ore, no banked credits, no building, no sickbay
    /// off the default, no trophy, no replace in flight (BP-12a-1, BP-12a-2), no undrained
    /// Foreman tally (BP-19a, tested by value) and no undrained hazard tally (BP-20a-2, tested by
    /// value), and journals
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
            .filter(|(_, e)| e.dig.is_none() && e.contracts_out.is_empty() && e.ore_banked == 0 && e.credits_banked == 0 && e.base == OpsBase::default() && e.sickbay.values().all(|b| *b == SickBay::default()) && e.trophies.is_empty() && e.credits_in_flight == 0 && e.foreman_due == ForemanDue::default() && e.hazard_due == HazardDue::default())
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
        inner.insert(campaign_id, OpsEntry { gateway, board, touched: now, dig: None, contracts_out: BTreeMap::new(), moved_at: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), contracts: HashMap::new(), sickbay: HashMap::new(), trophies: Vec::new(), credits_in_flight: 0, crew_fight: None, wages_at: now, bench_at: now, squads: 1, hire_in_flight: false, foreman_due: ForemanDue::default(), hazard_due: HazardDue::default(), trip_reports: Vec::new(), trip_seq: 0 });
        true
    }

    /// `start_op_for` with no squad named (`named: false`), the Fast way: today's start.
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
        self.start_op_for(campaign_id, gateway, board, site, op_seed, dig, contract, false, RouteKind::Fast, now)
    }

    /// Starts the outbound leg to `site`: `depart_at = now`, `arrive_at = now + the chosen
    /// way's seconds` (BP-20a-1): the site's hex is looked up in the entry's board routes, and
    /// `route` Safe with a Safe way there takes the Safe way's seconds, its detour over the Fast
    /// one stored for the haul's ambush; anything else takes the Fast way's (stored as `Fast`). A
    /// board with no ways for the hex (the store tests' own boards) takes the site's `travel_secs`.
    /// A campaign with no entry is cached with `board` first (`Full` when every entry at the cap
    /// holds an op). A dig passes `dig` (its tier comes from the site); a contract passes
    /// `contract` (BP-11a-2; its mission comes from the site). The squad (Section and Commander)
    /// is the one in `dig`/`contract`, as the route built it; `named` is "the client named a
    /// squad" (BP-18a-1b-1). Refusals in order, under the lock, after settling:
    /// - `Busy`: a dig start while the dig slot is held; or an unnamed start while any slot is
    ///   held, except a Crew-alone dig start, which needs only the dig slot empty (G-B). So an
    ///   unnamed start lands only with no op out, or as a Crew-alone dig in the empty dig slot;
    /// - named, for an op that takes a Section: `SectionBusy` (`OpsEntry::section_busy`);
    /// - `Taken`: another slot's contract has the target's mission, or a contract slot already
    ///   holds the new op's key (two `None`-keyed contracts; Reviewer BP-18a-1a F1);
    /// - named: `CommanderBusy` (`OpsEntry::commander_busy`) for the op's Commander.
    #[allow(clippy::too_many_arguments)]
    pub fn start_op_for(
        &self,
        campaign_id: Uuid,
        gateway: (i32, i32),
        board: Arc<OpsBoard>,
        site: &OpsSite,
        op_seed: u32,
        dig: Option<DigStart>,
        contract: Option<ContractStart>,
        named: bool,
        route: RouteKind,
        now: DateTime<Utc>,
    ) -> Result<(), StartRefusal> {
        let mut inner = self.lock();
        if !inner.contains_key(&campaign_id) {
            if !self.make_room(&mut inner) {
                return Err(StartRefusal::Full);
            }
            inner.insert(campaign_id, OpsEntry { gateway, board, touched: now, dig: None, contracts_out: BTreeMap::new(), moved_at: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), contracts: HashMap::new(), sickbay: HashMap::new(), trophies: Vec::new(), credits_in_flight: 0, crew_fight: None, wages_at: now, bench_at: now, squads: 1, hire_in_flight: false, foreman_due: ForemanDue::default(), hazard_due: HazardDue::default(), trip_reports: Vec::new(), trip_seq: 0 });
        }
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(StartRefusal::Full);
        };
        e.settle_arrival(now);
        // BP-18a-1b-1: a dig start needs the dig slot empty; an unnamed start needs every slot
        // empty, except a Crew-alone dig (it takes no Section, so it can't collide with a contract).
        let crew_alone = dig.as_ref().is_some_and(|d| !d.escort);
        if (dig.is_some() && e.dig.is_some()) || (!named && !crew_alone && e.any_op()) {
            return Err(StartRefusal::Busy);
        }
        let lead = lead_of(dig.as_ref(), contract.as_ref());
        if named && lead.and_then(|(_, s)| s).is_some_and(|s| e.section_busy(s)) {
            return Err(StartRefusal::SectionBusy);
        }
        // BP-20a-1: the way, from the entry's board; Safe only where the board offers it.
        let (leg_secs, detour_secs, route) = match e.board.routes.get(&(site.q, site.r)) {
            Some(w) => match (route, &w.safe) {
                (RouteKind::Safe, Some(safe)) => (safe.secs, safe.secs.saturating_sub(w.fast.secs), RouteKind::Safe),
                _ => (w.fast.secs, 0, RouteKind::Fast),
            },
            None => (site.travel_secs, 0, RouteKind::Fast),
        };
        // BP-20a-2 (W4): the chosen way's hazards, both legs; none where the board has no ways.
        let kinds = match e.board.routes.get(&(site.q, site.r)) {
            Some(w) if route == RouteKind::Safe => w.safe_kinds.clone(),
            Some(w) => w.fast_kinds.clone(),
            None => Vec::new(),
        };
        let hazards = OpHazards { out: kinds.clone(), home: kinds, pct: self.hazard_pct };
        let op = new_op(site, op_seed, dig, contract, route, detour_secs, hazards, leg_secs, now);
        let key = slot_of(&op);
        let taken = op.contract.as_ref().is_some_and(|c| e.mission_held(c.mission_id, None)) || (key != SlotKey::Dig && e.slot(key).is_some());
        if taken {
            return Err(StartRefusal::Taken);
        }
        if named && lead.is_some_and(|(c, _)| e.commander_busy(c)) {
            return Err(StartRefusal::CommanderBusy);
        }
        e.insert_op(op);
        e.crew_fight = None;
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// Starts the homeward leg once the outbound one has arrived, taking the site's time again.
    /// From a dig the team walks away: the ore on site is left (`OpEnd::WalkedAway`, F6);
    /// BP-8b-2's pull is the loaded return. Refusals in order: `NoOp` → `AlreadyHomeward` →
    /// `NotArrived` → `RaidPending` (raiders can't be walked away from). `return_op_for` with no
    /// squad named.
    pub fn return_op(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Result<(), ReturnRefusal> {
        self.return_op_for(campaign_id, None, now)
    }

    /// `return_op` for the op `squad` selects (BP-18a-1b-1, `OpsEntry::select`): the selection
    /// comes first (`SayWhich` with no squad named and two or more ops out; `NoOp` when the
    /// squad has none), then today's refusals on that op. Writes `moved_at = now`.
    pub fn return_op_for(&self, campaign_id: Uuid, squad: Option<OpSquad>, now: DateTime<Utc>) -> Result<(), ReturnRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(ReturnRefusal::NoOp);
        };
        e.settle_arrival(now);
        let key = match e.select(squad) {
            Ok(k) => k,
            Err(Select::NoOp) => return Err(ReturnRefusal::NoOp),
            Err(Select::SayWhich) => return Err(ReturnRefusal::SayWhich),
        };
        let Some(op) = e.slot_mut(key) else {
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
    /// (`new_op`). It accepts what `return_op` accepts, except a dig whose guard does not stand.
    /// Refusals in order: `NoOp` → `AlreadyHomeward` → `NotArrived` → `RaidPending` (a dig's
    /// raid) → `GuardNotHere` (a dig whose guard does not stand yet, BP-18a-1a) → `SameSite` (the
    /// target's hex is the team's `at`; hexes, not site ids, since a refreshed contract keeps its
    /// id).
    /// - `next_op_for` with no squad named: it acts on the one op out (`SayWhich` with two).
    /// - The leg runs from `op.at`, never from a board lookup: the hop's hexes at the target's
    ///   seconds per hex (`travel_secs / distance`, distance at least 1), at least 1 s. Home from
    ///   there is the target's own `travel_secs` (its Fast way: a next's op is always Fast,
    ///   BP-20a-1; a `route` on `ops/next` is ignored).
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
        self.next_op_for(campaign_id, None, site, op_seed, dig, contract, now)
    }

    /// `next_op` for the op `squad` selects (BP-18a-1b-1, `OpsEntry::select`). Refusals in
    /// order: `NoOp` (no entry) → the selection (`SayWhich`, `NoOp`) → today's checks on the
    /// selected op → then, with the moving squad copied in (below), `DigHeld` (the target is a
    /// dig and another slot holds the dig) → `Taken` (another slot's contract has the target's
    /// mission, or another contract slot holds the new op's key; Reviewer BP-18a-1a F1).
    /// - **The squad moves with its op (spec check A2):** the new op's Commander and Section are
    ///   the leaving op's (a contract's `ContractState`, a guarded dig's `DigState`), whatever the
    ///   route built; every other field is the route's. A bare op keeps the route's values.
    /// - Only the selected slot is cleared; the new op goes in its own (`slot_of`). Writes
    ///   `moved_at = now`.
    #[allow(clippy::too_many_arguments)]
    pub fn next_op_for(
        &self,
        campaign_id: Uuid,
        squad: Option<OpSquad>,
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
        let key = match e.select(squad) {
            Ok(k) => k,
            Err(Select::NoOp) => return Err(NextRefusal::NoOp),
            Err(Select::SayWhich) => return Err(NextRefusal::SayWhich),
        };
        let Some(op) = e.slot(key) else {
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
        if op.dig.as_ref().is_some_and(|d| !d.guarded(now)) {
            return Err(NextRefusal::GuardNotHere);
        }
        if (site.q, site.r) == op.at {
            return Err(NextRefusal::SameSite);
        }
        let hop = hex_axial_distance(op.at.0, op.at.1, site.q, site.r);
        let leg_secs = (site.travel_secs / site.distance.max(1) * hop).max(1);
        // A2: the squad on the leaving op moves with it. The dig arm takes any dig's squad and is
        // sound only because GuardNotHere (above) refuses an unguarded dig first (Reviewer
        // BP-18a-1b-1 F1): a change there filters this to `guarded_dig()` or adds
        // `section_busy`/`commander_busy` at next.
        let moving = op.contract.as_ref().map(|c| (c.commander_id, c.section_id)).or_else(|| op.dig.as_ref().map(|d| (d.commander_id, d.section_id)));
        let (mut dig, mut contract) = (dig, contract);
        if let Some((commander_id, section_id)) = moving {
            dig = dig.map(|d| DigStart { commander_id, section_id, ..d });
            contract = contract.map(|_| ContractStart { commander_id, section_id });
        }
        // BP-20a-2 (W4): the hop crosses nothing; home is the target's Fast way.
        let home = e.board.routes.get(&(site.q, site.r)).map_or_else(Vec::new, |w| w.fast_kinds.clone());
        let hazards = OpHazards { out: Vec::new(), home, pct: self.hazard_pct };
        let next = new_op(site, op_seed, dig, contract, RouteKind::Fast, 0, hazards, leg_secs, now);
        let target = slot_of(&next);
        if target == SlotKey::Dig && e.dig.is_some() && key != SlotKey::Dig {
            return Err(NextRefusal::DigHeld);
        }
        let held_elsewhere = next.contract.as_ref().is_some_and(|c| e.mission_held(c.mission_id, Some(key))) || (target != SlotKey::Dig && target != key && e.slot(target).is_some());
        if held_elsewhere {
            return Err(NextRefusal::Taken);
        }
        // BP-19a: a dig left for the next site owes its Foreman as one left by choice.
        if let Some(d) = e.slot(key).and_then(|op| op.dig.as_ref()) {
            let due = d.settlement(DigClose::LeftForNext, d.carried);
            e.foreman_due.add(due);
        }
        e.clear_slot(key);
        e.insert_op(next);
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
        // BP-18a-1a: the dig slot; a contract slot alone is today's `Contract`.
        let held = e.any_op();
        let Some(op) = e.dig.as_mut() else {
            return Err(if held { DrillRefusal::Contract } else { DrillRefusal::NoOp });
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
            dig.crew_heads_lost = dig.crew_heads_lost.saturating_add(heads);
            return Ok(DrillOutcome::Meltdown { crew_id: dig.crew_id, heads: heads as i16 });
        }
        dig.ore_on_site += base.ore(dig.drills, dig.tier);
        let Some((result, heads)) = crew else {
            return Ok(DrillOutcome::Drilled);
        };
        let crew_id = dig.crew_id;
        dig.crew_heads_lost = dig.crew_heads_lost.saturating_add(u32::try_from(heads).unwrap_or(0));
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
            RaidResult::Wiped => {
                let due = dig.settlement(DigClose::CrewWiped, dig.carried);
                e.foreman_due.add(due);
                e.dig = None;
            }
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
        // BP-18a-1a: the dig slot; a contract slot alone is today's `Contract`.
        let held = e.any_op();
        let Some(op) = e.dig.as_mut() else {
            return Err(if held { PullRefusal::Contract } else { PullRefusal::NoOp });
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
        if ambush_d100(op.op_seed) <= (base.haul_ambush_pct(load) + op.detour_secs * DEMO_AMBUSH_PCT_PER_DETOUR_SEC).min(DEMO_AMBUSH_CAP_PCT) {
            dig.ambush_at = Some(now + Duration::milliseconds(i64::from(op.travel_secs) * 500));
        }
        e.moved_at = Some(now);
        e.touched = now;
        Ok(())
    }

    /// The battle waiting for its fight (a raid, or a haul's ambush once fired, or a contract's
    /// fight once the team is on site (BP-11a-2)), after settling the op: the op's Commander, the
    /// trigger and its Section. `None` when none is pending. The first of `pending_raids`.
    pub fn pending_raid(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Option<PendingRaid> {
        self.pending_raids(campaign_id, now).into_iter().next()
    }

    /// Every battle waiting for its fight (BP-18a-1b-1, H3), after settling, in slot order: the
    /// dig's, then the contracts' by key. Empty with no entry. **Skip semantics (Reviewer
    /// BP-18a-1a F2):** a slot with no trigger is skipped and does not hide the slots after it;
    /// so is a contract slot whose trigger has no contract (unreachable: `contract_trigger`
    /// needs one). Each battle carries its own trigger, so the latch matches it on its own.
    pub fn pending_raids(&self, campaign_id: Uuid, now: DateTime<Utc>) -> Vec<PendingRaid> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Vec::new();
        };
        e.settle_arrival(now);
        e.ops().filter_map(|op| {
            if let Some(trigger) = contract_trigger(op, now) {
                let c = op.contract.as_ref()?;
                return Some(PendingRaid { commander_id: c.commander_id, trigger, section_id: c.section_id, crew: None });
            }
            let dig = op.dig.as_ref()?;
            // BP-17a: a battle born before the guard stands is the Crew's (only an ambush can be:
            // an unguarded raid is fought inside `drill_op`), rolled here from the Crew stream.
            dig.raid.map(|trigger| {
                let crew = (!dig.guarded(trigger.trigger_at)).then(|| {
                    let (result, heads) = crew_fight_roll(op.op_seed, DEMO_CREW_AMBUSH_DRAW);
                    CrewRoll { crew_id: dig.crew_id, result, heads }
                });
                PendingRaid { commander_id: dig.commander_id, trigger, section_id: dig.section_id, crew }
            })
        })
        .collect()
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
        if trigger.kind == BattleKind::Contract {
            // BP-18a-1a: the slot whose contract trigger is this one; its Wipeout clears that slot.
            let Some(key) = e.slot_keys().into_iter().find(|k| e.slot(*k).is_some_and(|op| contract_trigger(op, now) == Some(*trigger))) else {
                return false;
            };
            match result {
                RaidResult::Held | RaidResult::Retreat => {
                    if let Some(c) = e.slot_mut(key).and_then(|op| op.contract.as_mut()) {
                        c.result = Some(result);
                    }
                }
                RaidResult::Wiped => e.clear_slot(key),
            }
            e.contracts.entry(trigger.mission_id).or_default().finished_at = Some(now);
        } else {
            // BP-18a-1a: a raid or an ambush is the dig slot's.
            let Some(op) = e.dig.as_mut() else {
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
                (_, RaidResult::Wiped) => {
                    let due = dig.settlement(DigClose::GuardWiped, dig.carried);
                    e.foreman_due.add(due);
                    e.dig = None;
                }
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
        // BP-18a-1a: the dig slot (the Crew's).
        let Some(op) = e.dig.as_mut() else {
            return false;
        };
        let Some(dig) = op.dig.as_mut() else {
            return false;
        };
        if dig.raid != Some(*trigger) {
            return false;
        }
        dig.raid = None;
        dig.crew_heads_lost = dig.crew_heads_lost.saturating_add(u32::try_from(crew.heads).unwrap_or(0));
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
            RaidResult::Wiped => {
                let due = dig.settlement(DigClose::CrewWiped, dig.carried);
                e.foreman_due.add(due);
                e.dig = None;
            }
        }
        e.crew_fight = Some(crew_fight_view("ambush", crew.result, crew.heads));
        e.moved_at = Some(now);
        e.touched = now;
        true
    }

    /// Drains what finished digs owe the Foreman (BP-19a) and claims the bench (BP-19b), after
    /// settling, so a homeward leg that has arrived but not yet been read lands inside the
    /// drain and the dig slot the bench reads is the settled one. The `take` and the advance of
    /// `bench_at` under the lock are the exactly-once claims: a second claim at the same `now`
    /// sees the default tally and 0 minutes. No entry claims nothing.
    pub fn take_leader_claim(&self, campaign_id: Uuid, now: DateTime<Utc>) -> LeaderClaim {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return LeaderClaim::default();
        };
        e.settle_arrival(now);
        let due = std::mem::take(&mut e.foreman_due);
        let hazards = std::mem::take(&mut e.hazard_due);
        let mins = (now - e.bench_at).num_minutes().max(0);
        e.bench_at += Duration::minutes(mins);
        LeaderClaim { due, bench_mins: u32::try_from(mins).unwrap_or(u32::MAX), foreman_out: e.dig.is_some(), busy: e.busy_commander_ids(), hazards }
    }

    /// The tally half of `take_leader_claim` (BP-19a's tests). Test-only: it discards the bench
    /// claim, so production never calls it.
    #[cfg(test)]
    pub fn take_foreman_due(&self, campaign_id: Uuid, now: DateTime<Utc>) -> ForemanDue {
        self.take_leader_claim(campaign_id, now).due
    }

    /// Puts a drained tally back after its write failed (BP-19a), so the next read retries. No
    /// entry (evicted or wiped since the drain): the tally is lost, a named residual.
    pub fn restore_foreman_due(&self, campaign_id: Uuid, due: ForemanDue) {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            e.foreman_due.add(due);
        }
    }

    /// Puts a drained hazard tally back after its write failed (BP-20a-2), so the next read
    /// retries. No entry (evicted or wiped since the drain): the tally is lost, a named residual.
    pub fn restore_hazard_due(&self, campaign_id: Uuid, due: &HazardDue) {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            e.hazard_due.add(due);
        }
    }

    /// A hazard's wounded join `section_id`'s sickbay (BP-20a-2), cut to `cap` survivors (the
    /// headcount the drain read), after settling. No entry: nothing (a named residual).
    pub fn wound_section(&self, campaign_id: Uuid, section_id: Uuid, wounded: u8, cap: u8, now: DateTime<Utc>) {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            e.settle_arrival(now);
            e.sickbay.entry(section_id).or_default().take_wounded(wounded, cap);
        }
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
        self.ops_state_all(campaign_id, now).0
    }

    /// `ops_state` and every held slot but the primary (BP-18a-1b-2), from one lock: the body's
    /// `more_ops`, each contract slot's key and its op (`view_of`), in key order. A held dig is
    /// always the primary (`slot_keys` is dig-first), so it is never here. Empty with one op or
    /// none. A second return, never a field on `OpsSnapshot`, which a test compares whole.
    pub fn ops_state_all(&self, campaign_id: Uuid, now: DateTime<Utc>) -> (OpsSnapshot, Vec<MoreOp>) {
        let (snapshot, more_ops, _, _) = self.ops_body(campaign_id, now);
        (snapshot, more_ops)
    }

    /// `ops_state_all` and the squads the wages count (BP-18a-2), from one lock: the body's
    /// `company.wages`; and the landed hazard trips (BP-20a-2, the body's `trip_reports`), a return
    /// beside the snapshot, never a field on `OpsSnapshot`. A campaign with no entry counts one squad (every demo member joins with
    /// one), and pays nothing until it has an entry.
    pub fn ops_body(&self, campaign_id: Uuid, now: DateTime<Utc>) -> (OpsSnapshot, Vec<MoreOp>, u8, Vec<TripReport>) {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            let none = OpsSnapshot { sites: None, op: None, ore_banked: 0, credits_banked: 0, base: OpsBase::default(), company: CompanyView::default(), crew_fight: None };
            return (none, Vec::new(), 1, Vec::new());
        };
        e.settle_arrival(now);
        let primary = e.primary_key();
        let more_ops = e
            .slot_keys()
            .into_iter()
            .filter(|k| Some(*k) != primary)
            .filter_map(|k| {
                let section_id = match k {
                    SlotKey::Contract(s) => s,
                    SlotKey::Dig => None,
                };
                e.view_of(k, now).map(|op| MoreOp { section_id, op })
            })
            .collect();
        let snapshot = OpsSnapshot {
            sites: Some(e.sites()),
            op: e.view(now),
            ore_banked: e.ore_banked,
            credits_banked: e.credits_banked,
            base: e.base,
            company: e.company(),
            crew_fight: e.crew_fight,
        };
        (snapshot, more_ops, e.squads, e.trip_reports.clone())
    }

    /// Which of `sections` the store holds busy (BP-18a-1b-2, `OpsEntry::section_busy`), after
    /// settling, for the route's Section pick. Advisory: the route picks without this lock, so a
    /// start that lands between this read and the route's store call is answered by the store's
    /// own `409` (BP-18a-1b-1's store tests decide that re-check; no pause seam reaches the race).
    pub fn busy_sections(&self, campaign_id: Uuid, sections: &[Uuid], now: DateTime<Utc>) -> Vec<Uuid> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Vec::new();
        };
        e.settle_arrival(now);
        sections.iter().copied().filter(|s| e.section_busy(*s)).collect()
    }

    /// Which of `commanders` the store holds busy (BP-18a-1b-2, `OpsEntry::commander_busy`), after
    /// settling, for the route's Commander pick; advisory, as `busy_sections`.
    pub fn busy_commanders(&self, campaign_id: Uuid, commanders: &[Uuid], now: DateTime<Utc>) -> Vec<Uuid> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Vec::new();
        };
        e.settle_arrival(now);
        commanders.iter().copied().filter(|c| e.commander_busy(*c)).collect()
    }

    /// Re-leads a pending battle (BP-18b-2a, Linux H1 option (A)): under the lock and with no
    /// await, writes `new` as the Commander of the op whose pending battle is
    /// `trigger` (a guarded dig's, or a contract's), and returns whether it wrote. It writes only
    /// while that battle is still pending, its recorded Commander is still `old`, `new != old`,
    /// and `new` is not busy (`commander_busy`). **Orders:** an escort or a start that takes `new`
    /// first wins (this then refuses); one that comes after sees `new` busy and picks another.
    /// No launch lock: it writes a Commander on an op, never a Section or a headcount. It does not
    /// settle: it finds the battle by the trigger's own time, which needs none, because the caller's
    /// `pending_raids` settled the entry just before, a contract's trigger depends only on its leg,
    /// `arrive_at` and result, and a dig compares its raid by value (Reviewer BP-18b-2a F2).
    pub fn relead_op(&self, campaign_id: Uuid, trigger: &RaidTrigger, old: Uuid, new: Uuid) -> bool {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return false;
        };
        let now = trigger.trigger_at;
        if new == old || e.commander_busy(new) {
            return false;
        }
        if trigger.kind == BattleKind::Contract {
            let Some(key) = e.slot_keys().into_iter().find(|k| e.slot(*k).is_some_and(|op| contract_trigger(op, now) == Some(*trigger))) else {
                return false;
            };
            match e.slot_mut(key).and_then(|op| op.contract.as_mut()) {
                Some(c) if c.commander_id == old => {
                    c.commander_id = new;
                    true
                }
                _ => false,
            }
        } else {
            match e.dig.as_mut().and_then(|op| op.dig.as_mut()) {
                Some(d) if d.raid == Some(*trigger) && d.commander_id == old => {
                    d.commander_id = new;
                    true
                }
                _ => false,
            }
        }
    }

    /// The Section on the op `squad` selects (BP-18a-1b-2, next's at-0 check), after settling, by
    /// `OpsEntry::select`: `Some` of the slot's Section (a contract's key, a guarded dig's guard,
    /// BP-18a-2), itself `None` for an op with no Section or an unguarded dig; `None` when the
    /// selection finds no op
    /// (`NoOp`, or `SayWhich` with two out), so the route falls back to the member's first Section
    /// and `next_op_for` answers the selection's refusal after the at-0 check, as today.
    pub fn squad_section(&self, campaign_id: Uuid, squad: Option<OpSquad>, now: DateTime<Utc>) -> Option<Option<Uuid>> {
        let mut inner = self.lock();
        let e = inner.get_mut(&campaign_id)?;
        e.settle_arrival(now);
        let key = e.select(squad).ok()?;
        match key {
            SlotKey::Contract(s) => Some(s),
            // BP-18a-2 (Reviewer BP-18a-1b-2 F2): the guard's Section only. A Crew-alone dig's
            // recorded home Section is not its squad (A4), so an unguarded dig has none here, the
            // at-0 check passes and `next_op_for` answers `GuardNotHere`. Any change to
            // `GuardNotHere` keeps this arm and the A2 copy in `next_op_for` on `guarded_dig()`.
            SlotKey::Dig => Some(e.guarded_dig().and_then(|d| d.section_id)),
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
        if e.any_op() {
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
    /// guard from `now + travel_secs` (`escort_at`; `op.travel_secs` is the chosen way's seconds,
    /// BP-20a-1) and fights every battle born from then on;
    /// `commander_id` and `section_id` (read by the route) become the dig's. Allowed while
    /// outbound (no fight can come before the arrival). Writes `moved_at` (a departure).
    /// Refusals in order: `NoOp` → `Contract` → `AlreadyHomeward` → `AlreadyEscorted` →
    /// `SectionBusy` (the Section sent leads a contract) → `CommanderBusy` (the Commander sent
    /// leads a contract) (BP-18a-1b-1, spec check A1/A3). The Crew-alone dig's own recorded
    /// Commander and Section are not busy while `escort_at` is `None` (A4).
    pub fn escort_op(&self, campaign_id: Uuid, commander_id: Uuid, section_id: Option<Uuid>, now: DateTime<Utc>) -> Result<(), EscortRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(EscortRefusal::NoOp);
        };
        e.settle_arrival(now);
        // BP-18a-1a: the dig slot; a contract slot alone is today's `Contract`.
        let held = e.any_op();
        let Some(op) = e.dig.as_ref() else {
            return Err(if held { EscortRefusal::Contract } else { EscortRefusal::NoOp });
        };
        let travel_secs = op.travel_secs;
        let Some(dig) = op.dig.as_ref() else {
            return Err(EscortRefusal::Contract);
        };
        if op.leg == OpLeg::Homeward {
            return Err(EscortRefusal::AlreadyHomeward);
        }
        if dig.escort_at.is_some() {
            return Err(EscortRefusal::AlreadyEscorted);
        }
        if section_id.is_some_and(|s| e.section_busy(s)) {
            return Err(EscortRefusal::SectionBusy);
        }
        if e.commander_busy(commander_id) {
            return Err(EscortRefusal::CommanderBusy);
        }
        let Some(dig) = e.dig.as_mut().and_then(|op| op.dig.as_mut()) else {
            return Err(EscortRefusal::Contract);
        };
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
        self.begin_hold(campaign_id, cost, None, now)
    }

    /// `begin_replace` for Section `section_id`'s Requisition (BP-17a; per Section since
    /// BP-18a-1b-1): it passes while that Section is not busy (`OpsEntry::section_busy`), so
    /// during an unescorted dig (the Section is at base) and while another Section is out. A
    /// Crew Hire still refuses while any op is out: the Crew is shared.
    pub fn begin_requisition(&self, campaign_id: Uuid, section_id: Uuid, cost: u32, now: DateTime<Utc>) -> Result<ReplaceHold, ReplaceRefusal> {
        self.begin_hold(campaign_id, cost, Some(section_id), now)
    }

    /// The hold behind both: `Some(s)` (a Requisition) refuses `OpOut` iff Section `s` is busy;
    /// `None` (the Crew Hire) iff any op is out. Restates Reviewer BP-18a-1a's `begin_hold` note
    /// per Section: two Sections can each be out on their own op.
    fn begin_hold(&self, campaign_id: Uuid, cost: u32, section: Option<Uuid>, now: DateTime<Utc>) -> Result<ReplaceHold, ReplaceRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(ReplaceRefusal::NotEnoughCredits);
        };
        e.settle_arrival(now);
        let out = match section {
            Some(s) => e.section_busy(s),
            None => e.any_op(),
        };
        if out {
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

    /// Debits a squad hire (BP-18a-2) and holds it in flight until `finish_hire`. `sections` is
    /// the member's Section count as the route read it. Settles first (the wages included).
    /// Refusals in order: `Cap` (`sections`, plus a hire in flight, already at `DEMO_HIRE_CAP`)
    /// → `NotEnoughCredits` (also no entry). Allowed while a squad is out: a hire reports at
    /// base. Check and debit are one critical section, no await. The route's count can be stale
    /// (a hire that finished between its read and this call), so the route recounts under the
    /// launch lock before it writes; this is the cheap early refusal. Writes `moved_at` (the keep
    /// list), as `begin_hold` does.
    pub fn begin_hire(&self, campaign_id: Uuid, sections: usize, now: DateTime<Utc>) -> Result<HireHold, HireRefusal> {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&campaign_id) else {
            return Err(HireRefusal::NotEnoughCredits);
        };
        e.settle_arrival(now);
        if sections + usize::from(e.hire_in_flight) >= DEMO_HIRE_CAP {
            return Err(HireRefusal::Cap);
        }
        if !e.debit_credits(DEMO_HIRE_CREDITS) {
            return Err(HireRefusal::NotEnoughCredits);
        }
        e.credits_in_flight = e.credits_in_flight.saturating_add(DEMO_HIRE_CREDITS);
        e.hire_in_flight = true;
        e.moved_at = Some(now);
        e.touched = now;
        Ok(HireHold { campaign_id, credits: DEMO_HIRE_CREDITS })
    }

    /// Ends a squad hire (BP-18a-2): the hold leaves flight, and without `landed` it goes back
    /// to the bank. `squads`, when the route knows it (its recount under the launch lock, plus
    /// one for a hire that landed), becomes the wage count; the partial minute before it bills at
    /// the new count. An entry gone by now went with its campaign, so nothing is recreated.
    pub fn finish_hire(&self, hold: HireHold, landed: bool, squads: Option<u8>) {
        let mut inner = self.lock();
        let Some(e) = inner.get_mut(&hold.campaign_id) else {
            return;
        };
        e.credits_in_flight = e.credits_in_flight.saturating_sub(hold.credits);
        e.hire_in_flight = false;
        if let Some(n) = squads {
            e.squads = n;
        }
        if !landed {
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
        // BP-18a-1a: the latest passed arrival over every slot.
        let arrived = e.ops().map(|op| op.arrive_at).filter(|t| *t <= now).max();
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

    /// Moves the campaign's ops, every slot (its leg and a haul's hidden ambush time), its last movement and
    /// its finished contracts' times (BP-11a-3) `secs` into the past, so a route test crosses an
    /// arrival or a refresh without sleeping. A trigger's time stays: a fight's `started_at` is
    /// the real clock.
    #[cfg(test)]
    pub(crate) fn rewind_op(&self, campaign_id: Uuid, secs: i64) {
        let mut inner = self.lock();
        if let Some(e) = inner.get_mut(&campaign_id) {
            let back = Duration::seconds(secs);
            for op in e.dig.iter_mut().chain(e.contracts_out.values_mut()) {
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

    /// Moves the campaign's wage clock `secs` into the past (BP-18a-2), so a route test pays
    /// wages without sleeping. `rewind_op` leaves it.
    #[cfg(test)]
    pub(crate) fn rewind_wages(&self, campaign_id: Uuid, secs: i64) {
        if let Some(e) = self.lock().get_mut(&campaign_id) {
            e.wages_at -= Duration::seconds(secs);
        }
    }

    /// Moves the campaign's bench clock `secs` into the past (BP-19b), so a route test benches
    /// its leaders without sleeping. `rewind_op` leaves it.
    #[cfg(test)]
    pub(crate) fn rewind_bench(&self, campaign_id: Uuid, secs: i64) {
        if let Some(e) = self.lock().get_mut(&campaign_id) {
            e.bench_at -= Duration::seconds(secs);
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

    /// Fixes the primary op's seed (the dig, else the first contract by key), so a test chooses
    /// its rolls instead of hoping for them.
    #[cfg(test)]
    pub(crate) fn set_op_seed(&self, campaign_id: Uuid, seed: u32) {
        if let Some(op) = self.lock().get_mut(&campaign_id).and_then(|e| e.primary_key().and_then(|k| e.slot_mut(k))) {
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
        // BP-20a-1 (Variant B): a plains corridor (7, 0) … (10, 0); `digs` is dig-only.
        let t = map(&[((6, 0), A), ((11, 0), A), ((12, 0), A), ((7, 0), A), ((8, 0), A), ((9, 0), A), ((10, 0), A)]);
        let veins = [vein(6, 0, 1), vein(11, 0, 1), vein(12, 0, 1)];
        let b = ops_board(&t, &veins, (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(11, 0, 1)]);
    }

    #[test]
    fn ops_board_takes_the_two_nearest_tier1_veins() {
        // BP-20a-1 (Variant B): plains corridors (6, 0), (7, 0) and (5, 1), (5, 2).
        let t = map(&[((8, 0), A), ((5, 3), A), ((9, 0), A), ((10, 0), A), ((6, 0), A), ((7, 0), A), ((5, 1), A), ((5, 2), A)]);
        let veins = [vein(10, 0, 1), vein(9, 0, 1), vein(8, 0, 1), vein(5, 3, 1)];
        let b = ops_board(&t, &veins, (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(5, 3, 1), (8, 0, 1)], "d = 3 twice, ties by (q, r)");
    }

    #[test]
    fn ops_board_tier3_is_the_nearest_within_12() {
        // BP-20a-1 (Variant B): a plains corridor (6, 0) … (17, 0), so (18, 0) is out of range,
        // not unreachable; `digs` is dig-only.
        let mut hexes = vec![((14, 0), A), ((15, 0), A), ((18, 0), A)];
        hexes.extend((6..=13).chain(16..=17).map(|q| ((q, 0), A)));
        let t = map(&hexes);
        let b = ops_board(&t, &[vein(15, 0, 3), vein(14, 0, 3), vein(18, 0, 3)], (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(14, 0, 3)]);
        let far = ops_board(&t, &[vein(18, 0, 3)], (5, 0), Uuid::nil());
        assert_eq!(digs(&far), vec![], "d = 13 is out of range");
    }

    /// Around gateway (5, 0): a Mountain vein (d = 2) is no dig site, and a Safe Zone hex
    /// (2, 0) (d = 3), reached by a plains corridor, is no contract hex; the pool is the five
    /// other passable hexes 3 to 6 out.
    #[test]
    fn ops_board_skips_an_impassable_vein_and_a_safe_zone_hex() {
        let t = map(&[
            ((7, 0), HexTerrain::Mountain),
            ((5, 3), A),
            ((5, -3), HexTerrain::Mountain),
            ((2, 0), A),
            ((8, 0), A),
            ((9, 0), A),
            ((10, 0), A),
            ((11, 0), A),
            // BP-20a-1 (Variant B): plains corridors from the gateway; (8, -1) is a fifth contract
            // candidate, so the pin is dig-only.
            ((5, 1), A),
            ((5, 2), A),
            ((6, 0), A),
            ((7, -1), A),
            ((8, -1), A),
            ((4, 0), A),
            ((3, 0), A),
        ]);
        let b = ops_board(&t, &[vein(7, 0, 1), vein(5, 3, 1)], (5, 0), Uuid::nil());
        assert_eq!(digs(&b), vec![(5, 3, 1)], "the Mountain vein is no dig site");
        let pool: Vec<(i32, i32)> = b.contract_pool.iter().map(|p| (p.0, p.1)).collect();
        assert_eq!(pool, vec![(8, -1), (8, 0), (9, 0), (10, 0), (11, 0)], "(2, 0) is reachable but in the Safe Zone");
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
    fn ops_travel_secs_is_the_fast_path_sum() {
        // BP-20a-1: AshenPlains at (6, 0) and (8, 0) … (16, 0); each hex entered costs its own
        // terrain's seconds: 2 + 1, and 2 + 1 + 9 × 2 + 4.
        let mut hexes = vec![((17, 0), HexTerrain::CooledMagma), ((7, 0), HexTerrain::CityState), ((6, 0), A)];
        hexes.extend((8..=16).map(|q| ((q, 0), A)));
        let t = map(&hexes);
        let b = ops_board(&t, &[vein(17, 0, 3), vein(7, 0, 1)], (5, 0), Uuid::nil());
        let secs: Vec<(String, u32)> = b.sites.iter().filter(|s| s.kind == "dig").map(|s| (s.site_id.clone(), s.travel_secs)).collect();
        assert_eq!(secs, vec![("dig:7,0".to_string(), 3), ("dig:17,0".to_string(), 25)]);
    }

    fn board() -> Arc<OpsBoard> {
        Arc::new(OpsBoard { sites: vec![], contract_pool: vec![], routes: HashMap::new() })
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

    /// The dig's exact keys; nothing names the seed, the Crew or the trigger (the op's only Commander key is `commander_id`),
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
        assert!(!text.contains("seed") && !text.contains("crew") && !text.replace("\"commander_id\"", "").contains("commander") && !text.contains("trigger"), "{text}");
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

    /// BP-20a-1: the line (6, 0) (7, 0) (8, 0) (9, 0) with `mid` at (7, 0), and a one-hex detour
    /// round it through (7, −1) and (8, −1); `detour` stands at (7, −1).
    fn detour_map(mid: HexTerrain, detour: HexTerrain) -> HashMap<String, HexTerrain> {
        map(&[((6, 0), A), ((7, 0), mid), ((8, 0), A), ((9, 0), A), ((7, -1), detour), ((8, -1), A)])
    }

    fn way(secs: u32, hazards: u32, path: &[(i32, i32)]) -> RoutePath {
        RoutePath { secs, hazards, path: path.to_vec() }
    }

    const LINE: [(i32, i32); 4] = [(6, 0), (7, 0), (8, 0), (9, 0)];
    const DETOUR: [(i32, i32); 5] = [(6, 0), (7, -1), (8, -1), (8, 0), (9, 0)];

    #[test]
    fn routes_go_round_a_toxic_zone() {
        let w = board_routes(&detour_map(HexTerrain::ToxicZone, A), (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!(w.fast, way(8, 1, &LINE));
        assert_eq!(w.safe, Some(way(10, 0, &DETOUR)));
    }

    #[test]
    fn routes_go_round_a_ruined_city() {
        let w = board_routes(&detour_map(HexTerrain::RuinedCity, A), (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!(w.fast, way(9, 1, &LINE));
        assert_eq!(w.safe, Some(way(10, 0, &DETOUR)));
    }

    #[test]
    fn routes_go_round_cooled_magma() {
        let mut hexes = vec![((6, 0), A), ((7, 0), HexTerrain::CooledMagma), ((8, 0), A), ((9, 0), A)];
        hexes.extend([(6, -1), (7, -2), (8, -2), (9, -2), (9, -1)].map(|h| (h, A)));
        let w = board_routes(&map(&hexes), (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!(w.fast, way(10, 1, &LINE));
        assert_eq!(w.safe, Some(way(12, 0, &[(6, -1), (7, -2), (8, -2), (9, -2), (9, -1), (9, 0)])));
    }

    /// H3: the line through CooledMagma and the detour both take 10 s, so Fast is the clean one
    /// and no Safe way is offered. The path is not pinned (N3c leaves this passing).
    #[test]
    fn fast_takes_the_clean_way_on_a_tie() {
        let w = board_routes(&detour_map(HexTerrain::CooledMagma, A), (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!((w.fast.secs, w.fast.hazards, &w.safe), (10, 0, &None));
    }

    #[test]
    fn routes_offer_one_way_with_no_detour() {
        let t = map(&[((6, 0), A), ((7, 0), HexTerrain::ToxicZone), ((8, 0), A), ((9, 0), A)]);
        let w = board_routes(&t, (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!((w.fast.secs, w.fast.hazards, &w.safe), (8, 1, &None));
    }

    #[test]
    fn routes_never_cross_impassable() {
        let w = board_routes(&detour_map(HexTerrain::ToxicZone, HexTerrain::MagmaFlow), (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!(w.fast, way(8, 1, &LINE));
        assert_eq!(w.safe, None);
    }

    /// Two 4 s ways to (7, −1); the tie goes to the smaller predecessor, (6, −1).
    #[test]
    fn routes_break_ties_on_the_smaller_hex() {
        let t = map(&[((6, 0), A), ((6, -1), A), ((7, -1), A)]);
        let w = board_routes(&t, (5, 0), &[(7, -1)]);
        assert_eq!(w[&(7, -1)].fast, way(4, 0, &[(6, -1), (7, -1)]));
    }

    /// H1: the site's own ToxicZone is not crossed on the way.
    #[test]
    fn routes_never_count_the_site_itself() {
        let t = map(&[((6, 0), A), ((7, 0), A), ((8, 0), A), ((9, 0), HexTerrain::ToxicZone)]);
        let w = board_routes(&t, (5, 0), &[(9, 0)]);
        let w = &w[&(9, 0)];
        assert_eq!((w.fast.secs, w.fast.hazards, &w.safe), (8, 0, &None));
    }

    #[test]
    fn routes_leave_an_unreachable_site_out() {
        let t = map(&[((6, 0), A), ((9, 0), A)]);
        let w = board_routes(&t, (5, 0), &[(6, 0), (9, 0)]);
        assert!(w.contains_key(&(6, 0)));
        assert!(!w.contains_key(&(9, 0)));
    }

    /// D6 on seed 57 (ambush d100 13), one drill (20 ore, 10 %): the Safe way's 2 s over the Fast
    /// one adds 2 × 2 points, 14 ≥ 13, so the haul is ambushed; the Fast way's 10 % is not.
    #[test]
    fn ops_detour_raises_the_haul_ambush() {
        let site = dig(1);
        let mut routes = HashMap::new();
        routes.insert((9, 1), SiteRoutes { fast: way(10, 1, &[(9, 1)]), safe: Some(way(12, 0, &[(9, 1)])), fast_kinds: vec![], safe_kinds: vec![] });
        let b = Arc::new(OpsBoard { sites: vec![site.clone()], contract_pool: vec![], routes });
        let store = DemoOpsStore::new(4);
        let (safe, fast) = (Uuid::from_u128(1), Uuid::from_u128(2));
        for (a, route) in [(safe, RouteKind::Safe), (fast, RouteKind::Fast)] {
            let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 100, section_id: None, escort: true };
            store.start_op_for(a, (5, 0), Arc::clone(&b), &site, 57, Some(crew), None, false, route, at(0)).expect("start");
            assert_eq!(store.drill_op(a, at(13)), Ok(DrillOutcome::Drilled));
            assert_eq!(store.pull_op(a, at(20)), Ok(()));
        }
        // Safe: 12 s home, so the ambush fires at the leg's midpoint, 26 s.
        assert_eq!(store.pending_raid(safe, at(26)).map(|p| p.trigger), Some(ambush_trigger(26)));
        assert_eq!(store.pending_raid(fast, at(25)), None);
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
        Arc::new(OpsBoard { sites, contract_pool: pool.iter().map(|&(q, r)| (q, r, 4, 8)).collect(), routes: HashMap::new() })
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

    // ---- BP-18a-1a: ops keyed by slots.

    /// A dig started alone at 0 s (`escort: false`), on site from 10 s, its seed `seed`.
    fn alone(store: &DemoOpsStore, a: Uuid, seed: u32) {
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 1000, section_id: None, escort: false };
        store.start_op(a, (5, 0), board(), &dig(1), seed, Some(crew), None, at(0)).expect("start");
    }

    /// BP-18a-1a: the guard is sent at 10 s and stands at 20 s; a Next at 11 s waits for it, the
    /// same Next at 20 s goes through.
    #[test]
    fn ops_store_next_waits_for_a_walking_guard() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(store.escort_op(a, COMMANDER, None, at(10)), Ok(()));
        assert_eq!(dig_view(&store, a, at(10)).escort_at, Some(ops_time(at(20))));
        assert_eq!(store.next_op(a, &site(), 9, None, Some(LEAD), at(11)), Err(NextRefusal::GuardNotHere));
        assert_eq!(store.next_op(a, &site(), 9, None, Some(LEAD), at(20)), Ok(()));
    }

    /// BP-18a-1a: with no guard sent, Next from the dig is refused however long the Crew waits.
    #[test]
    fn ops_store_next_refuses_a_crew_alone_dig() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(store.next_op(a, &site(), 9, None, Some(LEAD), at(10)), Err(NextRefusal::GuardNotHere));
        assert_eq!(store.next_op(a, &site(), 9, None, Some(LEAD), at(30)), Err(NextRefusal::GuardNotHere));
    }

    /// Reviewer BP-17a F1 (store twin, no wall clock). The guard is sent at 10 s and stands at
    /// 20 s; seed 59 drills twice with no raid, and the 60-ore haul pulled at 13 s is ambushed at
    /// the leg's midpoint, 18 s: born before the guard stands, so it is the Crew's, rolled from
    /// the Crew stream (`crew_fight_roll_pins`: seed 59's ambush falls back with 2).
    #[test]
    fn ops_store_ambush_before_the_guard_stands_is_the_crews() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(store.escort_op(a, COMMANDER, None, at(10)), Ok(()));
        store.set_op_seed(a, 59);
        assert_eq!(store.drill_op(a, at(11)), Ok(DrillOutcome::Drilled));
        assert_eq!(store.drill_op(a, at(12)), Ok(DrillOutcome::Drilled));
        assert_eq!(store.pull_op(a, at(13)), Ok(()));
        let pending = store.pending_raid(a, at(18)).expect("the ambush is revealed at 18 s");
        assert_eq!(pending.trigger, RaidTrigger { trigger_at: at(18), mission_id: DEMO_AMBUSH_MISSION, kind: BattleKind::Ambush });
        assert_eq!(pending.crew, Some(CrewRoll { crew_id: CREW, result: RaidResult::Retreat, heads: 2 }));
    }

    /// BP-18a-1a, the dig term of `make_room`: cap 2; `a` holds a dig on site with nothing
    /// banked and is the least recently touched, `b` holds a board only. A new campaign evicts
    /// `b`, never `a`.
    #[test]
    fn ops_store_eviction_skips_a_dig_with_nothing_banked() {
        let store = DemoOpsStore::new(2);
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        on_site(&store, a, 1, 1000, 7);
        assert!(store.put_board(b, (5, 0), board(), at(1)));
        assert!(store.put_board(c, (5, 0), board(), at(11)));
        assert_eq!(store.len(), 2);
        assert!(store.op_view(a, at(12)).is_some_and(|op| op.dig.is_some()), "the dig entry is kept");
        assert!(store.board(b, (5, 0), at(12)).is_none(), "the board-only entry is evicted");
    }

    // ---- BP-18a-1b-1: the second op, store half.

    const S1: Uuid = Uuid::from_u128(0x51);
    const S2: Uuid = Uuid::from_u128(0x52);
    const C1: Uuid = Uuid::from_u128(0xD1);
    const C2: Uuid = Uuid::from_u128(0xD2);

    /// A contract to `site` led by `commander` and Section `section`, started with the squad named.
    fn named(store: &DemoOpsStore, a: Uuid, site: &OpsSite, commander: Uuid, section: Uuid, now: DateTime<Utc>) -> Result<(), StartRefusal> {
        let lead = ContractStart { commander_id: commander, section_id: Some(section) };
        store.start_op_for(a, (5, 0), board(), site, 7, None, Some(lead), true, RouteKind::Fast, now)
    }

    /// With a Crew-alone dig and a contract out, a return that names no squad can't pick; naming
    /// the dig brings the Crew home.
    #[test]
    fn ops_store_a_bare_return_with_two_ops_is_say_which() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(store.return_op(a, at(11)), Err(ReturnRefusal::SayWhich));
        assert_eq!(store.return_op_for(a, Some(OpSquad::Dig), at(11)), Ok(()));
    }

    /// With a dig and a contract out, the body's op is the dig (F3: `primary_key`).
    #[test]
    fn ops_store_the_primary_op_is_the_dig() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert!(store.op_view(a, at(1)).expect("an op").dig.is_some(), "the dig is the primary op");
    }

    /// The guard Section, once sent, can't lead a contract.
    #[test]
    fn ops_store_a_named_start_refuses_the_guard_section() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(store.escort_op(a, C1, Some(S1), at(10)), Ok(()));
        assert_eq!(named(&store, a, &site(), C2, S1, at(11)), Err(StartRefusal::SectionBusy));
    }

    /// One contract, one squad: a second Section can't take the contract another holds.
    #[test]
    fn ops_store_a_named_start_refuses_a_taken_contract() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(named(&store, a, &site(), C2, S2, at(1)), Err(StartRefusal::Taken));
    }

    /// A Commander leads one op at a time.
    #[test]
    fn ops_store_a_named_start_refuses_a_busy_commander() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(named(&store, a, &supply_raid(), C1, S2, at(1)), Err(StartRefusal::CommanderBusy));
    }

    /// A Section on a contract can't be sent to guard the dig.
    #[test]
    fn ops_store_escort_refuses_a_busy_section() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(store.escort_op(a, C2, Some(S1), at(10)), Err(EscortRefusal::SectionBusy));
    }

    /// A Commander on a contract can't be sent to guard the dig.
    #[test]
    fn ops_store_escort_refuses_a_busy_commander() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(store.escort_op(a, C1, Some(S2), at(10)), Err(EscortRefusal::CommanderBusy));
    }

    /// A next from a contract into a dig while the Crew already digs is refused before the
    /// insert (Reviewer BP-18a-1a F1: without the check, `insert_op`'s assertion fires).
    #[test]
    fn ops_store_next_into_a_held_dig_is_refused() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        let crew = DigStart { crew_id: CREW, commander_id: C1, threshold: 1000, section_id: Some(S1), escort: true };
        assert_eq!(store.next_op_for(a, Some(OpSquad::Section(S1)), &dig(1), 9, Some(crew), None, at(11)), Err(NextRefusal::DigHeld));
    }

    /// A next keeps the squad of the op it leaves (A2): the route's squad (S2/C2) is never taken.
    #[test]
    fn ops_store_next_keeps_the_squad_it_leaves() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        let other = ContractStart { commander_id: C2, section_id: Some(S2) };
        assert_eq!(store.next_op_for(a, Some(OpSquad::Section(S1)), &supply_raid(), 9, None, Some(other), at(11)), Ok(()));
        assert_eq!(named(&store, a, &site(), C2, S2, at(12)), Ok(()), "S2 and C2 are free");
    }

    /// G-B: a Crew-alone dig starts beside a contract with no squad named; any other unnamed
    /// start is still `Busy` (H2).
    #[test]
    fn ops_store_a_crew_alone_dig_starts_beside_a_contract() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 1000, section_id: None, escort: false };
        assert_eq!(store.start_op(a, (5, 0), board(), &dig(1), 7, Some(crew), None, at(1)), Ok(()));
        assert_eq!(store.start_op(a, (5, 0), board(), &supply_raid(), 7, None, Some(LEAD), at(2)), Err(StartRefusal::Busy));
    }

    /// A Requisition is per Section: S2 at base takes one while S1 is out; S1 can't.
    #[test]
    fn ops_store_requisition_is_per_section() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        store.set_credits(a, 240);
        let hold = store.begin_requisition(a, S2, 120, at(1)).expect("S2 is at base");
        store.finish_replace(hold, false);
        assert_eq!(store.begin_requisition(a, S1, 120, at(2)), Err(ReplaceRefusal::OpOut));
    }

    /// Spec check §9 (i): `settle_contracts` keeps a refreshed contract off every slot's hex.
    /// Board pool `[(8,0), (10,0), (3,4)]`; S1 on `GUARD_DUTY` (8,0) and S2 on `SUPPLY_RAID`
    /// (10,0) both arrive at 10; `SUPPLY_RAID` latched at 11 (due 56), `GUARD_DUTY` at 20 (due
    /// 65). At 56 `SUPPLY_RAID`'s only free hex is (3,4); at 65 `GUARD_DUTY` has none ((8,0) its
    /// own, (10,0) S2's team, (3,4) shown), so it stays off. Every free list has one element.
    #[test]
    fn ops_store_a_refreshed_contract_skips_every_held_hex() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        let b = contract_board(&[(8, 0), (10, 0), (3, 4)]);
        let lead = |c, s| Some(ContractStart { commander_id: c, section_id: Some(s) });
        assert_eq!(store.start_op_for(a, (5, 0), Arc::clone(&b), &b.sites[0], 7, None, lead(C1, S1), true, RouteKind::Fast, at(0)), Ok(()));
        assert_eq!(store.start_op_for(a, (5, 0), Arc::clone(&b), &b.sites[2], 7, None, lead(C2, S2), true, RouteKind::Fast, at(0)), Ok(()));
        let supply = RaidTrigger { trigger_at: at(10), mission_id: "SUPPLY_RAID", kind: BattleKind::Contract };
        assert!(store.latch_raid(a, &supply, RaidResult::Held, FightPay::credits(0), at(11)));
        assert!(store.latch_raid(a, &guard_duty_arrival(), RaidResult::Held, FightPay::credits(0), at(20)));
        let at56 = shown(&store, a, at(56));
        let back = at56.iter().find(|s| s.site_id == "contract:SUPPLY_RAID").expect("SUPPLY_RAID is back at 56 s");
        assert_eq!((back.q, back.r), (3, 4));
        assert_eq!(guard_duty(&shown(&store, a, at(65))), None, "no free hex: S2's team stands on (10,0)");
    }

    /// Spec check §9 (ii), Reviewer BP-18a-1a F5: two arrivals settled by one read keep the
    /// later move. The contract goes home at 10 (home 20), the dig at 12 (home 22); the read at
    /// 30 settles the dig (22) first, then the contract (20).
    #[test]
    fn ops_store_two_arrivals_keep_the_later_move() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(store.return_op_for(a, Some(OpSquad::Section(S1)), at(10)), Ok(()));
        assert_eq!(store.return_op_for(a, Some(OpSquad::Dig), at(12)), Ok(()));
        assert_eq!(store.op_view(a, at(30)), None, "both home");
        assert!(store.moved_since(a, at(21), at(30)), "the dig's arrival at 22 s is the last move");
    }

    /// H3, F2's order: every due battle, the contracts by key.
    #[test]
    fn ops_store_pending_raids_lists_every_due_battle() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(named(&store, a, &supply_raid(), C2, S2, at(0)), Ok(()));
        let got: Vec<(Uuid, Option<Uuid>, RaidTrigger)> = store.pending_raids(a, at(12)).into_iter().map(|p| (p.commander_id, p.section_id, p.trigger)).collect();
        let supply = RaidTrigger { trigger_at: at(12), mission_id: "SUPPLY_RAID", kind: BattleKind::Contract };
        assert_eq!(got, vec![(C1, Some(S1), guard_duty_arrival()), (C2, Some(S2), supply)]);
    }

    // ---- BP-18a-1b-2: the route half's store reads.

    /// T16: a Crew-alone dig and two named contracts: the snapshot's op is the dig, the second
    /// value both contracts in key order (S1 < S2), and `ops_state` is the first value.
    #[test]
    fn ops_store_ops_state_all_lists_every_slot_but_the_primary() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(named(&store, a, &supply_raid(), C2, S2, at(0)), Ok(()));
        let (snapshot, more) = store.ops_state_all(a, at(1));
        assert_eq!(snapshot.op.as_ref().map(|op| op.kind), Some("dig"));
        let got: Vec<(Option<Uuid>, &str, String)> = more.iter().map(|m| (m.section_id, m.op.kind, m.op.site_id.clone())).collect();
        assert_eq!(got, vec![(Some(S1), "contract", "contract:GUARD_DUTY".to_string()), (Some(S2), "contract", supply_raid().site_id)]);
        assert_eq!(store.ops_state(a, at(1)), store.ops_state_all(a, at(1)).0);
    }

    /// T17: the busy reads follow `section_busy`/`commander_busy`: a Crew-alone dig holds no
    /// one, a contract its squad, a guard its squad.
    #[test]
    fn ops_store_busy_reads_follow_the_busy_rule() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        assert_eq!(store.busy_sections(a, &[S1, S2], at(1)), vec![S1]);
        assert_eq!(store.busy_commanders(a, &[C1, C2], at(1)), vec![C1]);
        assert_eq!(store.escort_op(a, C2, Some(S2), at(1)), Ok(()));
        assert_eq!(store.busy_sections(a, &[S1, S2], at(1)), vec![S1, S2]);
        assert_eq!(store.busy_commanders(a, &[C1, C2], at(1)), vec![C1, C2]);
    }

    /// T18: `squad_section` reads the selected op's Section, and `None` when the selection finds
    /// no op (`NoOp`, or `SayWhich` with two out).
    #[test]
    fn ops_store_squad_section_reads_the_selected_op() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert_eq!(named(&store, a, &site(), C2, S2, at(0)), Ok(()));
        assert_eq!(store.squad_section(a, None, at(1)), Some(Some(S2)));
        assert_eq!(store.squad_section(a, Some(OpSquad::Section(S2)), at(1)), Some(Some(S2)));
        assert_eq!(store.squad_section(a, Some(OpSquad::Section(S1)), at(1)), None);
        assert_eq!(store.squad_section(a, Some(OpSquad::Dig), at(1)), None);
        alone(&store, a, 7);
        assert_eq!(store.squad_section(a, None, at(1)), None);
    }

    // ---- BP-18a-2: wages and the squad hire. Every entry is made by `put_board` at `at(0)`, so
    // the wage clock starts there.

    fn wage_entry(store: &DemoOpsStore, a: Uuid, credits: u32) {
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        store.set_credits(a, credits);
    }

    fn credits_at(store: &DemoOpsStore, a: Uuid, secs: i64) -> u32 {
        store.ops_state(a, at(secs)).credits_banked
    }

    /// One squad pays 5 a whole minute: nothing at 59 s, 5 at 60 s, 10 at 179 s.
    #[test]
    fn wages_pay_per_whole_minute() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 100);
        assert_eq!(credits_at(&store, a, 59), 100);
        assert_eq!(credits_at(&store, a, 60), 95);
        assert_eq!(credits_at(&store, a, 179), 90);
    }

    /// N5: the anchor moves by whole minutes, so the 30 s past the first minute carry: 95 at 90 s
    /// (anchor 60 s), 90 at 130 s (anchor 120 s).
    #[test]
    fn wages_carry_a_partial_minute() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 100);
        assert_eq!(credits_at(&store, a, 90), 95);
        assert_eq!(credits_at(&store, a, 130), 90);
    }

    /// N4: 2 minutes owe 10 against 7 banked; the bank stops at 0 and the rest is forgiven.
    #[test]
    fn wages_never_go_below_0() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 7);
        assert_eq!(credits_at(&store, a, 120), 0);
        store.set_credits(a, 50);
        assert_eq!(credits_at(&store, a, 121), 50, "nothing carried as a debt");
    }

    /// N6: after a landed hire the wages count two squads: 10 a minute.
    #[test]
    fn wages_are_per_squad() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 700);
        let hold = store.begin_hire(a, 1, at(0)).expect("a hire");
        store.finish_hire(hold, true, Some(2));
        assert_eq!(store.ops_body(a, at(1)).2, 2);
        assert_eq!(credits_at(&store, a, 59), 100);
        assert_eq!(credits_at(&store, a, 60), 90);
    }

    /// A clock behind the anchor pays nothing and leaves the anchor where it was.
    #[test]
    fn wages_pay_nothing_behind_the_anchor() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 100);
        assert_eq!(credits_at(&store, a, -120), 100);
        assert_eq!(credits_at(&store, a, 60), 95);
    }

    /// N1: with two Sections the hire refuses `Cap` and debits nothing.
    #[test]
    fn begin_hire_refuses_at_two_squads() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 600);
        assert_eq!(store.begin_hire(a, 2, at(1)), Err(HireRefusal::Cap));
        assert_eq!(credits_at(&store, a, 2), 600);
    }

    /// N2: one Section and a hire in flight make two: a second hire refuses `Cap`, though the
    /// bank could pay for it.
    #[test]
    fn begin_hire_refuses_while_one_is_in_flight() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 1200);
        let hold = store.begin_hire(a, 1, at(1)).expect("the first hire");
        assert_eq!(store.begin_hire(a, 1, at(2)), Err(HireRefusal::Cap));
        assert_eq!(credits_at(&store, a, 3), 600);
        store.finish_hire(hold, false, None);
    }

    /// 599 banked is short; a campaign with no entry has banked nothing.
    #[test]
    fn begin_hire_refuses_when_short() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 599);
        assert_eq!(store.begin_hire(a, 1, at(1)), Err(HireRefusal::NotEnoughCredits));
        assert_eq!(credits_at(&store, a, 2), 599);
        assert_eq!(store.begin_hire(Uuid::from_u128(9), 1, at(1)), Err(HireRefusal::NotEnoughCredits));
    }

    /// `landed: false` refunds and ends the flight (a second hire then passes); `landed: true`
    /// keeps the debit and sets the wage count.
    #[test]
    fn finish_hire_refunds_or_keeps_the_debit() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        wage_entry(&store, a, 600);
        let hold = store.begin_hire(a, 1, at(1)).expect("a hire");
        assert_eq!(credits_at(&store, a, 2), 0);
        store.finish_hire(hold, false, None);
        assert_eq!(credits_at(&store, a, 3), 600, "refunded");
        assert_eq!(store.ops_body(a, at(3)).2, 1, "no count given, none set");
        let hold = store.begin_hire(a, 1, at(4)).expect("the flight ended");
        store.finish_hire(hold, true, Some(2));
        assert_eq!(credits_at(&store, a, 5), 0, "kept");
        assert_eq!(store.ops_body(a, at(5)).2, 2);
    }

    // ---- BP-19a: the Foreman's settlement and the threshold bands.

    fn due(stress: u32, xp: u32) -> ForemanDue {
        ForemanDue { stress, xp }
    }

    /// Wiki's rows (BP-19a prompt, T1): `(drills, heads, ended_by, carried, close) → (stress, xp)`.
    #[test]
    fn dig_settlement_pays_by_how_the_dig_ended() {
        let rows = [
            ((2, 0, Some(OpEnd::Pulled), 40, DigClose::Home), (16, 25)),
            ((0, 0, Some(OpEnd::Pulled), 0, DigClose::Home), (10, 8)),
            ((3, 2, Some(OpEnd::Meltdown), 0, DigClose::Home), (29, 15)),
            ((1, 0, Some(OpEnd::WalkedAway), 0, DigClose::Home), (13, 8)),
            ((2, 3, Some(OpEnd::Retreat), 20, DigClose::Home), (31, 15)),
            ((2, 8, None, 0, DigClose::CrewWiped), (56, 0)),
            ((2, 0, None, 0, DigClose::GuardWiped), (16, 15)),
            ((4, 0, None, 0, DigClose::LeftForNext), (22, 8)),
        ];
        for ((drills, heads, ended_by, carried, close), (stress, xp)) in rows {
            assert_eq!(dig_settlement(drills, heads, ended_by, carried, close), due(stress, xp), "{drills} {heads} {ended_by:?} {carried} {close:?}");
        }
    }

    /// Wiki's rows (BP-19a prompt, T2): `(stress, shattered, rank, coolant_jockey) → threshold`.
    #[test]
    fn foreman_threshold_follows_the_band_and_rank() {
        let rows = [
            ((30, false, 1, false), 100),
            ((30, false, 1, true), 115),
            ((31, false, 1, false), 95),
            ((31, false, 1, true), 102),
            ((70, false, 1, true), 102),
            ((71, false, 1, false), 90),
            ((71, false, 1, true), 90),
            ((99, false, 1, true), 90),
            ((100, false, 1, true), 90),
            ((40, true, 1, false), 90),
            ((99, false, 2, false), 92),
            ((100, true, 2, false), 90),
            ((0, false, 3, true), 119),
            ((0, false, 5, false), 108),
        ];
        for ((stress, is_shattered, rank, coolant_jockey), want) in rows {
            let f = ForemanRead { stress, is_shattered, rank, coolant_jockey };
            assert_eq!(foreman_threshold(f), want, "{f:?}");
        }
    }

    /// BP-19c: the band at every cut (`Commander_Stress_System` §2: 0–30, 31–70, 71–99, and 100
    /// or the flag). (100, false) decides the 100 term alone (the stores flag a row at 100, so no
    /// route reaches it); (40, true) and (0, true) decide the flag alone.
    #[test]
    fn stress_band_cuts_and_the_flag() {
        let rows = [
            (0, false, StressBand::Rested),
            (30, false, StressBand::Rested),
            (31, false, StressBand::Strained),
            (70, false, StressBand::Strained),
            (71, false, StressBand::BreakingPoint),
            (99, false, StressBand::BreakingPoint),
            (100, false, StressBand::Shattered),
            (40, true, StressBand::Shattered),
            (0, true, StressBand::Shattered),
        ];
        for (stress, is_shattered, want) in rows {
            assert_eq!(stress_band(stress, is_shattered), want, "({stress}, {is_shattered})");
        }
    }

    /// Seed 2, two drills (60 ore) pulled at 20 s, home at 30 s: the first drain is the landed
    /// dig's (10 + 3 × 2, 25), the second is the default.
    #[test]
    fn foreman_due_drains_once() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 2, 2);
        assert_eq!(store.take_foreman_due(a, at(30)), due(16, 25));
        assert_eq!(store.take_foreman_due(a, at(31)), ForemanDue::default());
    }

    /// E1, a landed haul: seed 2, three drills (120 ore) pulled at 20 s, home at 30 s.
    #[test]
    fn foreman_due_landed_haul() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        pulled(&store, a, 2, 3);
        assert_eq!(store.take_foreman_due(a, at(30)), due(10 + 3 * 3, 25));
    }

    /// E2 then E1: seed 1 melts down on its 7th drill at 20 s with 2 heads
    /// (`ops_store_meltdown_rolls_no_raid`), home at 30 s.
    #[test]
    fn foreman_due_meltdown() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 1);
        for i in 1..=6 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(store.drill_op(a, at(20)), Ok(DrillOutcome::Meltdown { crew_id: CREW, heads: 2 }));
        assert_eq!(store.take_foreman_due(a, at(30)), due(10 + 3 * 7 + 5 * 2, 15));
    }

    /// E7 then E1: seed 2, two drills, walked away at 13 s, home at 23 s.
    #[test]
    fn foreman_due_walked_away() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 2);
        store.drill_op(a, at(11)).expect("drill");
        store.drill_op(a, at(12)).expect("drill");
        assert_eq!(store.return_op(a, at(13)), Ok(()));
        assert_eq!(store.take_foreman_due(a, at(23)), due(10 + 3 * 2, 8));
    }

    /// E4 then E1: seed 45 alone, drill 1 at 11 s brings raiders and the Crew falls back with 2
    /// heads (`crew_fight_roll_pins`, `ops_unescorted_raid_is_the_crews_retreat`), home at 21 s.
    #[test]
    fn foreman_due_retreat_home() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 45);
        assert_eq!(store.drill_op(a, at(11)), Ok(DrillOutcome::CrewFight { crew_id: CREW, heads: 2 }));
        assert_eq!(store.take_foreman_due(a, at(21)), due(10 + 3 + 5 * 2, 15));
    }

    /// E3: seed 505 alone, drill 1 at 11 s: the Crew is wiped out, 8 heads
    /// (`crew_fight_roll_pins`, `ops_crew_wipeout_ends_the_op_and_takes_every_head`).
    #[test]
    fn foreman_due_crew_wiped_on_site() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 505);
        assert_eq!(store.drill_op(a, at(11)), Ok(DrillOutcome::CrewFight { crew_id: CREW, heads: 8 }));
        assert_eq!(store.take_foreman_due(a, at(12)), due(10 + 3 + 40, 0));
    }

    /// E5: seed 7's raid on drill 1 (`ops_store_latch_applies_each_result`), the guard's fight
    /// latched Wiped at 20 s.
    #[test]
    fn foreman_due_guard_wiped() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        on_site(&store, a, 1, 100, 7);
        store.drill_op(a, at(11)).expect("drill");
        assert!(store.latch_raid(a, &trigger(11), RaidResult::Wiped, FightPay::credits(0), at(20)));
        assert_eq!(store.take_foreman_due(a, at(21)), due(10 + 3, 15));
    }

    /// E6: the haul of `ops_store_ambush_before_the_guard_stands_is_the_crews` (seed 59, two
    /// drills, the Crew's ambush revealed at 18 s), latched as a Wipeout of 8 heads at 19 s.
    #[test]
    fn foreman_due_crew_wiped_on_the_haul() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        alone(&store, a, 7);
        assert_eq!(store.escort_op(a, COMMANDER, None, at(10)), Ok(()));
        store.set_op_seed(a, 59);
        assert_eq!(store.drill_op(a, at(11)), Ok(DrillOutcome::Drilled));
        assert_eq!(store.drill_op(a, at(12)), Ok(DrillOutcome::Drilled));
        assert_eq!(store.pull_op(a, at(13)), Ok(()));
        let pending = store.pending_raid(a, at(18)).expect("the ambush is revealed at 18 s");
        assert!(store.latch_crew(a, &pending.trigger, CrewRoll { crew_id: CREW, result: RaidResult::Wiped, heads: 8 }, at(19)));
        assert!(store.op_view(a, at(19)).is_none(), "the op is cleared");
        assert_eq!(store.take_foreman_due(a, at(20)), due(10 + 3 * 2 + 5 * 8, 0));
    }

    /// E8: an escorted seed-2 dig, four drills, sent on to `site()`'s contract at 20 s; then a
    /// contract sent on from its slot (`next_to_supply_raid`) on a fresh entry owes nothing.
    #[test]
    fn foreman_due_next_from_a_dig() {
        let store = DemoOpsStore::new(4);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        on_site(&store, a, 1, 1000, 2);
        for i in 1..=4 {
            assert_eq!(store.drill_op(a, at(10 + i)), Ok(DrillOutcome::Drilled), "drill {i}");
        }
        assert_eq!(store.next_op(a, &site(), 9, None, Some(LEAD), at(20)), Ok(()));
        assert_eq!(store.take_foreman_due(a, at(21)), due(10 + 3 * 4, 8));
        next_to_supply_raid(&store, b);
        assert_eq!(store.take_foreman_due(b, at(21)), ForemanDue::default());
    }

    /// Cap 2. `a` walks away from a one-drill dig (seed 2) and is home at 22 s, banking nothing:
    /// only its tally keeps it, so the new `c` evicts the board-only `b`. Drained, `a` is the
    /// least recently touched and goes for `d`; then `c`, a fresh insert, goes for `e`.
    #[test]
    fn make_room_keeps_an_undrained_foreman_due() {
        let store = DemoOpsStore::new(2);
        let (a, b, c, d, e) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3), Uuid::from_u128(4), Uuid::from_u128(5));
        on_site(&store, a, 1, 100, 2);
        store.drill_op(a, at(11)).expect("drill");
        assert_eq!(store.return_op(a, at(12)), Ok(()));
        assert_eq!(store.ops_snapshot(a, at(22)), (None, 0, OpsBase::default()), "home, nothing banked");
        assert!(store.put_board(b, (5, 0), board(), at(30)));
        assert!(store.put_board(c, (5, 0), board(), at(40)));
        assert_eq!(store.len(), 2);
        assert!(store.board(b, (5, 0), at(41)).is_none(), "the board-only entry is evicted");
        assert_eq!(store.take_foreman_due(a, at(42)), due(13, 8), "the tally kept `a`");
        assert!(store.put_board(d, (5, 0), board(), at(50)));
        assert!(store.board(a, (5, 0), at(51)).is_none(), "drained, `a` is evicted");
        assert!(store.put_board(e, (5, 0), board(), at(60)));
        assert!(store.board(c, (5, 0), at(61)).is_none(), "a fresh entry is evicted");
        assert!(store.board(d, (5, 0), at(61)).is_some() && store.board(e, (5, 0), at(61)).is_some());
    }

    /// A put-back tally is drained once.
    #[test]
    fn foreman_due_restore_adds_back() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        store.restore_foreman_due(a, due(16, 25));
        assert_eq!(store.take_foreman_due(a, at(1)), due(16, 25));
        assert_eq!(store.take_foreman_due(a, at(2)), ForemanDue::default());
    }

    // ---- BP-19b: the bench claim. Every entry is made at `at(0)`, so `bench_at` starts there.

    /// S1: whole minutes only, claimed once; the anchor moves by whole minutes, so a part-minute
    /// carries (61 s claims 1 and leaves `bench_at` at 60 s; 3 min 1 s then claims 2).
    #[test]
    fn bench_claim_takes_whole_minutes_once() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        assert_eq!(store.take_leader_claim(a, at(59)).bench_mins, 0);
        assert_eq!(store.take_leader_claim(a, at(61)).bench_mins, 1);
        assert_eq!(store.take_leader_claim(a, at(61)).bench_mins, 0, "claimed once");
        assert_eq!(store.take_leader_claim(a, at(181)).bench_mins, 2);
    }

    /// S2: the claim reports the dig slot (the Foreman out) and the busy leads: none with no op; a
    /// guarded dig holds the Foreman and its Commander; a Crew-alone dig holds the Foreman only; a
    /// contract holds its lead only; a homeward haul not yet arrived still holds the Foreman.
    #[test]
    fn bench_claim_reports_the_dig_and_the_busy_leads() {
        let store = DemoOpsStore::new(2);
        let a = Uuid::from_u128(1);
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        let c = store.take_leader_claim(a, at(1));
        assert_eq!((c.foreman_out, c.busy), (false, vec![]), "no op");

        let b = Uuid::from_u128(2);
        on_site(&store, b, 1, 1000, 7);
        let c = store.take_leader_claim(b, at(11));
        assert_eq!((c.foreman_out, c.busy), (true, vec![COMMANDER]), "a guarded dig");

        let store = DemoOpsStore::new(2);
        alone(&store, a, 7);
        let c = store.take_leader_claim(a, at(11));
        assert_eq!((c.foreman_out, c.busy), (true, vec![]), "a Crew-alone dig");

        let store = DemoOpsStore::new(2);
        assert_eq!(named(&store, a, &site(), C1, S1, at(0)), Ok(()));
        let c = store.take_leader_claim(a, at(1));
        assert_eq!((c.foreman_out, c.busy), (false, vec![C1]), "a contract out");

        let store = DemoOpsStore::new(2);
        pulled(&store, a, 2, 2);
        let c = store.take_leader_claim(a, at(25));
        assert!(c.foreman_out, "a homeward haul not yet arrived");
    }

    // ---- BP-20a-2: hazards on the way (§4e C3).

    /// T1 (F-A): the kinds a way crosses, in path order, the site never and the gateway's
    /// neighbour always; their count is the way's `hazards`. (c) is `routes_go_round_a_toxic_zone`'s map.
    #[test]
    fn hazard_kinds_follow_the_way_and_skip_the_site() {
        let check = |w: &SiteRoutes| {
            assert_eq!(w.fast_kinds.len(), w.fast.hazards as usize);
            if let Some(s) = &w.safe {
                assert_eq!(w.safe_kinds.len(), s.hazards as usize);
            }
        };
        let t = map(&[((6, 0), A), ((7, 0), A), ((8, 0), HexTerrain::ToxicZone)]);
        let w = &board_routes(&t, (5, 0), &[(8, 0)])[&(8, 0)];
        assert_eq!((w.fast_kinds.clone(), w.safe_kinds.clone()), (vec![], vec![]), "the site's own gas never counts");
        check(w);
        let t = map(&[((6, 0), HexTerrain::RuinedCity), ((7, 0), A)]);
        let w = &board_routes(&t, (5, 0), &[(7, 0)])[&(7, 0)];
        assert_eq!((w.fast_kinds.clone(), w.safe_kinds.clone()), (vec![Hazard::Ruins], vec![]), "path[0] counts");
        check(w);
        let w = &board_routes(&detour_map(HexTerrain::ToxicZone, A), (5, 0), &[(9, 0)])[&(9, 0)];
        assert_eq!((w.fast_kinds.clone(), w.safe_kinds.clone()), (vec![Hazard::Gas], vec![]));
        check(w);
    }

    fn hit(hazard: Hazard, leg: HazardLeg, ore: u32, stress: u32, wounded: u32, morale: u32) -> HazardHit {
        HazardHit { hazard, leg, ore, stress, wounded, morale }
    }

    /// T2 (F-J): seed 1's hazard draws are 21, 7, 17, 51 (measured with a Python port of `rng.rs`):
    /// out Gas misses, out Ruins hits, home Ruins (travel order: the way reversed) hits, home Gas misses.
    #[test]
    fn hazard_hits_roll_out_then_home_in_travel_order() {
        use Hazard::{Gas, Ruins};
        assert_eq!(
            hazard_hits(1, 20, HazardPayer::Crew, &[Gas, Ruins], &[Gas, Ruins], 0),
            Some(vec![hit(Ruins, HazardLeg::Out, 0, 5, 0, 5), hit(Ruins, HazardLeg::Home, 0, 5, 0, 5)])
        );
    }

    /// T3: no knob or no hazard, no report; both, one roll per hex per leg.
    #[test]
    fn hazard_hits_need_a_hazard_and_a_knob() {
        use Hazard::Gas;
        assert_eq!(hazard_hits(9, 0, HazardPayer::Crew, &[Gas], &[Gas], 0), None);
        assert_eq!(hazard_hits(9, 100, HazardPayer::Crew, &[], &[], 0), None);
        assert_eq!(hazard_hits(9, 100, HazardPayer::Crew, &[Gas], &[Gas], 0).map(|h| h.len()), Some(2));
    }

    /// T4 (W1): every roll hits at 100; a Section's hits are wounds (gas also stress), a Crew's
    /// are stress and morale, and a crust takes a quarter of the cart on the way home only.
    #[test]
    fn hazard_costs_by_payer_and_leg() {
        use Hazard::{Crust, Gas, Ruins};
        use HazardLeg::{Home, Out};
        assert_eq!(
            hazard_hits(3, 100, HazardPayer::Section, &[Gas, Crust, Ruins], &[], 0),
            Some(vec![hit(Gas, Out, 0, 10, 1, 0), hit(Crust, Out, 0, 0, 1, 0), hit(Ruins, Out, 0, 0, 1, 0)])
        );
        assert_eq!(
            hazard_hits(3, 100, HazardPayer::Crew, &[Gas, Crust, Ruins], &[Crust], 40),
            Some(vec![hit(Gas, Out, 0, 10, 0, 5), hit(Crust, Out, 0, 0, 0, 0), hit(Ruins, Out, 0, 5, 0, 5), hit(Crust, Home, 10, 0, 0, 0)])
        );
    }

    /// T5 (W5): each kind once, in the order met; nothing on a clean way.
    #[test]
    fn hazard_warning_names_each_kind_once() {
        assert_eq!(hazard_warning(&[Hazard::Gas, Hazard::Ruins, Hazard::Gas]), Some("gas, ruins".to_string()));
        assert_eq!(hazard_warning(&[Hazard::Crust]), Some("soft crust".to_string()));
        assert_eq!(hazard_warning(&[]), None);
    }

    /// A board holding `sites`, with a Fast way only to `at` crossing `kinds`.
    fn hazard_board(sites: Vec<OpsSite>, at: (i32, i32), kinds: Vec<Hazard>) -> Arc<OpsBoard> {
        let mut routes = HashMap::new();
        let hazards = kinds.len() as u32;
        routes.insert(at, SiteRoutes { fast: way(10, hazards, &[at]), safe: None, fast_kinds: kinds, safe_kinds: vec![] });
        Arc::new(OpsBoard { sites, contract_pool: vec![], routes })
    }

    /// A store at cap 2 with the hazard knob at `pct`.
    fn hazard_store(pct: u32) -> DemoOpsStore {
        let mut store = DemoOpsStore::new(2);
        store.set_hazard_pct(pct);
        store
    }

    /// T6: `ops_detour_raises_the_haul_ambush`'s Fast flow (seed 57, one drill, 20 ore, pulled at
    /// 20 s, no ambush, home at 30 s), the Crew alone, over crust then gas, every roll a hit,
    /// against a twin at 0: nothing is owed on the way out; at the landing the home crust takes
    /// 5 of 20, the Foreman owes 20 more and the Crew 10 morale.
    #[test]
    fn a_dig_pays_its_hazards_when_it_lands() {
        use Hazard::{Crust, Gas};
        use HazardLeg::{Home, Out};
        let a = Uuid::from_u128(1);
        let site = dig(1);
        let (hot, twin) = (hazard_store(100), hazard_store(0));
        for store in [&hot, &twin] {
            let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 100, section_id: None, escort: false };
            let b = hazard_board(vec![site.clone()], (9, 1), vec![Crust, Gas]);
            store.start_op(a, (5, 0), b, &site, 57, Some(crew), None, at(0)).expect("start");
            assert_eq!(store.take_leader_claim(a, at(10)).hazards, HazardDue::default(), "nothing owed on the way out");
            assert!(store.ops_body(a, at(10)).3.is_empty());
            assert_eq!(store.drill_op(a, at(13)), Ok(DrillOutcome::Drilled));
            assert_eq!(store.pull_op(a, at(20)), Ok(()));
        }
        assert_eq!((hot.ops_state(a, at(30)).ore_banked, twin.ops_state(a, at(30)).ore_banked), (15, 20));
        let (hot_claim, twin_claim) = (hot.take_leader_claim(a, at(30)), twin.take_leader_claim(a, at(30)));
        assert_eq!(hot_claim.due.stress, twin_claim.due.stress + 20);
        assert_eq!(hot_claim.hazards, HazardDue { morale: BTreeMap::from([(CREW, 10)]), ..HazardDue::default() });
        assert_eq!(
            hot.ops_body(a, at(31)).3,
            vec![TripReport {
                seq: 1,
                site_id: site.site_id.clone(),
                route: RouteKind::Fast,
                hits: vec![hit(Crust, Out, 0, 0, 0, 0), hit(Gas, Out, 0, 10, 0, 5), hit(Gas, Home, 0, 10, 0, 5), hit(Crust, Home, 5, 0, 0, 0)],
            }]
        );
    }

    /// T7: `ops_store_eviction_skips_banked_credits`' flow (latched at 12 s, back at 13 s, home at
    /// 23 s) with `SECTION` sent, over gas both ways: the Commander owes 20 stress and the Section 2
    /// wounded; nothing is banked by the road.
    #[test]
    fn a_contract_owes_its_section_and_commander_when_it_lands() {
        let store = hazard_store(100);
        let a = Uuid::from_u128(1);
        let lead = ContractStart { commander_id: COMMANDER, section_id: Some(SECTION) };
        store.start_op(a, (5, 0), hazard_board(vec![site()], (8, 0), vec![Hazard::Gas]), &site(), 7, None, Some(lead), at(0)).expect("start");
        let t = RaidTrigger { trigger_at: at(10), mission_id: "GUARD_DUTY", kind: BattleKind::Contract };
        assert!(store.latch_raid(a, &t, RaidResult::Held, FightPay::credits(0), at(12)));
        store.return_op(a, at(13)).expect("return");
        assert_eq!(store.ops_state(a, at(23)).ore_banked, 0);
        assert_eq!(
            store.take_leader_claim(a, at(23)).hazards,
            HazardDue { stress: BTreeMap::from([(COMMANDER, 20)]), wounded: BTreeMap::from([(SECTION, 2)]), morale: BTreeMap::new() }
        );
        let reports = store.ops_body(a, at(24)).3;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].hits, vec![hit(Hazard::Gas, HazardLeg::Out, 0, 10, 1, 0), hit(Hazard::Gas, HazardLeg::Home, 0, 10, 1, 0)]);
    }

    /// T8 (W1): an escorted dig over gas, walked away and home: the Crew pays, the escort's
    /// Section and Commander nothing.
    #[test]
    fn a_guarded_dig_charges_the_crew_not_the_escort() {
        let store = hazard_store(100);
        let a = Uuid::from_u128(1);
        let site = dig(1);
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 100, section_id: Some(SECTION), escort: true };
        store.start_op(a, (5, 0), hazard_board(vec![site.clone()], (9, 1), vec![Hazard::Gas]), &site, 2, Some(crew), None, at(0)).expect("start");
        assert_eq!(store.return_op(a, at(11)), Ok(()));
        assert_eq!(
            store.take_leader_claim(a, at(21)).hazards,
            HazardDue { stress: BTreeMap::new(), wounded: BTreeMap::new(), morale: BTreeMap::from([(CREW, 10)]) }
        );
    }

    /// T9 (W4): an escorted dig at `dig(1)` (no ways on the board) hops to `dig(3)`, whose Fast
    /// way crosses gas: the hop (4 s) crosses nothing, the way home rolls once.
    #[test]
    fn a_next_hop_rolls_only_its_way_home() {
        let store = hazard_store(100);
        let a = Uuid::from_u128(1);
        let (from, to) = (dig(1), dig(3));
        let crew = DigStart { crew_id: CREW, commander_id: COMMANDER, threshold: 100, section_id: None, escort: true };
        let b = hazard_board(vec![from.clone(), to.clone()], (9, 3), vec![Hazard::Gas]);
        store.start_op(a, (5, 0), b, &from, 2, Some(crew), None, at(0)).expect("start");
        assert_eq!(store.next_op(a, &to, 2, Some(crew), None, at(10)), Ok(()));
        assert_eq!(store.return_op(a, at(14)), Ok(()));
        let reports = store.ops_body(a, at(24)).3;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].hits, vec![hit(Hazard::Gas, HazardLeg::Home, 0, 10, 0, 5)]);
    }

    /// T10 (`make_room_keeps_an_undrained_foreman_due` is the model): (a) a put-back tally of zeros
    /// leaves the entry at the default, so it is evicted; (b) an undrained tally keeps it until drained.
    #[test]
    fn make_room_keeps_an_undrained_hazard_tally() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let store = DemoOpsStore::new(1);
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        store.restore_hazard_due(a, &HazardDue { stress: BTreeMap::from([(COMMANDER, 0)]), wounded: BTreeMap::from([(SECTION, 0)]), morale: BTreeMap::from([(CREW, 0)]) });
        assert_eq!(store.start_op(b, (5, 0), board(), &site(), 7, None, Some(LEAD), at(1)), Ok(()));
        assert!(store.board(a, (5, 0), at(2)).is_none(), "a zero tally keeps nothing");

        let store = DemoOpsStore::new(1);
        assert!(store.put_board(a, (5, 0), board(), at(0)));
        store.restore_hazard_due(a, &HazardDue { morale: BTreeMap::from([(CREW, 5)]), ..HazardDue::default() });
        assert_eq!(store.start_op(b, (5, 0), board(), &site(), 7, None, Some(LEAD), at(1)), Err(StartRefusal::Full));
        assert_eq!(store.take_leader_claim(a, at(2)).hazards.morale, BTreeMap::from([(CREW, 5)]));
        assert_eq!(store.start_op(b, (5, 0), board(), &site(), 7, None, Some(LEAD), at(3)), Ok(()));
    }
}
