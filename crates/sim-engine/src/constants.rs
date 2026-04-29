// Structural and protocol constants — values that appear in match arms, algorithm
// definitions, or fixed-ratio formulas that are NOT game-balance tuning knobs.
// These must never be env-configurable; changing them is a breaking protocol change.

// ── AP/AT combat protocol ─────────────────────────────────────────────────────

/// d100 hit threshold: roll + accuracy − evasion must EXCEED this to hit.
pub const HIT_ROLL_THRESHOLD: i32 = 50;

// ── Mission score formula ─────────────────────────────────────────────────────

/// Maximum skill level (1–10 scale). Used to normalise avg skill to a 0–1 fraction.
pub const SKILL_LEVEL_MAX: f64 = 10.0;

/// Maximum number of squad members whose count contributes to the squad-size bonus.
/// Members beyond this cap do not add further bonus points.
pub const MAX_SQUAD_BONUS_UNITS: i32 = 4;

// ── Commander stress tier thresholds (used in match arms) ─────────────────────

/// Hard ceiling on stress_level. Match-arm constant; must not be made configurable.
pub const MAX_STRESS: u8 = 100;

/// Lower bound of the BREAKING_POINT stress tier (71–99).
pub const STRESS_BREAKING_POINT_MIN: u8 = 71;
/// Upper bound of the BREAKING_POINT stress tier (71–99).
pub const STRESS_BREAKING_POINT_MAX: u8 = 99;

/// Lower bound of the STRAINED stress tier (31–70).
pub const STRESS_STRAINED_MIN: u8 = 31;
/// Upper bound of the STRAINED stress tier (31–70).
pub const STRESS_STRAINED_MAX: u8 = 70;

/// Buff multiplier for a Strained commander (half effect).
pub const STRESS_STRAINED_BUFF_MULT: f32 = 0.5;

/// Commander rank required to be eligible for retirement to the Advisor Board.
pub const COMMANDER_RETIRE_RANK: u8 = 5;

// ── Mulberry32 PRNG algorithm constants ───────────────────────────────────────
// FINANCIAL REQUIREMENT: altering these constants breaks input-log replay and
// Prize Pool dispute resolution. They are immutable by design.

/// State-increment constant for the Mulberry32 PRNG.
pub const MULBERRY32_INCREMENT: u32 = 0x6d2b79f5;

/// 2³² as f64 — divisor used to convert a Mulberry32 u32 output into [0.0, 1.0).
pub const MULBERRY32_F64_DIVISOR: f64 = 4_294_967_296.0;

// ── FNV-1a hash algorithm constants ───────────────────────────────────────────
// Used in seed_from_str() to derive deterministic seeds from mission ID + timestamp.
// Changing these alters every seeded run and breaks log-replay parity.

/// FNV-1a 32-bit offset basis.
pub const FNV1A_OFFSET_BASIS: u32 = 2_166_136_261;

/// FNV-1a 32-bit prime.
pub const FNV1A_PRIME: u32 = 16_777_619;

// ── WebSocket streaming ───────────────────────────────────────────────────────

/// mpsc channel buffer depth for outbound CombatTickEvent messages.
pub const TICK_CHANNEL_BUFFER: usize = 32;
