// Loot table roller.
// Loot Find bonus drains from Basic at cfg.loot_drain_multiplier× and redistributes
// upward, capped at cfg.max_loot_bonus_cap to prevent Elite spam
// (which would crater the cNFT market — we're not animals)

use crate::config::SimConfig;
use crate::game_types::{LootDrop, QualityGrade};
use crate::rng::Rng;

// ── Base weights (must sum to 100) ────────────────────────────────────────────

const WEIGHT_BASIC:       f64 = 60.0;
const WEIGHT_STANDARD:    f64 = 25.0;
const WEIGHT_SPECIALIZED: f64 = 10.0;
const WEIGHT_SUPERIOR:    f64 =  4.0;
const WEIGHT_ELITE:       f64 =  1.0;

// ── Item name pools (placeholder names — will expand with full item DB) ───────

const POOL_BASIC: &[&str] = &[
    "Salvaged Ammo Cache", "Dented Flak Vest", "Cracked Visor Helmet",
    "Busted Stimpack", "Scrap Plating", "Low-Grade Ore Sample",
];

const POOL_STANDARD: &[&str] = &[
    "Refurbished Laser Pistol", "Corporate Assault Vest", "Signal Jammer Module",
    "Pre-Severance Medkit", "Tactical Webbing Rig", "Gunmetal Iron Ingot (\u{d7}5)",
];

const POOL_SPECIALIZED: &[&str] = &[
    "Military-Grade Laser Carbine", "Void-Treated Combat Plate",
    "Cogsmith's Multi-Tool", "Psionic Dampener Helm",
    "Star-Silt Ore Sample", "Encrypted Corpo Datapad",
];

const POOL_SUPERIOR: &[&str] = &[
    "Pre-Severance Plasma Cutter", "Exo-Frame Shoulder Guard \"Bastion\"",
    "Experimental Breach Charge", "Dark-Core Ore Shard",
    "Tactical HUD \"Ironveil Mk.IV\"", "Rogue AI Comms Module",
];

const POOL_ELITE: &[&str] = &[
    "Sunspear Laser Rifle Mk.V", "Void-Weave Exosuit \"Relic\"",
    "Emperor's Ore Pendant (smells faintly of bin chicken)",
    "Pre-Severance Jump Drive Fragment",
    "Squeaker's Signed Biscuit (DO NOT EAT)", // don't eat it
];

// ── Weight table ──────────────────────────────────────────────────────────────

type WeightTable = [(QualityGrade, f64); 5];

/// Adjusts grade weights based on the squad's total loot bonus.
fn build_weight_table(total_loot_bonus: u32, cfg: &SimConfig) -> WeightTable {
    let bonus  = total_loot_bonus.min(cfg.max_loot_bonus_cap) as f64;
    let drain  = bonus * cfg.loot_drain_multiplier;
    let uplift = bonus;

    [
        (QualityGrade::Basic,       (WEIGHT_BASIC - drain).max(cfg.loot_basic_weight_min)),
        (QualityGrade::Standard,    WEIGHT_STANDARD    + uplift * cfg.loot_standard_uplift_frac),
        (QualityGrade::Specialized, WEIGHT_SPECIALIZED + uplift * cfg.loot_specialized_uplift_frac),
        (QualityGrade::Superior,    WEIGHT_SUPERIOR    + uplift * cfg.loot_superior_uplift_frac),
        (QualityGrade::Elite,       WEIGHT_ELITE       + uplift * cfg.loot_elite_uplift_frac),
    ]
}

/// Selects a quality grade using weighted random. Iteration order matches the table array.
fn pick_quality_grade(rng: &mut Rng, table: &WeightTable) -> QualityGrade {
    let total: f64 = table.iter().map(|(_, w)| w).sum();
    let mut roll = rng.next_f64() * total;

    for (grade, weight) in table {
        roll -= weight;
        if roll <= 0.0 {
            return grade.clone();
        }
    }
    QualityGrade::Basic // fallback
}

/// Picks a random item name from the given grade's pool.
fn pick_item_name(rng: &mut Rng, grade: &QualityGrade) -> String {
    let pool: &[&str] = match grade {
        QualityGrade::Basic       => POOL_BASIC,
        QualityGrade::Standard    => POOL_STANDARD,
        QualityGrade::Specialized => POOL_SPECIALIZED,
        QualityGrade::Superior    => POOL_SUPERIOR,
        QualityGrade::Elite       => POOL_ELITE,
    };
    let idx = (rng.next_f64() * pool.len() as f64) as usize;
    pool[idx].to_string()
}

/// Rolls for loot. Only called on mission success.
/// Returns None if the loot roll itself fails — not every success drops loot.
///
/// Drop chance scales with difficulty: 44% at D1, 80% at D10.
pub fn roll_loot(rng: &mut Rng, total_loot_bonus: u32, mission_difficulty: u8, cfg: &SimConfig) -> Option<LootDrop> {
    let drop_chance = (cfg.loot_drop_chance_base + mission_difficulty as u32 * cfg.loot_drop_chance_per_diff) as f64 / 100.0;
    if !rng.chance(drop_chance) {
        return None;
    }

    let table     = build_weight_table(total_loot_bonus, cfg);
    let grade     = pick_quality_grade(rng, &table);
    let item_name = pick_item_name(rng, &grade);

    Some(LootDrop {
        is_nft_candidate: grade == QualityGrade::Elite, // hook for future Solana cNFT minting
        quality_grade: grade,
        item_name,
    })
}
