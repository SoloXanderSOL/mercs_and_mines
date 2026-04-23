// Mission definitions — sourced from GDD v6, Mission Types sheet.
// Category remapping applied per Director ruling 2026-04-23:
//   Intel → Sabotage, Mining → Extraction, Raid → Assault.
// Terrain field replaced by MissionEnvironment; Space → Orbital.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::game_types::{MissionCategory, MissionDefinition, MissionEnvironment};

static MISSIONS: OnceLock<HashMap<&'static str, MissionDefinition>> = OnceLock::new();

/// Returns the static mission definitions map. Callers use `.values()` or `.get(key)` directly.
pub fn missions() -> &'static HashMap<&'static str, MissionDefinition> {
    MISSIONS.get_or_init(build)
}

fn build() -> HashMap<&'static str, MissionDefinition> {
    let mut m = HashMap::new();

    m.insert("GUARD_DUTY", MissionDefinition {
        id:                  "GUARD_DUTY".into(),
        name:                "Guard Duty".into(),
        category:            MissionCategory::Defense,
        difficulty:          2,
        duration_minutes:    120,
        environment:         MissionEnvironment::Industrial,
        credit_reward:       400,
        ore_reward:          0,
        base_hp_loss_chance: 20,
        base_kia_multiplier: 0.8,
        flavor_text:         "Stand here. Look menacing. Try not to get shot. Simple enough. Usually.".into(),
    });

    // Intel → Sabotage
    m.insert("RECON_RUN", MissionDefinition {
        id:                  "RECON_RUN".into(),
        name:                "Recon Run".into(),
        category:            MissionCategory::Sabotage,
        difficulty:          3,
        duration_minutes:    240,
        environment:         MissionEnvironment::Urban,
        credit_reward:       650,
        ore_reward:          0,
        base_hp_loss_chance: 25,
        base_kia_multiplier: 0.9,
        flavor_text:         "Get in. Get eyes on the target. Get out. Don't get shot. Two out of four is fine.".into(),
    });

    // Mining → Extraction
    m.insert("ORE_EXTRACTION", MissionDefinition {
        id:                  "ORE_EXTRACTION".into(),
        name:                "Ore Extraction".into(),
        category:            MissionCategory::Extraction,
        difficulty:          3,
        duration_minutes:    480,
        environment:         MissionEnvironment::Underground,
        credit_reward:       300,
        ore_reward:          150,
        base_hp_loss_chance: 20,
        base_kia_multiplier: 0.7,
        flavor_text:         "The rocks are rich down here. The air is bad. The tunnels are unstable. But the ore is VERY good.".into(),
    });

    // Raid → Assault
    m.insert("SUPPLY_RAID", MissionDefinition {
        id:                  "SUPPLY_RAID".into(),
        name:                "Supply Raid".into(),
        category:            MissionCategory::Assault,
        difficulty:          5,
        duration_minutes:    360,
        environment:         MissionEnvironment::Wasteland,
        credit_reward:       900,
        ore_reward:          80,
        base_hp_loss_chance: 40,
        base_kia_multiplier: 1.0,
        flavor_text:         "Their convoy. Your guns. Their supplies. Your profit. It's practically a business transaction.".into(),
    });

    m.insert("INDUSTRIAL_SABOTAGE", MissionDefinition {
        id:                  "INDUSTRIAL_SABOTAGE".into(),
        name:                "Industrial Sabotage".into(),
        category:            MissionCategory::Sabotage,
        difficulty:          6,
        duration_minutes:    480,
        environment:         MissionEnvironment::Industrial,
        credit_reward:       1200,
        ore_reward:          0,
        base_hp_loss_chance: 45,
        base_kia_multiplier: 1.1,
        flavor_text:         "Blow up their refinery. Cripple their output. Don't blow up your mercs. In that order of preference.".into(),
    });

    // Mining → Extraction
    m.insert("DEEP_CORE_SURVEY", MissionDefinition {
        id:                  "DEEP_CORE_SURVEY".into(),
        name:                "Deep Core Survey".into(),
        category:            MissionCategory::Extraction,
        difficulty:          5,
        duration_minutes:    480,
        environment:         MissionEnvironment::Underground,
        credit_reward:       200,
        ore_reward:          350,
        base_hp_loss_chance: 30,
        base_kia_multiplier: 0.8,
        flavor_text:         "The ore concentration maps say there's something extraordinary down there. The seismic reports say there's also something else. Nobody's sure what.".into(),
    });

    m.insert("ASSASSINATION", MissionDefinition {
        id:                  "ASSASSINATION".into(),
        name:                "Assassination Contract".into(),
        category:            MissionCategory::Assault,
        difficulty:          7,
        duration_minutes:    600,
        environment:         MissionEnvironment::Urban,
        credit_reward:       2500,
        ore_reward:          0,
        base_hp_loss_chance: 55,
        base_kia_multiplier: 1.3,
        flavor_text:         "A corporate director needs to stop directing. Discretion is required. Well. Discretion-adjacent.".into(),
    });

    // Space → Orbital
    m.insert("CRASHED_SHIP_RAID", MissionDefinition {
        id:                  "CRASHED_SHIP_RAID".into(),
        name:                "Crashed Ship Salvage".into(),
        category:            MissionCategory::Extraction,
        difficulty:          7,
        duration_minutes:    540,
        environment:         MissionEnvironment::Orbital,
        credit_reward:       800,
        ore_reward:          200,
        base_hp_loss_chance: 50,
        base_kia_multiplier: 1.2,
        flavor_text:         "A pre-Severance frigate came down hard in Sector 6. Half the planet is already racing to it. Whatever's still in the hold belongs to whoever gets there bloodiest.".into(),
    });

    // Raid → Assault
    m.insert("ORE_HEIST", MissionDefinition {
        id:                  "ORE_HEIST".into(),
        name:                "Ore Heist".into(),
        category:            MissionCategory::Assault,
        difficulty:          8,
        duration_minutes:    600,
        environment:         MissionEnvironment::Industrial,
        credit_reward:       1500,
        ore_reward:          500,
        base_hp_loss_chance: 60,
        base_kia_multiplier: 1.4,
        flavor_text:         "They spent three months extracting that ore. You're going to spend one night taking it. Efficiency.".into(),
    });

    m.insert("BLACK_SITE_RAID", MissionDefinition {
        id:                  "BLACK_SITE_RAID".into(),
        name:                "Black Site Raid".into(),
        category:            MissionCategory::Assault,
        difficulty:          9,
        duration_minutes:    720,
        environment:         MissionEnvironment::Industrial,
        credit_reward:       5000,
        ore_reward:          100,
        base_hp_loss_chance: 75,
        base_kia_multiplier: 1.8,
        flavor_text:         "Nobody is supposed to know this facility exists. You now know it exists. You have two options: attack it, or forget you ever heard of it. Nobody forgets.".into(),
    });

    m.insert("SECTOR_9_ASSAULT", MissionDefinition {
        id:                  "SECTOR_9_ASSAULT".into(),
        name:                "Sector 9: Resonance Crater".into(),
        category:            MissionCategory::Assault,
        difficulty:          10,
        duration_minutes:    900,
        environment:         MissionEnvironment::Wasteland,
        credit_reward:       3000,
        ore_reward:          800,
        base_hp_loss_chance: 80,
        base_kia_multiplier: 2.0,
        flavor_text:         "The Void-Glass Prime deposit. Everyone knows where it is. Everyone knows what it's worth. Everyone knows how many people have died trying to take it. Everyone goes anyway.".into(),
    });

    m
}
