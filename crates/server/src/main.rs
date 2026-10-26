use std::sync::Arc;
use std::time::Duration;
use sqlx::postgres::PgPoolOptions;
use mercs_server::config::Config;
use mercs_server::lifecycle::run_sector_lifecycle_task;
use mercs_server::repository::{
    PostgresCampaignJoinRepository, PostgresCampaignRepository,
    PostgresConvoyRepository,
    PostgresCrewRepository, PostgresForemanRepository,
    PostgresCommanderRepository, PostgresInputLogRepository,
    PostgresMembershipRepository,
    PostgresSectionRepository, RedisSectorStateRepository, RedisSessionStateRepository,
    RedisTimerRepository,
};
use mercs_server::state::AppState;
use mercs_server::supervisor::supervise;
use redis::Client as RedisClient;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    mercs_server::try_init_tracing().expect("failed to install the tracing subscriber");

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set (add it to .env or the environment)");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("Failed to connect to Postgres — is DATABASE_URL correct and the server reachable?");

    // Fail fast: refuse to start if Postgres is unreachable.
    let _probe = pool.acquire().await
        .expect("Failed to acquire initial DB connection — Postgres may be down");
    drop(_probe);

    // Migrations directory: migrations/ at workspace root (two levels above crates/server/).
    // Lenient on applied versions it doesn't embed, so a rollback can boot (GAP-57).
    mercs_server::boot_migrator()
        .run(&pool)
        .await
        .expect("Database migrations failed");

    let redis_url = std::env::var("REDIS_URL")
        .expect("REDIS_URL must be set (add it to .env or the environment)");

    let redis_client = RedisClient::open(redis_url)
        .expect("Invalid REDIS_URL");
    let redis_mgr = redis::aio::ConnectionManager::new(redis_client)
        .await
        .expect("Failed to connect to Redis — is REDIS_URL correct and the server reachable?");
    tracing::info!("Redis connection manager ready");

    // Visibility only: a quarantined timer never fires, so say so at every boot.
    let mut quarantine_conn = redis_mgr.clone();
    match mercs_server::repository::timer::quarantine_size(&mut quarantine_conn).await {
        Ok(0) => {}
        Ok(n) => tracing::warn!("{n} timer member(s) in timers:quarantine — they will never fire; inspect and resolve by hand"),
        Err(e) => tracing::warn!("could not read the size of timers:quarantine: {e:?}"),
    }

    let config = Arc::new(Config::from_env());
    let bind_addr = config.server.bind_addr.clone();
    let mut state = AppState::new(config.clone());
    state.campaign_repo   = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.commander_repo  = Arc::new(PostgresCommanderRepository::new(pool.clone()));
    state.membership_repo = Arc::new(PostgresMembershipRepository::new(pool.clone()));
    state.section_repo    = Arc::new(PostgresSectionRepository::new(pool.clone()));
    state.foreman_repo    = Arc::new(PostgresForemanRepository::new(pool.clone()));
    state.crew_repo       = Arc::new(PostgresCrewRepository::new(pool.clone()));
    state.campaign_join_repo = Arc::new(PostgresCampaignJoinRepository::new(pool.clone()));
    state.sector_repo     = Arc::new(RedisSectorStateRepository::new(redis_mgr.clone()));
    state.timer_repo      = Arc::new(RedisTimerRepository::new(redis_mgr.clone()));
    state.convoy_repo     = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    state.session_repo    = Arc::new(RedisSessionStateRepository::new(redis_mgr.clone(), config.server.combat_session_stale_secs));
    state.input_log_repo  = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    state.pool  = Some(pool);
    state.redis = Some(redis_mgr);
    let state = Arc::new(state);

    let lifecycle_repo = Arc::clone(&state.campaign_repo);
    tokio::spawn(supervise(
        "lifecycle",
        Duration::from_secs(5),
        Duration::from_secs(60),
        move || run_sector_lifecycle_task(Arc::clone(&lifecycle_repo)),
    ));

    let expiry_state = Arc::clone(&state);
    tokio::spawn(supervise(
        "convoy_expiry",
        Duration::from_secs(5),
        Duration::from_secs(60),
        move || mercs_server::convoy_expiry::run_convoy_expiry_task(Arc::clone(&expiry_state)),
    ));

    let sweep_state = Arc::clone(&state);
    tokio::spawn(supervise(
        "auth_sweep",
        Duration::from_secs(5),
        Duration::from_secs(60),
        move || mercs_server::auth::run_auth_sweep_task(Arc::clone(&sweep_state)),
    ));

    let app = mercs_server::routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind {}", bind_addr));
    match listener.local_addr() {
        Ok(addr) => {
            tracing::info!("Mercs & Mines server listening on {addr}");
            if let Some(w) = mercs_server::config::public_bind_warning(&addr) {
                tracing::warn!("{w}");
            }
        }
        Err(e) => tracing::warn!(
            "listening on {bind_addr}, but could not read the bound address ({e}); skipping the public-bind check"
        ),
    }
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.expect("Server error");
}
