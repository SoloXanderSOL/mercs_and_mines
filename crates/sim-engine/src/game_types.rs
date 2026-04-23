// Game type definitions: units, commanders, missions, logistics, NPCs.
// Audited against Canon_Type_Reference.md (2026-04-21). Legacy TS values removed.

use serde::{Deserialize, Serialize};

use crate::types::{ConvoyVehicle, Pack};
pub use shared::{Coordinates, EntityVision};

// ----------------------------------------------------------------
// SPECIES — Commander / Advisor origins only.
// Not used for deployable combat units.
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Species {
    #[serde(rename = "Human (Corporate)")]
    HumanCorporate,
    #[serde(rename = "Human (Underhive)")]
    HumanUnderhive,
    #[serde(rename = "Raccoon (Mod-Moped)")]
    RaccoonModMoped,
    #[serde(rename = "Raccoon (Rocker-Chopper)")]
    RaccoonRockerChopper,
    #[serde(rename = "Hamster Attachment")]
    HamsterAttachment,
}

// ----------------------------------------------------------------
// UNIT CLASS — combat formation type, not individual merc role.
// Controls combat resolution logic (headcount model, scatter rules, etc.)
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnitClass {
    Section, // player units + organised human NPCs; max 8; standard headcount model
    Pack,    // Raccoon Biker Gangs; 12–15; Pack Scatter Rule at ≤50% headcount
    Drove,   // War Boar Reivers; 4–8 mounted; two-phase combat (Charge → Brawl)
    Swarm,   // Aerial Drone Swarms; 3–6 drones; aerialAT mechanic; ignores ground cover
}

// ----------------------------------------------------------------
// UNIT ARCHETYPE — hire-screen role of an individual merc.
// Replaces VeterancySpec. Distinct from UnitClass (formation type).
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnitArchetype {
    Vanguard,
    Sawbones,
    GhostWire,
    TunnelRunner,
    PsiOperative,
    Prospector,
    Pyroclast,
    WarBoarRider,
    Valkyrie,
}

// ----------------------------------------------------------------
// VETERANCY SPEC — earned specialisations for player Sections, unlocked through XP.
// A Section carries a Vec<VeterancySpec> — multiple specs are possible simultaneously.
// Distinct from UnitArchetype: VeterancySpec describes what a Section EARNS;
// UnitArchetype describes what a hireable unit IS at point of hire.
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VeterancySpec {
    Vanguard,
    Sawbones,
    GhostWire,
    TunnelRunner,
    Pyroclast,
}

// ----------------------------------------------------------------
// HARDPOINTS — two separate slot systems for merc and mining vehicles.
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MercHardpoint {
    Light,
    Heavy,
    Armor,
    Transport,
    Utility,
    Psychic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MiningHardpoint {
    Drill,
    Coolant,
    Cargo,
    Armor,
    Utility,
}

// ----------------------------------------------------------------
// QUALITY GRADE — replaces Rarity. Director ruling 2026-04-21.
// Unified 1–5 scale for equipment, Blueprints, Research Teams, and cNFTs.
// ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QualityGrade {
    Basic,
    Standard,
    Specialized,
    Superior,
    Elite,
}

// ----------------------------------------------------------------
// MISSION CATEGORY
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MissionCategory {
    Assault,
    Defense,
    Escort,
    Extermination,
    Sabotage,
    Extraction,
}

// ----------------------------------------------------------------
// TERRAIN — canonical hex terrain types.
// MagmaFlow and VolcanicCaldera are impassable.
// Ravine is impassable without a Bridge-Layer in convoy.
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Terrain {
    AshenPlains,
    ScorchedDesert,
    RuinedCity,
    CityState,
    MagmaFlow,
    CooledMagma,
    VolcanicCaldera,
    Mountain,
    Ravine,
    ToxicZone,
}

// ----------------------------------------------------------------
// MISSION ENVIRONMENT — combat setting of a mission.
// Director ruling 2026-04-23: separate from hex map Terrain.
// Used by the resolver for unit ability checks.
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MissionEnvironment {
    Industrial,  // factories, refineries — Pyroclast removes terrain penalty
    Urban,       // city ruins, dense structures — WarBoarRider immune to penalty
    Underground, // tunnels, mine shafts — TunnelRunner bonus applies
    Wasteland,   // open badlands, ash plains — no ability bonus triggers
    Orbital,     // space stations, void platforms — replaces legacy Space value
}

// ----------------------------------------------------------------
// UNIT STATUS
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnitStatus {
    Ready,
    OnMission,
    Wounded,
    #[serde(rename = "MIA")]
    Mia,
    #[serde(rename = "KIA")]
    Kia,
}

// ----------------------------------------------------------------
// EQUIPMENT
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Equipment {
    pub id: String,
    pub name: String,
    pub slot: MercHardpoint,
    pub quality_grade: QualityGrade,
    pub success_bonus: i32,
    pub damage_shield: i32,
    pub resource_yield_bonus: i32,
    pub crafting_cost_ore: u32,
}

// ----------------------------------------------------------------
// UNITS
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitDefinition {
    pub archetype: UnitArchetype,
    pub emoji: String,
    pub hiring_cost: u32,
    /// 1–10
    pub base_skill: u8,
    pub monthly_upkeep: u32,
    /// e.g. "10 Rations"
    pub upkeep_extras: String,
    pub hardpoints: Vec<MercHardpoint>,
    pub success_mod: i32,
    pub damage_shield_mod: i32,
    pub loot_bonus: i32,
    pub async_ability: String,
    pub passive_trait: String,
    pub flavor_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub name: String,
    pub definition: UnitDefinition,
    /// Current skill level; grows with XP.
    pub skill: u8,
    pub xp: u32,
    pub current_hp: i32,
    pub max_hp: i32,
    pub status: UnitStatus,
    pub equipment: Vec<Equipment>,
}

// ----------------------------------------------------------------
// COMMANDERS
// Ref: Commander_and_Advisor_System.md + Commander_Stress_System.md
// ----------------------------------------------------------------

/// Thresholds: 0–30 RESTED | 31–70 STRAINED | 71–99 BREAKING_POINT | 100 SHATTERED
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StressTier {
    Rested,
    Strained,
    BreakingPoint,
    Shattered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderPassiveBuffs {
    pub accuracy: i32,
    pub evasion: i32,
    pub damage_reduction: i32,
}

/// INVARIANT: is_kia and is_shattered are completely independent.
///   is_kia       — Permadeath on total wipeout. Burns cNFT if minted. PERMANENT.
///   is_shattered — stress_level hit 100. NOT permadeath. NEVER triggers cNFT burn.
///   can_retreat  — locked false when stress tier is BREAKING_POINT (71–99).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commander {
    pub id: String,
    pub name: String,
    pub species: Species,
    /// 1–5: Corporal → Regimental Colonel
    pub rank: u8,
    pub skill: u8,
    /// Added to all squad success rolls.
    pub success_aura: i32,
    pub quality_grade: QualityGrade,
    pub ability: String,
    pub flavor_text: String,
    /// 0–100 (percentage). Accumulates via deployment and casualties.
    pub stress_level: u8,
    pub is_kia: bool,
    pub is_shattered: bool,
    pub can_retreat: bool,
    pub passive_buffs: CommanderPassiveBuffs,
    pub attached_unit_id: Option<String>,
}

/// Retired Rank 5 Commanders on the Board of Directors.
pub type AdvisorBoard = Vec<Commander>;

// ----------------------------------------------------------------
// MISSIONS
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDefinition {
    pub id: String,
    pub name: String,
    pub category: MissionCategory,
    /// 1–10
    pub difficulty: u8,
    pub duration_minutes: u32,
    pub environment: MissionEnvironment,
    pub credit_reward: u32,
    pub ore_reward: u32,
    /// Base percentage chance (0–100) each unit takes HP damage.
    pub base_hp_loss_chance: u8,
    /// Multiplier on the KIA check; higher values are more lethal.
    pub base_kia_multiplier: f32,
    pub flavor_text: String,
}

// ----------------------------------------------------------------
// SQUAD
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Squad {
    pub units: Vec<Unit>,
    pub commander: Option<Commander>,
}

// ----------------------------------------------------------------
// ASYNC BATTLE REPORT (RNG mission resolver output)
// Distinct from CombatReport, which is the AP/AT tick engine output.
// ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub base_skill_score: i32,
    pub squad_size_bonus: i32,
    pub gear_bonus: i32,
    pub ability_bonus: i32,
    pub commander_bonus: i32,
    pub biscuit_coefficient: i32,
    pub mission_type_modifier: i32,
    pub difficulty_penalty: i32,
    pub total_score: i32,
    pub success_threshold: i32,
    pub raw_roll: i32,
    pub margin: i32,
}

/// Director ruling 2026-04-21: Critical Success / Critical Failure are legacy values. Removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutcomeType {
    FullSuccess,
    PartialSuccess,
    TacticalRetreat,
    Wipeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitBattleResult {
    pub unit_id: String,
    pub unit_name: String,
    /// Human-readable role label; String to allow NPC type names in AARs.
    pub unit_type: String,
    pub emoji: String,
    pub hp_lost: i32,
    pub hp_remaining: i32,
    pub final_status: UnitStatus,
    pub status_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootDrop {
    pub quality_grade: QualityGrade,
    pub item_name: String,
    /// True for Elite — future Solana cNFT hook.
    pub is_nft_candidate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rewards {
    pub credits: u32,
    pub ore: u32,
    pub loot_drop: Option<LootDrop>,
}

/// `timestamp` must be injected from the session_start InputLogEntry — never from wall clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BattleReport {
    pub report_id: String,
    pub timestamp: String,
    pub mission_id: String,
    pub mission_name: String,
    pub mission_category: MissionCategory,
    pub difficulty: u8,
    pub environment: MissionEnvironment,
    pub commander_name: Option<String>,
    pub outcome: OutcomeType,
    pub score_breakdown: ScoreBreakdown,
    pub unit_results: Vec<UnitBattleResult>,
    pub rewards: Rewards,
    pub narrative_tag: String,
}

// ----------------------------------------------------------------
// LOGISTICS & HEX MAP
// Ref: Hex_Map_and_Travel.md · Helium-3.md · Detection_and_Fog_of_War.md
// ----------------------------------------------------------------

/// Created only when the fuel gate check passes in deployConvoy().
/// `departure_time` and `arrival_time` are Unix timestamps (seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvoyRecord {
    pub origin: Coordinates,
    pub destination: Coordinates,
    pub departure_time: i64,
    pub arrival_time: i64,
    pub fuel_loaded: u32,
    pub vehicles: Vec<ConvoyVehicle>,
    /// False on creation. Set only by the combat/event system (Dead Duck state).
    pub is_dead_duck: bool,
}

/// Returned by deployConvoy() when the fuel gate fails. No convoy record is created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepartureRejected {
    pub fuel_loaded: u32,
    pub total_fuel_cost: u32,
    pub shortfall: u32,
}

// ----------------------------------------------------------------
// NPC MAP ENTITIES
// Ref: Detection_and_Fog_of_War.md §2
// ----------------------------------------------------------------

/// No SCATTERED state — scatter is an instantaneous removal event, not a persistent state.
/// Director ruling 2026-04-21.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NpcState {
    Anchored, // War Boar default; fixed to spawn hex
    Patrol,   // Raccoon default; random walk within leash radius
    Sentry,   // Aerial Drone variant; 2-hex patrol around Pylon; 6-hex max pursuit
    Pursuing, // active chase after detection threshold crossed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NpcType {
    WarBoar,
    RaccoonBiker,
    AerialDrone,
}

/// Hex-map NPC with AI positioning state and its combat unit payload.
/// `pack` carries the combat data for RaccoonBiker only.
/// Drove (WarBoar) and Swarm (AerialDrone) combat structs are pending Phase 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcUnit {
    pub id: String,
    pub npc_type: NpcType,
    pub current_hex: Coordinates,
    /// Spawn point. Never changes. Used for patrol leash and respawn.
    pub anchor_hex: Coordinates,
    pub state: NpcState,
    /// Minutes per hex. Boar: 40. Raccoon: 10.
    pub movement_speed: u32,
    /// Sensor reach in hexes. Boar: 5. Raccoon: 2.
    pub detection_radius: u32,
    pub radar_signature: u32,
    pub pack: Option<Pack>,
    /// True if player recon spotted this NPC during its approach.
    /// Determines CombatInitiationType: true → Spotted, false → Ambush.
    /// Reset to false after each engagement.
    pub was_spotted_during_approach: bool,
    pub target_convoy_id: Option<String>,
    pub respawn_cooldown_minutes: u32,
}

/// Lightweight convoy snapshot for NPC AI detection checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConvoy {
    pub id: String,
    pub current_hex: Coordinates,
    pub destination: Coordinates,
    /// Pre-computed sum of radar_signature across all convoy vehicles.
    pub noise_radius: u32,
    pub is_in_transit: bool,
    /// Bloodhound: vision_radius 2. Owl Sensor-Rig: vision_radius 5.
    pub recon_assets: Vec<EntityVision>,
}
