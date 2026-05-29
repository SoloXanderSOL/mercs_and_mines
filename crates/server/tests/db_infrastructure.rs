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
    CampaignLifecycle, CampaignRepository, NewCampaignInstance,
    PostgresCampaignRepository, VictoryTickerType,
    CommanderRecord, CommanderRepository, PostgresCommanderRepository,
    SectionRecord, SectionRepository, PostgresSectionRepository,
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

#[tokio::test]
async fn campaign_instance_crud_is_correct() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresCampaignRepository::new(pool.clone());

    // 1. Create a campaign.
    let params = NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  0xDEADBEEF_i64,
    };
    let campaign = repo.create_campaign(&params).await.expect("create failed");
    assert_eq!(campaign.state, CampaignLifecycle::Pending);
    assert_eq!(campaign.map_seed, 0xDEADBEEF_i64);

    // 2. Fetch it back.
    let fetched = repo.get_campaign(campaign.campaign_id).await
        .expect("get failed")
        .expect("not found");
    assert_eq!(fetched.campaign_id, campaign.campaign_id);

    // 3. Transition state Pending → Active.
    repo.update_sector_state(campaign.campaign_id, CampaignLifecycle::Active).await
        .expect("state update failed");
    let active = repo.get_campaign(campaign.campaign_id).await
        .expect("get failed")
        .expect("not found");
    assert_eq!(active.state, CampaignLifecycle::Active);

    // 4. Apply a ticker delta.
    let new_val = repo.apply_ticker_delta(
        campaign.campaign_id,
        VictoryTickerType::OneWorldGunvernment,
        15.0,
    ).await.expect("ticker delta failed");
    assert!((new_val - 15.0).abs() < f64::EPSILON);

    // 5. Clamp test — 200.0 delta on a 15.0 base should return 100.0.
    let clamped = repo.apply_ticker_delta(
        campaign.campaign_id,
        VictoryTickerType::OneWorldGunvernment,
        200.0,
    ).await.expect("ticker delta clamp failed");
    assert!((clamped - 100.0).abs() < f64::EPSILON);

    // 6. list_campaigns_by_state includes our campaign.
    let active_list = repo.list_campaigns_by_state(CampaignLifecycle::Active).await
        .expect("list failed");
    assert!(active_list.iter().any(|c| c.campaign_id == campaign.campaign_id));

    // Post-test cleanup.
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign.campaign_id)
        .execute(&pool)
        .await
        .expect("post-test cleanup failed");
}

#[tokio::test]
async fn commander_section_crud_is_correct() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let commander_repo = PostgresCommanderRepository::new(pool.clone());
    let section_repo   = PostgresSectionRepository::new(pool.clone());
    let campaign_repo  = PostgresCampaignRepository::new(pool.clone());

    // 1. Create a campaign_instance (FK prerequisite).
    let campaign = campaign_repo.create_campaign(&NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  0xCAFEBABE_i64,
    }).await.expect("create campaign failed");
    let campaign_id = campaign.campaign_id;

    // Pre-test cleanup in case a previous run left rows.
    sqlx::query("DELETE FROM section_records WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("pre-test section cleanup failed");
    sqlx::query("DELETE FROM commander_records WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("pre-test commander cleanup failed");

    let commander_id = uuid::Uuid::new_v4();
    let wallet_bytes = [2u8; 32].to_vec();

    // Insert a player_accounts row so the FK on commander_records.player_wallet is satisfied.
    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
         VALUES ($1, 0, 0)
         ON CONFLICT (wallet_address) DO NOTHING"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await
    .expect("player_accounts FK seed failed");

    // 2. Create a CommanderRecord.
    commander_repo.create_commander(CommanderRecord {
        commander_id,
        campaign_id,
        player_wallet:   wallet_bytes.clone(),
        name:            "Sgt. Ironclad".into(),
        rank:            1,
        xp:              0,
        stress:          0,
        origin:          "Outer Rim".into(),
        faction_bias:    "Trust".into(),
        specialization:  "Heavy Weapons".into(),
        fatal_flaw:      "Reckless".into(),
        veteran_trait:   None,
        is_shattered:    false,
        is_kia:          false,
        is_nft:          false,
        prng_seed_state: 0xDEAD_i64,
        created_at:      chrono::Utc::now(),
        updated_at:      chrono::Utc::now(),
    }).await.expect("create_commander failed");

    // 3. get_commander — all fields round-trip.
    let fetched = commander_repo.get_commander(commander_id).await
        .expect("get_commander error")
        .expect("commander not found");
    assert_eq!(fetched.commander_id, commander_id);
    assert_eq!(fetched.name, "Sgt. Ironclad");
    assert_eq!(fetched.rank, 1);
    assert_eq!(fetched.xp, 0);
    assert_eq!(fetched.stress, 0);
    assert!(!fetched.is_shattered);
    assert!(!fetched.is_kia);
    assert!(fetched.veteran_trait.is_none());

    // 4. update_stress.
    commander_repo.update_stress(commander_id, 35).await.expect("update_stress failed");
    let after_stress = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(after_stress.stress, 35);

    // 5. update_rank_and_xp.
    commander_repo.update_rank_and_xp(commander_id, 2, 100).await.expect("update_rank_and_xp failed");
    let after_rank = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(after_rank.rank, 2);
    assert_eq!(after_rank.xp, 100);

    // 6. set_shattered — shattered ≠ kia (load-bearing canon assertion).
    commander_repo.set_shattered(commander_id).await.expect("set_shattered failed");
    let after_shattered = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert!(after_shattered.is_shattered, "is_shattered must be true");
    assert!(!after_shattered.is_kia, "is_kia must remain false after set_shattered — Shattered is NOT Permadeath");

    // 7. Create a SectionRecord linked to the same campaign and commander.
    let section_id = uuid::Uuid::new_v4();
    section_repo.create_section(SectionRecord {
        section_id,
        campaign_id,
        commander_id: Some(commander_id),
        name:         "Alpha Section".into(),
        headcount:    8,
        loadout:      serde_json::json!({}),
        xp:           0,
        created_at:   chrono::Utc::now(),
        updated_at:   chrono::Utc::now(),
    }).await.expect("create_section failed");

    // 8. get_section — fields round-trip.
    let sec = section_repo.get_section(section_id).await
        .expect("get_section error")
        .expect("section not found");
    assert_eq!(sec.section_id, section_id);
    assert_eq!(sec.commander_id, Some(commander_id));
    assert_eq!(sec.headcount, 8);

    // 9. update_headcount.
    section_repo.update_headcount(section_id, 5).await.expect("update_headcount failed");
    let after_hc = section_repo.get_section(section_id).await.unwrap().unwrap();
    assert_eq!(after_hc.headcount, 5);

    // 10. list_sections_by_commander.
    let by_cmd = section_repo.list_sections_by_commander(commander_id).await
        .expect("list_sections_by_commander failed");
    assert_eq!(by_cmd.len(), 1);

    // 11. set_kia — section must survive (ON DELETE SET NULL), commander_id becomes None.
    commander_repo.set_kia(commander_id).await.expect("set_kia failed");
    let after_kia = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert!(after_kia.is_kia, "is_kia must be true");

    // Postgres ON DELETE SET NULL fires only when the commander_records row is deleted,
    // not when is_kia is set. The row still exists; commander_id on the section is unchanged.
    // We explicitly null it to simulate what campaign cleanup would do.
    section_repo.assign_commander(section_id, None).await.expect("assign_commander(None) failed");
    let sec_after_kia = section_repo.get_section(section_id).await.unwrap().unwrap();
    assert!(sec_after_kia.commander_id.is_none(), "commander_id must be None after KIA cleanup");
    assert_eq!(sec_after_kia.headcount, 5, "headcount must be preserved after KIA");

    // 12. delete_sections_by_campaign.
    let deleted_sections = section_repo.delete_sections_by_campaign(campaign_id).await
        .expect("delete_sections_by_campaign failed");
    assert!(deleted_sections > 0, "expected at least one section deleted");
    let remaining = section_repo.list_sections_by_campaign(campaign_id).await.unwrap();
    assert!(remaining.is_empty(), "no sections should remain after delete_sections_by_campaign");

    // 13. delete_commanders_by_campaign.
    let deleted_commanders = commander_repo.delete_commanders_by_campaign(campaign_id).await
        .expect("delete_commanders_by_campaign failed");
    assert!(deleted_commanders > 0, "expected at least one commander deleted");
    let remaining_cmds = commander_repo.list_commanders_by_campaign(campaign_id).await.unwrap();
    assert!(remaining_cmds.is_empty(), "no commanders should remain after delete_commanders_by_campaign");

    // Post-test cleanup.
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("post-test campaign cleanup failed");
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("post-test player_accounts cleanup failed");
}
