// Combat type definitions — data shapes only, no logic.
// Source: src/types/index.ts (Section-based AP vs AT engine block)

use serde::{Deserialize, Serialize};

// ----------------------------------------------------------------
// WEAPON & ARMOR TAGS
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WeaponTag {
    Laser,
    Slug,
    Missile,
    Plasma,
    Flamer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArmorTag {
    Unarmored,
    LightArmor,
    HeavyArmor,
    Building,
}

// ----------------------------------------------------------------
// COMBAT INITIATION
// ----------------------------------------------------------------

/// AMBUSH = threat was inside Fog of War; defender firing is suppressed on Tick 1.
/// SPOTTED = revealed by recon; standard flow, Distress Timer fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CombatInitiationType {
    Ambush,
    Spotted,
}

// ----------------------------------------------------------------
// VEHICLE vs SECTION COMBAT
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatWeapon {
    pub name: String,
    /// Must exceed target AT to deal any damage.
    pub ap: i32,
    pub base_damage: i32,
    pub tag: WeaponTag,
    /// Weapon-specific accuracy modifier (0–100, may be negative).
    pub accuracy: i32,
}

/// Infantry section of up to 8 members treated as one tactical object.
/// Each surviving member fires once per tick (swarm mechanic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub name: String,
    /// Hard cap of 8 per GDD.
    pub max_strength: u32,
    pub current_strength: u32,
    /// HP per member — converts incoming damage into a kill count.
    pub individual_hp: i32,
    pub accuracy: i32,
    pub evasion: i32,
    pub weapon: CombatWeapon,
    pub armor_at: i32,
    pub armor_tag: ArmorTag,
}

/// Armored vehicle. Crew is abstracted into cost and XP.
/// Each weapon in `weapons` fires once per tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vehicle {
    pub id: String,
    pub name: String,
    pub hp: i32,
    pub max_hp: i32,
    /// Armor Threshold — attacker AP must meet or exceed AT to deal damage.
    pub at: i32,
    pub armor_tag: ArmorTag,
    pub evasion: i32,
    pub weapons: Vec<CombatWeapon>,
}

/// Result of a single vehicle weapon firing in one tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VehicleWeaponEvent {
    pub weapon_name: String,
    /// Human-readable roll breakdown, e.g. "D100(47) + 80 - 30 = 97".
    pub hit_roll_breakdown: String,
    pub is_hit: bool,
    pub ap_vs_at: String,
    pub is_penetration: bool,
    pub raw_damage: i32,
    pub tag_multiplier: f32,
    pub final_damage: i32,
    pub kill_count: u32,
}

/// Aggregated result of an entire Section's swarm volley in one tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionFireEvent {
    /// Equal to current_strength at time of firing.
    pub shots_total: u32,
    pub hits_total: u32,
    pub ap_vs_at: String,
    pub is_penetration: bool,
    pub raw_damage_per_shot: i32,
    pub tag_multiplier: f32,
    pub tag_note: String,
    pub final_damage_per_shot: i32,
    pub total_damage: i32,
    /// None when the Section is firing at a Vehicle (no infantry kill count applies).
    pub kill_count: Option<u32>,
}

/// Full state snapshot and events for one tick of a Vehicle vs Section engagement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickLog {
    pub tick: u32,
    pub vehicle_events: Vec<VehicleWeaponEvent>,
    /// None when the defender's firing phase is suppressed (Ambush Tick 1).
    pub section_event: Option<SectionFireEvent>,
    /// True only on Tick 1 of an Ambush engagement.
    pub defender_suppressed: bool,
    pub section_strength_after: u32,
    pub vehicle_hp_after: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CombatOutcome {
    SectionVictory,
    VehicleVictory,
    Draw,
    MaxTicksReached,
}

/// Lightweight vehicle descriptor used in convoy manifests and AARs.
/// Not to be confused with the canonical VehicleClass enum (9-class fleet taxonomy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvoyVehicle {
    pub name: String,
    pub fuel_cost_per_hex: u32,
    pub radar_signature: u32,
}

/// After-Action Report produced by resolveCombat().
/// `timestamp` must be injected from the session_start InputLogEntry — never from wall clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatReport {
    pub report_id: String,
    pub timestamp: String,
    pub section_name: String,
    pub vehicle_name: String,
    pub combat_initiation_type: CombatInitiationType,
    pub ticks: Vec<TickLog>,
    pub outcome: CombatOutcome,
    pub section_final_strength: u32,
    pub section_max_strength: u32,
    pub vehicle_final_hp: i32,
    pub vehicle_max_hp: i32,
    pub narrative_summary: String,
    pub defending_convoy_vehicles: Vec<ConvoyVehicle>,
}

// ----------------------------------------------------------------
// PACK — NPC-EXCLUSIVE (Raccoon Biker Gangs)
// Ref: Combat_Math_Resolution.md §6
// Do NOT model Raccoon encounters with the Section type.
// ----------------------------------------------------------------

/// Raccoon Biker Pack. Same headcount-swarm model as Section but
/// with a larger roster (12–15) and a scatter threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    pub id: String,
    pub name: String,
    /// 12–15 for Raccoon Bikers.
    pub max_strength: u32,
    pub current_strength: u32,
    /// Low — each Biker is fragile.
    pub individual_hp: i32,
    pub accuracy: i32,
    /// High — fast bikes, hard to hit.
    pub evasion: i32,
    pub weapon: CombatWeapon,
    pub armor_at: i32,
    pub armor_tag: ArmorTag,
    /// Pack breaks and scatters when current_strength drops to or below this value (~50% of max).
    pub scatter_threshold: u32,
}

/// Aggregated result of an entire Pack's volley against a Section in one tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackFireEvent {
    pub shots_total: u32,
    pub hits_total: u32,
    pub ap_vs_at: String,
    pub is_penetration: bool,
    pub raw_damage_per_shot: i32,
    pub tag_multiplier: f32,
    pub tag_note: String,
    pub final_damage_per_shot: i32,
    pub total_damage: i32,
    pub kill_count: u32,
}

/// Full state snapshot for one tick of a Pack vs Section engagement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackTickLog {
    pub tick: u32,
    pub pack_fire_event: PackFireEvent,
    /// None when the defender's firing phase is suppressed (Ambush Tick 1).
    pub section_fire_event: Option<SectionFireEvent>,
    /// True only on Tick 1 of an Ambush engagement.
    pub defender_suppressed: bool,
    pub section_strength_after: u32,
    pub pack_strength_after: u32,
    /// True if pack_strength_after dropped to or below scatter_threshold this tick.
    pub pack_scattered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackAssaultOutcome {
    SectionVictory,
    PackVictory,
    PackScattered,
    Draw,
    MaxTicksReached,
}

/// After-Action Report produced by resolvePackAssault().
/// `timestamp` must be injected from the session_start InputLogEntry — never from wall clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackAssaultReport {
    pub report_id: String,
    pub timestamp: String,
    pub section_name: String,
    pub pack_name: String,
    pub combat_initiation_type: CombatInitiationType,
    pub ticks: Vec<PackTickLog>,
    pub outcome: PackAssaultOutcome,
    pub section_final_strength: u32,
    pub section_max_strength: u32,
    pub pack_final_strength: u32,
    pub pack_max_strength: u32,
    pub narrative_summary: String,
    pub defending_convoy_vehicles: Vec<ConvoyVehicle>,
}
