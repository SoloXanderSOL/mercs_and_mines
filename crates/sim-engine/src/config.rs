// Simulation balance configuration — all tunable game-logic constants.
// Default values are the Phase 0 canonical values from the GDD.
// The server populates this from .env at startup and passes &SimConfig
// to every resolver function that needs it.

/// All tunable game-balance values for the simulation engine.
/// Derive Clone so the server can cheaply hand copies to async tasks.
#[derive(Debug, Clone)]
pub struct SimConfig {
    // ── Mission resolver: damage resolution ──────────────────────────────────

    /// HP-loss chance multiplier applied on a successful mission (softens the blow).
    pub hp_loss_mult_success: f64,
    /// HP-loss chance multiplier applied on a failed mission (increases danger).
    pub hp_loss_mult_failure: f64,
    /// Minimum effective HP-loss chance after damage-shield clamp (%).
    pub hp_loss_clamp_min: f64,

    /// d100 roll threshold (inclusive) for taking 1 HP lost vs 2 HP lost on success.
    pub hp_roll_success_threshold: u32,
    /// d100 roll threshold (inclusive) for taking 2 HP lost vs 3 HP lost on failure.
    pub hp_roll_failure_threshold: u32,

    /// Base KIA chance (%) on a successful mission before kia_multiplier scaling.
    pub kia_base_chance_success: f64,
    /// Base KIA chance (%) on a failed mission before kia_multiplier scaling.
    pub kia_base_chance_failure: f64,

    /// Sawbones Trauma Protocol: chance to convert a KIA result to Wounded instead.
    pub sawbones_trauma_chance: f64,

    // ── Mission resolver: outcome thresholds ─────────────────────────────────

    /// Margin (success_probability − raw_roll) that separates Full Success from
    /// Partial Success, and Wipeout from Tactical Retreat.
    pub outcome_margin_threshold: f64,

    /// Reward multiplier applied to credits and ore on a Full Success outcome.
    pub full_success_reward_mult: f64,

    // ── Mission score calculation ─────────────────────────────────────────────

    /// Score points added per additional squad member beyond the first.
    pub squad_size_bonus_per_unit: i32,

    /// Weight factor that converts normalised average skill (0–1) into a score.
    /// score = (avg_skill / SKILL_LEVEL_MAX) × base_skill_score_weight
    pub base_skill_score_weight: f64,

    // ── Mission-type / environment ability bonuses ───────────────────────────

    /// GhostWire: bonus on Sabotage and Extraction missions.
    pub ghost_wire_mission_bonus: i32,
    /// TunnelRunner: bonus in Underground environments.
    pub tunnel_runner_env_bonus: i32,
    /// Prospector: bonus on Extraction missions.
    pub prospector_extraction_bonus: i32,
    /// Pyroclast: bonus in Industrial environments (removes terrain penalty).
    pub pyroclast_industrial_bonus: i32,

    /// Vanguard stack: bonus score per additional Vanguard beyond the first.
    pub vanguard_stack_bonus_per_unit: i32,
    /// Vanguard stack: total cap on the extra bonus from stacking Vanguards.
    pub max_vanguard_stack_bonus: i32,

    // ── Commander stress ──────────────────────────────────────────────────────

    /// Stress penalty applied once at mission deployment start.
    pub deployment_stress_penalty: u32,
    /// Stress penalty applied per casualty during combat.
    pub casualty_stress_penalty: u32,

    // ── Loot roller ───────────────────────────────────────────────────────────

    /// Maximum loot bonus points accepted before capping (prevents Elite spam).
    pub max_loot_bonus_cap: u32,
    /// Per-bonus-point drain from Basic grade weight.
    pub loot_drain_multiplier: f64,
    /// Floor weight for Basic grade — prevents it from hitting zero.
    pub loot_basic_weight_min: f64,
    /// Fraction of uplift redistributed to Standard grade.
    pub loot_standard_uplift_frac: f64,
    /// Fraction of uplift redistributed to Specialized grade.
    pub loot_specialized_uplift_frac: f64,
    /// Fraction of uplift redistributed to Superior grade.
    pub loot_superior_uplift_frac: f64,
    /// Fraction of uplift redistributed to Elite grade.
    pub loot_elite_uplift_frac: f64,
    /// Base loot drop chance (%) before difficulty scaling.
    pub loot_drop_chance_base: u32,
    /// Additional drop chance (%) gained per mission difficulty tier.
    pub loot_drop_chance_per_diff: u32,

    // ── Units ─────────────────────────────────────────────────────────────────

    /// Starting current_hp and max_hp for newly created merc units.
    pub unit_default_hp: i32,

    // ── Streaming combat ─────────────────────────────────────────────────────

    /// Delay in milliseconds between each streamed CombatTickEvent.
    pub tick_stream_delay_ms: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            hp_loss_mult_success:         0.4,
            hp_loss_mult_failure:         1.5,
            hp_loss_clamp_min:            2.0,
            hp_roll_success_threshold:    60,
            hp_roll_failure_threshold:    40,
            kia_base_chance_success:      10.0,
            kia_base_chance_failure:      35.0,
            sawbones_trauma_chance:       0.30,
            outcome_margin_threshold:     25.0,
            full_success_reward_mult:     1.5,
            squad_size_bonus_per_unit:    3,
            base_skill_score_weight:      50.0,
            ghost_wire_mission_bonus:     7,
            tunnel_runner_env_bonus:      8,
            prospector_extraction_bonus:  5,
            pyroclast_industrial_bonus:   15,
            vanguard_stack_bonus_per_unit: 2,
            max_vanguard_stack_bonus:     6,
            deployment_stress_penalty:    10,
            casualty_stress_penalty:      5,
            max_loot_bonus_cap:           50,
            loot_drain_multiplier:        1.5,
            loot_basic_weight_min:        5.0,
            loot_standard_uplift_frac:    0.4,
            loot_specialized_uplift_frac: 0.3,
            loot_superior_uplift_frac:    0.2,
            loot_elite_uplift_frac:       0.1,
            loot_drop_chance_base:        40,
            loot_drop_chance_per_diff:    4,
            unit_default_hp:              10,
            tick_stream_delay_ms:         750,
        }
    }
}
