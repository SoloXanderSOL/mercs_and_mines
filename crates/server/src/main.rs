use std::sync::Arc;
use sqlx::postgres::PgPoolOptions;
use mercs_server::config::Config;
use mercs_server::repository::{
    PostgresAccountRepository, PostgresCampaignRepository,
    PostgresCommanderRepository, PostgresInputLogRepository,
    PostgresMembershipRepository,
    PostgresSectionRepository, RedisSectorStateRepository, RedisSessionStateRepository,
    RedisTimerRepository,
};
use mercs_server::state::AppState;
use redis::Client as RedisClient;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

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
    sqlx::migrate!("../../migrations")
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
    println!("Redis connection manager ready");

    let config = Arc::new(Config::from_env());
    let bind_addr = config.server.bind_addr.clone();
    let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "./logs".into());
    let mut state = AppState::new(std::path::PathBuf::from(log_dir), config);
    state.account_repo    = Arc::new(PostgresAccountRepository::new(pool.clone()));
    state.campaign_repo   = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.commander_repo  = Arc::new(PostgresCommanderRepository::new(pool.clone()));
    state.membership_repo = Arc::new(PostgresMembershipRepository::new(pool.clone()));
    state.section_repo    = Arc::new(PostgresSectionRepository::new(pool.clone()));
    state.sector_repo     = Arc::new(RedisSectorStateRepository::new(redis_mgr.clone()));
    state.timer_repo      = Arc::new(RedisTimerRepository::new(redis_mgr.clone()));
    state.session_repo    = Arc::new(RedisSessionStateRepository::new(redis_mgr.clone()));
    state.input_log_repo  = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    state.pool  = Some(pool);
    state.redis = Some(redis_mgr);
    let state = Arc::new(state);

    let app = mercs_server::routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind {}", bind_addr));
    println!("Mercs and Mines server listening on {}", bind_addr);
    axum::serve(listener, app).await.expect("Server error");
}
