/// Integration tests for Postgres infrastructure.
///
/// Requires a live Postgres instance. Set TEST_DATABASE_URL to run:
///   TEST_DATABASE_URL=postgres://user:pass@localhost/mercs_test cargo test
///
/// Skipped automatically when TEST_DATABASE_URL is absent so that plain
/// `cargo test` continues to work without a database.
use mercs_server::campaign_init::initialize_campaign;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use mercs_server::repository::{
    AccountRepository, GcnLedgerEntry, PlayerAccount, PlayerProfile, PostgresAccountRepository,
    WalletAddress, CampaignLifecycle, CampaignRepository, NewCampaignInstance,
    PostgresCampaignRepository, VictoryTickerType,
    CommanderRecord, CommanderRepository, PostgresCommanderRepository,
    InMemoryInputLogRepository, InputLogRepository, PostgresInputLogRepository,
    MembershipRepository, PostgresMembershipRepository,
    SectionRecord, SectionRepository, PostgresSectionRepository,
    HexOccupant, OccupationStatus, RedisSectorStateRepository, SectorState, SectorStateRepository,
    DeploymentTimer, RedisTimerRepository, TimerRepository, TimerType,
    CombatSession, RedisSessionStateRepository, SessionStateRepository,
    ConvoyDbRecord, ConvoyRepository, PostgresConvoyRepository,
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
        gcn_balance: 0,
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
    // Wallet [2u8;32] is shared with initialize_campaign_orchestrates_correctly.
    // Clear any orphaned membership rows so the post-test player_accounts delete succeeds.
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(&[2u8; 32].to_vec()).execute(&pool).await.ok();

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

#[tokio::test]
async fn player_campaign_membership_is_correct() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let membership_repo = PostgresMembershipRepository::new(pool.clone());

    let wallet_bytes = [7u8; 32].to_vec();
    let sector_id = uuid::Uuid::new_v4();

    // Step 1 — seed: player_accounts row + campaign_instances row.
    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
         VALUES ($1, 0, 0)
         ON CONFLICT (wallet_address) DO NOTHING"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await
    .expect("player_accounts seed failed");

    let campaign_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO campaign_instances (sector_id, map_seed, state)
         VALUES ($1, $2, 'Pending'::sector_state)
         RETURNING campaign_id"
    )
    .bind(sector_id)
    .bind(0xFEEDFACE_i64)
    .fetch_one(&pool)
    .await
    .expect("campaign_instances seed failed");

    // Step 2 — add_member with no spawn hex.
    let membership = membership_repo
        .add_member(&wallet_bytes, campaign_id, None, None)
        .await
        .expect("add_member failed");
    assert!(!membership.membership_id.is_nil(), "membership_id must not be nil");
    assert!(membership.is_active, "is_active must be true on creation");
    assert!(membership.gateway_hex_q.is_none(), "gateway_hex_q must be None before assignment");
    assert!(membership.gateway_hex_r.is_none(), "gateway_hex_r must be None before assignment");

    // Step 3 — get_membership: fetch by (wallet, campaign_id).
    let fetched = membership_repo
        .get_membership(&wallet_bytes, campaign_id)
        .await
        .expect("get_membership error")
        .expect("membership not found");
    assert_eq!(fetched.membership_id, membership.membership_id);
    assert_eq!(fetched.campaign_id, campaign_id);
    assert_eq!(fetched.wallet_address, wallet_bytes);
    assert!(fetched.is_active);

    // Step 4 — assign_gateway_hex.
    membership_repo
        .assign_gateway_hex(&wallet_bytes, campaign_id, 3, -2)
        .await
        .expect("assign_gateway_hex failed");
    let after_hex = membership_repo
        .get_membership(&wallet_bytes, campaign_id)
        .await
        .expect("get_membership error")
        .expect("membership not found after hex assignment");
    assert_eq!(after_hex.gateway_hex_q, Some(3), "gateway_hex_q must be 3");
    assert_eq!(after_hex.gateway_hex_r, Some(-2), "gateway_hex_r must be -2");

    // Step 5 — duplicate insert must be rejected by uq_player_campaign.
    let dup = membership_repo
        .add_member(&wallet_bytes, campaign_id, None, None)
        .await;
    assert!(dup.is_err(), "duplicate (wallet, campaign_id) must be rejected");

    // Step 6 — list_members_by_campaign: exactly one row for this campaign.
    let members = membership_repo
        .list_members_by_campaign(campaign_id)
        .await
        .expect("list_members_by_campaign failed");
    assert_eq!(members.len(), 1, "expected exactly one member for this campaign");
    assert_eq!(members[0].membership_id, membership.membership_id);

    // Step 7 — cascade delete: deleting the campaign row must cascade to membership.
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("cascade delete of campaign_instances failed");
    let after_cascade = membership_repo
        .get_membership(&wallet_bytes, campaign_id)
        .await
        .expect("get_membership error after cascade");
    assert!(after_cascade.is_none(), "membership must be None after campaign cascade delete");

    // Teardown: player_accounts (campaign already gone, no restrict blocker).
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("post-test player_accounts cleanup failed");
}

#[tokio::test]
async fn gcn_ledger_is_append_only_and_queryable() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresAccountRepository::new(pool.clone());
    let wallet = WalletAddress([3u8; 32]);
    let wallet_bytes = wallet.0.to_vec();

    // Pre-test cleanup.
    sqlx::query("DELETE FROM gcn_transaction_ledger WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("pre-test ledger cleanup failed");
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("pre-test player_accounts cleanup failed");

    // Step 1: seed a fresh player_accounts row.
    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance) VALUES ($1, 0, 0)"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await
    .expect("player_accounts seed failed");

    // Step 2: append entry A (credit 500).
    repo.append_gcn_entry(
        &wallet,
        GcnLedgerEntry {
            entry_id: uuid::Uuid::nil(),
            wallet,
            delta: 500,
            balance_after: 0,
            entry_type: "founding_courtesy".to_string(),
            session_id: None,
            memo: Some("Founding Courtesy He3 grant".to_string()),
            recorded_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("append entry A failed");

    // Step 2: append entry B (debit 50).
    repo.append_gcn_entry(
        &wallet,
        GcnLedgerEntry {
            entry_id: uuid::Uuid::nil(),
            wallet,
            delta: -50,
            balance_after: 0,
            entry_type: "mission_payout".to_string(),
            session_id: None,
            memo: None,
            recorded_at: chrono::Utc::now(),
        },
    )
    .await
    .expect("append entry B failed");

    // Step 3: assert gcn_balance is 450.
    let balance: i64 = sqlx::query_scalar(
        "SELECT gcn_balance FROM player_accounts WHERE wallet_address = $1"
    )
    .bind(&wallet_bytes)
    .fetch_one(&pool)
    .await
    .expect("gcn_balance query failed");
    assert_eq!(balance, 450, "gcn_balance must be 450 after +500 and -50");

    // Step 4: get_gcn_ledger — two entries, correct deltas, correct balance_after, A before B.
    let ledger = repo.get_gcn_ledger(&wallet).await.expect("get_gcn_ledger failed");
    assert_eq!(ledger.len(), 2, "expected exactly two ledger entries");
    let entry_a = &ledger[0];
    let entry_b = &ledger[1];
    assert_eq!(entry_a.delta, 500);
    assert_eq!(entry_a.balance_after, 500);
    assert_eq!(entry_a.entry_type, "founding_courtesy");
    assert_eq!(entry_a.memo, Some("Founding Courtesy He3 grant".to_string()));
    assert_eq!(entry_b.delta, -50);
    assert_eq!(entry_b.balance_after, 450);
    assert_eq!(entry_b.entry_type, "mission_payout");
    assert!(entry_b.memo.is_none());
    assert!(
        entry_a.recorded_at <= entry_b.recorded_at,
        "entry A must precede entry B in recorded_at order"
    );

    // Step 5: UPDATE must be blocked by the immutability trigger.
    let tamper = sqlx::query(
        "UPDATE gcn_transaction_ledger SET memo = 'TAMPERED' WHERE wallet_address = $1"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await;
    assert!(tamper.is_err(), "UPDATE must be blocked by the immutability trigger");

    // Step 6: DELETE must be blocked by the immutability trigger.
    let delete = sqlx::query(
        "DELETE FROM gcn_transaction_ledger WHERE wallet_address = $1"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await;
    assert!(delete.is_err(), "DELETE must be blocked by the immutability trigger");

    // Post-test cleanup: delete via truncate-style raw SQL bypassing triggers is not available;
    // triggers block DELETE, so we use a superuser-level workaround: disable triggers for cleanup.
    sqlx::query("ALTER TABLE gcn_transaction_ledger DISABLE TRIGGER gcn_ledger_no_delete")
        .execute(&pool)
        .await
        .expect("disable trigger for cleanup failed");
    sqlx::query("DELETE FROM gcn_transaction_ledger WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("post-test ledger cleanup failed");
    sqlx::query("ALTER TABLE gcn_transaction_ledger ENABLE TRIGGER gcn_ledger_no_delete")
        .execute(&pool)
        .await
        .expect("re-enable trigger failed");
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes)
        .execute(&pool)
        .await
        .expect("post-test player_accounts cleanup failed");
}

#[tokio::test]
async fn redis_ping_succeeds() {
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping Redis integration test");
            return;
        }
    };

    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mut mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    let pong: String = redis::cmd("PING")
        .query_async(&mut mgr)
        .await
        .expect("PING command failed");

    assert_eq!(pong, "PONG");
}

#[tokio::test]
async fn sector_state_repository_redis_is_correct() {
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping Redis sector integration test");
            return;
        }
    };

    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");
    let repo = RedisSectorStateRepository::new(mgr.clone());

    let sector_id  = uuid::Uuid::new_v4();
    let campaign_id = uuid::Uuid::new_v4();

    // 1. Upsert a sector; verify it appears in list_sectors (via sectors:all Set).
    let state = SectorState {
        sector_id,
        campaign_id,
        occupation_status: OccupationStatus::Neutral,
        owner: None,
        deployed_unit_count: 3,
        active_timer_ids: vec![],
        terrain: std::collections::HashMap::new(),
        magma_veins: vec![],
        hex_occupancy: std::collections::HashMap::new(),
    };
    repo.upsert_sector(state.clone()).await.expect("upsert_sector failed");

    let all = repo.list_sectors().await.expect("list_sectors failed");
    assert!(all.iter().any(|s| s.sector_id == sector_id), "upserted sector must appear in list_sectors");

    // 2. Round-trip get_sector.
    let fetched = repo.get_sector(sector_id).await.expect("get_sector failed").expect("get_sector returned None");
    assert_eq!(fetched.sector_id, sector_id);
    assert_eq!(fetched.campaign_id, campaign_id);
    assert_eq!(fetched.deployed_unit_count, 3);

    // 3. Player presence round-trip.
    let wallet_a = WalletAddress([5u8; 32]);
    let wallet_b = WalletAddress([6u8; 32]);
    let players = vec![wallet_a, wallet_b];
    repo.set_player_presence(sector_id, &players).await.expect("set_player_presence failed");
    let presence = repo.get_player_presence(sector_id).await.expect("get_player_presence failed");
    assert_eq!(presence.len(), 2);
    assert!(presence.contains(&wallet_a));
    assert!(presence.contains(&wallet_b));

    // 4. Clean up test keys so the test is idempotent.
    use redis::AsyncCommands;
    let mut conn = mgr.clone();
    conn.del::<_, ()>(vec![
        format!("sector:{}:hex_state", sector_id),
        format!("sector:{}:player_presence", sector_id),
    ])
    .await
    .expect("DEL cleanup failed");
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string())
        .await
        .expect("SREM cleanup failed");
}

#[tokio::test]
async fn convoy_record_crud() {
    let Some(pool) = test_pool().await else {
        eprintln!("TEST_DATABASE_URL not set — skipping convoy_record_crud");
        return;
    };

    let repo = PostgresConvoyRepository::new(pool.clone());
    let convoy_id = uuid::Uuid::new_v4();

    let record = ConvoyDbRecord {
        convoy_id,
        sector_id:     uuid::Uuid::new_v4(),
        owner_wallet:  vec![7u8; 32],
        origin_q:      3,
        origin_r:      -2,
        destination_q: 7,
        destination_r: 1,
        route:         serde_json::json!([{"q": 3, "r": -2}, {"q": 7, "r": 1}]),
        arrival_time:  chrono::Utc::now() + chrono::Duration::minutes(40),
        vehicle_class: "BehemothMegaTruck".to_string(),
        cargo:         serde_json::json!({"magma_ore": 100}),
        in_transit:    true,
        created_at:    chrono::Utc::now(),
    };

    // create
    let returned_id = repo.create_convoy(record.clone()).await.expect("create_convoy failed");
    assert_eq!(returned_id, convoy_id);

    // get — verify initial state
    let fetched = repo.get_convoy(convoy_id).await.expect("get_convoy failed").expect("convoy not found");
    assert!(fetched.in_transit);
    assert_eq!(fetched.vehicle_class, "BehemothMegaTruck");
    assert_eq!(fetched.owner_wallet, vec![7u8; 32]);

    // mark_arrived
    repo.mark_arrived(convoy_id).await.expect("mark_arrived failed");

    // get — verify in_transit flipped
    let fetched2 = repo.get_convoy(convoy_id).await.expect("get_convoy failed").expect("convoy not found after mark_arrived");
    assert!(!fetched2.in_transit, "in_transit must be false after mark_arrived");

    // cleanup
    sqlx::query!("DELETE FROM convoy_records WHERE convoy_id = $1", convoy_id)
        .execute(&pool)
        .await
        .expect("cleanup DELETE failed");
}

#[tokio::test]
async fn timer_repository_redis_is_correct() {
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping Redis timer integration test");
            return;
        }
    };

    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");
    let repo = RedisTimerRepository::new(mgr.clone());

    let sector_id  = uuid::Uuid::new_v4();
    let timer_a_id = uuid::Uuid::new_v4();
    let timer_b_id = uuid::Uuid::new_v4();
    let wallet     = WalletAddress([9u8; 32]);

    let now = chrono::Utc::now();

    let timer_a = DeploymentTimer {
        timer_id:     timer_a_id,
        player_wallet: wallet,
        sector_id,
        timer_type:   TimerType::ConvoyArrival,
        fires_at:     now - chrono::Duration::seconds(60),
    };
    let timer_b = DeploymentTimer {
        timer_id:     timer_b_id,
        player_wallet: wallet,
        sector_id,
        timer_type:   TimerType::DeploymentExpiry,
        fires_at:     now + chrono::Duration::seconds(3600),
    };

    // 1. Schedule both timers.
    repo.schedule_timer(timer_a.clone()).await.expect("schedule timer_a failed");
    repo.schedule_timer(timer_b.clone()).await.expect("schedule timer_b failed");

    // 2. get_due_timers(now) — only the past timer should be returned.
    let due = repo.get_due_timers(now).await.expect("get_due_timers failed");
    assert_eq!(due.len(), 1, "expected exactly one due timer");
    assert_eq!(due[0].timer_id, timer_a_id, "due timer must be timer_a");

    // 3. Verify payloads round-trip: get_due_timers returns the full struct.
    assert_eq!(due[0].sector_id, sector_id);
    assert!(matches!(due[0].timer_type, TimerType::ConvoyArrival));

    // 4. Cancel the future timer.
    repo.cancel_timer(timer_b_id).await.expect("cancel timer_b failed");

    // 5. After cancel, scanning far-future window still returns only timer_a.
    let far_future = now + chrono::Duration::seconds(7200);
    let after_cancel = repo.get_due_timers(far_future).await.expect("get_due_timers (far_future) failed");
    assert_eq!(after_cancel.len(), 1, "only timer_a must remain after timer_b is cancelled");
    assert_eq!(after_cancel[0].timer_id, timer_a_id);

    // 6. Teardown — remove all test keys so the test is idempotent.
    use redis::AsyncCommands;
    let mut conn = mgr.clone();
    conn.del::<_, ()>(vec![
        format!("timer:{}", timer_a_id),
        format!("timer:{}:sector_id", timer_a_id),
        format!("sector:{}:timers", sector_id),
    ])
    .await
    .expect("DEL cleanup (timer_a keys) failed");
    conn.zrem::<_, _, ()>("timers:global", timer_a_id.to_string())
        .await
        .expect("ZREM timers:global cleanup failed");
}

#[tokio::test]
async fn cancel_timer_removes_from_both_sorted_sets() {
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping cancel_timer sorted-set test");
            return;
        }
    };

    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");
    let repo = RedisTimerRepository::new(mgr.clone());

    let sector_id = uuid::Uuid::new_v4();
    let timer_id  = uuid::Uuid::new_v4();
    let wallet    = WalletAddress([7u8; 32]);

    let timer = DeploymentTimer {
        timer_id,
        player_wallet: wallet,
        sector_id,
        timer_type: TimerType::DeploymentExpiry,
        fires_at: chrono::Utc::now() - chrono::Duration::seconds(1),
    };

    repo.schedule_timer(timer).await.expect("schedule_timer failed");

    // Verify it appears before cancel.
    let before = repo.get_due_timers(chrono::Utc::now() + chrono::Duration::seconds(60))
        .await
        .expect("get_due_timers before cancel failed");
    assert!(before.iter().any(|t| t.timer_id == timer_id), "timer must appear before cancel");

    repo.cancel_timer(timer_id).await.expect("cancel_timer failed");

    // After cancel, a far-future scan must not return the cancelled timer.
    let after = repo.get_due_timers(chrono::Utc::now() + chrono::Duration::seconds(7200))
        .await
        .expect("get_due_timers after cancel failed");
    assert!(
        !after.iter().any(|t| t.timer_id == timer_id),
        "cancelled timer must not appear in get_due_timers"
    );

    // Verify both sorted sets no longer contain the timer_id.
    use redis::AsyncCommands;
    let mut conn = mgr.clone();
    let global_score: Option<f64> = conn
        .zscore("timers:global", timer_id.to_string())
        .await
        .expect("ZSCORE timers:global failed");
    assert!(global_score.is_none(), "timer must be removed from timers:global");

    let sector_score: Option<f64> = conn
        .zscore(format!("sector:{}:timers", sector_id), timer_id.to_string())
        .await
        .expect("ZSCORE sector timers failed");
    assert!(sector_score.is_none(), "timer must be removed from sector:{{id}}:timers");

    // Teardown — keys should already be deleted by cancel_timer, but clean up the sector key.
    conn.del::<_, ()>(format!("sector:{}:timers", sector_id))
        .await
        .expect("DEL sector timers cleanup failed");
}

#[tokio::test]
async fn session_state_repository_redis_is_correct() {
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping Redis session integration test");
            return;
        }
    };

    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");
    let repo = RedisSessionStateRepository::new(mgr.clone(), 300);

    let session_id = uuid::Uuid::new_v4();

    // 1. Build a CombatSession with a minimal CombatResolveRequest.
    use sim_engine::types::{ArmorTag, CombatWeapon, Section, Vehicle, WeaponTag};
    let section = Section {
        id:               "test_section".into(),
        name:             "Test Rifles".into(),
        max_strength:     4,
        current_strength: 4,
        individual_hp:    10,
        accuracy:         60,
        evasion:          10,
        weapon: CombatWeapon {
            name:     "Rifle".into(),
            ap:       2,
            base_damage: 5,
            tag:      WeaponTag::Slug,
            accuracy: 0,
        },
        armor_at:  0,
        armor_tag: ArmorTag::Unarmored,
    };
    let vehicle = Vehicle {
        id:       "test_vehicle".into(),
        name:     "Test Truck".into(),
        hp:       50,
        max_hp:   50,
        at:       2,
        armor_tag: ArmorTag::LightArmor,
        evasion:  5,
        weapons:  vec![],
    };
    let req = mercs_server::api_types::CombatResolveRequest {
        section,
        vehicle,
        max_ticks:                  Some(10),
        seed_override:              Some(42),
        combat_initiation_type:     None,
        defending_convoy_vehicles:  None,
        commander:                  None,
    };
    let session = CombatSession {
        params:     req,
        created_at: chrono::Utc::now(),
    };

    // 2. save_session — assert Ok.
    repo.save_session(session_id, session).await.expect("save_session failed");

    // 3. get_session — assert Some; assert created_at is populated.
    let fetched = repo.get_session(session_id).await
        .expect("get_session error")
        .expect("get_session returned None");
    assert!(fetched.created_at.timestamp() > 0, "created_at must be a real timestamp");

    // 4. consume_session — assert Some (session is returned and removed).
    let consumed = repo.consume_session(session_id).await
        .expect("consume_session error")
        .expect("consume_session returned None");
    assert_eq!(consumed.params.seed_override, Some(42));

    // 5. get_session — assert None (gone after consume).
    let after_consume = repo.get_session(session_id).await
        .expect("get_session error after consume");
    assert!(after_consume.is_none(), "session must be None after consume");

    // 6. consume_session again — assert None (idempotent, not an error).
    let double_consume = repo.consume_session(session_id).await
        .expect("second consume_session must not error");
    assert!(double_consume.is_none(), "second consume must return None");
}

#[tokio::test]
async fn commander_generate_and_persist() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let commander_repo = PostgresCommanderRepository::new(pool.clone());
    let campaign_repo  = PostgresCampaignRepository::new(pool.clone());

    // 1. Seed campaign (FK prerequisite).
    let campaign = campaign_repo.create_campaign(&NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  0xABCD1234_i64,
    }).await.expect("create campaign failed");
    let campaign_id = campaign.campaign_id;

    // Wallet [8u8;32] — unique to this test to avoid FK conflicts with parallel test setup.
    let wallet_bytes = [8u8; 32].to_vec();

    // Pre-test cleanup.
    sqlx::query("DELETE FROM commander_records WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();

    // Seed player_accounts FK.
    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
         VALUES ($1, 0, 0) ON CONFLICT (wallet_address) DO NOTHING"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await
    .expect("player_accounts FK seed failed");

    // 2. Generate a commander deterministically.
    let record = mercs_server::commander_gen::generate_commander(
        wallet_bytes.clone(),
        campaign_id,
        42u32,
    );

    // 3. Assert generated fields before persisting.
    assert!(!record.name.is_empty(), "name must be non-empty");
    assert_eq!(record.rank, 1);
    assert_eq!(record.xp, 0);
    assert_eq!(record.stress, 0);
    assert!(!record.is_shattered);
    assert!(!record.is_kia);
    assert!(!record.is_nft);
    assert!(record.veteran_trait.is_none());
    assert_eq!(record.prng_seed_state, 42i64);
    assert_eq!(record.campaign_id, campaign_id);

    let canonical_origins = [
        "Human (Corporate)", "Human (Underhive)",
        "Raccoon (Mod-Moped)", "Raccoon (Rocker-Chopper)", "Hamster Attachment",
    ];
    assert!(
        canonical_origins.contains(&record.origin.as_str()),
        "origin '{}' not canonical", record.origin
    );

    // 4. Persist.
    let commander_id = record.commander_id;
    let expected_name = record.name.clone();
    commander_repo.create_commander(record).await.expect("create_commander failed");

    // 5. Round-trip: get back and assert fidelity.
    let fetched = commander_repo.get_commander(commander_id).await
        .expect("get_commander error")
        .expect("commander not found after create");
    assert_eq!(fetched.name, expected_name);
    assert_eq!(fetched.prng_seed_state, 42i64);
    assert_eq!(fetched.rank, 1);
    assert_eq!(fetched.campaign_id, campaign_id);

    // Cleanup.
    sqlx::query!("DELETE FROM commander_records WHERE commander_id = $1", commander_id)
        .execute(&pool).await.ok();
}

#[tokio::test]
async fn input_log_is_append_only_and_queryable() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresInputLogRepository::new(pool.clone());
    let session_id = uuid::Uuid::new_v4();

    // Pre-test cleanup in case a previous run left rows.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for pre-test cleanup failed");
    sqlx::query("DELETE FROM input_logs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("pre-test input_logs cleanup failed");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after pre-test cleanup failed");
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("pre-test session_configs cleanup failed");

    // Step 1: save a SessionConfig; assert Ok.
    let config = shared::SessionConfig {
        session_id:    session_id.to_string(),
        build_version: "0.1.0".into(),
        seed:          0xDEADBEEF_CAFEBABE_u64,
        sector_id:     uuid::Uuid::new_v4(),
        campaign_id:   uuid::Uuid::new_v4(),
        sector_tier:   "Contested".into(),
        ruleset:       "standard_v1".into(),
    };
    repo.save_session_config(&config).await.expect("save_session_config failed");

    // Step 2: get_session_config — round-trips correctly, seed survives u64 → i64 → u64.
    let fetched_config = repo.get_session_config(&session_id).await
        .expect("get_session_config error")
        .expect("get_session_config returned None");
    assert_eq!(fetched_config.session_id, config.session_id);
    assert_eq!(fetched_config.seed, config.seed, "seed must round-trip u64→i64→u64 without loss");
    assert_eq!(fetched_config.sector_id, config.sector_id);

    // Step 3: append three entries with distinct tick/seq.
    let entries = vec![
        shared::InputLogEntry {
            tick: 0, seq: 0,
            event_type: "session_start".into(),
            player_id: None,
            payload: serde_json::json!({"note": "entry A"}),
            narrative_event: None,
        },
        shared::InputLogEntry {
            tick: 1, seq: 0,
            event_type: "player_action".into(),
            player_id: Some("wallet_abc".into()),
            payload: serde_json::json!({"action": "move"}),
            narrative_event: Some("Unit moved north.".into()),
        },
        shared::InputLogEntry {
            tick: 2, seq: 0,
            event_type: "combat_end".into(),
            player_id: None,
            payload: serde_json::json!({"outcome": "victory"}),
            narrative_event: None,
        },
    ];
    for entry in &entries {
        repo.append_entry(&session_id, entry).await.expect("append_entry failed");
    }

    // Step 4: get_entries_by_session — 3 entries, ordered (tick, seq) ASC.
    let retrieved = repo.get_entries_by_session(&session_id).await
        .expect("get_entries_by_session failed");
    assert_eq!(retrieved.len(), 3, "expected exactly 3 entries");
    assert_eq!(retrieved[0].tick, 0);
    assert_eq!(retrieved[0].event_type, "session_start");
    assert_eq!(retrieved[1].tick, 1);
    assert_eq!(retrieved[1].player_id, Some("wallet_abc".into()));
    assert_eq!(retrieved[1].narrative_event, Some("Unit moved north.".into()));
    assert_eq!(retrieved[2].tick, 2);
    assert_eq!(retrieved[2].event_type, "combat_end");

    // Step 5: UPDATE must be blocked by the immutability trigger.
    let tamper = sqlx::query(
        "UPDATE input_logs SET event_type = 'TAMPERED' WHERE session_id = $1"
    )
    .bind(session_id)
    .execute(&pool)
    .await;
    assert!(tamper.is_err(), "UPDATE must be blocked by the immutability trigger");

    // Step 6: DELETE must be blocked by the immutability trigger.
    let delete_attempt = sqlx::query(
        "DELETE FROM input_logs WHERE session_id = $1"
    )
    .bind(session_id)
    .execute(&pool)
    .await;
    assert!(delete_attempt.is_err(), "DELETE must be blocked by the immutability trigger");

    // Post-test cleanup: disable triggers to allow deletion.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup failed");
    sqlx::query("DELETE FROM input_logs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("post-test input_logs cleanup failed");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup failed");
    sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable session_configs triggers for cleanup failed");
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("post-test session_configs cleanup failed");
    sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable session_configs triggers after cleanup failed");
}

#[tokio::test]
async fn sector_lifecycle_task_transitions_active_to_ending() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresCampaignRepository::new(pool.clone());

    // Insert a campaign, set it Active, then backdate ends_at by 1 hour.
    let params = NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  0xDEAD_CAFE_i64,
    };
    let campaign = repo.create_campaign(&params).await.expect("create failed");
    repo.update_sector_state(campaign.campaign_id, CampaignLifecycle::Active).await
        .expect("transition to Active failed");
    sqlx::query("UPDATE campaign_instances SET ends_at = $1 WHERE campaign_id = $2")
        .bind(chrono::Utc::now() - chrono::Duration::hours(1))
        .bind(campaign.campaign_id)
        .execute(&pool)
        .await
        .expect("ends_at backdate failed");

    // Run one poll cycle — should detect expired ends_at and transition to Ending.
    mercs_server::lifecycle::poll_lifecycle_once(&repo).await;

    let updated = repo.get_campaign(campaign.campaign_id).await
        .expect("get failed")
        .expect("not found");
    assert_eq!(
        updated.state,
        CampaignLifecycle::Ending,
        "Active campaign with expired ends_at must transition to Ending after poll"
    );

    // Post-test cleanup.
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign.campaign_id)
        .execute(&pool)
        .await
        .expect("post-test cleanup failed");
}

#[tokio::test]
async fn activate_campaign_is_idempotent() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresCampaignRepository::new(pool.clone());

    // 1. Create a Pending campaign.
    let params = NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  0xC0FFEE_i64,
    };
    let campaign = repo.create_campaign(&params).await.expect("create failed");
    let campaign_id = campaign.campaign_id;

    // 2. Build victory_tickers with all 10 variants at 0.0.
    let mut ticker_map = serde_json::Map::new();
    for variant in [
        VictoryTickerType::MilitaryDominance,
        VictoryTickerType::OneWorldGunvernment,
        VictoryTickerType::SqueakingProphets,
        VictoryTickerType::VoidCallers,
        VictoryTickerType::JumpLaneRestorers,
        VictoryTickerType::SingularitySeekers,
        VictoryTickerType::CapitalistDomination,
        VictoryTickerType::GrandSyndicate,
        VictoryTickerType::EmperorBobMovement,
        VictoryTickerType::TrashKhansHorde,
    ] {
        ticker_map.insert(variant.as_json_key().to_string(), serde_json::Value::from(0.0_f64));
    }
    let tickers = serde_json::Value::Object(ticker_map);

    // 3. First call — happy path: Pending → Active.
    let ends_at = chrono::Utc::now() + chrono::Duration::days(90);
    let activated = repo.activate_campaign(campaign_id, ends_at, tickers.clone()).await
        .expect("activate_campaign (first call) failed");
    assert!(activated, "first activate_campaign call must return true (1 row updated)");

    let after_first = repo.get_campaign(campaign_id).await
        .expect("get_campaign error")
        .expect("campaign not found after first activate");
    assert_eq!(after_first.state, CampaignLifecycle::Active, "state must be Active after first call");
    assert!(after_first.ends_at.is_some(), "ends_at must be set after first call");
    let ticker_obj = after_first.victory_tickers.as_object().expect("victory_tickers must be an object");
    assert_eq!(ticker_obj.len(), 10, "all 10 tickers must be present");
    for val in ticker_obj.values() {
        assert!(
            (val.as_f64().expect("ticker value must be f64") - 0.0_f64).abs() < f64::EPSILON,
            "all ticker values must be 0.0"
        );
    }

    // 4. Second call — idempotency: Active campaign is not overwritten.
    let different_ends_at = chrono::Utc::now() + chrono::Duration::days(1);
    let activated_again = repo.activate_campaign(campaign_id, different_ends_at, tickers).await
        .expect("activate_campaign (second call) must not error");
    assert!(!activated_again, "second activate_campaign call must return false (0 rows updated — already Active)");

    let after_second = repo.get_campaign(campaign_id).await
        .expect("get_campaign error")
        .expect("campaign not found after second activate");
    assert_eq!(after_second.state, CampaignLifecycle::Active, "state must still be Active");
    // ends_at must still reflect the ~90-day value, not the ~1-day value from the second call.
    assert!(
        after_second.ends_at.unwrap() > chrono::Utc::now() + chrono::Duration::days(80),
        "ends_at must not be overwritten by the second activate call"
    );

    // 5. Post-test cleanup.
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("post-test cleanup failed");
}

#[tokio::test]
async fn campaign_scoped_log_entry_roundtrips() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresInputLogRepository::new(pool.clone());
    let campaign_id = uuid::Uuid::new_v4();

    // Step 1: append a campaign-scoped entry (no real campaign_instances row needed —
    // input_logs.campaign_id has no FK constraint).
    let entry = shared::InputLogEntry {
        tick: 1,
        seq: 0,
        event_type: "campaign_started".into(),
        player_id: None,
        payload: serde_json::json!({}),
        narrative_event: None,
    };
    repo.append_campaign_entry(&campaign_id, &entry)
        .await
        .expect("append_campaign_entry failed");

    // Step 2: read back via direct query.
    let row = sqlx::query!(
        r#"SELECT session_id, campaign_id, event_type
           FROM input_logs
           WHERE campaign_id = $1"#,
        campaign_id,
    )
    .fetch_one(&pool)
    .await
    .expect("fetch row failed");

    // Step 3: assert fields.
    assert!(row.session_id.is_none(), "session_id must be NULL for a campaign-scoped entry");
    assert_eq!(row.campaign_id, Some(campaign_id), "campaign_id must match");
    assert_eq!(row.event_type, "campaign_started");

    // Step 4: immutability trigger must reject UPDATE.
    let tamper = sqlx::query(
        "UPDATE input_logs SET event_type = 'tampered' WHERE campaign_id = $1"
    )
    .bind(campaign_id)
    .execute(&pool)
    .await;
    assert!(tamper.is_err(), "UPDATE must be blocked by the immutability trigger");

    // Post-test cleanup: disable triggers to bypass immutability.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup failed");
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("post-test input_logs cleanup failed");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup failed");
}

#[tokio::test]
async fn sha256_integrity_matches_db_reconstruction() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresInputLogRepository::new(pool.clone());
    let session_id = uuid::Uuid::new_v4();

    // Pre-test cleanup: session_configs has no immutability trigger.
    // Fresh UUID means no prior input_logs rows — no trigger manipulation needed.
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("pre-test session_configs cleanup failed");

    let config = shared::SessionConfig {
        session_id:    session_id.to_string(),
        build_version: "0.1.0".into(),
        seed:          0xABCD_1234_5678_EF90_u64,
        sector_id:     uuid::Uuid::new_v4(),
        campaign_id:   uuid::Uuid::new_v4(),
        sector_tier:   "Hostile".into(),
        ruleset:       "standard_v1".into(),
    };
    repo.save_session_config(&config).await.expect("save_session_config failed");

    // Append entries in non-sequential order to exercise the ordering guarantee.
    let entry_tick2 = shared::InputLogEntry {
        tick: 2, seq: 0,
        event_type: "combat_end".into(),
        player_id: None,
        payload: serde_json::json!({"outcome": "victory"}),
        narrative_event: None,
    };
    let entry_tick1_seq0 = shared::InputLogEntry {
        tick: 1, seq: 0,
        event_type: "player_action".into(),
        player_id: Some("wallet_integrity".into()),
        payload: serde_json::json!({"action": "fire"}),
        narrative_event: Some("Unit fired at target.".into()),
    };
    let entry_tick1_seq1 = shared::InputLogEntry {
        tick: 1, seq: 1,
        event_type: "server_decision".into(),
        player_id: None,
        payload: serde_json::json!({"roll": 42}),
        narrative_event: None,
    };

    // Append in deliberately scrambled order.
    repo.append_entry(&session_id, &entry_tick2).await.expect("append tick=2 failed");
    repo.append_entry(&session_id, &entry_tick1_seq0).await.expect("append tick=1 seq=0 failed");
    repo.append_entry(&session_id, &entry_tick1_seq1).await.expect("append tick=1 seq=1 failed");

    // Compute expected hash locally: config header + entries sorted (tick ASC, seq ASC).
    let ordered = [&entry_tick1_seq0, &entry_tick1_seq1, &entry_tick2];
    let mut expected_buf = Vec::new();
    let header_line = serde_json::to_string(&config).expect("serialize config failed");
    expected_buf.extend_from_slice(header_line.as_bytes());
    expected_buf.extend_from_slice(b"\n");
    for entry in &ordered {
        let line = serde_json::to_string(entry).expect("serialize entry failed");
        expected_buf.extend_from_slice(line.as_bytes());
        expected_buf.extend_from_slice(b"\n");
    }
    let mut hasher = Sha256::new();
    hasher.update(&expected_buf);
    let expected_hash = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>();

    // Compute via DB reconstruction path.
    let integrity = mercs_server::integrity::compute_session_integrity(&repo, session_id)
        .await
        .expect("compute_session_integrity failed");

    assert_eq!(
        integrity.log_hash, expected_hash,
        "DB-reconstructed hash must match locally computed hash"
    );
    assert_eq!(integrity.session_id, config.session_id);
    assert_eq!(integrity.seed, config.seed);
    assert_eq!(integrity.build_version, config.build_version);

    // Post-test cleanup.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup failed");
    sqlx::query("DELETE FROM input_logs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("post-test input_logs cleanup failed");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup failed");
    sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable session_configs triggers for cleanup failed");
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("post-test session_configs cleanup failed");
    sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable session_configs triggers after cleanup failed");
}

#[tokio::test]
async fn initialize_campaign_orchestrates_correctly() {
    let db_url = match std::env::var("TEST_DATABASE_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping initialize_campaign integration test");
            return;
        }
    };
    let redis_url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping initialize_campaign integration test");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("Failed to connect to test Postgres");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Migrations failed");

    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let redis_mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    let campaign_repo   = PostgresCampaignRepository::new(pool.clone());
    let membership_repo = PostgresMembershipRepository::new(pool.clone());
    let sector_repo     = RedisSectorStateRepository::new(redis_mgr.clone());
    let input_log_repo  = PostgresInputLogRepository::new(pool.clone());

    // Pre-test: clear membership rows left by any prior panicked run.
    // wallet_address has ON DELETE RESTRICT, so memberships must be removed before
    // player_accounts can be deleted. The cascade on campaign_id only covers the
    // campaign created in THIS run — orphaned rows from prior crashes must be wiped by wallet.
    let test_wallets: Vec<Vec<u8>> = (1u8..=3).map(|i| vec![i; 32]).collect();
    for w in &test_wallets {
        sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
            .bind(w).execute(&pool).await.expect("pre-test membership cleanup");
    }
    for w in &test_wallets {
        sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
            .bind(w).execute(&pool).await.ok();
    }

    let sector_id = uuid::Uuid::new_v4();
    let campaign = campaign_repo.create_campaign(&NewCampaignInstance {
        sector_id,
        map_seed: 12345_i64,
    }).await.expect("create_campaign failed");
    let campaign_id = campaign.campaign_id;

    let wallets: Vec<Vec<u8>> = (1u8..=3).map(|i| vec![i; 32]).collect();
    for w in &wallets {
        sqlx::query(
            "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
             VALUES ($1, 0, 0)
             ON CONFLICT (wallet_address) DO NOTHING"
        )
        .bind(w)
        .execute(&pool)
        .await
        .expect("player_accounts FK seed failed");
    }
    for w in &wallets {
        membership_repo.add_member(w, campaign_id, None, None)
            .await
            .expect("add_member failed");
    }

    initialize_campaign(
        campaign_id,
        &campaign_repo,
        &membership_repo,
        &sector_repo,
        &input_log_repo,
    ).await.expect("initialize_campaign returned Err");

    let updated = campaign_repo.get_campaign(campaign_id).await
        .expect("get_campaign error")
        .expect("campaign not found after initialize");
    assert_eq!(updated.state, CampaignLifecycle::Active, "state must be Active after initialize");
    let ends_at = updated.ends_at.expect("ends_at must be Some after initialize");
    assert!(
        ends_at > chrono::Utc::now() + chrono::Duration::days(80),
        "ends_at must be at least 80 days in the future"
    );

    let sector_state = sector_repo.get_sector(sector_id).await
        .expect("get_sector failed")
        .expect("get_sector returned None — upsert_sector must have written to Redis");
    assert!(
        !sector_state.terrain.is_empty(),
        "terrain must be populated in Redis after initialize"
    );

    let members = membership_repo.list_members_by_campaign(campaign_id).await
        .expect("list_members_by_campaign failed");
    assert_eq!(members.len(), 3, "expected 3 members");
    for m in &members {
        assert!(m.gateway_hex_q.is_some(), "gateway_hex_q must be Some after initialize");
        assert!(m.gateway_hex_r.is_some(), "gateway_hex_r must be Some after initialize");
    }

    let log_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM input_logs WHERE campaign_id = $1 AND event_type = 'campaign_started'"
    )
    .bind(campaign_id)
    .fetch_one(&pool)
    .await
    .expect("log count query failed");
    assert_eq!(log_count, 1, "input_logs must have exactly one campaign_started entry — retry must not produce duplicates");

    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup failed");
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("post-test input_logs cleanup failed");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup failed");

    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("post-test campaign cleanup failed");

    for w in &wallets {
        sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
            .bind(w)
            .execute(&pool)
            .await
            .expect("post-test player_accounts cleanup failed");
    }

    use redis::AsyncCommands;
    let mut conn = redis_mgr.clone();
    conn.del::<_, ()>(format!("sector:{}:hex_state", sector_id))
        .await
        .expect("DEL sector hex_state cleanup failed");
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string())
        .await
        .expect("SREM sectors:all cleanup failed");
}

#[tokio::test]
async fn session_config_is_immutable() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let session_id = uuid::Uuid::new_v4();

    // Step 1: INSERT a session_configs row.
    sqlx::query(
        "INSERT INTO session_configs (session_id, seed, build_version, sector_id, campaign_id, sector_tier, ruleset)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"
    )
    .bind(session_id)
    .bind(0xDEADBEEF_i64)
    .bind("0.1.0")
    .bind(uuid::Uuid::nil())
    .bind(uuid::Uuid::nil())
    .bind("Contested")
    .bind("standard_v1")
    .execute(&pool)
    .await
    .expect("session_configs INSERT failed");

    // Step 2: UPDATE must be blocked by the immutability trigger.
    let tamper = sqlx::query(
        "UPDATE session_configs SET seed = 999 WHERE session_id = $1"
    )
    .bind(session_id)
    .execute(&pool)
    .await;
    assert!(tamper.is_err(), "UPDATE must be blocked by the immutability trigger");

    // Step 3: DELETE must be blocked by the immutability trigger.
    let delete_attempt = sqlx::query(
        "DELETE FROM session_configs WHERE session_id = $1"
    )
    .bind(session_id)
    .execute(&pool)
    .await;
    assert!(delete_attempt.is_err(), "DELETE must be blocked by the immutability trigger");

    // Post-test cleanup: disable triggers to allow deletion.
    sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup failed");
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1")
        .bind(session_id).execute(&pool).await.expect("post-test session_configs cleanup failed");
    sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup failed");
}

/// Fail-closed admin auth: the launch endpoint must answer 401 whenever ADMIN_API_KEY
/// is absent or empty, no matter what the client sends. The old code skipped the check
/// entirely when the variable was unset, which made this an unauthenticated write
/// endpoint on a public host.
///
/// Canon: Phase_1_Section_2_Campaign_State_Machine.md, brick 2b-5 —
/// "401 if wrong; 401 if the var is unset — the check never skips."
///
/// All the unauthorized cases live in ONE test deliberately: they mutate the
/// process-global ADMIN_API_KEY, and separate `#[test]` fns would race under a parallel
/// runner. Needs neither Postgres nor Redis — the check returns before any repository
/// is touched, so the default in-memory AppState is enough.
#[tokio::test]
async fn admin_launch_endpoint_fails_closed() {
    use std::sync::Arc;
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;

    let config = Arc::new(mercs_server::config::Config::default());
    let state = Arc::new(mercs_server::state::AppState::new(
        std::path::PathBuf::from("/tmp"),
        config,
    ));
    let uri = format!("/api/admin/campaign/{}/launch", uuid::Uuid::new_v4());

    // Case 1: ADMIN_API_KEY unset, no header at all.
    std::env::remove_var("ADMIN_API_KEY");
    let req = Request::builder()
        .method("POST")
        .uri(&uri)
        .body(Body::empty())
        .unwrap();
    let resp = mercs_server::routes::router(Arc::clone(&state)).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(), StatusCode::UNAUTHORIZED,
        "unset ADMIN_API_KEY + no header must return 401"
    );

    // Case 2: ADMIN_API_KEY unset, client supplies a header anyway. This is the
    // regression guard — the old code let this straight through.
    let req = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("x-admin-key", "anything-at-all")
        .body(Body::empty())
        .unwrap();
    let resp = mercs_server::routes::router(Arc::clone(&state)).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(), StatusCode::UNAUTHORIZED,
        "unset ADMIN_API_KEY must never skip the check"
    );

    // Case 3: ADMIN_API_KEY set, wrong header.
    std::env::set_var("ADMIN_API_KEY", "test-only-not-a-real-admin-key");
    let req = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("x-admin-key", "wrong-key")
        .body(Body::empty())
        .unwrap();
    let resp = mercs_server::routes::router(Arc::clone(&state)).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(), StatusCode::UNAUTHORIZED,
        "wrong X-Admin-Key must return 401"
    );

    std::env::remove_var("ADMIN_API_KEY");
}

#[tokio::test]
async fn admin_launch_campaign_returns_200_then_409() {
    use std::sync::Arc;
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;

    let db_url = match std::env::var("TEST_DATABASE_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping admin launch integration test");
            return;
        }
    };
    let redis_url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping admin launch integration test");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("Failed to connect to test Postgres");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("Migrations failed");

    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let redis_mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    // Build an AppState backed by real repos for the router
    let config = Arc::new(mercs_server::config::Config::default());
    let mut state = mercs_server::state::AppState::new(
        std::path::PathBuf::from("/tmp"),
        config,
    );
    state.campaign_repo   = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.membership_repo = Arc::new(PostgresMembershipRepository::new(pool.clone()));
    state.sector_repo     = Arc::new(RedisSectorStateRepository::new(redis_mgr.clone()));
    state.input_log_repo  = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    let state = Arc::new(state);

    // Wallets 10–12 (unique to this test — no conflict with other integration tests)
    let wallets: Vec<Vec<u8>> = (10u8..=12).map(|i| vec![i; 32]).collect();

    // Pre-test cleanup in case a prior panicked run left rows
    for w in &wallets {
        sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
            .bind(w).execute(&pool).await.ok();
    }
    for w in &wallets {
        sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
            .bind(w).execute(&pool).await.ok();
    }

    // Setup: Pending campaign + 3 members (mirrors initialize_campaign_orchestrates_correctly)
    let sector_id = uuid::Uuid::new_v4();
    let campaign_repo   = PostgresCampaignRepository::new(pool.clone());
    let membership_repo = PostgresMembershipRepository::new(pool.clone());

    let campaign = campaign_repo.create_campaign(&NewCampaignInstance {
        sector_id,
        map_seed: 55555_i64,
    }).await.expect("create_campaign failed");
    let campaign_id = campaign.campaign_id;

    for w in &wallets {
        sqlx::query(
            "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
             VALUES ($1, 0, 0)
             ON CONFLICT (wallet_address) DO NOTHING"
        )
        .bind(w).execute(&pool).await.expect("player_accounts seed failed");
    }
    for w in &wallets {
        membership_repo.add_member(w, campaign_id, None, None)
            .await.expect("add_member failed");
    }

    // Admin auth fails closed, so every request below must carry the key.
    // Obviously-fake test value — the real key lives only on the box. See guard rail 3
    // of LINUX_HANDOFF_MERGE_PREP_2026-08-26 and .env.example.
    const TEST_ADMIN_KEY: &str = "test-only-not-a-real-admin-key";
    std::env::set_var("ADMIN_API_KEY", TEST_ADMIN_KEY);

    // First POST — expect 200
    let app = mercs_server::routes::router(Arc::clone(&state));
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/admin/campaign/{}/launch", campaign_id))
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "first launch must return 200");

    // Verify DB: campaign must now be Active with ends_at set
    let updated = campaign_repo.get_campaign(campaign_id).await
        .expect("get_campaign error")
        .expect("campaign not found after launch");
    assert_eq!(updated.state, CampaignLifecycle::Active, "state must be Active after launch");
    assert!(updated.ends_at.is_some(), "ends_at must be Some after launch");

    // Second POST — expect 409 (oneshot consumed app above; build a new one)
    let app2 = mercs_server::routes::router(Arc::clone(&state));
    let req2 = Request::builder()
        .method("POST")
        .uri(format!("/api/admin/campaign/{}/launch", campaign_id))
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp2 = app2.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::CONFLICT, "second launch must return 409");

    std::env::remove_var("ADMIN_API_KEY");

    // Cleanup
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup");
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("cleanup input_logs");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup");

    // CASCADE on campaign_id removes player_campaign_membership rows
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("cleanup campaign_instances");

    for w in &wallets {
        sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
            .bind(w).execute(&pool).await.expect("cleanup player_accounts");
    }

    use redis::AsyncCommands;
    let mut conn = redis_mgr.clone();
    conn.del::<_, ()>(format!("sector:{}:hex_state", sector_id))
        .await.expect("DEL sector hex_state cleanup");
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string())
        .await.expect("SREM sectors:all cleanup");
}

#[tokio::test]
async fn hex_occupancy_add_remove_roundtrips() {
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping hex occupancy integration test");
            return;
        }
    };

    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");
    let repo = RedisSectorStateRepository::new(mgr.clone());

    let sector_id  = uuid::Uuid::new_v4();
    let campaign_id = uuid::Uuid::new_v4();

    // Seed the sector with no occupants.
    let base_state = SectorState {
        sector_id,
        campaign_id,
        occupation_status: OccupationStatus::Neutral,
        owner: None,
        deployed_unit_count: 0,
        active_timer_ids: vec![],
        terrain: std::collections::HashMap::new(),
        magma_veins: vec![],
        hex_occupancy: std::collections::HashMap::new(),
    };
    repo.upsert_sector(base_state).await.expect("upsert_sector failed");

    let occ_a = HexOccupant { id: uuid::Uuid::new_v4(), owner_wallet: vec![1u8; 32], unit_type: "Tank".to_string() };
    let occ_b = HexOccupant { id: uuid::Uuid::new_v4(), owner_wallet: vec![2u8; 32], unit_type: "Scout".to_string() };
    let id_a = occ_a.id;
    let id_b = occ_b.id;

    // Add both occupants to hex (2, 3).
    repo.add_hex_occupant(sector_id, 2, 3, occ_a).await.expect("add occ_a at (2,3) failed");
    repo.add_hex_occupant(sector_id, 2, 3, occ_b).await.expect("add occ_b at (2,3) failed");

    // Add occ_a (by id_a clone) to a different hex (5, -1).
    let occ_a2 = HexOccupant { id: id_a, owner_wallet: vec![1u8; 32], unit_type: "Tank".to_string() };
    repo.add_hex_occupant(sector_id, 5, -1, occ_a2).await.expect("add occ_a at (5,-1) failed");

    // Assert: "2,3" has 2 entries, "5,-1" has 1 entry.
    let fetched = repo.get_sector(sector_id).await.expect("get_sector failed").expect("sector not found");
    let at_2_3 = fetched.hex_occupancy.get("2,3").expect("key '2,3' missing");
    assert_eq!(at_2_3.len(), 2, "expected 2 occupants at (2,3)");
    let at_5_n1 = fetched.hex_occupancy.get("5,-1").expect("key '5,-1' missing");
    assert_eq!(at_5_n1.len(), 1, "expected 1 occupant at (5,-1)");

    // Remove occ_a from hex (2, 3).
    repo.remove_hex_occupant(sector_id, 2, 3, id_a).await.expect("remove_hex_occupant failed");

    // Assert: "2,3" now has 1 entry (occ_b only), "5,-1" unchanged.
    let fetched2 = repo.get_sector(sector_id).await.expect("get_sector failed").expect("sector not found");
    let at_2_3_after = fetched2.hex_occupancy.get("2,3").expect("key '2,3' missing after remove");
    assert_eq!(at_2_3_after.len(), 1, "expected 1 occupant at (2,3) after remove");
    assert_eq!(at_2_3_after[0].id, id_b, "remaining occupant at (2,3) must be occ_b");
    let at_5_n1_after = fetched2.hex_occupancy.get("5,-1").expect("key '5,-1' missing after remove");
    assert_eq!(at_5_n1_after.len(), 1, "expected 1 occupant at (5,-1) — unaffected by remove");

    // Cleanup.
    use redis::AsyncCommands;
    let mut conn = mgr.clone();
    conn.del::<_, ()>(vec![
        format!("sector:{}:hex_state", sector_id),
        format!("sector:{}:player_presence", sector_id),
    ])
    .await
    .expect("DEL cleanup failed");
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string())
        .await
        .expect("SREM cleanup failed");
}

#[tokio::test]
async fn convoy_dispatch_creates_db_record_and_timer() {
    use std::sync::Arc;
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;
    use redis::AsyncCommands;
    use shared::SessionToken;
    use chrono::Utc;

    let db_url = match std::env::var("TEST_DATABASE_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping convoy dispatch integration test");
            return;
        }
    };
    let redis_url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping convoy dispatch integration test");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("Failed to connect to test Postgres");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("Migrations failed");

    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let redis_mgr = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    // Build AppState with real repos
    let config = Arc::new(mercs_server::config::Config::default());
    let mut state = mercs_server::state::AppState::new(
        std::path::PathBuf::from("/tmp"),
        config,
    );
    state.campaign_repo  = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.convoy_repo    = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    state.sector_repo    = Arc::new(RedisSectorStateRepository::new(redis_mgr.clone()));
    state.timer_repo     = Arc::new(RedisTimerRepository::new(redis_mgr.clone()));
    state.input_log_repo = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    let state = Arc::new(state);

    // Wallet [88u8; 32] — distinct from all other test wallets
    let wallet_bytes = [88u8; 32];
    let wallet_b58 = bs58::encode(wallet_bytes).into_string();
    let token_id = "convoy-dispatch-test-token".to_string();
    let now = Utc::now().timestamp();
    let session_token = SessionToken {
        token_id:       token_id.clone(),
        account_id:     wallet_b58.clone(),
        wallet_address: wallet_b58.clone(),
        issued_at:      now,
        expires_at:     now + 7200,
    };
    state.sessions.insert(token_id.clone(), session_token);

    // Create a campaign in Postgres (sector_id is just a UUID — no Postgres FK)
    let campaign_repo = PostgresCampaignRepository::new(pool.clone());
    let campaign = campaign_repo.create_campaign(&NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  77777_i64,
    }).await.expect("create_campaign failed");
    let campaign_id = campaign.campaign_id;
    let sector_id   = campaign.sector_id;

    // Seed SectorState in Redis with terrain at destination hex (5, 3)
    let mut terrain = std::collections::HashMap::new();
    terrain.insert("5,3".to_string(), mercs_server::map_gen::HexTerrain::AshenPlains);
    let sector_state = SectorState {
        sector_id,
        campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
        active_timer_ids:    vec![],
        terrain,
        magma_veins:         vec![],
        hex_occupancy:       std::collections::HashMap::new(),
    };
    state.sector_repo.upsert_sector(sector_state).await.expect("upsert_sector failed");

    // Fire POST /api/campaign/:id/convoy/dispatch
    let app = mercs_server::routes::router(Arc::clone(&state));
    let body = serde_json::json!({
        "origin_q": 3, "origin_r": -2,
        "destination_q": 5, "destination_r": 3,
        "vehicle_class": "BehemothMegaTruck",
        "cargo": {}
    });
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/campaign/{}/convoy/dispatch", campaign_id))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token_id))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "dispatch must return 201");

    // Parse convoy_id from response body
    let resp_bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let resp_json: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    let convoy_id: uuid::Uuid = resp_json["convoy_id"].as_str().unwrap().parse().unwrap();

    // Assert: convoy in DB with in_transit = true
    let conv_repo = PostgresConvoyRepository::new(pool.clone());
    let fetched = conv_repo.get_convoy(convoy_id).await
        .expect("get_convoy error")
        .expect("convoy not found in DB after dispatch");
    assert!(fetched.in_transit, "convoy must be in_transit after dispatch");
    assert_eq!(fetched.vehicle_class, "BehemothMegaTruck");

    // Assert: timer key exists in Redis
    let mut conn = redis_mgr.clone();
    let timer_json: Option<String> = conn
        .get(format!("timer:{}", convoy_id))
        .await
        .expect("Redis GET timer key failed");
    assert!(timer_json.is_some(), "timer key timer:{convoy_id} must exist in Redis after dispatch");

    // ── Cleanup ───────────────────────────────────────────────────────────────
    sqlx::query!("DELETE FROM convoy_records WHERE convoy_id = $1", convoy_id)
        .execute(&pool).await.expect("cleanup convoy_records");

    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup");
    sqlx::query!("DELETE FROM input_logs WHERE campaign_id = $1", campaign_id)
        .execute(&pool).await.expect("cleanup input_logs");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup");

    sqlx::query!("DELETE FROM campaign_instances WHERE campaign_id = $1", campaign_id)
        .execute(&pool).await.expect("cleanup campaign_instances");

    let timer_id_str  = convoy_id.to_string();
    let sector_id_str = sector_id.to_string();
    conn.del::<_, ()>(vec![
        format!("timer:{}", convoy_id),
        format!("timer:{}:sector_id", convoy_id),
    ]).await.expect("DEL timer keys cleanup");
    conn.zrem::<_, _, ()>("timers:global", &timer_id_str).await.expect("ZREM timers:global");
    conn.zrem::<_, _, ()>(format!("sector:{}:timers", sector_id), &timer_id_str).await.expect("ZREM sector timers");
    conn.del::<_, ()>(vec![
        format!("sector:{}:hex_state", sector_id),
        format!("sector:{}:player_presence", sector_id),
    ]).await.expect("DEL sector keys cleanup");
    conn.srem::<_, _, ()>("sectors:all", &sector_id_str).await.expect("SREM sectors:all");
    let _ = sector_id_str; // silence unused warning
}

#[tokio::test]
async fn convoy_timer_fires_and_marks_arrived() {
    use std::sync::Arc;
    use mercs_server::config::Config;
    use mercs_server::convoy_expiry::poll_expiry_once;
    use mercs_server::repository::InMemoryInputLogRepository;
    use mercs_server::state::AppState;
    use redis::AsyncCommands;

    let db_url = match std::env::var("TEST_DATABASE_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping");
            return;
        }
    };
    let redis_url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping");
            return;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("Failed to connect to Postgres");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("Migrations failed");

    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let mut conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    let convoy_repo = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    let timer_repo  = Arc::new(RedisTimerRepository::new(conn.clone()));
    let sector_repo = Arc::new(RedisSectorStateRepository::new(conn.clone()));
    let log_repo    = Arc::new(InMemoryInputLogRepository::new());

    let convoy_id   = uuid::Uuid::new_v4();
    let sector_id   = uuid::Uuid::new_v4();
    let campaign_id = uuid::Uuid::new_v4();
    let origin_q = 3i32;
    let origin_r = -2i32;
    let destination_q = 5i32;
    let destination_r = 3i32;

    // Seed SectorState in Redis so get_sector returns Some (needed for campaign_id lookup)
    sector_repo.upsert_sector(SectorState {
        sector_id,
        campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
        active_timer_ids:    vec![],
        terrain:             std::collections::HashMap::new(),
        magma_veins:         vec![],
        hex_occupancy:       std::collections::HashMap::new(),
    }).await.expect("upsert_sector failed");

    // Create convoy record with arrival_time in the past (already due)
    let record = ConvoyDbRecord {
        convoy_id,
        sector_id,
        owner_wallet: vec![7u8; 32],
        origin_q,
        origin_r,
        destination_q,
        destination_r,
        route: serde_json::json!([
            {"q": origin_q, "r": origin_r},
            {"q": destination_q, "r": destination_r}
        ]),
        arrival_time: chrono::Utc::now() - chrono::Duration::seconds(1),
        vehicle_class: "BehemothMegaTruck".to_string(),
        cargo: serde_json::json!({}),
        in_transit: true,
        created_at: chrono::Utc::now(),
    };
    convoy_repo.create_convoy(record).await.expect("create_convoy failed");

    // Schedule timer — timer_id == convoy_id is the load-bearing invariant
    let timer = DeploymentTimer {
        timer_id:      convoy_id,
        player_wallet: WalletAddress([7u8; 32]),
        sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      chrono::Utc::now() - chrono::Duration::seconds(1),
    };
    timer_repo.schedule_timer(timer).await.expect("schedule_timer failed");

    // Add origin hex occupant (poll_expiry_once will remove it)
    sector_repo.add_hex_occupant(sector_id, origin_q, origin_r, HexOccupant {
        id:           convoy_id,
        owner_wallet: vec![7u8; 32],
        unit_type:    "BehemothMegaTruck".to_string(),
    }).await.expect("add_hex_occupant failed");

    // Run expiry poll
    let mut state = AppState::new(std::path::PathBuf::from("."), Arc::new(Config::default()));
    state.convoy_repo    = convoy_repo.clone();
    state.sector_repo    = sector_repo.clone();
    state.timer_repo     = timer_repo.clone();
    state.input_log_repo = log_repo.clone();
    poll_expiry_once(&state).await;

    // Assert: convoy is no longer in_transit
    let fetched = convoy_repo.get_convoy(convoy_id).await
        .expect("get_convoy error")
        .expect("convoy should still exist after expiry");
    assert!(!fetched.in_transit, "in_transit should be false after poll_expiry_once");

    // ── Cleanup ───────────────────────────────────────────────────────────────
    sqlx::query!("DELETE FROM convoy_records WHERE convoy_id = $1", convoy_id)
        .execute(&pool).await.expect("cleanup convoy_records");

    let convoy_id_str = convoy_id.to_string();
    let sector_id_str = sector_id.to_string();
    // Timer keys cleaned up by cancel_timer inside poll_expiry_once; DEL is idempotent
    conn.del::<_, ()>(vec![
        format!("timer:{}", convoy_id),
        format!("timer:{}:sector_id", convoy_id),
    ]).await.ok();
    conn.zrem::<_, _, ()>("timers:global", &convoy_id_str).await.ok();
    conn.zrem::<_, _, ()>(format!("sector:{}:timers", sector_id), &convoy_id_str).await.ok();
    conn.del::<_, ()>(vec![
        format!("sector:{}:hex_state", sector_id),
        format!("sector:{}:player_presence", sector_id),
    ]).await.ok();
    conn.srem::<_, _, ()>("sectors:all", &sector_id_str).await.ok();
    let _ = (convoy_id_str, sector_id_str);
}

#[tokio::test]
async fn convoy_ws_receives_arrived_event() {
    use std::sync::Arc;
    use mercs_server::config::Config;
    use mercs_server::convoy_expiry::{poll_expiry_once, ConvoyEvent};
    use mercs_server::repository::InMemoryInputLogRepository;
    use mercs_server::state::AppState;
    use redis::AsyncCommands;
    use tokio::sync::mpsc;

    let db_url = match std::env::var("TEST_DATABASE_URL").ok() {
        Some(u) => u,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping"); return; }
    };
    let redis_url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => { eprintln!("TEST_REDIS_URL not set — skipping"); return; }
    };

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .expect("Failed to connect to Postgres");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("Migrations failed");

    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let mut conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to connect to Redis");

    let convoy_id   = uuid::Uuid::new_v4();
    let sector_id   = uuid::Uuid::new_v4();
    let campaign_id = uuid::Uuid::new_v4();
    let origin_q      = 1i32;
    let origin_r      = 0i32;
    let destination_q = 3i32;
    let destination_r = 2i32;

    // Build AppState with real Postgres + Redis repos
    let convoy_repo = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    let timer_repo  = Arc::new(RedisTimerRepository::new(conn.clone()));
    let sector_repo = Arc::new(RedisSectorStateRepository::new(conn.clone()));
    let log_repo    = Arc::new(InMemoryInputLogRepository::new());

    let mut state = AppState::new(std::path::PathBuf::from("."), Arc::new(Config::default()));
    state.convoy_repo    = convoy_repo.clone();
    state.sector_repo    = sector_repo.clone();
    state.timer_repo     = timer_repo.clone();
    state.input_log_repo = log_repo.clone();

    // Inject WS sender for this campaign — poll_expiry_once will fire into it
    let (tx, mut rx) = mpsc::unbounded_channel::<ConvoyEvent>();
    state.convoy_event_senders.insert(campaign_id, tx);

    let state = Arc::new(state);

    // Seed SectorState so get_sector returns campaign_id
    sector_repo.upsert_sector(SectorState {
        sector_id,
        campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
        active_timer_ids:    vec![],
        terrain:             std::collections::HashMap::new(),
        magma_veins:         vec![],
        hex_occupancy:       std::collections::HashMap::new(),
    }).await.expect("upsert_sector failed");

    // Create convoy record already due
    let record = ConvoyDbRecord {
        convoy_id,
        sector_id,
        owner_wallet: vec![9u8; 32],
        origin_q,
        origin_r,
        destination_q,
        destination_r,
        route: serde_json::json!([
            {"q": origin_q, "r": origin_r},
            {"q": destination_q, "r": destination_r}
        ]),
        arrival_time: chrono::Utc::now() - chrono::Duration::seconds(1),
        vehicle_class: "BehemothMegaTruck".to_string(),
        cargo: serde_json::json!({"he3": 100}),
        in_transit: true,
        created_at: chrono::Utc::now(),
    };
    convoy_repo.create_convoy(record).await.expect("create_convoy failed");

    // Schedule timer — timer_id == convoy_id invariant
    let timer = DeploymentTimer {
        timer_id:      convoy_id,
        player_wallet: WalletAddress([9u8; 32]),
        sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      chrono::Utc::now() - chrono::Duration::seconds(1),
    };
    timer_repo.schedule_timer(timer).await.expect("schedule_timer failed");

    // Add origin occupant (poll_expiry_once removes it)
    sector_repo.add_hex_occupant(sector_id, origin_q, origin_r, HexOccupant {
        id:           convoy_id,
        owner_wallet: vec![9u8; 32],
        unit_type:    "BehemothMegaTruck".to_string(),
    }).await.expect("add_hex_occupant failed");

    // Run expiry — should fire ConvoyArrived into the injected sender
    poll_expiry_once(&state).await;

    // Assert: WS message received within 2 seconds
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        rx.recv(),
    )
    .await
    .expect("timeout — convoy WS message not received within 2s")
    .expect("channel closed unexpectedly");

    match msg {
        ConvoyEvent::ConvoyArrived { convoy_id: cid, .. } => {
            assert_eq!(cid, convoy_id, "ConvoyArrived has wrong convoy_id");
        }
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    sqlx::query!("DELETE FROM convoy_records WHERE convoy_id = $1", convoy_id)
        .execute(&pool).await.expect("cleanup convoy_records");

    let convoy_id_str = convoy_id.to_string();
    let sector_id_str = sector_id.to_string();
    conn.del::<_, ()>(vec![
        format!("timer:{}", convoy_id),
        format!("timer:{}:sector_id", convoy_id),
    ]).await.ok();
    conn.zrem::<_, _, ()>("timers:global", &convoy_id_str).await.ok();
    conn.zrem::<_, _, ()>(format!("sector:{}:timers", sector_id), &convoy_id_str).await.ok();
    conn.del::<_, ()>(vec![
        format!("sector:{}:hex_state", sector_id),
        format!("sector:{}:player_presence", sector_id),
    ]).await.ok();
    conn.srem::<_, _, ()>("sectors:all", &sector_id_str).await.ok();
    let _ = (convoy_id_str, sector_id_str);
}
