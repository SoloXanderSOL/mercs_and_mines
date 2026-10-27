// Server + simulation config — loaded once at startup via dotenvy.
// Every field in THIS struct has a fallback default. That is not the same as the
// server running without a .env file: DATABASE_URL and REDIS_URL are read directly
// in main.rs, have no defaults, and panic when absent.
// See .env.example for the full variable list and commentary.

use sim_engine::config::SimConfig;
use crate::genesis::RpcUrl;

// ── Helper readers ────────────────────────────────────────────────────────────

/// Reads `key` as a `T`. Unset → `default`, silently. Set but unparseable (`str::parse`
/// does not trim) → `default`, with a warning naming the key and the raw value. A value
/// that parses is used exactly as before, so no environment changes what it yields.
/// Takes a lookup closure so tests never touch the process environment.
fn parse_or_warn<T: std::str::FromStr>(get: impl Fn(&str) -> Option<String>, key: &str, default: T) -> T {
    let Some(raw) = get(key) else { return default };
    match raw.parse::<T>() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("{} is set to {:?}, which does not parse as {}; ignoring it", key, raw, std::any::type_name::<T>());
            default
        }
    }
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn env_u32(key: &str, default: u32) -> u32 { parse_or_warn(env_lookup, key, default) }
fn env_u64(key: &str, default: u64) -> u64 { parse_or_warn(env_lookup, key, default) }
fn env_f64(key: &str, default: f64) -> f64 { parse_or_warn(env_lookup, key, default) }
fn env_i32(key: &str, default: i32) -> i32 { parse_or_warn(env_lookup, key, default) }

pub(crate) const DECISIVE_SUCCESS_REWARD_MULT_KEY: &str = "DECISIVE_SUCCESS_REWARD_MULT";

/// Reads `key`, falling back to `default`. A value is usable only if it parses as `f64`
/// AND is finite — `NaN`, `inf` and unparseable text all count as absent, with a
/// warning, because a NaN multiplier silently pays 0.
/// Takes a lookup closure so tests never touch the process environment;
/// `from_env` passes `|k| std::env::var(k).ok()`.
fn finite_f64_or_warn(get: impl Fn(&str) -> Option<String>, key: &str, default: f64) -> f64 {
    let Some(raw) = get(key) else { return default };
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() => v,
        _ => {
            tracing::warn!("{} is set to {:?}, which is not a finite number; ignoring it", key, raw);
            default
        }
    }
}

/// Reads a `u32` stress penalty. Unset → `default`, silently. Set but not a `u32`
/// (`""`, `"-5"`, `"10.0"`, `" 7"`; `str::parse` does not trim) → `default`, with a warning.
/// Above `warn_above` → the value as given, with a warning and **no clamp**: a penalty that
/// size shatters a Commander in one application, which is legal, merely suspicious.
/// Takes a lookup closure so tests never touch the process environment.
fn u32_with_warn(get: impl Fn(&str) -> Option<String>, key: &str, default: u32, warn_above: u32) -> u32 {
    let Some(raw) = get(key) else { return default };
    match raw.parse::<u32>() {
        Ok(v) => {
            if v > warn_above {
                tracing::warn!("{} is {}, above MAX_STRESS ({}): one application shatters any Commander; using it as given", key, v, warn_above);
            }
            v
        }
        Err(_) => {
            tracing::warn!("{} is set to {:?}, which is not a whole number; ignoring it", key, raw);
            default
        }
    }
}

/// The env key for the mainnet RPC URL (BP-4). Its value carries a provider key.
pub(crate) const SOLANA_MAINNET_RPC_URL_KEY: &str = "SOLANA_MAINNET_RPC_URL";

/// Reads an RPC URL that may carry a provider key. Unset or empty → `None`, silently. Set
/// but not an `http`/`https` URL → `None`, with a warning that names the key and never the
/// value (unlike `parse_or_warn`, which journals the raw value).
fn rpc_url_or_warn(get: impl Fn(&str) -> Option<String>, key: &str) -> Option<RpcUrl> {
    let raw = get(key)?;
    if raw.is_empty() {
        return None;
    }
    let url = RpcUrl::parse(&raw);
    if url.is_none() {
        tracing::warn!("{key} is set but is not a usable http(s) URL; genesis lookups are off");
    }
    url
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
    /// The join switch, from `MERCS_JOIN_ENABLED` (exactly `true` opens joins). Off by default until INF-6's rate limit is live.
    pub join_enabled: bool,
    /// The mainnet Solana JSON-RPC URL for the Genesis Token lookup (BP-4). It carries a
    /// provider key: `RpcUrl`'s `Debug` redacts it. `None` (unset, empty or unusable)
    /// turns the lookup off.
    pub solana_mainnet_rpc_url: Option<RpcUrl>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr:                  DEFAULT_BIND_ADDR.to_string(),
            session_duration_secs:      7200,
            challenge_expiry_secs:      60,
            combat_session_stale_secs:  300,
            default_max_ticks:          50,
            join_enabled:               false,
            solana_mainnet_rpc_url:     None,
        }
    }
}

impl ServerConfig {
    fn from_env() -> Self {
        Self::from_lookup(env_lookup)
    }

    /// Reads every field through `get`, so tests pass a fixed map and never touch the
    /// process environment (Reviewer 5a-3c F1).
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            bind_addr:                  get("SERVER_BIND_ADDR").unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string()),
            session_duration_secs:      parse_or_warn(&get, "SESSION_DURATION_SECS",     7200),
            challenge_expiry_secs:      parse_or_warn(&get, "CHALLENGE_EXPIRY_SECS",     60),
            combat_session_stale_secs:  parse_or_warn(&get, "COMBAT_SESSION_STALE_SECS", 300),
            default_max_ticks:          parse_or_warn(&get, "DEFAULT_MAX_TICKS",         50),
            join_enabled:               parse_or_warn(&get, "MERCS_JOIN_ENABLED",        false),
            solana_mainnet_rpc_url:     rpc_url_or_warn(&get, SOLANA_MAINNET_RPC_URL_KEY),
        }
    }
}

/// The boot warning for a non-loopback bind (Security_Mandate §2i). `None` on loopback,
/// IPv4-mapped loopback included; otherwise the text `main` journals once.
pub fn public_bind_warning(addr: &std::net::SocketAddr) -> Option<String> {
    if addr.ip().to_canonical().is_loopback() {
        return None;
    }
    Some(format!(
        "server is bound to {addr}, which is not a loopback address: it is reachable without nginx, bypassing its controls; bind 127.0.0.1 with SERVER_BIND_ADDR unless this is local development (Security_Mandate §2i)"
    ))
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
                decisive_success_reward_mult: finite_f64_or_warn(
                    |k| std::env::var(k).ok(),
                    DECISIVE_SUCCESS_REWARD_MULT_KEY,
                    d.decisive_success_reward_mult,
                ),
                squad_size_bonus_per_unit:    env_i32  ("SQUAD_SIZE_BONUS_PER_UNIT",    d.squad_size_bonus_per_unit),
                base_skill_score_weight:      env_f64  ("BASE_SKILL_SCORE_WEIGHT",      d.base_skill_score_weight),
                ghost_wire_mission_bonus:     env_i32  ("GHOST_WIRE_MISSION_BONUS",     d.ghost_wire_mission_bonus),
                tunnel_runner_env_bonus:      env_i32  ("TUNNEL_RUNNER_ENV_BONUS",      d.tunnel_runner_env_bonus),
                prospector_extraction_bonus:  env_i32  ("PROSPECTOR_EXTRACTION_BONUS",  d.prospector_extraction_bonus),
                pyroclast_industrial_bonus:   env_i32  ("PYROCLAST_INDUSTRIAL_BONUS",   d.pyroclast_industrial_bonus),
                vanguard_stack_bonus_per_unit: env_i32 ("VANGUARD_STACK_BONUS_PER_UNIT",d.vanguard_stack_bonus_per_unit),
                max_vanguard_stack_bonus:     env_i32  ("MAX_VANGUARD_STACK_BONUS",     d.max_vanguard_stack_bonus),
                deployment_stress_penalty:    u32_with_warn(|k| std::env::var(k).ok(), "DEPLOYMENT_STRESS_PENALTY", d.deployment_stress_penalty, u32::from(sim_engine::constants::MAX_STRESS)),
                casualty_stress_penalty:      u32_with_warn(|k| std::env::var(k).ok(), "CASUALTY_STRESS_PENALTY",   d.casualty_stress_penalty,   u32::from(sim_engine::constants::MAX_STRESS)),
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
    use super::{
        finite_f64_or_warn, parse_or_warn, public_bind_warning, u32_with_warn, Config, ServerConfig,
        DECISIVE_SUCCESS_REWARD_MULT_KEY, DEFAULT_BIND_ADDR,
    };
    use std::net::SocketAddr;
    use std::collections::HashMap;

    /// Fixed-map lookup — the reward-mult tests never mutate the process environment.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| map.get(k).cloned()
    }

    fn reward_mult(pairs: &[(&str, &str)]) -> f64 {
        finite_f64_or_warn(lookup(pairs), DECISIVE_SUCCESS_REWARD_MULT_KEY, 1.5)
    }

    #[test]
    fn reward_mult_reads_key() {
        assert_eq!(reward_mult(&[(DECISIVE_SUCCESS_REWARD_MULT_KEY, "2.0")]), 2.0);
    }

    #[test]
    fn reward_mult_unparseable_counts_as_absent() {
        assert_eq!(reward_mult(&[(DECISIVE_SUCCESS_REWARD_MULT_KEY, "not-a-number")]), 1.5);
    }

    #[test]
    fn reward_mult_defaults_when_unset() {
        assert_eq!(reward_mult(&[]), 1.5);
    }

    /// `"NaN"` and `"inf"` parse as f64 — they must still count as absent.
    #[test]
    fn reward_mult_non_finite_counts_as_absent() {
        assert_eq!(reward_mult(&[(DECISIVE_SUCCESS_REWARD_MULT_KEY, "NaN")]), 1.5);
        assert_eq!(reward_mult(&[(DECISIVE_SUCCESS_REWARD_MULT_KEY, "inf")]), 1.5);
    }

    /// GAP-40 part 1 — every numeric reader type warns and falls back on an unparseable
    /// value, stays silent on an unset one, and uses a parseable one as given.
    #[test]
    fn env_readers_warn_and_fall_back() {
        fn check<T: std::str::FromStr + PartialEq + std::fmt::Debug + Copy>(
            default: T,
            valid: (&str, T),
            bad: &[&str],
        ) {
            let read = |pairs: &[(&str, &str)]| parse_or_warn(lookup(pairs), "K", default);
            assert_eq!(read(&[]), default, "unset → default");
            assert_eq!(read(&[("K", valid.0)]), valid.1);
            for b in bad {
                assert_eq!(read(&[("K", b)]), default, "{b:?} must fall back to the default");
            }
        }
        let common = ["1,5", "", " 7", "abc"];
        let unsigned = ["1,5", "", " 7", "abc", "-5"];
        check::<u32>(10, ("7", 7), &unsigned);
        check::<u64>(10, ("7", 7), &unsigned);
        check::<usize>(10, ("7", 7), &unsigned);
        check::<i32>(10, ("-7", -7), &common);
        check::<i64>(10, ("-7", -7), &common);
        check::<f64>(1.5, ("2.25", 2.25), &common);
    }

    /// 5a-3c — the join switch is closed unless something opens it.
    #[test]
    fn join_switch_defaults_off() {
        assert_eq!(Config::default().server.join_enabled, false);
    }

    /// 5a-3c — only the exact text `true` opens the join switch; everything else, unset
    /// included, keeps it closed.
    #[test]
    fn join_switch_parses_exact_true_only() {
        // `None` is the unset case.
        let read = |value: Option<&str>| {
            let key = "MERCS_JOIN_ENABLED";
            let pairs: Vec<(&str, &str)> = value.into_iter().map(|v| (key, v)).collect();
            parse_or_warn(lookup(&pairs), key, false)
        };
        assert_eq!(read(Some("true")), true, "\"true\" opens the switch");
        for bad in ["false", "1", "TRUE", ""] {
            assert_eq!(read(Some(bad)), false, "{bad:?} must keep the switch closed");
        }
        assert_eq!(read(None), false, "unset keeps the switch closed");
    }

    /// INF-2 — a stress penalty that is set but unusable warns and falls back; one above
    /// MAX_STRESS warns and is kept as given (no clamp).
    #[test]
    fn stress_penalty_warns_and_falls_back_without_clamping() {
        let pen = |pairs: &[(&str, &str)]| u32_with_warn(lookup(pairs), "K", 10, 100);
        assert_eq!(pen(&[]), 10, "unset → default");
        assert_eq!(pen(&[("K", "7")]), 7);
        assert_eq!(pen(&[("K", "100")]), 100, "MAX_STRESS itself is not suspicious");
        assert_eq!(pen(&[("K", "150")]), 150, "above MAX_STRESS: warn, never clamp");
        for bad in ["", "abc", "-5", "10.0", " 7"] {
            assert_eq!(pen(&[("K", bad)]), 10, "{bad:?} must fall back to the default");
        }
    }

    /// The bind address must be loopback unless someone deliberately overrides it.
    /// Safety is the default state, not a systemd drop-in bolted on afterwards —
    /// that drop-in is defence in depth, not the guarantee. See Security_Mandate.md 2i.
    /// Read through `from_lookup`, so the repo `.env` (which binds 0.0.0.0) cannot leak in.
    #[test]
    fn bind_addr_defaults_to_loopback_and_honours_override() {
        // No SERVER_BIND_ADDR anywhere → loopback.
        assert_eq!(DEFAULT_BIND_ADDR, "127.0.0.1:3000");
        assert_eq!(
            ServerConfig::from_lookup(lookup(&[])).bind_addr,
            "127.0.0.1:3000",
            "with SERVER_BIND_ADDR unset the server must bind loopback, not 0.0.0.0"
        );
        assert_eq!(
            ServerConfig::default().bind_addr,
            "127.0.0.1:3000",
            "Default and from_env must not drift apart"
        );

        // Explicit override still works — local dev must be able to bind the LAN.
        assert_eq!(
            ServerConfig::from_lookup(lookup(&[("SERVER_BIND_ADDR", "0.0.0.0:3000")])).bind_addr,
            "0.0.0.0:3000",
            "an explicit SERVER_BIND_ADDR must still be honoured"
        );
    }

    /// INF-6b — with nothing set, `from_lookup` keeps the join switch closed.
    #[test]
    fn server_config_from_lookup_keeps_joins_closed_when_unset() {
        assert_eq!(ServerConfig::from_lookup(lookup(&[])).join_enabled, false);
    }

    /// INF-6b — `from_lookup` reads the join switch through its lookup.
    #[test]
    fn server_config_from_lookup_reads_the_join_switch() {
        assert_eq!(ServerConfig::from_lookup(lookup(&[("MERCS_JOIN_ENABLED", "true")])).join_enabled, true);
    }

    /// BP-4 — the RPC URL: unset, empty or unusable → `None`, an http(s) URL → `Some`, and
    /// the config's `Debug` never shows the URL or its key.
    #[test]
    fn solana_rpc_url_reads_and_never_prints() {
        let key = "SOLANA_MAINNET_RPC_URL";
        let read = |pairs: &[(&str, &str)]| ServerConfig::from_lookup(lookup(pairs)).solana_mainnet_rpc_url;
        assert!(read(&[]).is_none(), "unset → None");
        for bad in ["", "not a url", "ftp://rpc.example/?api-key=SECRETKEY"] {
            assert!(read(&[(key, bad)]).is_none(), "{bad:?} must read as None");
        }
        assert!(ServerConfig::default().solana_mainnet_rpc_url.is_none(), "Default → None");
        let cfg = ServerConfig::from_lookup(lookup(&[(key, "https://rpc.example/?api-key=SECRETKEY")]));
        assert!(cfg.solana_mainnet_rpc_url.is_some(), "an https URL → Some");
        let shown = format!("{cfg:?}");
        assert!(!shown.contains("SECRETKEY") && !shown.contains("rpc.example"), "Debug leaked the URL: {shown}");
        assert!(shown.contains("RpcUrl(<redacted>)"), "Debug still lists the field: {shown}");
    }

    /// INF-6b — an unspecified (all-interfaces) bind warns, naming the address.
    #[test]
    fn public_bind_warning_fires_for_unspecified_addresses() {
        for a in ["0.0.0.0:3000", "[::]:3000"] {
            let addr: SocketAddr = a.parse().unwrap();
            let w = public_bind_warning(&addr).unwrap_or_else(|| panic!("{a} must warn"));
            assert!(w.contains(&addr.to_string()), "the warning must name {a}: {w}");
        }
    }

    /// INF-6b — a loopback bind is silent, IPv4-mapped loopback included.
    #[test]
    fn public_bind_warning_is_silent_on_loopback() {
        for a in ["127.0.0.1:3000", "[::1]:3000", "[::ffff:127.0.0.1]:3000"] {
            let addr: SocketAddr = a.parse().unwrap();
            assert_eq!(public_bind_warning(&addr), None, "{a} is loopback");
        }
    }
}
