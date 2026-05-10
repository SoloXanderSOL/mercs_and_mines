// "The Ore Run" — static hackathon demo scenario definition.
// All names, numbers, and loadouts are fixed. Nothing here is procedurally generated.
// Source of truth: Battle_Simulation_Demo_Scenario.md

use serde::{Deserialize, Serialize};

// ── Scrap-Rocket constants ──────────────────────────────────────────────────

pub const WARTHOG_HP: i32 = 800;
/// High-AP weapon — penetrates all vehicle armour in this engagement.
pub const AP_SCRAP_ROCKET: i32 = 80;
/// ~35% of Warthog HP — a genuine threat, not a kill.
pub const SCRAP_ROCKET_DAMAGE: i32 = (WARTHOG_HP as f32 * 0.35) as i32; // 280
pub const SCRAP_ROCKET_FIRE_TICK: u32 = 2;
/// d100 roll ≤ this → misfire. Draw #1 in tick 2 RNG sequence.
pub const SCRAP_ROCKET_MISFIRE_THRESHOLD: u32 = 30;

// ── Vehicle stats ───────────────────────────────────────────────────────────

pub const WARTHOG_AT: i32 = 30;
pub const WARTHOG_EVASION: i32 = 10;
pub const DUSTMITE_HP: i32 = 350;
pub const DUSTMITE_AT: i32 = 20; // 0 armour slots — lighter protection
pub const DUSTMITE_EVASION: i32 = 45; // high evasion FAV
pub const RHINO_HP: i32 = 600;
pub const RHINO_AT: i32 = 30; // 2 armour slots
pub const RHINO_EVASION: i32 = 5;

// ── Weapon stats ────────────────────────────────────────────────────────────

/// Thumper grenade launcher — AoE tag. Kills this many pack members per hit.
pub const THUMPER_AOE_KILLS_PER_HIT: u32 = 3;
pub const THUMPER_AP: i32 = 50;
pub const THUMPER_ACCURACY: i32 = 80;

pub const SPITFIRE_AP: i32 = 25;
pub const SPITFIRE_DAMAGE: i32 = 12;
pub const SPITFIRE_ACCURACY: i32 = 70;

// ── Pack stats ──────────────────────────────────────────────────────────────

pub const PACK_INDIVIDUAL_HP: i32 = 25;
/// Raccoon AP — beats infantry AT 10, cannot beat vehicle AT 20+.
pub const PACK_WEAPON_AP: i32 = 12;
pub const PACK_WEAPON_DAMAGE: i32 = 7;
pub const PACK_WEAPON_ACCURACY: i32 = 40;
pub const PACK_EVASION: i32 = 55;
pub const PACK_ARMOR_AT: i32 = 0;

// ── Section stats ───────────────────────────────────────────────────────────

pub const SECTION_INDIVIDUAL_HP: i32 = 10;
pub const SECTION_EVASION: i32 = 45;
pub const SECTION_ARMOR_AT: i32 = 10; // Scavenged Kevlar
pub const SLUGGER_AP: i32 = 12;
pub const SLUGGER_DAMAGE: i32 = 8;
pub const SLUGGER_ACCURACY: i32 = 45;

// ── Max ticks before forced resolution ─────────────────────────────────────

pub const ORE_RUN_MAX_TICKS: u32 = 12;

// ── Approach system ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Approach {
    AggressiveCharge, // Rock
    TerrainManeuver,  // Paper
    MaintainRange,    // Scissors
    EscapeRearguard,  // No RPS face
}

/// Resolved modifier stack applied for the entire engagement.
/// Produced by resolve_approach_modifiers() before Tick 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproachModifiers {
    /// Multiplied onto final_damage before kill calculation. 1.0 = no change.
    pub damage_dealt_multiplier: f32,
    /// Added to own evasion stat (flat, applied to hit rolls).
    pub own_evasion_bonus: i32,
    /// Subtracted from enemy evasion stat.
    pub enemy_evasion_penalty: i32,
    /// Added to own accuracy stat.
    pub own_accuracy_bonus: i32,
    /// Subtracted from enemy accuracy stat.
    pub enemy_accuracy_penalty: i32,
    /// Only set for EscapeRearguard: main force exits after this tick.
    pub escape_after_tick: Option<u32>,
}

impl ApproachModifiers {
    fn zeroed() -> Self {
        Self {
            damage_dealt_multiplier: 1.0,
            own_evasion_bonus: 0,
            enemy_evasion_penalty: 0,
            own_accuracy_bonus: 0,
            enemy_accuracy_penalty: 0,
            escape_after_tick: None,
        }
    }
}

/// Resolve both sides' modifiers simultaneously before Tick 1.
/// Returns (defender_modifiers, attacker_modifiers).
///
/// Raccoons always charge. This is characterisation, not a simplification.
pub fn resolve_approach_modifiers(player: Approach) -> (ApproachModifiers, ApproachModifiers) {
    let raccoon = Approach::AggressiveCharge;
    let mut def = base_modifiers(player);
    let mut att = base_modifiers(raccoon);
    apply_matchup_bonus(player, raccoon, &mut def, &mut att);
    (def, att)
}

fn base_modifiers(approach: Approach) -> ApproachModifiers {
    let mut m = ApproachModifiers::zeroed();
    match approach {
        Approach::AggressiveCharge => {
            m.damage_dealt_multiplier = 1.15;
            m.enemy_evasion_penalty = 10;
        }
        Approach::TerrainManeuver => {
            m.own_evasion_bonus = 20;
            m.enemy_accuracy_penalty = 10;
        }
        Approach::MaintainRange => {
            m.own_accuracy_bonus = 10;
            m.enemy_accuracy_penalty = 15; // melee-capable enemies at close range
        }
        Approach::EscapeRearguard => {
            m.escape_after_tick = Some(3);
        }
    }
    m
}

/// Rock beats Scissors: Aggressive vs Maintain Range → +20% additional damage (total +35%).
/// Paper beats Rock: Terrain vs Aggressive → +20% additional evasion (total +40%).
/// Scissors beats Paper: Maintain vs Terrain → +20% additional accuracy (total +30%).
/// Matched: no bonus. Any vs Escape: no standard matchup bonus (Escape special rules apply).
fn apply_matchup_bonus(
    player: Approach,
    raccoon: Approach,
    def: &mut ApproachModifiers,
    att: &mut ApproachModifiers,
) {
    if player == Approach::EscapeRearguard || raccoon == Approach::EscapeRearguard {
        return; // Escape special rules govern; no standard RPS bonus.
    }
    match (player, raccoon) {
        // Player Rock beats Player Scissors (raccoon always Rock, so Rock vs Rock = no bonus)
        // Player Scissors vs Raccoon Rock → Raccoon wins: att gets +20% damage
        (Approach::MaintainRange, Approach::AggressiveCharge) => {
            att.damage_dealt_multiplier += 0.20; // raccoon Rock beats player Scissors
        }
        // Player Paper beats Raccoon Rock
        (Approach::TerrainManeuver, Approach::AggressiveCharge) => {
            def.own_evasion_bonus += 20; // total +40%
        }
        // Raccoon Rock beats Player Scissors (same as first arm — already covered)
        // Player Rock vs Raccoon Rock: matched, no bonus
        (Approach::AggressiveCharge, Approach::AggressiveCharge) => {}
        _ => {} // other combinations against fixed AggressiveCharge raccoons are covered above
    }
}

// ── Flavour text pools ──────────────────────────────────────────────────────

/// All event types that can trigger a flavour text draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlavourEvent {
    ThumperAoEHit,
    SpitfireHit,
    InfantrySluggerHit,
    InfantryTakesCasualty,
    PackScatter,
    ScrapRocketHit,
    ScrapRocketMisfire,
}

/// Returns the flavour text pool for a given event.
/// Selection is done by the caller using the engagement RNG (draw #4 per tick).
pub fn flavour_pool(event: FlavourEvent) -> &'static [&'static str] {
    match event {
        FlavourEvent::ThumperAoEHit => &[
            "The Warthog's grenade launcher coughs once. The internal organs of the lead pack scatter across the road like a broken meat piñata.",
            "Thumper fires into the mass. The raccoons disperse even more chaotically than they usually do.",
            "Fourteen kilos of high explosive persuades six raccoons to (briefly) reconsider their life choices.",
        ],
        FlavourEvent::SpitfireHit => &[
            "The rotary gun speaks. The raccoons seem to have less to say now.",
            "Spitfire rakes the flank. Two raccoons go down. Their packmates don't notice yet.",
            "Another burst of fire, another gap in the formation. The boombox keeps playing.",
        ],
        FlavourEvent::InfantrySluggerHit => &[
            "Section 1 opens up. The sluggers aren't elegant, but proximity is doing the work.",
            "Wage Slaves fire in controlled bursts. At this range, 'controlled' is generous.",
        ],
        FlavourEvent::InfantryTakesCasualty => &[
            "A pistol round finds a gap in Corporal Yates's Kevlar. He is not pleased.",
            "Section 2 takes two down. They close ranks. The gap where Henriksson was feels larger than it should.",
            "The raccoons have numbers and enthusiasm. Their enthusiasm is surprisingly effective.",
        ],
        FlavourEvent::PackScatter => &[
            "'Rocker Boyz' hits fifty percent attrition. They turn around and run away with extreme prejudice.",
            "The Goth Collective breaks and rides — in the direction of away, specifically.",
            "Mod Squad scatters into the dust. Their revenge tour has been rescheduled indefinitely.",
            "Punk Agenda fragments. Whatever they were arguing about, they've agreed to argue elsewhere.",
        ],
        FlavourEvent::ScrapRocketHit => &[
            "The raccoons' Scrap-Rocket fires. The Warthog's armour holds. The noise is indescribable. The smell is somehow worse.",
            "Something very large and unstable hits the Dustbreaker's flank with a loud bang. She shudders. She holds. The crew seems pleasantly surprised to still be alive.",
        ],
        FlavourEvent::ScrapRocketMisfire => &[
            "The Scrap-Rocket misfires, like you'd expect a firework display organised by raccoons would. Three raccoons are missing in a cloud of smoke; one seems to be clinging to the rocket as it reaches for the clouds.",
            "Punk Agenda's designated rocket enthusiast has achieved liftoff in an unplanned direction. The pack is three members lighter. The boombox continues unconcerned.",
        ],
    }
}

/// Pick a flavour string from the pool using the engagement RNG.
/// Pass a u32 draw from the RNG; this function does no RNG calls itself.
pub fn pick_flavour(pool: &'static [&'static str], rng_draw: u32) -> &'static str {
    pool[rng_draw as usize % pool.len()]
}

// ── Engagement result types (contract with the UI layer) ───────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Outcome {
    Win,
    Loss,
}

/// Enough context for the UI to render a named callout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightEvent {
    pub tick: u32,
    pub kind: HighlightKind,
    pub pack_name: Option<String>,
    pub flavour: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HighlightKind {
    ScrapRocketMisfire,
    ScrapRocketHit,
    PackScatter,
}

/// The contract between the combat logic layer and the UI (Prompt 2).
/// Defined here so the UI can import the type without depending on resolver internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngagementResult {
    pub outcome: Outcome,
    pub defender_kia: u32,
    pub attacker_kia: u32,
    pub packs_routed: u32,
    /// Packs whose strength hit exactly 0 (destroyed outright, distinct from scattered).
    pub packs_destroyed: u32,
    pub ticks_elapsed: u32,
    pub misfire_occurred: bool,
    /// ScrapRocketMisfire + PackScatter events in tick order, for post-battle screen.
    pub highlights: Vec<HighlightEvent>,
}

// ── Static scenario data ────────────────────────────────────────────────────

/// Weapon kind for the scenario layer.
/// Standard weapons go through the AP/AT hit-roll pipeline.
/// AoE weapons skip the per-target damage formula and kill a fixed number on hit.
#[derive(Debug, Clone)]
pub enum ScenarioWeaponKind {
    Standard { base_damage: i32 },
    /// Kills `kills_per_hit` pack members on a successful hit — no per-member roll.
    Aoe { kills_per_hit: u32 },
}

#[derive(Debug, Clone)]
pub struct ScenarioWeapon {
    pub name: &'static str,
    pub ap: i32,
    pub accuracy: i32,
    pub kind: ScenarioWeaponKind,
}

#[derive(Debug, Clone)]
pub struct OreRunVehicle {
    pub id: u8,
    pub name: &'static str,
    pub hp: i32,
    pub max_hp: i32,
    pub at: i32,
    pub evasion: i32,
    pub weapons: Vec<ScenarioWeapon>,
    /// Stable targeting: index of the pack this vehicle prefers, overridden to highest if None.
    /// None = always target the largest active pack.
    pub preferred_pack: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct OreRunSection {
    pub id: u8,
    pub name: &'static str,
    pub max_strength: u32,
    pub current_strength: u32,
    pub individual_hp: i32,
    pub accuracy: i32,
    pub evasion: i32,
    pub weapon_ap: i32,
    pub weapon_damage: i32,
    pub weapon_accuracy: i32,
    pub armor_at: i32,
    /// Which pack index this section fires at.
    pub target_pack: usize,
}

#[derive(Debug, Clone)]
pub struct OreRunPack {
    pub id: u8,
    pub name: &'static str,
    pub max_strength: u32,
    pub current_strength: u32,
    pub individual_hp: i32,
    pub accuracy: i32,
    pub evasion: i32,
    pub weapon_ap: i32,
    pub weapon_damage: i32,
    pub armor_at: i32,
    pub scatter_threshold: u32,
    /// If Some, this pack fires at the given section index. None = fires at vehicles only.
    pub fires_at_section: Option<usize>,
    /// Pack 3 (Punk Agenda) carries the Scrap-Rocket. Cleared after Tick 2.
    pub has_scrap_rocket: bool,
    pub scattered: bool,
    pub destroyed: bool,
}

impl OreRunPack {
    pub fn is_active(&self) -> bool {
        !self.scattered && !self.destroyed && self.current_strength > 0
    }
}

/// Returns the scenario in its starting state.
/// Call once per engagement run; clone if you need a fresh copy.
pub fn the_ore_run() -> (Vec<OreRunVehicle>, Vec<OreRunSection>, Vec<OreRunPack>) {
    let vehicles = vec![
        OreRunVehicle {
            id: 0,
            name: "Warthog \"Dustbreaker\"",
            hp: WARTHOG_HP,
            max_hp: WARTHOG_HP,
            at: WARTHOG_AT,
            evasion: WARTHOG_EVASION,
            weapons: vec![
                ScenarioWeapon {
                    name: "Thumper GL",
                    ap: THUMPER_AP,
                    accuracy: THUMPER_ACCURACY,
                    kind: ScenarioWeaponKind::Aoe {
                        kills_per_hit: THUMPER_AOE_KILLS_PER_HIT,
                    },
                },
                ScenarioWeapon {
                    name: "Spitfire Rotary",
                    ap: SPITFIRE_AP,
                    accuracy: SPITFIRE_ACCURACY,
                    kind: ScenarioWeaponKind::Standard {
                        base_damage: SPITFIRE_DAMAGE,
                    },
                },
            ],
            preferred_pack: None, // targets highest headcount each tick
        },
        OreRunVehicle {
            id: 1,
            name: "Dust-Mite \"Gadfly\"",
            hp: DUSTMITE_HP,
            max_hp: DUSTMITE_HP,
            at: DUSTMITE_AT,
            evasion: DUSTMITE_EVASION,
            weapons: vec![ScenarioWeapon {
                name: "Spitfire Rotary",
                ap: SPITFIRE_AP,
                accuracy: SPITFIRE_ACCURACY,
                kind: ScenarioWeaponKind::Standard {
                    base_damage: SPITFIRE_DAMAGE,
                },
            }],
            preferred_pack: None, // spec: targets highest-headcount pack each tick
        },
        OreRunVehicle {
            id: 2,
            name: "Rhino \"Iron Coffin\"",
            hp: RHINO_HP,
            max_hp: RHINO_HP,
            at: RHINO_AT,
            evasion: RHINO_EVASION,
            weapons: vec![ScenarioWeapon {
                name: "Spitfire Rotary",
                ap: SPITFIRE_AP,
                accuracy: SPITFIRE_ACCURACY,
                kind: ScenarioWeaponKind::Standard {
                    base_damage: SPITFIRE_DAMAGE,
                },
            }],
            preferred_pack: None,
        },
        OreRunVehicle {
            id: 3,
            name: "Rhino \"Paid in Full\"",
            hp: RHINO_HP,
            max_hp: RHINO_HP,
            at: RHINO_AT,
            evasion: RHINO_EVASION,
            weapons: vec![ScenarioWeapon {
                name: "Spitfire Rotary",
                ap: SPITFIRE_AP,
                accuracy: SPITFIRE_ACCURACY,
                kind: ScenarioWeaponKind::Standard {
                    base_damage: SPITFIRE_DAMAGE,
                },
            }],
            preferred_pack: None,
        },
    ];

    let sections = vec![
        OreRunSection {
            id: 0,
            name: "Section 1 \"Scrap Dogs\"",
            max_strength: 8,
            current_strength: 8,
            individual_hp: SECTION_INDIVIDUAL_HP,
            accuracy: SLUGGER_ACCURACY,
            evasion: SECTION_EVASION,
            weapon_ap: SLUGGER_AP,
            weapon_damage: SLUGGER_DAMAGE,
            weapon_accuracy: SLUGGER_ACCURACY,
            armor_at: SECTION_ARMOR_AT,
            target_pack: 0, // Scrap Dogs engage Mod Squad (lead pack)
        },
        OreRunSection {
            id: 1,
            name: "Section 2 \"Wage Slaves\"",
            max_strength: 8,
            current_strength: 8,
            individual_hp: SECTION_INDIVIDUAL_HP,
            accuracy: SLUGGER_ACCURACY,
            evasion: SECTION_EVASION,
            weapon_ap: SLUGGER_AP,
            weapon_damage: SLUGGER_DAMAGE,
            weapon_accuracy: SLUGGER_ACCURACY,
            armor_at: SECTION_ARMOR_AT,
            target_pack: 1, // Wage Slaves engage Rocker Boyz
        },
    ];

    let packs = vec![
        OreRunPack {
            id: 0,
            name: "Mod Squad",
            max_strength: 14,
            current_strength: 14,
            individual_hp: PACK_INDIVIDUAL_HP,
            accuracy: PACK_WEAPON_ACCURACY,
            evasion: PACK_EVASION,
            weapon_ap: PACK_WEAPON_AP,
            weapon_damage: PACK_WEAPON_DAMAGE,
            armor_at: PACK_ARMOR_AT,
            scatter_threshold: 7, // floor(14 * 0.5)
            fires_at_section: Some(0),
            has_scrap_rocket: false,
            scattered: false,
            destroyed: false,
        },
        OreRunPack {
            id: 1,
            name: "Rocker Boyz",
            max_strength: 13,
            current_strength: 13,
            individual_hp: PACK_INDIVIDUAL_HP,
            accuracy: PACK_WEAPON_ACCURACY,
            evasion: PACK_EVASION,
            weapon_ap: PACK_WEAPON_AP,
            weapon_damage: PACK_WEAPON_DAMAGE,
            armor_at: PACK_ARMOR_AT,
            scatter_threshold: 6, // floor(13 * 0.5)
            fires_at_section: Some(1),
            has_scrap_rocket: false,
            scattered: false,
            destroyed: false,
        },
        OreRunPack {
            id: 2,
            name: "Goth Collective",
            max_strength: 12,
            current_strength: 12,
            individual_hp: PACK_INDIVIDUAL_HP,
            accuracy: PACK_WEAPON_ACCURACY,
            evasion: PACK_EVASION,
            weapon_ap: PACK_WEAPON_AP,
            weapon_damage: PACK_WEAPON_DAMAGE,
            armor_at: PACK_ARMOR_AT,
            scatter_threshold: 6, // floor(12 * 0.5)
            fires_at_section: None, // fires at vehicles — no penetration
            has_scrap_rocket: false,
            scattered: false,
            destroyed: false,
        },
        OreRunPack {
            id: 3,
            name: "Punk Agenda",
            max_strength: 15,
            current_strength: 15,
            individual_hp: PACK_INDIVIDUAL_HP,
            accuracy: PACK_WEAPON_ACCURACY,
            evasion: PACK_EVASION,
            weapon_ap: PACK_WEAPON_AP,
            weapon_damage: PACK_WEAPON_DAMAGE,
            armor_at: PACK_ARMOR_AT,
            scatter_threshold: 7, // floor(15 * 0.5)
            fires_at_section: None,
            has_scrap_rocket: true,
            scattered: false,
            destroyed: false,
        },
    ];

    (vehicles, sections, packs)
}
