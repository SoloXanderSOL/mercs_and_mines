// Unit definitions — sourced from GDD v6, Mercenary Units sheet.
// Hardpoint mapping: Weapon → Light, HeavyWeapon → Heavy, Armour → Armor, Tool → Utility.
// Mount and Vehicle slots removed; see per-unit overrides below.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::config::SimConfig;
use crate::game_types::{MercHardpoint, Unit, UnitArchetype, UnitDefinition, UnitStatus};

static UNIT_DEFINITIONS: OnceLock<HashMap<&'static str, UnitDefinition>> = OnceLock::new();

/// Returns the static unit definition map. Callers use `.values()` or `.get(key)` directly.
pub fn unit_definitions() -> &'static HashMap<&'static str, UnitDefinition> {
    UNIT_DEFINITIONS.get_or_init(|| {
        let keys: &[&'static str] = &[
            "VANGUARD", "SAWBONES", "GHOST_WIRE", "TUNNEL_RUNNER",
            "PSI_OPERATIVE", "PROSPECTOR", "PYROCLAST", "WAR_BOAR_RIDER", "VALKYRIE",
        ];
        keys.iter()
            .filter_map(|&k| get_definition(k).map(|d| (k, d)))
            .collect()
    })
}

/// Creates a Unit from a definition key, caller-supplied ID, and optional skill override.
/// Returns None if `def_key` is not a recognised definition.
pub fn create_unit(
    def_key: &str,
    name: String,
    id: String,
    skill_override: Option<u8>,
    cfg: &SimConfig,
) -> Option<Unit> {
    let def = get_definition(def_key)?;
    let skill = skill_override.unwrap_or(def.base_skill);
    Some(Unit {
        id,
        name,
        definition: def,
        skill,
        xp: 0,
        current_hp: cfg.unit_default_hp,
        max_hp: cfg.unit_default_hp,
        status: UnitStatus::Ready,
        equipment: vec![],
    })
}

fn get_definition(key: &str) -> Option<UnitDefinition> {
    use MercHardpoint::{Armor, Heavy, Light, Psychic, Utility};
    use UnitArchetype::*;

    let def = match key {
        "VANGUARD" => UnitDefinition {
            archetype: Vanguard,
            emoji: "🔫".into(),
            hiring_cost: 500,
            base_skill: 3,
            monthly_upkeep: 50,
            upkeep_extras: String::new(),
            hardpoints: vec![Light, Armor],
            success_mod: 4,
            damage_shield_mod: 0,
            loot_bonus: 0,
            async_ability: "FORCE MULTIPLIER — Each additional Vanguard in squad adds +2% Success Rate (max ×4).".into(),
            passive_trait: "STANDARD ISSUE — No special upkeep. Easiest to replace.".into(),
            flavor_text: "Pre-Severance security grunts. Reliable as the rock they're standing on.".into(),
        },
        "SAWBONES" => UnitDefinition {
            archetype: Sawbones,
            emoji: "🩺".into(),
            hiring_cost: 750,
            base_skill: 3,
            monthly_upkeep: 65,
            upkeep_extras: String::new(),
            hardpoints: vec![Armor, Utility],
            success_mod: 0,
            damage_shield_mod: 12,
            loot_bonus: 0,
            async_ability: "TRAUMA PROTOCOL — Reduces permanent unit death chance by 30%. KIA → Injured on a 30% roll.".into(),
            passive_trait: "TRIAGE INSTINCT — Heals 1 additional unit HP post-mission for free.".into(),
            flavor_text: "Trained in a pre-Severance hospital ship. Now works for whoever pays.".into(),
        },
        "GHOST_WIRE" => UnitDefinition {
            archetype: GhostWire,
            emoji: "💻".into(),
            hiring_cost: 1100,
            base_skill: 4,
            monthly_upkeep: 90,
            upkeep_extras: String::new(),
            hardpoints: vec![Utility, Armor],
            success_mod: 8,
            damage_shield_mod: 0,
            loot_bonus: 0,
            async_ability: "SOFT BREACH — Intel & Extraction missions treated as one difficulty tier lower for RNG calc.".into(),
            passive_trait: "SYSTEM GHOST — Cannot be targeted by enemy sabotage events.".into(),
            flavor_text: "Speaks to machines better than people. Machines are more honest anyway.".into(),
        },
        "TUNNEL_RUNNER" => UnitDefinition {
            archetype: TunnelRunner,
            emoji: "🦆".into(),
            hiring_cost: 900,
            base_skill: 4,
            monthly_upkeep: 45,
            upkeep_extras: "10 Rations".into(),
            hardpoints: vec![Light, Light],
            success_mod: 15,
            damage_shield_mod: -10,
            loot_bonus: 0,
            async_ability: "BERSERKER SURGE — On Assault missions, +15% Success but +10% Injury risk. Glory or pain.".into(),
            passive_trait: "DUCK FURY — Dual-wields at no penalty. +8% Success in Underground/Tunnel missions.".into(),
            flavor_text: "Small. Furious. Surprisingly difficult to stop once they get going.".into(),
        },
        "PSI_OPERATIVE" => UnitDefinition {
            archetype: PsiOperative,
            emoji: "🐹".into(),
            hiring_cost: 2200,
            base_skill: 6,
            monthly_upkeep: 120,
            upkeep_extras: "3 Biscuits".into(),
            hardpoints: vec![Psychic],
            success_mod: 8,
            damage_shield_mod: 0,
            loot_bonus: 12,
            async_ability: "SYNAPTIC OVERLOAD — Mind-Links the squad. All units use the highest Skill Level in the group for mission calc.".into(),
            passive_trait: "PSYCHIC RESONANCE — Tier scales with Skill Level. If Squeaker says it's fine, it's fine.".into(),
            flavor_text: "Communicates only telepathically. Very cute. Definitely not influencing you. Have a biscuit.".into(),
        },
        "PROSPECTOR" => UnitDefinition {
            archetype: Prospector,
            emoji: "⛏️".into(),
            hiring_cost: 850,
            base_skill: 4,
            monthly_upkeep: 75,
            upkeep_extras: String::new(),
            hardpoints: vec![Utility, Armor],
            success_mod: 3,
            damage_shield_mod: 0,
            loot_bonus: 15,
            async_ability: "DEEP SCAN — Mining/Salvage missions: +25% Ore yield. Activates secondary roll for rare ore.".into(),
            passive_trait: "ORE SENSE — Passively identifies higher-concentration ore zones on the map.".into(),
            flavor_text: "Spent 20 years underground. Has opinions about rock layers. Many opinions.".into(),
        },
        "PYROCLAST" => UnitDefinition {
            archetype: Pyroclast,
            emoji: "🔥".into(),
            hiring_cost: 900,
            base_skill: 4,
            monthly_upkeep: 80,
            upkeep_extras: String::new(),
            hardpoints: vec![Heavy, Utility],
            success_mod: 15,
            damage_shield_mod: 0,
            loot_bonus: 0,
            async_ability: "CONTROLLED DEMO — Removes -15% Success penalty on Industrial targets. Prevents collateral damage.".into(),
            passive_trait: "BANG THEORY — Can sabotage enemy buildings as a mission type unique to this class.".into(),
            flavor_text: "The explosion was exactly the right size, thank you very much.".into(),
        },
        // Mount slot removed per hardpoint ruling — War Boar Rider gets [Light].
        "WAR_BOAR_RIDER" => UnitDefinition {
            archetype: WarBoarRider,
            emoji: "🐗".into(),
            hiring_cost: 700,
            base_skill: 4,
            monthly_upkeep: 30,
            upkeep_extras: "25 Rations".into(),
            hardpoints: vec![Light],
            success_mod: 20,
            damage_shield_mod: -5,
            loot_bonus: 5,
            async_ability: "SCRAP-SOVEREIGN — +20% Success on Raid & Sabotage missions. Reduces enemy building efficiency by 10% on successful raids.".into(),
            passive_trait: "BEAST OF WAR — Mounted on war boar. Immune to Urban terrain penalties. Requires Rations or deserts after 3 missions.".into(),
            flavor_text: "Came for the ore. Stayed for the carnage. Left with your generator.".into(),
        },
        // Vehicle slot removed per hardpoint ruling — Valkyrie gets [Heavy, Armor].
        "VALKYRIE" => UnitDefinition {
            archetype: Valkyrie,
            emoji: "🚁".into(),
            hiring_cost: 1600,
            base_skill: 6,
            monthly_upkeep: 130,
            upkeep_extras: "50 Fuel".into(),
            hardpoints: vec![Heavy, Armor],
            success_mod: 0,
            damage_shield_mod: 15,
            loot_bonus: 0,
            async_ability: "RAPID EXTRACTION — Reduces mission Duration by 20%. On failure, guarantees 50%+ of squad survives (no KIA).".into(),
            passive_trait: "EVAC PROTOCOL — Can abort a mission mid-timer (once per 48hrs) and recover squad with no penalty.".into(),
            flavor_text: "Three tour veteran of the Crust-War. The ship is older than she is. Both are indestructible.".into(),
        },
        _ => return None,
    };
    Some(def)
}
