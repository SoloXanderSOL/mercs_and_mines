/// Integration tests for Postgres infrastructure.
///
/// Requires a live Postgres instance. Set TEST_DATABASE_URL to run:
///   TEST_DATABASE_URL=postgres://user:pass@localhost/mercs_test cargo test
///
/// Skipped automatically when TEST_DATABASE_URL is absent so that plain
/// `cargo test` continues to work without a database.
use sqlx::postgres::PgPoolOptions;
use mercs_server::repository::{
    AccountRepository, PlayerAccount, PlayerProfile, PostgresAccountRepository, WalletAddress,
};

async fn test_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("Failed to connect to test Postgres");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Migrations failed");
    Some(pool)
}

#[tokio::test]
async fn db_pool_connects_and_migrations_run() {
    if test_pool().await.is_none() {
        eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
    }
}

#[tokio::test]
async fn player_account_upsert_is_idempotent() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresAccountRepository::new(pool.clone());
    let wallet = WalletAddress([1u8; 32]);
    let wallet_bytes = wallet.0.to_vec();

    // Pre-test cleanup in case a previous run left a row.
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("pre-test cleanup failed");

    let account = PlayerAccount {
        wallet,
        trust_standing: 0,
        profile: PlayerProfile { display_name: None, sector_id: None },
        gcn_ledger: vec![],
    };

    repo.upsert_account(account.clone()).await.expect("first upsert failed");
    repo.upsert_account(account).await.expect("second upsert (idempotent) failed");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .fetch_one(&pool)
        .await
        .expect("COUNT query failed");
    assert_eq!(count, 1, "expected exactly one row after two upserts");

    let gcn_balance: i64 = sqlx::query_scalar("SELECT gcn_balance FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .fetch_one(&pool)
        .await
        .expect("gcn_balance query failed");
    assert_eq!(gcn_balance, 0, "gcn_balance must not change on re-login");

    // Post-test cleanup.
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("post-test cleanup failed");
}
