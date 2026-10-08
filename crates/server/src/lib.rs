pub mod campaign_init;
pub mod commander_gen;
pub mod config;
pub mod api_types;
pub mod auth;
pub mod integrity;
pub mod convoy_collision;
pub mod convoy_expiry;
pub mod lifecycle;
pub mod map_gen;
pub mod log_reader;
pub mod log_writer;
pub mod repeat_warn;
pub mod repository;
pub mod routes;
pub mod state;

/// Installs the global `tracing` subscriber: filter from `RUST_LOG` (default `info`),
/// written to stderr, ANSI colour only when stderr is a terminal (journald and pipes get
/// plain text). Idempotent: a second call is a no-op, so tests may call it.
/// `main` calls `dotenvy::dotenv()` first, so `RUST_LOG` from `.env` is honoured, and
/// calls this before `Config::from_env()`, which logs.
pub fn init_tracing() {
    use std::io::IsTerminal;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

#[cfg(test)]
mod tests {
    /// INF-2 — the subscriber is actually installed (INF-1 finding 1). No other lib test
    /// installs one, so this cannot pass by accident of test order.
    #[test]
    fn init_tracing_installs_global_subscriber() {
        super::init_tracing();
        super::init_tracing(); // idempotent: a second call must not panic
        assert!(tracing::dispatcher::has_been_set());
    }
}
