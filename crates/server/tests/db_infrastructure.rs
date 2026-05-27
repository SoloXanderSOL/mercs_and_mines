/// Integration test for Postgres infrastructure (brick 1a-0).
///
/// Requires a live Postgres instance. Set TEST_DATABASE_URL to run:
///   TEST_DATABASE_URL=postgres://user:pass@localhost/mercs_test cargo test db_pool_connects
///
/// Skipped automatically when TEST_DATABASE_URL is absent so that plain
/// `cargo test` continues to work without a database.
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
async fn db_pool_connects_and_migrations_run() {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("Failed to connect to test Postgres");

    // Migrations directory: migrations/ at workspace root (../../ relative to crates/server/).
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Migrations failed");

    let _conn = pool.acquire().await.expect("Failed to acquire connection after migrations");
}
