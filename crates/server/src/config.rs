// Server + simulation config — loaded once at startup via dotenvy.
// Every field in THIS struct has a fallback default. That is not the same as the
// server running without a .env file: DATABASE_URL and REDIS_URL are read directly
// in main.rs, have no defaults, and panic when absent.
// See .env.example for the full variable list and commentary.

use sim_engine::config::SimConfig;

// ── Helper readers ────────────────────────────────────────────────────────────

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_i32(key: &str, default: i32) -> i32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ── Server config ─────────────────────────────────────────────────────────────

/// Fallback bind address when SERVER_BIND_ADDR is unset. LOOPBACK ON PURPOSE.
///
/// The safe state must be the default. nginx terminates TLS and proxies only `/api/`
/// to this binary; a public bind puts the game API on the internet *and* serves `app/`
/// straight out of the ServeDir fallback in `routes/mod.rs`, which bypasses nginx
/// entirely — including the `/_dev/` deny rule. There is no firewall rule behind this.
///
/// Binding `0.0.0.0` is a deliberate local-dev opt-in (testing from a phone on the LAN,
/// for instance) set via SERVER_BIND_ADDR. It must never be the production value.
/// See Security_Mandate.md section 2i.
///
/// One constant, used by both `Default` and `from_env`, so the two cannot drift apart.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// TCP bind address for the Axum HTTP server. See [`DEFAULT_BIND_ADDR`].
    pub bind_addr: String,
    /// TEEPIN session duration in seconds (2 hours = 7200).
    pub session_duration_secs: i64,
    /// Seconds before an unanswered TEEPIN challenge expires.
    pub challenge_expiry_secs: i64,
    /// Seconds before a queued WS combat session is considered stale and rejected.
    pub combat_session_stale_secs: u64,
    /// Default max-ticks cap when the client does not supply one.
    pub default_max_ticks: usize,
    /// Solana JSON-RPC endpoint (e.g. Helius devnet).
    pub rpc_url: String,
    /// Path to the treasury keypair JSON file used to pay for on-chain transactions.
    pub treasury_keypair_path: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr:                  DEFAULT_BIND_ADDR.to_string(),
            session_duration_secs:      7200,
            challenge_expiry_secs:      60,
            combat_session_stale_secs:  300,
            default_max_ticks:          50,
            rpc_url:                    "https://api.devnet.solana.com".to_string(),
            treasury_keypair_path:      "/home/ajone/.config/solana/id.json".to_string(),
        }
    }
}

impl ServerConfig {
    fn from_env() -> Self {
        Self {
            bind_addr:                  env_str  ("SERVER_BIND_ADDR",            DEFAULT_BIND_ADDR),
            session_duration_secs:      env_i64  ("SESSION_DURATION_SECS",       7200),
            challenge_expiry_secs:      env_i64  ("CHALLENGE_EXPIRY_SECS",       60),
            combat_session_stale_secs:  env_u64  ("COMBAT_SESSION_STALE_SECS",   300),
            default_max_ticks:          env_usize("DEFAULT_MAX_TICKS",           50),
            rpc_url:                    env_str  ("SOLANA_RPC_URL",              "https://api.devnet.solana.com"),
            treasury_keypair_path:      env_str  ("TREASURY_KEYPAIR_PATH",       "/home/ajone/.config/solana/id.json"),
        }
    }
}

// ── Top-level config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub sim: SimConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            sim:    SimConfig::default(),
        }
    }
}

impl Config {
    /// Reads all configuration from environment variables (already loaded via
    /// `dotenvy::dotenv()` in `main`). Falls back to canonical defaults for any
    /// variable that is absent or unparseable.
    pub fn from_env() -> Self {
        let d = SimConfig::default();
        Self {
            server: ServerConfig::from_env(),
            sim: SimConfig {
                hp_loss_mult_success:         env_f64  ("HP_LOSS_MULT_SUCCESS",         d.hp_loss_mult_success),
                hp_loss_mult_failure:         env_f64  ("HP_LOSS_MULT_FAILURE",         d.hp_loss_mult_failure),
                hp_loss_clamp_min:            env_f64  ("HP_LOSS_CLAMP_MIN",            d.hp_loss_clamp_min),
                hp_roll_success_threshold:    env_u32  ("HP_ROLL_SUCCESS_THRESHOLD",    d.hp_roll_success_threshold),
                hp_roll_failure_threshold:    env_u32  ("HP_ROLL_FAILURE_THRESHOLD",    d.hp_roll_failure_threshold),
                kia_base_chance_success:      env_f64  ("KIA_BASE_CHANCE_SUCCESS",      d.kia_base_chance_success),
                kia_base_chance_failure:      env_f64  ("KIA_BASE_CHANCE_FAILURE",      d.kia_base_chance_failure),
                sawbones_trauma_chance:       env_f64  ("SAWBONES_TRAUMA_CHANCE",       d.sawbones_trauma_chance),
                outcome_margin_threshold:     env_f64  ("OUTCOME_MARGIN_THRESHOLD",     d.outcome_margin_threshold),
                full_success_reward_mult:     env_f64  ("FULL_SUCCESS_REWARD_MULT",     d.full_success_reward_mult),
                squad_size_bonus_per_unit:    env_i32  ("SQUAD_SIZE_BONUS_PER_UNIT",    d.squad_size_bonus_per_unit),
                base_skill_score_weight:      env_f64  ("BASE_SKILL_SCORE_WEIGHT",      d.base_skill_score_weight),
                ghost_wire_mission_bonus:     env_i32  ("GHOST_WIRE_MISSION_BONUS",     d.ghost_wire_mission_bonus),
                tunnel_runner_env_bonus:      env_i32  ("TUNNEL_RUNNER_ENV_BONUS",      d.tunnel_runner_env_bonus),
                prospector_extraction_bonus:  env_i32  ("PROSPECTOR_EXTRACTION_BONUS",  d.prospector_extraction_bonus),
                pyroclast_industrial_bonus:   env_i32  ("PYROCLAST_INDUSTRIAL_BONUS",   d.pyroclast_industrial_bonus),
                vanguard_stack_bonus_per_unit: env_i32 ("VANGUARD_STACK_BONUS_PER_UNIT",d.vanguard_stack_bonus_per_unit),
                max_vanguard_stack_bonus:     env_i32  ("MAX_VANGUARD_STACK_BONUS",     d.max_vanguard_stack_bonus),
                deployment_stress_penalty:    env_u32  ("DEPLOYMENT_STRESS_PENALTY",    d.deployment_stress_penalty),
                casualty_stress_penalty:      env_u32  ("CASUALTY_STRESS_PENALTY",      d.casualty_stress_penalty),
                max_loot_bonus_cap:           env_u32  ("MAX_LOOT_BONUS_CAP",           d.max_loot_bonus_cap),
                loot_drain_multiplier:        env_f64  ("LOOT_DRAIN_MULTIPLIER",        d.loot_drain_multiplier),
                loot_basic_weight_min:        env_f64  ("LOOT_BASIC_WEIGHT_MIN",        d.loot_basic_weight_min),
                loot_standard_uplift_frac:    env_f64  ("LOOT_STANDARD_UPLIFT_FRAC",   d.loot_standard_uplift_frac),
                loot_specialized_uplift_frac: env_f64  ("LOOT_SPECIALIZED_UPLIFT_FRAC",d.loot_specialized_uplift_frac),
                loot_superior_uplift_frac:    env_f64  ("LOOT_SUPERIOR_UPLIFT_FRAC",   d.loot_superior_uplift_frac),
                loot_elite_uplift_frac:       env_f64  ("LOOT_ELITE_UPLIFT_FRAC",      d.loot_elite_uplift_frac),
                loot_drop_chance_base:        env_u32  ("LOOT_DROP_CHANCE_BASE",        d.loot_drop_chance_base),
                loot_drop_chance_per_diff:    env_u32  ("LOOT_DROP_CHANCE_PER_DIFF",    d.loot_drop_chance_per_diff),
                unit_default_hp:              env_i32  ("UNIT_DEFAULT_HP",              d.unit_default_hp),
                tick_stream_delay_ms:         env_u64  ("TICK_STREAM_DELAY_MS",         d.tick_stream_delay_ms),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ServerConfig, DEFAULT_BIND_ADDR};

    /// The bind address must be loopback unless someone deliberately overrides it.
    /// Safety is the default state, not a systemd drop-in bolted on afterwards —
    /// that drop-in is defence in depth, not the guarantee. See Security_Mandate.md 2i.
    ///
    /// Both cases live in one test on purpose: they mutate the process-global
    /// SERVER_BIND_ADDR, and separate `#[test]` fns would race under a parallel runner.
    /// Note the repo's own `.env` sets SERVER_BIND_ADDR=0.0.0.0:3000 and the canonical
    /// test command sources it, so the unset case MUST clear the variable explicitly.
    #[test]
    fn bind_addr_defaults_to_loopback_and_honours_override() {
        let restore = std::env::var("SERVER_BIND_ADDR").ok();

        // No SERVER_BIND_ADDR anywhere → loopback.
        std::env::remove_var("SERVER_BIND_ADDR");
        assert_eq!(DEFAULT_BIND_ADDR, "127.0.0.1:3000");
        assert_eq!(
            ServerConfig::from_env().bind_addr,
            "127.0.0.1:3000",
            "with SERVER_BIND_ADDR unset the server must bind loopback, not 0.0.0.0"
        );
        assert_eq!(
            ServerConfig::default().bind_addr,
            "127.0.0.1:3000",
            "Default and from_env must not drift apart"
        );

        // Explicit override still works — local dev must be able to bind the LAN.
        std::env::set_var("SERVER_BIND_ADDR", "0.0.0.0:3000");
        assert_eq!(
            ServerConfig::from_env().bind_addr,
            "0.0.0.0:3000",
            "an explicit SERVER_BIND_ADDR must still be honoured"
        );

        match restore {
            Some(v) => std::env::set_var("SERVER_BIND_ADDR", v),
            None => std::env::remove_var("SERVER_BIND_ADDR"),
        }
    }
}
