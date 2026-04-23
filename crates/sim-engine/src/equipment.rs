// Equipment definitions — starter set, sourced from GDD v6.
// Hardpoint mapping: Weapon→Light, HeavyWeapon→Heavy, Armour→Armor, Tool→Utility.
// Quality mapping:   Common→Basic, Uncommon→Standard, Rare→Specialized, Epic→Superior, Legendary→Elite.
// Map key string is the canonical id value on each struct.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::game_types::{Equipment, MercHardpoint, QualityGrade};

static EQUIPMENT: OnceLock<HashMap<&'static str, Equipment>> = OnceLock::new();

/// Returns the static equipment definitions map. Callers use `.values()` or `.get(key)` directly.
pub fn equipment() -> &'static HashMap<&'static str, Equipment> {
    EQUIPMENT.get_or_init(build)
}

fn build() -> HashMap<&'static str, Equipment> {
    use MercHardpoint::{Armor, Heavy, Light, Utility};
    use QualityGrade::{Basic, Elite, Specialized, Standard, Superior};

    let mut m = HashMap::new();

    // ── Weapons ───────────────────────────────────────────────────────────────

    m.insert("GRUNT_PISTOL_R1", Equipment {
        id:                   "GRUNT_PISTOL_R1".into(),
        name:                 "Grunt Pistol Mk.I".into(),
        slot:                 Light,
        quality_grade:        Basic,
        success_bonus:        3,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    20,
    });

    m.insert("GRUNT_PISTOL_R3", Equipment {
        id:                   "GRUNT_PISTOL_R3".into(),
        name:                 "Grunt Pistol Mk.III".into(),
        slot:                 Light,
        quality_grade:        Standard,
        success_bonus:        8,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    90,
    });

    m.insert("GRUNT_PISTOL_R5", Equipment {
        id:                   "GRUNT_PISTOL_R5".into(),
        name:                 "Grunt Pistol Mk.V \"Inheritance\"".into(),
        slot:                 Light,
        quality_grade:        Superior,
        success_bonus:        16,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    600,
    });

    m.insert("LASER_RIFLE_R1", Equipment {
        id:                   "LASER_RIFLE_R1".into(),
        name:                 "Laser Rifle Mk.I".into(),
        slot:                 Light,
        quality_grade:        Basic,
        success_bonus:        5,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    40,
    });

    m.insert("LASER_RIFLE_R3", Equipment {
        id:                   "LASER_RIFLE_R3".into(),
        name:                 "Laser Rifle Mk.III".into(),
        slot:                 Light,
        quality_grade:        Specialized,
        success_bonus:        12,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    180,
    });

    m.insert("LASER_RIFLE_R5", Equipment {
        id:                   "LASER_RIFLE_R5".into(),
        name:                 "Laser Rifle Mk.V \"Sunspear\"".into(),
        slot:                 Light,
        quality_grade:        Elite,
        success_bonus:        22,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    1200,
    });

    // HeavyWeapon → Heavy
    m.insert("HEAVY_LASER_RIFLE_R2", Equipment {
        id:                   "HEAVY_LASER_RIFLE_R2".into(),
        name:                 "Heavy Laser Rifle Mk.II".into(),
        slot:                 Heavy,
        quality_grade:        Standard,
        success_bonus:        9,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    120,
    });

    m.insert("ASSAULT_RIFLE_R2", Equipment {
        id:                   "ASSAULT_RIFLE_R2".into(),
        name:                 "Assault Rifle Mk.II".into(),
        slot:                 Light,
        quality_grade:        Standard,
        success_bonus:        8,
        damage_shield:        2,
        resource_yield_bonus: 0,
        crafting_cost_ore:    100,
    });

    // HeavyWeapon → Heavy
    m.insert("MINIGUN_R3", Equipment {
        id:                   "MINIGUN_R3".into(),
        name:                 "Minigun Mk.III \"Lung Capacity\"".into(),
        slot:                 Heavy,
        quality_grade:        Specialized,
        success_bonus:        15,
        damage_shield:        -5,
        resource_yield_bonus: 0,
        crafting_cost_ore:    350,
    });

    // ── Armour → Armor ────────────────────────────────────────────────────────

    m.insert("FLAK_VEST_R1", Equipment {
        id:                   "FLAK_VEST_R1".into(),
        name:                 "Flak Vest Mk.I".into(),
        slot:                 Armor,
        quality_grade:        Basic,
        success_bonus:        0,
        damage_shield:        5,
        resource_yield_bonus: 0,
        crafting_cost_ore:    25,
    });

    m.insert("FLAK_VEST_R3", Equipment {
        id:                   "FLAK_VEST_R3".into(),
        name:                 "Flak Vest Mk.III".into(),
        slot:                 Armor,
        quality_grade:        Standard,
        success_bonus:        0,
        damage_shield:        12,
        resource_yield_bonus: 0,
        crafting_cost_ore:    110,
    });

    m.insert("COMBAT_PLATE_R2", Equipment {
        id:                   "COMBAT_PLATE_R2".into(),
        name:                 "Combat Plate Mk.II".into(),
        slot:                 Armor,
        quality_grade:        Standard,
        success_bonus:        0,
        damage_shield:        10,
        resource_yield_bonus: 0,
        crafting_cost_ore:    80,
    });

    m.insert("COMBAT_PLATE_R4", Equipment {
        id:                   "COMBAT_PLATE_R4".into(),
        name:                 "Combat Plate Mk.IV \"Ironwall\"".into(),
        slot:                 Armor,
        quality_grade:        Superior,
        success_bonus:        2,
        damage_shield:        20,
        resource_yield_bonus: 0,
        crafting_cost_ore:    450,
    });

    m.insert("VOID_WEAVE_R5", Equipment {
        id:                   "VOID_WEAVE_R5".into(),
        name:                 "Void-Weave Exosuit \"Relic\"".into(),
        slot:                 Armor,
        quality_grade:        Elite,
        success_bonus:        5,
        damage_shield:        30,
        resource_yield_bonus: 0,
        crafting_cost_ore:    1800,
    });

    // ── Utility ───────────────────────────────────────────────────────────────

    m.insert("STIMPACK_R1", Equipment {
        id:                   "STIMPACK_R1".into(),
        name:                 "Stimpack Mk.I".into(),
        slot:                 Utility,
        quality_grade:        Basic,
        success_bonus:        2,
        damage_shield:        3,
        resource_yield_bonus: 0,
        crafting_cost_ore:    15,
    });

    // Tool → Utility
    m.insert("ORE_SCANNER_R2", Equipment {
        id:                   "ORE_SCANNER_R2".into(),
        name:                 "Ore Scanner Mk.II".into(),
        slot:                 Utility,
        quality_grade:        Standard,
        success_bonus:        3,
        damage_shield:        0,
        resource_yield_bonus: 12,
        crafting_cost_ore:    70,
    });

    m.insert("TACTICAL_HUD_R3", Equipment {
        id:                   "TACTICAL_HUD_R3".into(),
        name:                 "Tactical HUD Mk.III".into(),
        slot:                 Utility,
        quality_grade:        Specialized,
        success_bonus:        10,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    200,
    });

    m.insert("BREACHING_CHARGE_R2", Equipment {
        id:                   "BREACHING_CHARGE_R2".into(),
        name:                 "Breaching Charge Mk.II".into(),
        slot:                 Utility,
        quality_grade:        Basic,
        success_bonus:        6,
        damage_shield:        0,
        resource_yield_bonus: 0,
        crafting_cost_ore:    60,
    });

    m
}
