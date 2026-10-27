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
    InputLogRepository, PostgresInputLogRepository,
    MembershipRepository, PostgresMembershipRepository,
    SectionRecord, SectionRepository, PostgresSectionRepository,
    HexOccupant, OccupationStatus, RedisSectorStateRepository, SectorState, SectorStateRepository,
    DeploymentTimer, RedisTimerRepository, TimerRepository, TimerType,
    CombatSession, RedisSessionStateRepository, SessionStateRepository,
    ConvoyDbRecord, ConvoyRepository, PostgresConvoyRepository,
    NewDeployment, DeploymentStart, outcome_db_str,
    ForemanRepository, PostgresForemanRepository,
    AdvisorRecord, AdvisorRepository, PostgresAdvisorRepository,
    CrewRecord, CrewRepository, PostgresCrewRepository,
    AugmenteeOwner, AugmenteeRecord, AugmenteeRepository, PostgresAugmenteeRepository,
    RepositoryError, AcquiredVia, InMemoryCommanderRepository,
};
use sim_engine::roster::{
    AdvisorQuirk, AdvisorSpecialization, Augment, AugmentCatalogue, CasualtyClass, CommanderFlaw,
    CommanderGrade, CommanderOrigin, CommanderSpecialization, FactionBias, FittingSource,
    ForemanFlaw, ForemanSpecialization,
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

    // Wallet [2u8;32] is shared with initialize_campaign_orchestrates_correctly.
    // Pre-clean by wallet in FK order (F5): a campaign_id pre-clean is a no-op, since
    // the campaign above is brand new.
    // Sections left by a panicked run, reached through their owner column (GAP-63, 0025).
    // Before 0025 they were reachable only through a live Commander, whose link was
    // ON DELETE SET NULL, so a Section outlived the join (0028 dropped that link).
    // Its owner is RESTRICT, so it must go before the player_accounts delete below.
    sqlx::query("DELETE FROM section_records WHERE player_wallet = $1")
        .bind(&[2u8; 32].to_vec()).execute(&pool).await.ok();
    sqlx::query("DELETE FROM commander_records WHERE player_wallet = $1")
        .bind(&[2u8; 32].to_vec()).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(&[2u8; 32].to_vec()).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
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
    // 5a-1 (0030): a roster row needs its owner's membership of the campaign.
    PostgresMembershipRepository::new(pool.clone())
        .add_member(&wallet_bytes, campaign_id, None, None)
        .await
        .expect("membership seed failed");

    // 2. Create a CommanderRecord.
    commander_repo.create_commander(CommanderRecord {
        commander_id,
        campaign_id,
        player_wallet:   wallet_bytes.clone(),
        name:            "Sgt. Ironclad".into(),
        rank:            1,
        xp:              0,
        stress:          0,
        origin:          CommanderOrigin::Bearocrat.as_str().into(),
        faction_bias:    FactionBias::TrustAsset.as_str().into(),
        specialization:  CommanderSpecialization::Cannoneer.as_str().into(),
        fatal_flaw:      CommanderFlaw::Cowardly.as_str().into(),
        veteran_trait:   None,
        is_shattered:    false,
        is_kia:          false,
        prng_seed_state: 0xDEAD_i64,
        commander_grade: Some(CommanderGrade::Distinguished.as_str().into()),
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

    // 7. Create a SectionRecord in the same campaign, owned by the same wallet. Sections
    // carry no Commander (7b-2): the link is per mission, on deployment_sections.
    let section_id = uuid::Uuid::new_v4();
    section_repo.create_section(SectionRecord {
        section_id,
        campaign_id,
        player_wallet: wallet_bytes.clone(),
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
    assert_eq!(sec.player_wallet, vec![2u8; 32], "the owner must round-trip");
    assert_eq!(sec.headcount, 8);

    // 9. update_headcount.
    section_repo.update_headcount(section_id, 5).await.expect("update_headcount failed");
    let after_hc = section_repo.get_section(section_id).await.unwrap().unwrap();
    assert_eq!(after_hc.headcount, 5);

    // 10. set_kia — the Commander's death leaves the Section untouched.
    commander_repo.set_kia(commander_id).await.expect("set_kia failed");
    let after_kia = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert!(after_kia.is_kia, "is_kia must be true");
    let sec_after_kia = section_repo.get_section(section_id).await.unwrap().unwrap();
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

/// BP-1: `delete_sector` removes the hex state key and the `sectors:all` member, and a
/// second delete is `Ok`.
#[tokio::test]
async fn redis_delete_sector_removes_key_and_set_member() {
    use redis::AsyncCommands;
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping redis_delete_sector_removes_key_and_set_member");
            return;
        }
    };
    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");
    let repo = RedisSectorStateRepository::new(mgr.clone());
    let mut conn = mgr.clone();

    let sector_id = uuid::Uuid::new_v4();
    repo.upsert_sector(SectorState {
        sector_id,
        campaign_id: uuid::Uuid::new_v4(),
        occupation_status: OccupationStatus::Neutral,
        owner: None,
        deployed_unit_count: 0,
        terrain: std::collections::HashMap::new(),
        magma_veins: vec![],
        hex_occupancy: std::collections::HashMap::new(),
    }).await.expect("upsert_sector failed");
    let member_before: bool = conn.sismember("sectors:all", sector_id.to_string()).await.expect("SISMEMBER");

    repo.delete_sector(sector_id).await.expect("delete_sector failed");
    let fetched = repo.get_sector(sector_id).await.expect("get_sector failed");
    let member_after: bool = conn.sismember("sectors:all", sector_id.to_string()).await.expect("SISMEMBER");
    let again = repo.delete_sector(sector_id).await;

    // Clean (a no-op when the delete worked), then assert.
    conn.del::<_, ()>(format!("sector:{}:hex_state", sector_id)).await.ok();
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string()).await.ok();

    assert!(member_before, "the upsert adds the sectors:all member");
    assert!(fetched.is_none(), "the sector is gone");
    assert!(!member_after, "the sectors:all member is gone");
    again.expect("a second delete is Ok");
}

/// BP-1c: `sector_exists` (one `EXISTS` on the hex state key, the demo join's sectorless
/// repair) follows the sector through an upsert and a delete, and says nothing about any
/// other sector.
#[tokio::test]
async fn redis_sector_exists_tracks_upsert_and_delete() {
    use redis::AsyncCommands;
    let url = match std::env::var("TEST_REDIS_URL").ok() {
        Some(u) => u,
        None => {
            eprintln!("TEST_REDIS_URL not set — skipping redis_sector_exists_tracks_upsert_and_delete");
            return;
        }
    };
    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mgr = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");
    let repo = RedisSectorStateRepository::new(mgr.clone());
    let mut conn = mgr.clone();

    // Collect.
    let sector_id = uuid::Uuid::new_v4();
    let before = repo.sector_exists(sector_id).await.expect("sector_exists before");
    repo.upsert_sector(SectorState {
        sector_id,
        campaign_id: uuid::Uuid::new_v4(),
        occupation_status: OccupationStatus::Neutral,
        owner: None,
        deployed_unit_count: 0,
        terrain: std::collections::HashMap::new(),
        magma_veins: vec![],
        hex_occupancy: std::collections::HashMap::new(),
    }).await.expect("upsert_sector failed");
    let after_upsert = repo.sector_exists(sector_id).await.expect("sector_exists after the upsert");
    let other = repo.sector_exists(uuid::Uuid::new_v4()).await.expect("sector_exists of another id");
    repo.delete_sector(sector_id).await.expect("delete_sector failed");
    let after_delete = repo.sector_exists(sector_id).await.expect("sector_exists after the delete");

    // Clean (a no-op when the delete worked), then assert.
    conn.del::<_, ()>(format!("sector:{}:hex_state", sector_id)).await.ok();
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string()).await.ok();

    assert!(!before, "nothing written yet");
    assert!(after_upsert, "the upsert wrote the hex state");
    assert!(!other, "another id stays absent");
    assert!(!after_delete, "the delete removed the hex state");
}

/// BP-1: on Postgres, `retire_if_active` archives only an `Active` campaign; `Pending`,
/// `Ending` and a missing id are `false` and unchanged. Fixture campaigns no wallet reaches.
#[tokio::test]
async fn pg_retire_if_active_moves_only_active() {
    let Some(pool) = test_pool().await else {
        eprintln!("TEST_DATABASE_URL not set — skipping pg_retire_if_active_moves_only_active");
        return;
    };
    let repo = PostgresCampaignRepository::new(pool.clone());
    let cases = [
        (CampaignLifecycle::Active,  CampaignLifecycle::Archived),
        (CampaignLifecycle::Pending, CampaignLifecycle::Pending),
        (CampaignLifecycle::Ending,  CampaignLifecycle::Ending),
    ];

    // Collect.
    let mut ids = Vec::new();
    let mut seen = Vec::new();
    for (i, (from, _)) in cases.iter().enumerate() {
        let c = repo
            .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 0xB1_00 + i as i64 })
            .await
            .expect("create_campaign failed")
            .campaign_id;
        ids.push(c);
        repo.update_sector_state(c, from.clone()).await.expect("update_sector_state failed");
        let retired = repo.retire_if_active(c).await;
        let state = repo.get_campaign(c).await.expect("get_campaign failed").map(|r| r.state);
        seen.push((retired, state));
    }
    let missing = repo.retire_if_active(uuid::Uuid::new_v4()).await;

    // Clean.
    for c in &ids {
        sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
            .bind(c).execute(&pool).await.expect("fixture cleanup failed");
    }

    // Assert.
    for ((from, to), (retired, state)) in cases.iter().zip(seen) {
        let expect_retired = *from == CampaignLifecycle::Active;
        assert_eq!(retired.expect("retire_if_active failed"), expect_retired, "{from:?}");
        assert_eq!(state, Some(to.clone()), "{from:?}");
    }
    assert!(!missing.expect("retire_if_active on a miss failed"), "a missing id is false");
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

    // land_convoy — lands once, and a second call lands nothing
    let campaign_id = uuid::Uuid::new_v4();
    let entry = shared::InputLogEntry {
        tick: 0,
        seq: 0,
        event_type: "convoy_arrived".to_string(),
        player_id: None,
        payload: serde_json::json!({ "convoy_id": convoy_id }),
        narrative_event: None,
    };
    let landed = repo.land_convoy(convoy_id, campaign_id, &entry).await.expect("land_convoy failed");
    assert_eq!(landed.map(|c| c.convoy_id), Some(convoy_id), "the first land returns the convoy");
    let again = repo.land_convoy(convoy_id, campaign_id, &entry).await.expect("second land_convoy failed");
    assert!(again.is_none(), "an already-landed convoy is not landed again");

    // get — verify in_transit flipped
    let fetched2 = repo.get_convoy(convoy_id).await.expect("get_convoy failed").expect("convoy not found after land_convoy");
    assert!(!fetched2.in_transit, "in_transit must be false after land_convoy");

    // cleanup
    sqlx::query!("DELETE FROM convoy_records WHERE convoy_id = $1", convoy_id)
        .execute(&pool)
        .await
        .expect("cleanup DELETE failed");
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();
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
    let due = repo.get_due_timers(now).await.expect("get_due_timers failed").due;
    assert_eq!(due.len(), 1, "expected exactly one due timer");
    assert_eq!(due[0].timer_id, timer_a_id, "due timer must be timer_a");

    // 3. Verify payloads round-trip: get_due_timers returns the full struct.
    assert_eq!(due[0].sector_id, sector_id);
    assert!(matches!(due[0].timer_type, TimerType::ConvoyArrival));

    // 4. Cancel the future timer.
    repo.cancel_timer(timer_b_id).await.expect("cancel timer_b failed");

    // 5. After cancel, scanning far-future window still returns only timer_a.
    let far_future = now + chrono::Duration::seconds(7200);
    let after_cancel = repo.get_due_timers(far_future).await.expect("get_due_timers (far_future) failed").due;
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
        .expect("get_due_timers before cancel failed")
        .due;
    assert!(before.iter().any(|t| t.timer_id == timer_id), "timer must appear before cancel");

    repo.cancel_timer(timer_id).await.expect("cancel_timer failed");

    // After cancel, a far-future scan must not return the cancelled timer.
    let after = repo.get_due_timers(chrono::Utc::now() + chrono::Duration::seconds(7200))
        .await
        .expect("get_due_timers after cancel failed")
        .due;
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

    // Pre-test cleanup by wallet, in FK order (F5).
    sqlx::query("DELETE FROM commander_records WHERE player_wallet = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
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
    // 5a-1 (0030): a roster row needs its owner's membership of the campaign.
    PostgresMembershipRepository::new(pool.clone())
        .add_member(&wallet_bytes, campaign_id, None, None)
        .await
        .expect("membership seed failed");

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
    assert!(record.veteran_trait.is_none());
    assert_eq!(record.prng_seed_state, 42i64);
    assert_eq!(record.campaign_id, campaign_id);

    assert!(
        CommanderOrigin::ALL.iter().any(|o| o.as_str() == record.origin),
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

    // Cleanup — all three rows this test seeded (Reviewer 4a-1 finding 1).
    sqlx::query!("DELETE FROM commander_records WHERE commander_id = $1", commander_id)
        .execute(&pool).await.ok();
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
}

/// Brick 4b-1 — `apply_mission_xp` through the Postgres backend. The default
/// trait method sequences `get_commander` + `update_rank_and_xp`; this proves the
/// derived rank and cumulative XP land in the row.
#[tokio::test]
async fn apply_mission_xp_persists_rank_and_xp() {
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
        map_seed:  0x4B01_i64,
    }).await.expect("create campaign failed");
    let campaign_id = campaign.campaign_id;

    // Wallet [9u8;32]: the only Postgres account/commander use. It also appears as a non-FK owner in timer_repository_redis_is_correct and convoy_ws_receives_arrived_event.
    let wallet_bytes = [9u8; 32].to_vec();

    // Pre-test cleanup by wallet, in FK order (F5).
    sqlx::query("DELETE FROM commander_records WHERE player_wallet = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
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
    // 5a-1 (0030): a roster row needs its owner's membership of the campaign.
    PostgresMembershipRepository::new(pool.clone())
        .add_member(&wallet_bytes, campaign_id, None, None)
        .await
        .expect("membership seed failed");

    // 2. Generate a commander sitting just below the rank-2 threshold.
    let mut record = mercs_server::commander_gen::generate_commander(
        wallet_bytes.clone(),
        campaign_id,
        42u32,
    );
    record.xp = 88;
    let commander_id = record.commander_id;
    commander_repo.create_commander(record).await.expect("create_commander failed");

    // 3. T1 Success: +12 XP → 100 → rank 2.
    commander_repo
        .apply_mission_xp(commander_id, 1, sim_engine::game_types::OutcomeType::Success)
        .await
        .expect("apply_mission_xp failed");

    // 4. Round-trip.
    let fetched = commander_repo.get_commander(commander_id).await
        .expect("get_commander error")
        .expect("commander not found after apply_mission_xp");
    assert_eq!(fetched.xp, 100, "xp must be 88 + 12");
    assert_eq!(fetched.rank, 2, "rank must be derived from cumulative xp");

    // Cleanup — all three tables.
    sqlx::query!("DELETE FROM commander_records WHERE commander_id = $1", commander_id)
        .execute(&pool).await.ok();
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
}

/// Brick 4c-1 (G3, Postgres) — the stress wrapper drives stress to 100 and the single
/// `update_stress` statement sets `is_shattered`; no `set_shattered` call.
#[tokio::test]
async fn commander_stress_wrapper_shatters_in_one_write() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let commander_repo = PostgresCommanderRepository::new(pool.clone());
    let campaign_repo  = PostgresCampaignRepository::new(pool.clone());

    let campaign = campaign_repo.create_campaign(&NewCampaignInstance {
        sector_id: uuid::Uuid::new_v4(),
        map_seed:  0x4C01_i64,
    }).await.expect("create campaign failed");
    let campaign_id = campaign.campaign_id;

    // Wallet [13u8;32] — unique to this test (10–12 belong to the launch-route test).
    let wallet_bytes = [13u8; 32].to_vec();

    // Pre-test cleanup by wallet, in FK order (F5).
    sqlx::query("DELETE FROM commander_records WHERE player_wallet = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();

    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
         VALUES ($1, 0, 0) ON CONFLICT (wallet_address) DO NOTHING"
    )
    .bind(&wallet_bytes)
    .execute(&pool)
    .await
    .expect("player_accounts FK seed failed");
    // 5a-1 (0030): a roster row needs its owner's membership of the campaign.
    PostgresMembershipRepository::new(pool.clone())
        .add_member(&wallet_bytes, campaign_id, None, None)
        .await
        .expect("membership seed failed");

    let record = mercs_server::commander_gen::generate_commander(
        wallet_bytes.clone(),
        campaign_id,
        42u32,
    );
    let commander_id = record.commander_id;
    commander_repo.create_commander(record).await.expect("create_commander failed");

    commander_repo.update_stress(commander_id, 95).await.expect("update_stress failed");
    let before = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(before.stress, 95);
    assert!(!before.is_shattered, "95 must not shatter");

    commander_repo
        .apply_deployment_stress(commander_id, &sim_engine::config::SimConfig::default())
        .await
        .expect("apply_deployment_stress failed");

    let after = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(after.stress, 100, "95 + default deployment penalty 10 caps at 100");
    assert!(after.is_shattered, "update_stress must set is_shattered in the same write");
    assert!(!after.is_kia, "Shattered is not Permadeath");

    // Stickiness on Postgres (Reviewer 4c-1 F2): a lower write keeps the flag.
    commander_repo.update_stress(commander_id, 40).await.expect("update_stress failed");
    let lower = commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert!(lower.stress == 40 && lower.is_shattered, "a lower stress write must not clear Shattered");

    sqlx::query!("DELETE FROM commander_records WHERE commander_id = $1", commander_id)
        .execute(&pool).await.ok();
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(&wallet_bytes).execute(&pool).await.ok();
}

/// Brick 4c-1 (G4, Postgres) — F3: every single-row mutator reports NotFound on a miss.
#[tokio::test]
async fn commander_mutators_unknown_id_return_not_found() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
            return;
        }
    };

    let repo = PostgresCommanderRepository::new(pool.clone());
    let id = uuid::Uuid::new_v4();
    use mercs_server::repository::RepositoryError;

    let r = repo.update_stress(id, 10).await;
    assert!(matches!(r, Err(RepositoryError::NotFound)), "update_stress: {:?}", r);
    let r = repo.update_rank_and_xp(id, 2, 100).await;
    assert!(matches!(r, Err(RepositoryError::NotFound)), "update_rank_and_xp: {:?}", r);
    let r = repo.set_veteran_trait(id, "Grizzled".into()).await;
    assert!(matches!(r, Err(RepositoryError::NotFound)), "set_veteran_trait: {:?}", r);
    let r = repo.set_shattered(id).await;
    assert!(matches!(r, Err(RepositoryError::NotFound)), "set_shattered: {:?}", r);
    let r = repo.set_kia(id).await;
    assert!(matches!(r, Err(RepositoryError::NotFound)), "set_kia: {:?}", r);
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
            event_type: "player_input".into(),
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
    assert_eq!(retrieved[2].event_type, "player_input");

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
    mercs_server::lifecycle::poll_lifecycle_once(&repo, &mut mercs_server::lifecycle::LifecycleWarns::new()).await;

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
        event_type: "player_input".into(),
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
        event_type: "player_input".into(),
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
    let join_repo       = PostgresCampaignJoinRepository::new(pool.clone());

    // One member (5a-3a: the launch guard refuses more until 5b-2).
    let wallet = [1u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    let campaign_id = seed_account_and_campaign(&pool, wallet, 12345_i64).await;
    let sector_id = campaign_repo.get_campaign(campaign_id).await
        .expect("get_campaign error").expect("seeded campaign").sector_id;

    let init = initialize_campaign(
        campaign_id,
        &join_repo,
        &membership_repo,
        &sector_repo,
        &input_log_repo,
    ).await;

    // Collect, clean, then assert.
    let updated = campaign_repo.get_campaign(campaign_id).await.expect("get_campaign error");
    let sector_state = sector_repo.get_sector(sector_id).await.expect("get_sector failed");
    let members = membership_repo.list_members_by_campaign(campaign_id).await
        .expect("list_members_by_campaign failed");
    let log_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM input_logs WHERE campaign_id = $1 AND event_type = 'campaign_started'"
    )
    .bind(campaign_id)
    .fetch_one(&pool)
    .await
    .expect("log count query failed");

    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup failed");
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("post-test input_logs cleanup failed");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup failed");
    clean_wallet_4g1(&pool, &wallet).await;
    use redis::AsyncCommands;
    let mut conn = redis_mgr.clone();
    conn.del::<_, ()>(format!("sector:{}:hex_state", sector_id))
        .await
        .expect("DEL sector hex_state cleanup failed");
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string())
        .await
        .expect("SREM sectors:all cleanup failed");

    init.expect("initialize_campaign returned Err");
    let updated = updated.expect("campaign not found after initialize");
    assert_eq!(updated.state, CampaignLifecycle::Active, "state must be Active after initialize");
    let ends_at = updated.ends_at.expect("ends_at must be Some after initialize");
    assert!(
        ends_at > chrono::Utc::now() + chrono::Duration::days(80),
        "ends_at must be at least 80 days in the future"
    );
    let sector_state = sector_state
        .expect("get_sector returned None — upsert_sector must have written to Redis");
    assert!(
        !sector_state.terrain.is_empty(),
        "terrain must be populated in Redis after initialize"
    );
    assert_eq!(members.len(), 1, "expected 1 member");
    for m in &members {
        assert!(m.gateway_hex_q.is_some(), "gateway_hex_q must be Some after initialize");
        assert!(m.gateway_hex_r.is_some(), "gateway_hex_r must be Some after initialize");
    }
    assert_eq!(log_count, 1, "input_logs must have exactly one campaign_started entry — retry must not produce duplicates");
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

/// A mission needs at least one unit (brick 4e-0, GAP-36 empty-squad half). The
/// route refuses before the mission lookup, so nothing rolls and nothing is logged.
/// Needs neither Postgres nor Redis — the default in-memory AppState is enough.
/// Since 4g-1 the route needs a session and a `commander_id`; the body check runs
/// before the Commander lookup, so the Commander need not exist.
#[tokio::test]
async fn mission_resolve_rejects_empty_squad() {
    use std::sync::Arc;
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;

    let config = Arc::new(mercs_server::config::Config::default());
    let state = mercs_server::state::AppState::new(
        config,
    );
    let token = insert_session(&state, [4u8; 32]);
    let state = Arc::new(state);
    let body = serde_json::json!({
        "squad": { "units": [], "commander": null },
        "mission_id": "GUARD_DUTY",
        "commander_id": uuid::Uuid::new_v4(),
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/mission/resolve")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let app = mercs_server::routes::with_test_peer(mercs_server::routes::router(Arc::clone(&state)), mercs_server::routes::TEST_PEER);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(), StatusCode::BAD_REQUEST,
        "a zero-unit deployment must be refused before it rolls"
    );
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
        config,
    );
    state.campaign_repo   = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.membership_repo = Arc::new(PostgresMembershipRepository::new(pool.clone()));
    state.sector_repo     = Arc::new(RedisSectorStateRepository::new(redis_mgr.clone()));
    state.input_log_repo  = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    // The trap (5a-3a): the launch reads through the join repository, so it is replaced too.
    state.campaign_join_repo = Arc::new(PostgresCampaignJoinRepository::new(pool.clone()));
    let state = Arc::new(state);

    // Setup: a Pending campaign with one member (wallet 10; the launch guard refuses more).
    let wallet = [10u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    let campaign_repo = PostgresCampaignRepository::new(pool.clone());
    let campaign_id = seed_account_and_campaign(&pool, wallet, 55555_i64).await;
    let sector_id = campaign_repo.get_campaign(campaign_id).await
        .expect("get_campaign error").expect("seeded campaign").sector_id;

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
    let status1 = app.oneshot(req).await.unwrap().status();

    // Read the DB after the first launch
    let updated = campaign_repo.get_campaign(campaign_id).await.expect("get_campaign error");

    // Second POST — expect 409 (oneshot consumed app above; build a new one)
    let app2 = mercs_server::routes::router(Arc::clone(&state));
    let req2 = Request::builder()
        .method("POST")
        .uri(format!("/api/admin/campaign/{}/launch", campaign_id))
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let status2 = app2.oneshot(req2).await.unwrap().status();

    std::env::remove_var("ADMIN_API_KEY");

    // Cleanup before asserting
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers for cleanup");
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("cleanup input_logs");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("re-enable triggers after cleanup");
    clean_wallet_4g1(&pool, &wallet).await;
    use redis::AsyncCommands;
    let mut conn = redis_mgr.clone();
    conn.del::<_, ()>(format!("sector:{}:hex_state", sector_id))
        .await.expect("DEL sector hex_state cleanup");
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string())
        .await.expect("SREM sectors:all cleanup");

    assert_eq!(status1, StatusCode::OK, "first launch must return 200");
    let updated = updated.expect("campaign not found after launch");
    assert_eq!(updated.state, CampaignLifecycle::Active, "state must be Active after launch");
    assert!(updated.ends_at.is_some(), "ends_at must be Some after launch");
    assert_eq!(status2, StatusCode::CONFLICT, "second launch must return 409");
}

/// 5a-3a T2 — a two-member campaign is refused with its own 409 and left untouched (the
/// launch guard, read under the join lock; GAP-58 until 5b-2).
#[tokio::test]
async fn admin_launch_refuses_a_campaign_with_two_members() {
    use std::sync::Arc;
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;

    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let (a, b) = ([60u8; 32], [61u8; 32]);
    for w in [a, b] { clean_wallet_4g1(&pool, &w).await; }
    assert_no_pending_campaign(&pool).await;
    let c = seed_account_and_campaign(&pool, a, 0x5A3A2_i64).await;
    seed_account(&pool, b).await;
    PostgresMembershipRepository::new(pool.clone()).add_member(&b, c, None, None).await.expect("second member");

    let mut state = mercs_server::state::AppState::new(Arc::new(mercs_server::config::Config::default()));
    state.campaign_repo      = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.membership_repo    = Arc::new(PostgresMembershipRepository::new(pool.clone()));
    state.campaign_join_repo = Arc::new(PostgresCampaignJoinRepository::new(pool.clone()));
    state.input_log_repo     = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    let state = Arc::new(state);

    const TEST_ADMIN_KEY: &str = "test-only-not-a-real-admin-key";
    std::env::set_var("ADMIN_API_KEY", TEST_ADMIN_KEY);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/admin/campaign/{}/launch", c))
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp = mercs_server::routes::router(Arc::clone(&state)).oneshot(req).await.unwrap();
    std::env::remove_var("ADMIN_API_KEY");
    let status = resp.status();
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("body"),
    ).expect("json body");

    let campaign = PostgresCampaignRepository::new(pool.clone()).get_campaign(c).await.expect("get_campaign");
    let members = PostgresMembershipRepository::new(pool.clone()).list_members_by_campaign(c).await.expect("members");
    let log_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM input_logs WHERE campaign_id = $1 AND event_type = 'campaign_started'"
    ).bind(c).fetch_one(&pool).await.expect("log count");

    // Always clean the log, then both wallets, before asserting.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.expect("disable triggers");
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.expect("cleanup input_logs");
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.expect("re-enable triggers");
    for w in [a, b] { clean_wallet_4g1(&pool, &w).await; }

    assert_eq!(status, StatusCode::CONFLICT, "a two-member launch must return 409");
    assert_eq!(body, serde_json::json!({"error": "campaign has more than one member"}));
    assert_eq!(campaign.expect("campaign").state, CampaignLifecycle::Pending, "the campaign stays Pending");
    assert_eq!(members.len(), 2);
    for m in &members {
        assert_eq!((m.gateway_hex_q, m.gateway_hex_r), (None, None), "no hex assigned");
    }
    assert_eq!(log_count, 0, "no campaign_started row");
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
    let mut state = pg_route_state(&pool);
    state.convoy_repo    = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    state.sector_repo    = Arc::new(RedisSectorStateRepository::new(redis_mgr.clone()));
    state.timer_repo     = Arc::new(RedisTimerRepository::new(redis_mgr.clone()));
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

    // An Active campaign in Postgres with the wallet as a member (GAP-74: dispatch refuses a
    // non-member and a campaign that isn't Active). sector_id is just a UUID — no Postgres FK.
    let campaign_repo = PostgresCampaignRepository::new(pool.clone());
    let campaign_id = seed_account_and_campaign(&pool, wallet_bytes, 77777_i64).await;
    assert!(
        campaign_repo.activate_campaign(campaign_id, Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
            .await.expect("activate_campaign failed"),
        "the dispatch campaign must activate"
    );
    let sector_id = campaign_repo.get_campaign(campaign_id).await
        .expect("get_campaign failed").expect("seeded campaign").sector_id;

    // Seed SectorState in Redis with terrain at destination hex (5, 3)
    let mut terrain = std::collections::HashMap::new();
    terrain.insert("5,3".to_string(), mercs_server::map_gen::HexTerrain::AshenPlains);
    let sector_state = SectorState {
        sector_id,
        campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
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
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(wallet_bytes.to_vec()).execute(&pool).await.expect("cleanup player_accounts");

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
    let mut state = AppState::new(Arc::new(Config::default()));
    state.convoy_repo    = convoy_repo.clone();
    state.sector_repo    = sector_repo.clone();
    state.timer_repo     = timer_repo.clone();
    state.input_log_repo = log_repo.clone();
    poll_expiry_once(&state, &mut mercs_server::convoy_expiry::ExpiryWarns::new()).await;

    // Assert: convoy is no longer in_transit
    let fetched = convoy_repo.get_convoy(convoy_id).await
        .expect("get_convoy error")
        .expect("convoy should still exist after expiry");
    assert!(!fetched.in_transit, "in_transit should be false after poll_expiry_once");

    // ── Cleanup ───────────────────────────────────────────────────────────────
    sqlx::query!("DELETE FROM convoy_records WHERE convoy_id = $1", convoy_id)
        .execute(&pool).await.expect("cleanup convoy_records");
    // land_convoy writes the convoy_arrived row to Postgres (GAP-44), whatever log repo the state holds.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();

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

// ── Brick 4g-1: deployment records, start_session, stress CHECK ─────────────

/// The per-wallet roster tables, in cleanup order. Each has `player_wallet` and
/// `campaign_id` columns; a new roster table adds itself here (Reviewer 7a-1 CF1).
/// `augmentee_records` (7d-1, `0029`) is first: its owner FKs RESTRICT both
/// `section_records` and `crew_records` (a wipe is a state, never a `DELETE`; Reviewer
/// 7b-2 CF2).
const ROSTER_TABLES: [&str; 6] = ["augmentee_records", "section_records", "commander_records", "foreman_records", "advisor_records", "crew_records"];

/// Removes everything a 4g-1 test wallet owns, in FK order: card_mint_pending →
/// deployment_sections → deployment_records → augmentee_records → section_records →
/// commander_records → foreman_records → advisor_records → crew_records (`ROSTER_TABLES`)
/// → every campaign collected through roster rows and membership → membership →
/// player_accounts.
/// `deployment_sections` (7b-2) RESTRICTs deployments, Commanders and Sections, and
/// carries the owner itself, so it goes first. `deployment_records.commander_id` is
/// ON DELETE RESTRICT, so it goes next; it is there because of Commanders.
/// Campaigns are collected through the roster rows **and** the wallet's membership rows,
/// so a test whose every roster insert is refused still has its campaign found
/// (Reviewer 7c-1 F1; `seed_account_and_campaign` writes the membership row). A
/// multi-wallet test cleans every wallet after collecting its results (Reviewer 7b-1 F1).
async fn clean_wallet_4g1(pool: &sqlx::PgPool, wallet: &[u8]) {
    sqlx::query("DELETE FROM card_mint_pending WHERE player_wallet = $1")
        .bind(wallet).execute(pool).await.expect("card_mint_pending cleanup failed");
    sqlx::query("DELETE FROM deployment_sections WHERE player_wallet = $1")
        .bind(wallet).execute(pool).await.expect("deployment_sections cleanup failed");
    sqlx::query("DELETE FROM deployment_records WHERE commander_id IN (SELECT commander_id FROM commander_records WHERE player_wallet = $1)")
        .bind(wallet).execute(pool).await.expect("deployment_records cleanup failed");
    let union = ROSTER_TABLES
        .map(|t| format!("SELECT campaign_id FROM {t} WHERE player_wallet = $1"))
        .join(" UNION ")
        + " UNION SELECT campaign_id FROM player_campaign_membership WHERE wallet_address = $1";
    let campaigns: Vec<uuid::Uuid> = sqlx::query_scalar(&union)
        .bind(wallet).fetch_all(pool).await.unwrap_or_default();
    for table in ROSTER_TABLES {
        sqlx::query(&format!("DELETE FROM {table} WHERE player_wallet = $1"))
            .bind(wallet).execute(pool).await
            .unwrap_or_else(|e| panic!("{table} cleanup failed: {e}"));
    }
    for c in campaigns {
        sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
            .bind(c).execute(pool).await.ok();
    }
    sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(wallet).execute(pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(wallet).execute(pool).await.ok();
}

/// Pre-cleans `wallet`, then seeds its account, a campaign, its membership (5a-1: `0030`'s
/// FK needs it, and `clean_wallet_4g1` then finds the campaign even with no Commander left)
/// and one generated Commander at the given stress. Returns (campaign_id, commander_id).
async fn seed_commander_4g1(pool: &sqlx::PgPool, wallet: [u8; 32], stress: i16) -> (uuid::Uuid, uuid::Uuid) {
    clean_wallet_4g1(pool, &wallet).await;
    let campaign_id = seed_account_and_campaign(pool, wallet, 0x4601_i64).await;
    // Active, so the mission route's S3 admits it (5a-3b) and the join's pick never sees it.
    let activated = PostgresCampaignRepository::new(pool.clone())
        .activate_campaign(campaign_id, chrono::Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
        .await
        .expect("activate_campaign failed");
    assert!(activated, "the seeded campaign must activate");
    let mut record = mercs_server::commander_gen::generate_commander(wallet.to_vec(), campaign_id, 42u32);
    record.stress = stress;
    let commander_id = record.commander_id;
    PostgresCommanderRepository::new(pool.clone())
        .create_commander(record)
        .await
        .expect("create_commander failed");
    (campaign_id, commander_id)
}

fn new_deployment_4g1(commander_id: uuid::Uuid, started_at: chrono::DateTime<chrono::Utc>) -> NewDeployment {
    NewDeployment {
        deployment_id: uuid::Uuid::new_v4(),
        commander_id,
        session_id: uuid::Uuid::new_v4(),
        mission_id: "GUARD_DUTY".into(),
        started_at,
        // No Sections, so no deployment_sections row: the owner and campaign are unused.
        player_wallet: vec![],
        campaign_id: uuid::Uuid::nil(),
        section_ids: vec![],
    }
}

/// 4g-1 — one active deployment per Commander is the database's rule, not the route's.
/// Two racing starts produce exactly one `true`, and a raw second `active` INSERT (no
/// ON CONFLICT) is rejected by the partial unique index itself.
#[tokio::test]
async fn deployment_second_active_row_is_refused_by_the_database() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [14u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 0).await;
    let repo = PostgresCommanderRepository::new(pool.clone());

    let now = chrono::Utc::now();
    let (a, b) = (new_deployment_4g1(commander_id, now), new_deployment_4g1(commander_id, now));
    let (ra, rb) = tokio::join!(repo.start_deployment(&a), repo.start_deployment(&b));
    let (ra, rb) = (ra.expect("start a failed") == DeploymentStart::Started, rb.expect("start b failed") == DeploymentStart::Started);
    assert!(ra ^ rb, "exactly one concurrent start must win (got {ra}, {rb})");

    let raw = sqlx::query(
        "INSERT INTO deployment_records (deployment_id, commander_id, session_id, mission_id, status, started_at)
         VALUES ($1, $2, $3, 'GUARD_DUTY', 'active', now())"
    )
    .bind(uuid::Uuid::new_v4())
    .bind(commander_id)
    .bind(uuid::Uuid::new_v4())
    .execute(&pool)
    .await;
    let err = raw.expect_err("a second active row must violate deployment_records_one_active");
    let code = err.as_database_error().and_then(|d| d.code()).map(|c| c.to_string());
    assert_eq!(code.as_deref(), Some("23505"), "expected a unique violation, got {err}");

    let deps = repo.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps.iter().filter(|d| d.status == "active").count(), 1);

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4g-1 — a crash leftover heals itself: the next start aborts an `active` row older
/// than STALE_DEPLOYMENT_AFTER and then succeeds. A fresh `active` row is never touched.
#[tokio::test]
async fn deployment_stale_active_row_is_aborted_on_next_start() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [14u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 0).await;
    let repo = PostgresCommanderRepository::new(pool.clone());

    let now = chrono::Utc::now();
    let stale = new_deployment_4g1(commander_id, now - chrono::Duration::minutes(6));
    assert_eq!(repo.start_deployment(&stale).await.unwrap(), DeploymentStart::Started);
    // Four minutes later is not yet stale: refused.
    assert_eq!(repo.start_deployment(&new_deployment_4g1(commander_id, now - chrono::Duration::minutes(2))).await.unwrap(), DeploymentStart::CommanderBusy);
    let fresh = new_deployment_4g1(commander_id, now);
    assert_eq!(repo.start_deployment(&fresh).await.unwrap(), DeploymentStart::Started, "a stale active row must not block the next start");

    let deps = repo.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps.len(), 2);
    assert_eq!((deps[0].deployment_id, deps[0].status.as_str()), (stale.deployment_id, "aborted"));
    assert!(deps[0].closed_at.is_some() && deps[0].outcome.is_none(), "aborted: closed, no outcome");
    assert_eq!((deps[1].deployment_id, deps[1].status.as_str()), (fresh.deployment_id, "active"));

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4g-1 — the atomic write is atomic in Postgres. The close statement runs last and is
/// guarded on `active`; on an already-resolved deployment it touches 0 rows, and the
/// stress and XP statements that already ran are rolled back.
#[tokio::test]
async fn resolve_deployment_rolls_back_when_the_close_fails() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [15u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 30).await;
    let repo = PostgresCommanderRepository::new(pool.clone());

    use sim_engine::game_types::OutcomeType;
    use sim_engine::resolver::{stress_after_casualties, stress_after_deployment};
    use mercs_server::commander_gen::{rank_for_xp, xp_award};
    let cfg = sim_engine::config::SimConfig::default();
    let seeded = repo.get_commander(commander_id).await.unwrap().unwrap();

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
    // First resolve: tier 5 Success, no casualties — stress 30 → 40, and a rank-up.
    let eff = repo.resolve_deployment(d.deployment_id, commander_id, 0, 5, 0.0, &OutcomeType::Success, &cfg)
        .await
        .expect("first resolve failed");
    let want_xp = seeded.xp + xp_award(5, &OutcomeType::Success);
    let want = (i16::from(stress_after_deployment(30, &cfg)), want_xp, rank_for_xp(want_xp), false);
    let before = repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!((before.stress, before.xp, before.rank, before.is_shattered), want);
    assert_eq!((eff.stress, eff.xp, eff.old_rank, eff.new_rank), (want.0, want.1, seeded.rank, want.2), "AppliedEffects is what committed");

    // Second resolve on the now-closed deployment: from the locked 40, ten casualties
    // drive stress to 100 — it would Shatter if it stuck.
    let would_xp = before.xp + xp_award(5, &OutcomeType::Defeat);
    let would_stress = stress_after_casualties(stress_after_deployment(40, &cfg), 10, &cfg);
    assert_eq!(would_stress, 100, "precondition: the rolled-back write would Shatter");
    assert!(would_xp > before.xp, "precondition: the rolled-back write would add XP");
    let r = repo
        .resolve_deployment(d.deployment_id, commander_id, 10, 5, 0.0, &OutcomeType::Defeat, &cfg)
        .await;
    assert!(matches!(r, Err(mercs_server::repository::RepositoryError::NotFound)), "got {:?}", r);

    let after = repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!((after.stress, after.xp, after.rank, after.is_shattered), want,
        "a failed close must roll back the stress and XP writes");
    let deps = repo.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps[0].status, "resolved");
    assert_eq!(deps[0].outcome.as_deref(), Some(outcome_db_str(&sim_engine::game_types::OutcomeType::Success)));

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4g-2 test (b), Postgres: a Commander killed between start and resolve is refused
/// under the row lock, and the rollback leaves everything as it was — the deployment
/// stays `active` for the route to abort.
#[tokio::test]
async fn resolve_deployment_refuses_a_kia_commander() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [18u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 30).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let cfg = sim_engine::config::SimConfig::default();

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
    repo.set_kia(commander_id).await.unwrap();
    let before = repo.get_commander(commander_id).await.unwrap().unwrap();

    let r = repo
        .resolve_deployment(d.deployment_id, commander_id, 10, 5, 0.0, &sim_engine::game_types::OutcomeType::Success, &cfg)
        .await;
    assert!(matches!(r, Err(mercs_server::repository::RepositoryError::Internal(_))), "got {:?}", r);

    let after = repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!((after.stress, after.xp, after.rank, after.is_shattered), (30, before.xp, before.rank, false),
        "a refused resolve changes nothing");
    let deps = repo.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].status, "active");

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4g-2 test (c): a stress write that commits while `resolve_deployment` is running is
/// built on, not overwritten. `tx_a` holds the Commander row lock; the resolver must wait
/// for it at its `SELECT … FOR NO KEY UPDATE`, then compute from what `tx_a` committed.
#[tokio::test]
async fn resolve_deployment_builds_on_a_concurrent_stress_write() {
    use sim_engine::game_types::OutcomeType;
    use sim_engine::resolver::stress_after_deployment;
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    // Three connections are busy at once (tx_a, the resolver, the poll); the shared
    // helper caps at 2, so this test has its own pool.
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let wide = PgPoolOptions::new().max_connections(4).connect(&url).await.expect("wide pool");
    let wallet = [17u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 20).await;
    let cfg = sim_engine::config::SimConfig::default();
    let expected = i16::from(stress_after_deployment(60, &cfg));
    let stale = i16::from(stress_after_deployment(20, &cfg));
    assert_ne!(expected, stale, "precondition: the two orders must be distinguishable");

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(PostgresCommanderRepository::new(wide.clone()).start_deployment(&d).await.unwrap(), DeploymentStart::Started);

    let mut tx_a = wide.begin().await.unwrap();
    let tx_a_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx_a)
        .await
        .unwrap();
    sqlx::query("UPDATE commander_records SET stress = 60 WHERE commander_id = $1")
        .bind(commander_id)
        .execute(&mut *tx_a)
        .await
        .unwrap();

    let repo = PostgresCommanderRepository::new(wide.clone());
    let task_cfg = cfg.clone();
    let resolver = tokio::spawn(async move {
        repo.resolve_deployment(d.deployment_id, commander_id, 0, 1, 0.0, &OutcomeType::Success, &task_cfg).await
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))"
        )
        .bind(tx_a_pid)
        .fetch_one(&wide)
        .await
        .unwrap();
        if waiting >= 1 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the resolver never blocked on the row lock");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tx_a.commit().await.unwrap();

    let eff = resolver.await.unwrap().expect("resolve failed");
    let after = PostgresCommanderRepository::new(pool.clone()).get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(after.stress, expected, "the concurrent write must be built on (stale would be {stale})");
    assert_eq!(eff.stress, expected);
    wide.close().await;

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4e-1: the card rows for one wallet, read at runtime (no `.sqlx/` entry).
/// (commander_id, deployment_id, session_id, campaign_id, commander_name, rank, foil)
async fn card_rows_4e1(pool: &sqlx::PgPool, wallet: &[u8]) -> Vec<(uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid, String, i16, bool)> {
    sqlx::query_as(
        "SELECT commander_id, deployment_id, session_id, campaign_id, commander_name, rank, foil
         FROM card_mint_pending WHERE player_wallet = $1"
    )
    .bind(wallet)
    .fetch_all(pool)
    .await
    .expect("card_mint_pending read failed")
}

/// 4e-1: a Wipeout marks the Commander KIA and queues one snapshot card of the record
/// and the fatal deployment, inside `resolve_deployment`'s transaction. Rank 1 on a
/// tier-1 mission with no ticker contribution is not Glorious.
#[tokio::test]
async fn test_mark_commander_kia_writes_kia_and_a_snapshot_card() {
    use sim_engine::game_types::OutcomeType;
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [21u8; 32];
    let (campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 30).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let cfg = sim_engine::config::SimConfig::default();
    let seeded = repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(seeded.rank, 1, "precondition");

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
    let eff = repo
        .resolve_deployment(d.deployment_id, commander_id, 2, 1, 0.0, &OutcomeType::Wipeout, &cfg)
        .await
        .expect("resolve failed");
    assert!(eff.is_kia);
    assert!(!eff.glorious);

    let after = repo.get_commander(commander_id).await.unwrap().unwrap();
    assert!(after.is_kia);
    let cards = card_rows_4e1(&pool, &wallet).await;
    assert_eq!(cards, vec![(commander_id, d.deployment_id, d.session_id, campaign_id, seeded.name.clone(), 1, false)]);
    // 4a-2: the card carries the dead Commander's Grade (Commander_Grade_System §4c).
    let card_grade: Option<String> = sqlx::query_scalar("SELECT commander_grade FROM card_mint_pending WHERE player_wallet = $1")
        .bind(&wallet[..])
        .fetch_one(&pool)
        .await
        .expect("card grade read failed");
    assert!(seeded.commander_grade.is_some(), "precondition: a generated Commander carries a Grade");
    assert_eq!(card_grade, seeded.commander_grade, "the card must copy the Commander's Grade");
    let deps = repo.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps[0].status, "resolved");
    assert_eq!(deps[0].outcome.as_deref(), Some(outcome_db_str(&OutcomeType::Wipeout)));

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4e-1: any Rank 5 death is Glorious, so its card is foil.
#[tokio::test]
async fn test_mark_commander_kia_rank_five_card_is_foil() {
    use sim_engine::game_types::OutcomeType;
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [22u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 30).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let cfg = sim_engine::config::SimConfig::default();
    // XP at the rank-5 threshold, so the resolve's rank_for_xp keeps rank 5.
    repo.update_rank_and_xp(commander_id, 5, 2600).await.unwrap();

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
    let eff = repo
        .resolve_deployment(d.deployment_id, commander_id, 2, 1, 0.0, &OutcomeType::Wipeout, &cfg)
        .await
        .expect("resolve failed");
    assert!(eff.is_kia && eff.glorious, "got {:?}", eff);
    let cards = card_rows_4e1(&pool, &wallet).await;
    assert_eq!(cards.len(), 1);
    assert_eq!((cards[0].5, cards[0].6), (5, true), "rank 5, foil");

    clean_wallet_4g1(&pool, &wallet).await;
}

/// Reviewer 4e-1 F1: the card stamps the locked rank. A Rank 5 Commander with low XP is
/// re-ranked by the resolve's rank `UPDATE`, which runs before the card insert in the same
/// transaction; the card must still say 5, the rank that made the death Glorious.
#[tokio::test]
async fn postgres_card_stamps_the_locked_rank() {
    use sim_engine::game_types::OutcomeType;
    let Some(pool) = test_pool().await else {
        eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test");
        return;
    };
    let wallet = [24u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 30).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let cfg = sim_engine::config::SimConfig::default();
    // An inconsistent pair: rank 5 with XP that rank_for_xp maps below 5.
    repo.update_rank_and_xp(commander_id, 5, 0).await.unwrap();

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
    let eff = repo
        .resolve_deployment(d.deployment_id, commander_id, 2, 1, 0.0, &OutcomeType::Wipeout, &cfg)
        .await
        .expect("resolve failed");
    let cards = card_rows_4e1(&pool, &wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;

    assert!(eff.is_kia && eff.glorious, "got {:?}", eff);
    assert!(eff.new_rank < 5, "precondition: the resolve re-ranks from XP, got {:?}", eff);
    assert_eq!(cards.len(), 1);
    assert_eq!((cards[0].5, cards[0].6), (5, true), "the card stamps the rank the Commander died at");
}

/// 4e-1: the KIA write and the card insert share the close's transaction. A second
/// resolve on an already-closed deployment runs both, then the close touches 0 rows,
/// and both roll back. The first resolve must not be a Wipeout, or the second would be
/// refused as KIA before it reached the writes.
#[tokio::test]
async fn test_mark_commander_kia_rolls_back_with_the_close() {
    use sim_engine::game_types::OutcomeType;
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [23u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 30).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let cfg = sim_engine::config::SimConfig::default();

    let d = new_deployment_4g1(commander_id, chrono::Utc::now());
    assert_eq!(repo.start_deployment(&d).await.unwrap(), DeploymentStart::Started);
    repo.resolve_deployment(d.deployment_id, commander_id, 0, 1, 0.0, &OutcomeType::Success, &cfg)
        .await
        .expect("first resolve failed");

    let r = repo
        .resolve_deployment(d.deployment_id, commander_id, 2, 1, 0.0, &OutcomeType::Wipeout, &cfg)
        .await;
    assert!(matches!(r, Err(mercs_server::repository::RepositoryError::NotFound)), "got {:?}", r);
    assert!(!repo.get_commander(commander_id).await.unwrap().unwrap().is_kia, "a failed close must roll back the KIA write");
    assert!(card_rows_4e1(&pool, &wallet).await.is_empty(), "a failed close must roll back the card");

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 4g-1 — `start_session` writes the header and its entry together or not at all. The
/// entry insert is forced to fail inside Postgres: `jsonb` rejects the `\u0000` escape.
#[tokio::test]
async fn start_session_leaves_no_header_when_the_entry_insert_fails() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let repo = PostgresInputLogRepository::new(pool.clone());
    let config_for = |sid: uuid::Uuid| shared::SessionConfig {
        session_id:    sid.to_string(),
        build_version: "test".into(),
        seed:          7,
        sector_id:     uuid::Uuid::nil(),
        campaign_id:   uuid::Uuid::nil(),
        sector_tier:   "Contested".into(),
        ruleset:       "standard_v1".into(),
    };
    let entry_with = |payload: serde_json::Value| shared::InputLogEntry {
        tick: 0, seq: 0, event_type: "session_start".into(), player_id: None, payload, narrative_event: None,
    };

    // Failure: the header insert runs, then the entry insert fails, and the header must not survive.
    let bad_sid = uuid::Uuid::new_v4();
    let r = repo.start_session(&config_for(bad_sid), &entry_with(serde_json::json!({ "x": "\u{0000}" }))).await;
    assert!(r.is_err(), "jsonb must reject a NUL escape; the forced failure did not fire");
    assert!(repo.get_session_config(&bad_sid).await.unwrap().is_none(), "orphan session_configs header left behind");
    assert!(repo.get_entries_by_session(&bad_sid).await.unwrap().is_empty());

    // Control: the same call with a clean payload writes both rows.
    let ok_sid = uuid::Uuid::new_v4();
    repo.start_session(&config_for(ok_sid), &entry_with(serde_json::json!({ "x": 1 }))).await.expect("start_session failed");
    assert!(repo.get_session_config(&ok_sid).await.unwrap().is_some());
    assert_eq!(repo.get_entries_by_session(&ok_sid).await.unwrap().len(), 1);

    // Post-test cleanup of the immutable tables.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE session_id = $1").bind(ok_sid).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1").bind(ok_sid).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL").execute(&pool).await.ok();
}

/// 4g-1 — migration 0016's CHECK rejects an out-of-range stress, and `create_commander`
/// clamps on insert and applies the Shattered invariant.
#[tokio::test]
async fn commander_stress_check_rejects_out_of_range() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [15u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 150).await;
    let repo = PostgresCommanderRepository::new(pool.clone());

    let rec = repo.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(rec.stress, 100, "create_commander must clamp 150 to 100");
    assert!(rec.is_shattered, "a record created at 100 is Shattered");

    let raw = sqlx::query("UPDATE commander_records SET stress = 101 WHERE commander_id = $1")
        .bind(commander_id)
        .execute(&pool)
        .await;
    let err = raw.expect_err("stress 101 must violate commander_records_stress_range");
    let code = err.as_database_error().and_then(|d| d.code()).map(|c| c.to_string());
    assert_eq!(code.as_deref(), Some("23514"), "expected a check violation, got {err}");

    clean_wallet_4g1(&pool, &wallet).await;
}

/// Brick B — migration 0018's CHECKs reject an origin and a faction bias the previous
/// binary's generator could write.
#[tokio::test]
async fn commander_roster_check_rejects_non_canonical_values() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [26u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 0).await;

    let cases = [
        ("origin", "UPDATE commander_records SET origin = 'Raccoon (Mod-Moped)' WHERE commander_id = $1", "commander_records_origin_canon"),
        ("faction_bias", "UPDATE commander_records SET faction_bias = 'GUN Loyalist' WHERE commander_id = $1", "commander_records_faction_bias_canon"),
        ("commander_grade", "UPDATE commander_records SET commander_grade = 'Legendary' WHERE commander_id = $1", "commander_records_commander_grade_canon"),
        ("fatal_flaw", "UPDATE commander_records SET fatal_flaw = 'Reckless' WHERE commander_id = $1", "commander_records_fatal_flaw_canon"),
        ("specialization", "UPDATE commander_records SET specialization = 'Marksmans Eye' WHERE commander_id = $1", "commander_records_specialization_canon"),
    ];
    for (column, sql, constraint) in cases {
        let err = sqlx::query(sql)
            .bind(commander_id)
            .execute(&pool)
            .await
            .expect_err(&format!("a non-canonical {column} must violate {constraint}"));
        let code = err.as_database_error().and_then(|d| d.code()).map(|c| c.to_string());
        assert_eq!(code.as_deref(), Some("23514"), "{constraint}: expected a check violation, got {err}");
    }

    clean_wallet_4g1(&pool, &wallet).await;
}

/// Brick B drift guard — every roster value passes migration 0018's CHECKs, so the SQL
/// lists and `sim_engine::roster` cannot diverge silently.
#[tokio::test]
async fn commander_roster_check_accepts_every_canonical_value() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [27u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 0).await;

    for origin in CommanderOrigin::ALL {
        sqlx::query("UPDATE commander_records SET origin = $2 WHERE commander_id = $1")
            .bind(commander_id)
            .bind(origin.as_str())
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("origin {:?} rejected by commander_records_origin_canon: {e}", origin.as_str()));
    }
    for bias in FactionBias::ALL {
        sqlx::query("UPDATE commander_records SET faction_bias = $2 WHERE commander_id = $1")
            .bind(commander_id)
            .bind(bias.as_str())
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("faction_bias {:?} rejected by commander_records_faction_bias_canon: {e}", bias.as_str()));
    }
    for grade in CommanderGrade::ALL {
        sqlx::query("UPDATE commander_records SET commander_grade = $2 WHERE commander_id = $1")
            .bind(commander_id)
            .bind(grade.as_str())
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("commander_grade {:?} rejected by commander_records_commander_grade_canon: {e}", grade.as_str()));
    }
    for flaw in CommanderFlaw::ALL {
        sqlx::query("UPDATE commander_records SET fatal_flaw = $2 WHERE commander_id = $1")
            .bind(commander_id)
            .bind(flaw.as_str())
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("fatal_flaw {:?} rejected by commander_records_fatal_flaw_canon: {e}", flaw.as_str()));
    }
    for spec in CommanderSpecialization::ALL {
        sqlx::query("UPDATE commander_records SET specialization = $2 WHERE commander_id = $1")
            .bind(commander_id)
            .bind(spec.as_str())
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("specialization {:?} rejected by commander_records_specialization_canon: {e}", spec.as_str()));
    }

    clean_wallet_4g1(&pool, &wallet).await;
}

/// The `'…'::text` literals of a `pg_get_constraintdef` IN-list, with `''` unescaped.
fn constraint_literals(def: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut rest = def;
    while let Some(start) = rest.find('\'') {
        let body = &rest[start + 1..];
        let mut value = String::new();
        let mut chars = body.char_indices().peekable();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if matches!(chars.peek(), Some((_, '\''))) {
                    chars.next();
                    value.push('\'');
                } else {
                    end = Some(i);
                    break;
                }
            } else {
                value.push(c);
            }
        }
        let end = end.unwrap_or_else(|| panic!("unterminated literal in {def}"));
        out.insert(value);
        rest = &body[end + 1..];
    }
    out
}

/// The `*_canon` CHECKs each roster table must carry: constraint name → the roster enum's
/// strings. One row per table; a new roster table adds a row here (7a-2: `advisor_records`).
fn roster_check_expectations() -> Vec<(&'static str, Vec<(&'static str, std::collections::BTreeSet<String>)>)> {
    fn strings(all: impl IntoIterator<Item = &'static str>) -> std::collections::BTreeSet<String> {
        all.into_iter().map(str::to_string).collect()
    }
    vec![
        ("commander_records", vec![
            ("commander_records_origin_canon", strings(CommanderOrigin::ALL.map(CommanderOrigin::as_str))),
            ("commander_records_faction_bias_canon", strings(FactionBias::ALL.map(FactionBias::as_str))),
            ("commander_records_commander_grade_canon", strings(CommanderGrade::ALL.map(CommanderGrade::as_str))),
            ("commander_records_fatal_flaw_canon", strings(CommanderFlaw::ALL.map(CommanderFlaw::as_str))),
            ("commander_records_specialization_canon", strings(CommanderSpecialization::ALL.map(CommanderSpecialization::as_str))),
            ("commander_records_acquired_via_canon", strings(AcquiredVia::ALL.map(AcquiredVia::as_str))),
        ]),
        ("foreman_records", vec![
            ("foreman_records_origin_canon", strings(CommanderOrigin::ALL.map(CommanderOrigin::as_str))),
            ("foreman_records_faction_bias_canon", strings(FactionBias::ALL.map(FactionBias::as_str))),
            ("foreman_records_specialization_canon", strings(ForemanSpecialization::ALL.map(ForemanSpecialization::as_str))),
            ("foreman_records_fatal_flaw_canon", strings(ForemanFlaw::ALL.map(ForemanFlaw::as_str))),
            ("foreman_records_foreman_grade_canon", strings(CommanderGrade::ALL.map(CommanderGrade::as_str))),
            ("foreman_records_acquired_via_canon", strings(AcquiredVia::ALL.into_iter().filter(|v| *v != AcquiredVia::Replacement).map(AcquiredVia::as_str))),
        ]),
        // 5a-1 (0030): only Commanders can be a replacement (ODQ-33); the filter is this
        // test's own oracle, not read from code.
        ("section_records", vec![
            ("section_records_acquired_via_canon", strings(AcquiredVia::ALL.into_iter().filter(|v| *v != AcquiredVia::Replacement).map(AcquiredVia::as_str))),
        ]),
        ("crew_records", vec![
            ("crew_records_acquired_via_canon", strings(AcquiredVia::ALL.into_iter().filter(|v| *v != AcquiredVia::Replacement).map(AcquiredVia::as_str))),
        ]),
        ("advisor_records", vec![
            ("advisor_records_origin_canon", strings(CommanderOrigin::ALL.map(CommanderOrigin::as_str))),
            ("advisor_records_faction_bias_canon", strings(FactionBias::ALL.map(FactionBias::as_str))),
            ("advisor_records_specialization_canon", strings(AdvisorSpecialization::ALL.map(AdvisorSpecialization::as_str))),
            ("advisor_records_quirk_canon", strings(AdvisorQuirk::ALL.map(AdvisorQuirk::as_str))),
        ]),
        ("augmentee_records", vec![
            ("augmentee_records_combat_augment_canon", strings(Augment::ALL.into_iter().filter(|a| a.catalogue() == AugmentCatalogue::Combat).map(Augment::as_str))),
            ("augmentee_records_industrial_augment_canon", strings(Augment::ALL.into_iter().filter(|a| a.catalogue() == AugmentCatalogue::Industrial).map(Augment::as_str))),
            ("augmentee_records_fitting_source_canon", strings(FittingSource::ALL.map(FittingSource::as_str))),
            ("augmentee_records_casualty_class_canon", strings(CasualtyClass::ALL.map(CasualtyClass::as_str))),
        ]),
        // 5a-5 (0032, ODQ-47): the death card copies the Commander's provenance, so it lists
        // every value the Commander's CHECK does.
        ("card_mint_pending", vec![
            ("card_mint_pending_acquired_via_canon", strings(AcquiredVia::ALL.map(AcquiredVia::as_str))),
        ]),
    ]
}

/// Asserts that `table`'s `*_canon` CHECKs are exactly `expected`, each listing exactly its
/// roster's strings in both directions. Reads through `conn` so a test transaction can
/// mutate a constraint first (7a-1 N4).
async fn assert_roster_checks_match(
    conn: &mut sqlx::PgConnection,
    table: &str,
    expected: &[(&'static str, std::collections::BTreeSet<String>)],
) {
    use std::collections::BTreeSet;
    let rows = sqlx::query!(
        r#"SELECT conname::text AS "conname!", pg_get_constraintdef(oid) AS "def!"
           FROM pg_constraint
           WHERE conrelid = $1::text::regclass AND contype = 'c' AND conname::text LIKE '%\_canon'"#,
        table
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap_or_else(|e| panic!("{table}: pg_constraint read failed: {e}"));

    let names: BTreeSet<&str> = rows.iter().map(|r| r.conname.as_str()).collect();
    let want_names: BTreeSet<&str> = expected.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, want_names, "{table}: the *_canon CHECKs must be exactly the {} guarded here", expected.len());

    for (name, roster) in expected {
        let def = &rows.iter().find(|r| r.conname == *name).unwrap().def;
        let sql = constraint_literals(def);
        let sql_only: Vec<_> = sql.difference(roster).collect();
        let roster_only: Vec<_> = roster.difference(&sql).collect();
        assert!(sql_only.is_empty(), "{table}.{name}: in SQL, not in roster: {sql_only:?}");
        assert!(roster_only.is_empty(), "{table}.{name}: in roster, not in SQL: {roster_only:?}");
    }
}

/// 4d-2 (Reviewer Brick B F2, CF3), generalised per table in 7a-1: every `*_canon` CHECK on
/// every roster table lists exactly its roster enum's strings, in both directions, and each
/// table has exactly the CHECKs listed in `roster_check_expectations`.
#[tokio::test]
async fn roster_checks_match_the_roster_both_ways() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let mut conn = pool.acquire().await.expect("acquire failed");
    for (table, expected) in roster_check_expectations() {
        assert_roster_checks_match(&mut conn, table, &expected).await;
    }
}

/// 4a-2 — `commander_grade` round-trips through `create_commander`, `get_commander` and
/// `list_commanders_by_campaign` unchanged, and is never recomputed from the traits
/// (4d-2, Reviewer 4a-2 F3: a hand-built record, not the generator).
#[tokio::test]
async fn test_commander_grade_round_trips() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [28u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;

    // The fixture's traits grade to Standard Issue (TQS 10 + 8 + 12 = 30); it is stored as
    // Heroic, so a read that recomputed the Grade from the traits would fail.
    use sim_engine::grade::commander_tqs;
    let tqs = commander_tqs(CommanderOrigin::Duck, CommanderSpecialization::Cannoneer, CommanderFlaw::Bloodthirsty);
    assert_eq!(tqs, 30);
    assert_eq!(CommanderGrade::from_tqs(tqs), CommanderGrade::StandardIssue, "the fixture must stay sharp");

    let campaign_id = seed_account_and_campaign(&pool, wallet, 0x4D02_i64).await;
    let commander_id = uuid::Uuid::new_v4();
    let repo = PostgresCommanderRepository::new(pool.clone());
    repo.create_commander(CommanderRecord {
        commander_id,
        campaign_id,
        player_wallet:   wallet.to_vec(),
        name:            "Cpl. Mallardus Drek".into(),
        rank:            1,
        xp:              0,
        stress:          0,
        origin:          CommanderOrigin::Duck.as_str().into(),
        faction_bias:    FactionBias::GunLoyalist.as_str().into(),
        specialization:  CommanderSpecialization::Cannoneer.as_str().into(),
        fatal_flaw:      CommanderFlaw::Bloodthirsty.as_str().into(),
        veteran_trait:   None,
        is_shattered:    false,
        is_kia:          false,
        prng_seed_state: 0x4D02_i64,
        commander_grade: Some(CommanderGrade::Heroic.as_str().into()),
        created_at:      chrono::Utc::now(),
        updated_at:      chrono::Utc::now(),
    }).await.expect("create_commander failed");

    let fetched = repo.get_commander(commander_id).await.unwrap().expect("commander not found");
    assert_eq!(fetched.commander_grade.as_deref(), Some("Heroic"), "get_commander must return the stored Grade");

    let listed = repo.list_commanders_by_campaign(campaign_id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].commander_grade.as_deref(), Some("Heroic"), "list_commanders_by_campaign must return the stored Grade");

    clean_wallet_4g1(&pool, &wallet).await;
}

/// Seeds `wallet`'s account, a fresh campaign and the wallet's membership of it; returns
/// the campaign id. The membership row is what lets `clean_wallet_4g1` find the campaign
/// when every roster insert a test makes is refused (Reviewer 7c-1 F1).
async fn seed_account_and_campaign(pool: &sqlx::PgPool, wallet: [u8; 32], map_seed: i64) -> uuid::Uuid {
    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
         VALUES ($1, 0, 0) ON CONFLICT (wallet_address) DO NOTHING"
    )
    .bind(wallet.to_vec())
    .execute(pool)
    .await
    .expect("player_accounts FK seed failed");
    let campaign_id = PostgresCampaignRepository::new(pool.clone())
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed })
        .await
        .expect("create campaign failed")
        .campaign_id;
    PostgresMembershipRepository::new(pool.clone())
        .add_member(&wallet, campaign_id, None, None)
        .await
        .expect("membership seed failed");
    campaign_id
}

/// A hand-built Foreman: Standard Issue traits (TQS 10 + 8 + 12 = 30), stored as Heroic so
/// a read that recomputed the Grade would fail; an apostrophe in the flaw string.
fn hand_built_foreman(wallet: [u8; 32], campaign_id: uuid::Uuid) -> mercs_server::repository::ForemanRecord {
    use sim_engine::grade::foreman_tqs;
    let tqs = foreman_tqs(CommanderOrigin::Duck, ForemanSpecialization::BlackMarketAuger, ForemanFlaw::SeamFever);
    assert_eq!(CommanderGrade::from_tqs(tqs), CommanderGrade::StandardIssue, "the fixture must stay sharp");
    mercs_server::repository::ForemanRecord {
        foreman_id:      uuid::Uuid::new_v4(),
        campaign_id,
        player_wallet:   wallet.to_vec(),
        name:            "Mallardus Drek".into(),
        rank:            1,
        xp:              0,
        stress:          17,
        origin:          CommanderOrigin::Duck.as_str().into(),
        faction_bias:    FactionBias::VoidCallerCollaborationist.as_str().into(),
        specialization:  ForemanSpecialization::BlackMarketAuger.as_str().into(),
        fatal_flaw:      ForemanFlaw::SeamFever.as_str().into(),
        foreman_grade:   CommanderGrade::Heroic.as_str().into(),
        is_shattered:    false,
        is_kia:          false,
        prng_seed_state: 0x7A01_i64,
        created_at:      chrono::Utc::now(),
        updated_at:      chrono::Utc::now(),
    }
}

/// 7a-1 — every roster field and `foreman_grade` survive `create_foreman`, `get_foreman`
/// and `list_foremen_by_campaign` unchanged.
#[tokio::test]
async fn test_foreman_record_round_trips() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [29u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    let campaign_id = seed_account_and_campaign(&pool, wallet, 0x7A01_i64).await;

    let repo = PostgresForemanRepository::new(pool.clone());
    let mut record = hand_built_foreman(wallet, campaign_id);
    record.fatal_flaw = ForemanFlaw::ProspectorsItch.as_str().into();
    repo.create_foreman(record.clone()).await.expect("create_foreman failed");

    let fetched = repo.get_foreman(record.foreman_id).await.unwrap().expect("foreman not found");
    let listed = repo.list_foremen_by_campaign(campaign_id).await.unwrap();
    assert_eq!(listed.len(), 1);
    for (path, got) in [("get_foreman", &fetched), ("list_foremen_by_campaign", &listed[0])] {
        assert_eq!(got.foreman_id, record.foreman_id, "{path}");
        assert_eq!(got.campaign_id, campaign_id, "{path}");
        assert_eq!(got.player_wallet, wallet.to_vec(), "{path}");
        assert_eq!(got.name, "Mallardus Drek", "{path}");
        assert_eq!(got.stress, 17, "{path}");
        assert_eq!(got.origin, "Duck", "{path}");
        assert_eq!(got.faction_bias, "Void-Caller (Collaborationist)", "{path}");
        assert_eq!(got.specialization, "Black Market Auger", "{path}");
        assert_eq!(got.fatal_flaw, "Prospector's Itch", "{path}");
        assert_eq!(got.foreman_grade, "Heroic", "{path}: the stored Grade, never recomputed");
        assert_eq!(got.prng_seed_state, 0x7A01_i64, "{path}");
    }
    assert!(repo.get_foreman(uuid::Uuid::new_v4()).await.unwrap().is_none());

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 7a-1 — `0022`'s CHECKs refuse a Commander-only flaw and a Commander-only specialization.
#[tokio::test]
async fn test_foreman_records_reject_non_canon_roster_values() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [30u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    let campaign_id = seed_account_and_campaign(&pool, wallet, 0x7A02_i64).await;

    let repo = PostgresForemanRepository::new(pool.clone());
    let record = hand_built_foreman(wallet, campaign_id);
    repo.create_foreman(record.clone()).await.expect("a canonical Foreman must insert");

    let cases = [
        ("fatal_flaw", "UPDATE foreman_records SET fatal_flaw = 'Bloodthirsty' WHERE foreman_id = $1", "foreman_records_fatal_flaw_canon"),
        ("specialization", "UPDATE foreman_records SET specialization = 'Cannoneer' WHERE foreman_id = $1", "foreman_records_specialization_canon"),
    ];
    for (column, sql, constraint) in cases {
        let err = sqlx::query(sql)
            .bind(record.foreman_id)
            .execute(&pool)
            .await
            .expect_err(&format!("a Commander-only {column} must violate {constraint}"));
        let db = err.as_database_error().unwrap_or_else(|| panic!("{constraint}: not a database error: {err}"));
        assert_eq!(db.code().as_deref(), Some("23514"), "{constraint}: expected a check violation, got {err}");
        assert_eq!(db.constraint(), Some(constraint), "{column}: wrong constraint");
    }

    clean_wallet_4g1(&pool, &wallet).await;
}

/// A hand-built Advisor (7a-2) with the given specialization string.
fn hand_built_advisor(wallet: [u8; 32], campaign_id: uuid::Uuid, specialization: &str) -> AdvisorRecord {
    AdvisorRecord {
        advisor_id:      uuid::Uuid::new_v4(),
        campaign_id,
        player_wallet:   wallet.to_vec(),
        name:            "Vance Darwin-Macaque".into(),
        rank:            1,
        origin:          CommanderOrigin::ScienceApe.as_str().into(),
        faction_bias:    FactionBias::JumpLaneRestorer.as_str().into(),
        specialization:  specialization.into(),
        quirk:           AdvisorQuirk::VanityMetrics.as_str().into(),
        prng_seed_state: 0x7A2_i64,
        created_at:      chrono::Utc::now(),
    }
}

/// 7a-2 — every roster field survives `create_advisor`, `get_advisor` and
/// `list_advisors_by_campaign` unchanged. A Research spec proves the CHECK holds all 14;
/// a second record carries the `&` string.
#[tokio::test]
async fn postgres_advisor_repository_round_trips() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [31u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    let campaign_id = seed_account_and_campaign(&pool, wallet, 0x7A21_i64).await;

    let repo = PostgresAdvisorRepository::new(pool.clone());
    let research = hand_built_advisor(wallet, campaign_id, "Head of Reverse Engineering");
    let ampersand = hand_built_advisor(wallet, campaign_id, "VP of Tectonic Risk & Planetology");
    repo.create_advisor(research.clone()).await.expect("a Research Advisor must insert");
    repo.create_advisor(ampersand.clone()).await.expect("the & string must insert");

    let fetched = repo.get_advisor(research.advisor_id).await.unwrap().expect("advisor not found");
    let mut listed = repo.list_advisors_by_campaign(campaign_id).await.unwrap();
    assert_eq!(listed.len(), 2);
    listed.sort_by_key(|r| r.specialization.clone());
    assert_eq!(listed[1].specialization, "VP of Tectonic Risk & Planetology");
    assert_eq!(listed[1].advisor_id, ampersand.advisor_id);
    for (path, got) in [("get_advisor", &fetched), ("list_advisors_by_campaign", &listed[0])] {
        assert_eq!(got.advisor_id, research.advisor_id, "{path}");
        assert_eq!(got.campaign_id, campaign_id, "{path}");
        assert_eq!(got.player_wallet, wallet.to_vec(), "{path}");
        assert_eq!(got.name, "Vance Darwin-Macaque", "{path}");
        assert_eq!(got.rank, 1, "{path}");
        assert_eq!(got.origin, "Science Ape", "{path}");
        assert_eq!(got.faction_bias, "Jump Lane Restorer", "{path}");
        assert_eq!(got.specialization, "Head of Reverse Engineering", "{path}");
        assert_eq!(got.quirk, "Vanity Metrics", "{path}");
        assert_eq!(got.prng_seed_state, 0x7A2_i64, "{path}");
    }
    assert!(repo.get_advisor(uuid::Uuid::new_v4()).await.unwrap().is_none());

    clean_wallet_4g1(&pool, &wallet).await;
}

/// 7a-2 — `0023`'s CHECK refuses an off-roster Quirk.
#[tokio::test]
async fn advisor_records_refuses_an_off_roster_quirk() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [31u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    let campaign_id = seed_account_and_campaign(&pool, wallet, 0x7A22_i64).await;

    let r = hand_built_advisor(wallet, campaign_id, "The Financial Patron");
    let res = sqlx::query(
        "INSERT INTO advisor_records (advisor_id, campaign_id, player_wallet, name, origin,
             faction_bias, specialization, quirk, prng_seed_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'Not A Quirk', $8)"
    )
    .bind(r.advisor_id)
    .bind(r.campaign_id)
    .bind(&r.player_wallet)
    .bind(&r.name)
    .bind(&r.origin)
    .bind(&r.faction_bias)
    .bind(&r.specialization)
    .bind(r.prng_seed_state)
    .execute(&pool)
    .await;
    clean_wallet_4g1(&pool, &wallet).await;
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("campaign cleanup failed");

    let err = res.expect_err("an off-roster Quirk must violate advisor_records_quirk_canon");
    let db = err.as_database_error().unwrap_or_else(|| panic!("not a database error: {err}"));
    assert_eq!(db.code().as_deref(), Some("23514"), "expected a check violation, got {err}");
    assert_eq!(db.constraint(), Some("advisor_records_quirk_canon"));
}

/// 7a-2 — deleting the campaign deletes its Advisors (`ON DELETE CASCADE`, ODQ-39 (g)).
#[tokio::test]
async fn advisor_records_cascade_with_their_campaign() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [31u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    let campaign_id = seed_account_and_campaign(&pool, wallet, 0x7A23_i64).await;

    let r = hand_built_advisor(wallet, campaign_id, "Director of Overland Logistics");
    PostgresAdvisorRepository::new(pool.clone()).create_advisor(r.clone()).await.expect("create_advisor failed");
    let count = |id: uuid::Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM advisor_records WHERE advisor_id = $1")
                .bind(id).fetch_one(&pool).await.expect("count failed")
        }
    };
    assert_eq!(count(r.advisor_id).await, 1);

    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("campaign delete failed");
    assert_eq!(count(r.advisor_id).await, 0, "the Advisor must go with its campaign");

    clean_wallet_4g1(&pool, &wallet).await;
}

/// Local copy of the AAR test's fixture (seed_override 42, max_ticks 25).
fn alpha_vs_scout_payload() -> serde_json::Value {
    serde_json::json!({
        "section": {
            "id": "alpha",
            "name": "Alpha Section",
            "max_strength": 5,
            "current_strength": 5,
            "individual_hp": 10,
            "accuracy": 60,
            "evasion": 20,
            "weapon": {
                "name": "Laser Carbine",
                "ap": 4,
                "base_damage": 8,
                "tag": "Laser",
                "accuracy": 0
            },
            "armor_at": 3,
            "armor_tag": "LightArmor"
        },
        "vehicle": {
            "id": "scout-1",
            "name": "Armored Scout",
            "hp": 40,
            "max_hp": 40,
            "at": 6,
            "armor_tag": "HeavyArmor",
            "evasion": 10,
            "weapons": [{
                "name": "Scout Cannon",
                "ap": 6,
                "base_damage": 12,
                "tag": "Slug",
                "accuracy": 0
            }]
        },
        "seed_override": 42,
        "max_ticks": 25,
        "combat_initiation_type": "SPOTTED"
    })
}

/// GAP-34 gate: a real route writes the input log in Postgres, the AAR replays it,
/// and the integrity digest moves when a logged entry is altered.
#[tokio::test]
async fn combat_route_logs_to_postgres_and_integrity_detects_tamper() {
    use std::sync::Arc;
    use axum::{body::Body, http::{Request, StatusCode}};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let pool = match test_pool().await {
        Some(p) => p,
        None => {
            eprintln!("TEST_DATABASE_URL not set — skipping combat route input log test");
            return;
        }
    };

    let mut state = mercs_server::state::AppState::new(Arc::new(mercs_server::config::Config::default()));
    state.input_log_repo = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    let app = mercs_server::routes::router(Arc::new(state));

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
    let get = |uri: String| Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap();

    // Step 1: resolve a combat through the real route.
    let req = Request::builder()
        .method("POST")
        .uri("/api/combat/resolve")
        .header("content-type", "application/json")
        .body(Body::from(alpha_vs_scout_payload().to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "POST /api/combat/resolve failed");
    let resolved = body_json(resp).await;
    let sid_str = resolved["session_id"].as_str().expect("session_id missing").to_string();
    let sid = uuid::Uuid::parse_str(&sid_str).unwrap();
    let original_report = resolved["report"].clone();

    // Step 2: exactly one session_start row, carrying the request and the timestamp.
    let repo = PostgresInputLogRepository::new(pool.clone());
    let entries = repo.get_entries_by_session(&sid).await.expect("get_entries_by_session failed");
    assert_eq!(entries.len(), 1, "the route must log exactly one entry (session_start)");
    assert_eq!(entries[0].event_type, "session_start");
    assert_eq!((entries[0].tick, entries[0].seq), (0, 0));
    assert_eq!(entries[0].payload["request"]["seed_override"], 42);
    assert!(entries[0].payload["timestamp"].is_string(), "session_start must carry the timestamp");

    // Step 3: the AAR replays the logged input to the same report.
    let resp = app.clone().oneshot(get(format!("/api/combat/aar/{sid_str}"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET /api/combat/aar/:id failed");
    let aar = body_json(resp).await;
    assert_eq!(aar["report"], original_report, "AAR replay diverged from the original report");

    // Step 4: integrity digest before tampering.
    let resp = app.clone().oneshot(get(format!("/api/session/{sid_str}/integrity"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET /api/session/:id/integrity failed");
    let h1 = body_json(resp).await["log_hash"].as_str().expect("log_hash missing").to_string();

    // Step 5: tamper with the logged input. Re-enable triggers before checking the result.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers failed");
    let tamper = sqlx::query(
        "UPDATE input_logs SET payload = jsonb_set(payload, '{request,seed_override}', '43') WHERE session_id = $1",
    )
    .bind(sid)
    .execute(&pool)
    .await;
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("enable triggers failed");
    assert_eq!(tamper.expect("tamper UPDATE failed").rows_affected(), 1);

    // Step 6: the digest must move.
    let resp = app.clone().oneshot(get(format!("/api/session/{sid_str}/integrity"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h2 = body_json(resp).await["log_hash"].as_str().expect("log_hash missing").to_string();
    assert_ne!(h1, h2, "integrity digest must change when a logged entry is altered");

    // Step 7: an unknown session is 404.
    let resp = app
        .clone()
        .oneshot(get(format!("/api/combat/aar/{}", uuid::Uuid::new_v4())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Step 8: cleanup.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers failed");
    let del_logs = sqlx::query("DELETE FROM input_logs WHERE session_id = $1")
        .bind(sid).execute(&pool).await;
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("enable triggers failed");
    del_logs.expect("cleanup input_logs failed");
    sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL")
        .execute(&pool).await.expect("disable triggers failed");
    let del_cfg = sqlx::query("DELETE FROM session_configs WHERE session_id = $1")
        .bind(sid).execute(&pool).await;
    sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL")
        .execute(&pool).await.expect("enable triggers failed");
    del_cfg.expect("cleanup session_configs failed");
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

    let mut state = AppState::new(Arc::new(Config::default()));
    state.convoy_repo    = convoy_repo.clone();
    state.sector_repo    = sector_repo.clone();
    state.timer_repo     = timer_repo.clone();
    state.input_log_repo = log_repo.clone();

    // Inject WS sender for this campaign — poll_expiry_once will fire into it
    let (tx, mut rx) = mpsc::channel::<ConvoyEvent>(64);
    state.convoy_event_senders.insert(campaign_id, tx);

    let state = Arc::new(state);

    // Seed SectorState so get_sector returns campaign_id
    sector_repo.upsert_sector(SectorState {
        sector_id,
        campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
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
    poll_expiry_once(&state, &mut mercs_server::convoy_expiry::ExpiryWarns::new()).await;

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
    // land_convoy writes the convoy_arrived row to Postgres (GAP-44), whatever log repo the state holds.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();

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

// ── Brick 4g-1: POST /api/mission/resolve wired to Commander records ────────

/// Inserts a live session for `wallet` and returns its bearer token.
fn insert_session(state: &mercs_server::state::AppState, wallet: [u8; 32]) -> String {
    let wallet_b58 = bs58::encode(wallet).into_string();
    let token_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    state.sessions.insert(token_id.clone(), shared::SessionToken {
        token_id:       token_id.clone(),
        account_id:     wallet_b58.clone(),
        wallet_address: wallet_b58,
        issued_at:      now,
        expires_at:     now + 7200,
    });
    token_id
}

/// Test-only failure seam: delegates reads to `inner`, and fails every write while
/// `fail_writes` is set. Lives here, not in production code.
struct FailingInputLog {
    inner: std::sync::Arc<dyn InputLogRepository + Send + Sync>,
    fail_writes: bool,
}

impl FailingInputLog {
    fn refuse(&self) -> Result<(), mercs_server::repository::RepositoryError> {
        if self.fail_writes {
            Err(mercs_server::repository::RepositoryError::Internal("FailingInputLog: forced write failure".into()))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl InputLogRepository for FailingInputLog {
    async fn append_entry(&self, session_id: &uuid::Uuid, entry: &shared::InputLogEntry) -> Result<(), mercs_server::repository::RepositoryError> {
        self.refuse()?;
        self.inner.append_entry(session_id, entry).await
    }
    async fn get_entries_by_session(&self, session_id: &uuid::Uuid) -> Result<Vec<shared::InputLogEntry>, mercs_server::repository::RepositoryError> {
        self.inner.get_entries_by_session(session_id).await
    }
    async fn save_session_config(&self, config: &shared::SessionConfig) -> Result<(), mercs_server::repository::RepositoryError> {
        self.refuse()?;
        self.inner.save_session_config(config).await
    }
    async fn get_session_config(&self, session_id: &uuid::Uuid) -> Result<Option<shared::SessionConfig>, mercs_server::repository::RepositoryError> {
        self.inner.get_session_config(session_id).await
    }
    async fn append_campaign_entry(&self, campaign_id: &uuid::Uuid, entry: &shared::InputLogEntry) -> Result<(), mercs_server::repository::RepositoryError> {
        self.refuse()?;
        self.inner.append_campaign_entry(campaign_id, entry).await
    }
    async fn start_session(&self, config: &shared::SessionConfig, entry: &shared::InputLogEntry) -> Result<(), mercs_server::repository::RepositoryError> {
        self.refuse()?;
        self.inner.start_session(config, entry).await
    }
}

/// One deployable unit, serialised from the sim types so the body always matches them.
fn mission_unit_json() -> serde_json::Value {
    use sim_engine::game_types::{Unit, UnitArchetype, UnitDefinition, UnitStatus};
    serde_json::to_value(Unit {
        id:         "u1".into(),
        name:       "Grunt".into(),
        definition: UnitDefinition {
            archetype:         UnitArchetype::Vanguard,
            emoji:             "V".into(),
            hiring_cost:       0,
            base_skill:        5,
            monthly_upkeep:    0,
            upkeep_extras:     String::new(),
            hardpoints:        vec![],
            success_mod:       10,
            damage_shield_mod: 0,
            loot_bonus:        0,
            async_ability:     String::new(),
            passive_trait:     String::new(),
            flavor_text:       String::new(),
        },
        skill:      6,
        xp:         0,
        current_hp: 10,
        max_hp:     10,
        status:     UnitStatus::Ready,
        equipment:  vec![],
    })
    .unwrap()
}

fn mission_body(commander_id: uuid::Uuid, section_ids: &[uuid::Uuid]) -> serde_json::Value {
    serde_json::json!({
        "squad": { "units": [mission_unit_json()] },
        "mission_id": "GUARD_DUTY",
        "commander_id": commander_id,
        "section_ids": section_ids,
    })
}

/// A Section owned by `wallet` in `campaign_id`, written through `state.section_repo`
/// (in-memory by default; a Postgres-backed route test swaps the repository first).
async fn seed_section(state: &mercs_server::state::AppState, wallet: [u8; 32], campaign_id: uuid::Uuid) -> uuid::Uuid {
    let section_id = uuid::Uuid::new_v4();
    state.section_repo.create_section(SectionRecord {
        section_id,
        campaign_id,
        player_wallet: wallet.to_vec(),
        name:          "Delta Section".into(),
        headcount:     8,
        loadout:       serde_json::json!({}),
        xp:            0,
        created_at:    chrono::Utc::now(),
        updated_at:    chrono::Utc::now(),
    }).await.expect("create_section failed");
    section_id
}

/// A Section with the same owner and campaign as the stored Commander `commander_id`.
async fn seed_section_for(state: &mercs_server::state::AppState, commander_id: uuid::Uuid) -> uuid::Uuid {
    let rec = state.commander_repo.get_commander(commander_id).await.unwrap().expect("seeded Commander");
    let wallet: [u8; 32] = rec.player_wallet.as_slice().try_into().expect("32-byte wallet");
    seed_section(state, wallet, rec.campaign_id).await
}

/// The `error` string of a route's JSON error body.
fn error_message(bytes: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("not a JSON error body ({e}): {}", String::from_utf8_lossy(bytes)));
    v["error"].as_str().expect("no error string").to_string()
}

/// An `Active` campaign in `state.campaign_repo` (5a-3b: missions need one, S3). Returns its id.
async fn seed_active_campaign(state: &mercs_server::state::AppState) -> uuid::Uuid {
    let id = state.campaign_repo
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 1 })
        .await
        .expect("create_campaign failed")
        .campaign_id;
    let activated = state.campaign_repo
        .activate_campaign(id, chrono::Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
        .await
        .expect("activate_campaign failed");
    assert!(activated, "the seeded campaign must activate");
    id
}

/// A generated in-memory Commander owned by `wallet`, in a fresh `Active` campaign. Returns its id.
async fn seed_in_memory_commander(state: &mercs_server::state::AppState, wallet: [u8; 32], mutate: impl FnOnce(&mut CommanderRecord)) -> uuid::Uuid {
    let campaign_id = seed_active_campaign(state).await;
    let mut rec = mercs_server::commander_gen::generate_commander(wallet.to_vec(), campaign_id, 42);
    mutate(&mut rec);
    let id = rec.commander_id;
    state.commander_repo.create_commander(rec).await.expect("create_commander failed");
    id
}

/// POSTs to the mission route; returns the status and the raw body bytes.
async fn post_mission(app: axum::Router, token: Option<&str>, body: &serde_json::Value) -> (axum::http::StatusCode, Vec<u8>) {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/mission/resolve")
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let app = mercs_server::routes::with_test_peer(app, mercs_server::routes::TEST_PEER);
    let resp = app.oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, bytes)
}

fn default_state() -> mercs_server::state::AppState {
    mercs_server::state::AppState::new(std::sync::Arc::new(mercs_server::config::Config::default()))
}

/// A Postgres route test starts here, so the join and the routes read one set of stores
/// (code `CLAUDE.md` §5's trap): the six join-read repositories and `campaign_join_repo`
/// swap together, plus the input log. Everything else stays in memory.
fn pg_route_state(pool: &sqlx::PgPool) -> mercs_server::state::AppState {
    use std::sync::Arc;
    let mut state = default_state();
    state.campaign_repo = Arc::new(PostgresCampaignRepository::new(pool.clone()));
    state.membership_repo = Arc::new(PostgresMembershipRepository::new(pool.clone()));
    state.commander_repo = Arc::new(PostgresCommanderRepository::new(pool.clone()));
    state.section_repo = Arc::new(PostgresSectionRepository::new(pool.clone()));
    state.foreman_repo = Arc::new(PostgresForemanRepository::new(pool.clone()));
    state.crew_repo = Arc::new(PostgresCrewRepository::new(pool.clone()));
    state.campaign_join_repo = Arc::new(PostgresCampaignJoinRepository::new(pool.clone()));
    state.input_log_repo = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    state
}

/// GET of the integrity route; returns the status only.
async fn get_integrity_status(app: axum::Router, session_id: uuid::Uuid) -> axum::http::StatusCode {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/session/{session_id}/integrity"))
        .body(Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

/// INF-3a (GAP-46): an unknown session is `404`, not `400`.
#[tokio::test]
async fn integrity_route_answers_404_for_an_unknown_session() {
    let app = mercs_server::routes::router(std::sync::Arc::new(default_state()));
    let status = get_integrity_status(app, uuid::Uuid::new_v4()).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
}

/// INF-3a (GAP-46): a log-store failure is `500`, not `400`. The store is a Postgres
/// repository over a test-local pool that is already closed, so every read fails with
/// `PoolClosed` → `RepositoryError::Internal`. No database is needed.
#[tokio::test]
async fn integrity_route_answers_500_when_the_log_store_fails() {
    let closed = PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .expect("lazy pool");
    closed.close().await;
    let mut state = default_state();
    state.input_log_repo = std::sync::Arc::new(PostgresInputLogRepository::new(closed));
    let app = mercs_server::routes::router(std::sync::Arc::new(state));
    let status = get_integrity_status(app, uuid::Uuid::new_v4()).await;
    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
}

/// BP-2a — end to end on Postgres: a real join, a launch through `initialize_campaign` (on
/// `dev` the join leaves the campaign `Pending` and this launches it; on `breakpoint` the
/// join already launched it, so `NotPending`), then the sector route answers `200` with the
/// member's own gateway from the membership row. The test reads only `gateway`.
#[tokio::test]
async fn sector_route_end_to_end_on_postgres() {
    use axum::{body::Body, http::{Request, StatusCode}};
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [85u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await; // the join's pick is global
    seed_account(&pool, wallet).await;

    let mut state = pg_route_state(&pool);
    std::sync::Arc::make_mut(&mut state.config).server.join_enabled = true; // 5a-3c: joins are off by default
    let state = std::sync::Arc::new(state);
    let token = insert_session(&state, wallet);
    let request = |method: &str, uri: String| Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let json_of = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null);

    // Collect.
    let (join_status, join_body) = send(&state, request("POST", "/api/campaign/join".into())).await;
    let campaign_id = json_of(&join_body)["campaign_id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
    let mut collected = None;
    if let Some(c) = campaign_id {
        let launch = initialize_campaign(
            c,
            &*state.campaign_join_repo,
            state.membership_repo.as_ref(),
            state.sector_repo.as_ref(),
            state.input_log_repo.as_ref(),
        )
        .await;
        let launched = matches!(launch, Ok(()) | Err(mercs_server::campaign_init::InitError::NotPending));
        let (status, body) = send(&state, request("GET", format!("/api/campaign/{c}/sector"))).await;
        let row = PostgresMembershipRepository::new(pool.clone()).get_membership(&wallet, c).await;
        collected = Some((launched, status, body, row));
    }

    // Clean: the immutable campaign_started row the launch wrote, then the wallet.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    if let Some(c) = campaign_id {
        sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.ok();
    }
    let enabled = sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await;
    clean_wallet_4g1(&pool, &wallet).await;

    // Assert.
    enabled.expect("input_logs triggers re-enabled"); // Reviewer BP-1b F4
    assert_eq!(join_status, StatusCode::CREATED, "{join_body}");
    let (launched, status, body, row) = collected.expect("the join returned a campaign id");
    assert!(launched, "the campaign launched (Ok or NotPending)");
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = row.expect("get_membership").expect("the membership exists");
    let (q, r) = (row.gateway_hex_q.expect("the launch assigned q"), row.gateway_hex_r.expect("the launch assigned r"));
    assert_eq!(json_of(&body)["gateway"], serde_json::json!({"q": q, "r": r}));
}

/// BP-1c — the sectorless repair on Postgres: a joined, launched campaign loses its sector
/// (class (a), a process killed mid-launch). The next join retires it (`retire_if_active`'s
/// real `UPDATE`) and answers `500`; the join after that skips the `Archived` campaign
/// (`live_membership`) and creates and launches a new one. The sector store is
/// `pg_route_state`'s in-memory one.
#[tokio::test]
async fn pg_join_repairs_an_active_campaign_with_no_sector() {
    use axum::{body::Body, http::{Request, StatusCode}};
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [94u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await; // the join's pick is global
    seed_account(&pool, wallet).await;

    let mut state = pg_route_state(&pool);
    std::sync::Arc::make_mut(&mut state.config).server.join_enabled = true; // 5a-3c: joins are off by default
    let state = std::sync::Arc::new(state);
    let token = insert_session(&state, wallet);
    let join_request = || Request::builder()
        .method("POST")
        .uri("/api/campaign/join")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let json_of = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null);
    let campaign_of = |body: &str| json_of(body)["campaign_id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
    let campaigns = PostgresCampaignRepository::new(pool.clone());

    // Collect.
    let (first_status, first_body) = send(&state, join_request()).await;
    let first = campaign_of(&first_body);
    let mut collected = None;
    if let Some(c1) = first {
        let before = campaigns.get_campaign(c1).await;
        let deleted = match &before {
            Ok(Some(c)) => state.sector_repo.delete_sector(c.sector_id).await.map(|_| c.sector_id),
            _ => Err(mercs_server::repository::RepositoryError::Internal("no campaign row".into())),
        };
        let (repair_status, repair_body) = send(&state, join_request()).await;
        let after = campaigns.get_campaign(c1).await;
        let (fresh_status, fresh_body) = send(&state, join_request()).await;
        let fresh = campaign_of(&fresh_body);
        let fresh_row = match fresh {
            Some(c2) => campaigns.get_campaign(c2).await.ok().flatten(),
            None => None,
        };
        let fresh_sector = match &fresh_row {
            Some(c) => state.sector_repo.sector_exists(c.sector_id).await.ok(),
            None => None,
        };
        let old_sector = match &deleted {
            Ok(s) => state.sector_repo.sector_exists(*s).await.ok(),
            Err(_) => None,
        };
        collected = Some((before, deleted, repair_status, repair_body, after, fresh_status, fresh_body, fresh, fresh_row, fresh_sector, old_sector));
    }

    // Clean: the immutable campaign_started rows both launches wrote, then the wallet (its
    // campaigns are collected through the membership rows, Archived included).
    let fresh_id = collected.as_ref().and_then(|c| c.7);
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    for c in [first, fresh_id].into_iter().flatten() {
        sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.ok();
    }
    let enabled = sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await;
    clean_wallet_4g1(&pool, &wallet).await;

    // Assert.
    enabled.expect("input_logs triggers re-enabled");
    assert_eq!(first_status, StatusCode::CREATED, "{first_body}");
    let (before, deleted, repair_status, repair_body, after, fresh_status, fresh_body, fresh, fresh_row, fresh_sector, old_sector) =
        collected.expect("the first join returned a campaign id");
    let before = before.expect("get_campaign").expect("the first campaign");
    assert_eq!(before.state, CampaignLifecycle::Active, "the join launched it");
    deleted.expect("the sector delete (the kill being modelled)");
    assert_eq!(repair_status, StatusCode::INTERNAL_SERVER_ERROR, "{repair_body}");
    let after = after.expect("get_campaign").expect("the first campaign");
    assert_eq!(after.state, CampaignLifecycle::Archived, "the sectorless campaign was retired");
    assert_eq!(old_sector, Some(false), "no sector left behind");
    assert_eq!(fresh_status, StatusCode::CREATED, "{fresh_body}");
    let fresh = fresh.expect("the retry returned a campaign id");
    assert_ne!(fresh, before.campaign_id, "the retry gets a new campaign");
    let fresh_row = fresh_row.expect("the new campaign's row");
    assert_eq!(fresh_row.state, CampaignLifecycle::Active, "the new campaign launched");
    assert_eq!(fresh_sector, Some(true), "with a sector");
    assert_eq!(state.launch_locks.len(), 0, "no launch lock left behind");
}

/// BP-2b-2 — end to end on Postgres: a real join writes the caller's starters and a launch
/// runs as in the sector test. Then a second member and their Commander are added (after the
/// launch: on `dev` a launch refuses a campaign with two members), so the route's answer
/// passes through the Postgres wallet filter. The oracle is a campaign-wide read here.
#[tokio::test]
async fn roster_route_end_to_end_on_postgres() {
    use axum::{body::Body, http::{Request, StatusCode}};
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [89u8; 32];
    let other = [90u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    clean_wallet_4g1(&pool, &other).await;
    assert_no_pending_campaign(&pool).await; // the join's pick is global
    seed_account(&pool, wallet).await;
    seed_account(&pool, other).await;

    let mut state = pg_route_state(&pool);
    std::sync::Arc::make_mut(&mut state.config).server.join_enabled = true; // 5a-3c: joins are off by default
    let state = std::sync::Arc::new(state);
    let token = insert_session(&state, wallet);
    let request = |method: &str, uri: String| Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let json_of = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null);

    // Collect.
    let (join_status, join_body) = send(&state, request("POST", "/api/campaign/join".into())).await;
    let campaign_id = json_of(&join_body)["campaign_id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
    let mut collected = None;
    if let Some(c) = campaign_id {
        let launch = initialize_campaign(
            c,
            &*state.campaign_join_repo,
            state.membership_repo.as_ref(),
            state.sector_repo.as_ref(),
            state.input_log_repo.as_ref(),
        )
        .await;
        let launched = matches!(launch, Ok(()) | Err(mercs_server::campaign_init::InitError::NotPending));
        let added = PostgresMembershipRepository::new(pool.clone()).add_member(&other, c, None, None).await;
        let theirs = mercs_server::commander_gen::generate_commander(other.to_vec(), c, 90);
        let theirs_id = theirs.commander_id;
        let created = PostgresCommanderRepository::new(pool.clone()).create_commander(theirs).await;
        let (status, body) = send(&state, request("GET", format!("/api/campaign/{c}/roster"))).await;
        let rows = (
            PostgresCommanderRepository::new(pool.clone()).list_commanders_by_campaign(c).await,
            PostgresForemanRepository::new(pool.clone()).list_foremen_by_campaign(c).await,
            PostgresSectionRepository::new(pool.clone()).list_sections_by_campaign(c).await,
            PostgresCrewRepository::new(pool.clone()).list_crews_by_campaign(c).await,
        );
        collected = Some((launched, added, created, theirs_id, status, body, rows));
    }

    // Clean: the immutable campaign_started row the launch wrote, then both wallets.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    if let Some(c) = campaign_id {
        sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.ok();
    }
    let enabled = sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await;
    clean_wallet_4g1(&pool, &wallet).await;
    clean_wallet_4g1(&pool, &other).await;

    // Assert.
    enabled.expect("input_logs triggers re-enabled");
    assert_eq!(join_status, StatusCode::CREATED, "{join_body}");
    let (launched, added, created, theirs_id, status, body, rows) = collected.expect("the join returned a campaign id");
    assert!(launched, "the campaign launched (Ok or NotPending)");
    added.expect("the second member joined after the launch");
    created.expect("the second member's Commander");
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = json_of(&body);
    let ids = |list: &str, key: &str| -> Vec<String> {
        body[list].as_array().expect("a list").iter().map(|v| v[key].as_str().expect("an id").to_string()).collect()
    };
    let (commanders, foremen, sections, crews) = rows;
    let commanders = commanders.expect("list Commanders");
    assert_eq!(commanders.len(), 2, "control: the campaign holds both members' Commanders");
    let mine: Vec<String> = commanders.iter().filter(|r| r.player_wallet == wallet).map(|r| r.commander_id.to_string()).collect();
    assert_eq!(mine.len(), 1, "the join wrote one starter Commander");
    assert_eq!(ids("commanders", "commander_id"), mine);
    assert!(!body.to_string().contains(&theirs_id.to_string()), "the second member's Commander leaked");
    let foremen: Vec<String> = foremen.expect("list Foremen").iter().map(|r| r.foreman_id.to_string()).collect();
    let sections: Vec<String> = sections.expect("list Sections").iter().map(|r| r.section_id.to_string()).collect();
    let crews: Vec<String> = crews.expect("list Crews").iter().map(|r| r.crew_id.to_string()).collect();
    assert_eq!((foremen.len(), sections.len(), crews.len()), (1, 1, 1), "the join wrote one starter of each");
    assert_eq!(ids("foremen", "foreman_id"), foremen);
    assert_eq!(ids("sections", "section_id"), sections);
    assert_eq!(ids("crews", "crew_id"), crews);
}

/// Sends `req` through a fresh router over `state`; returns the status and the body text.
async fn send(
    state: &std::sync::Arc<mercs_server::state::AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> (axum::http::StatusCode, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use mercs_server::routes::{with_test_peer, TEST_PEER};
    let app = with_test_peer(mercs_server::routes::router(std::sync::Arc::clone(state)), TEST_PEER);
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn post_json(uri: &str, body: serde_json::Value) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

/// INF-4 (GAP-52): the auth tests' key. Wallet 20 is a signing-key seed, used in memory
/// only; its derived public key is not `[20u8; 32]`.
fn auth_test_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[20u8; 32])
}

/// Requests a challenge for `wallet` through the real route; returns the challenge text.
async fn fetch_challenge(state: &std::sync::Arc<mercs_server::state::AppState>, wallet: &str) -> String {
    let (status, body) = send(state, post_json("/api/auth/challenge", serde_json::json!({ "wallet_address": wallet }))).await;
    assert_eq!(status, axum::http::StatusCode::OK, "challenge route: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["challenge"].as_str().expect("challenge text").to_string()
}

/// Signs `message` with `key` and posts it to the verify route against `challenge`.
async fn verify_with(
    state: &std::sync::Arc<mercs_server::state::AppState>,
    key: &ed25519_dalek::SigningKey,
    challenge: &str,
    message: &str,
) -> (axum::http::StatusCode, String) {
    use ed25519_dalek::Signer;
    let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let signature = bs58::encode(key.sign(message.as_bytes()).to_bytes()).into_string();
    send(state, post_json("/api/auth/verify", serde_json::json!({
        "wallet_address": wallet,
        "challenge":      challenge,
        "signature":      signature,
    }))).await
}

/// INF-4: the version route is public and reports the compiled-in version and build SHA.
#[tokio::test]
async fn version_route_reports_version_and_build_sha() {
    let state = std::sync::Arc::new(default_state());
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/version")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, body) = send(&state, req).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["build_sha"], option_env!("MERCS_BUILD_SHA").unwrap_or("unknown"));
}

/// INF-4 (GAP-52): a correctly signed challenge issues a session the server holds.
#[tokio::test]
async fn auth_verify_accepts_a_signed_challenge() {
    let state = std::sync::Arc::new(default_state());
    let key = auth_test_key();
    let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let challenge = fetch_challenge(&state, &wallet).await;

    let (status, body) = verify_with(&state, &key, &challenge, &challenge).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let token_id = v["token_id"].as_str().expect("token_id");
    assert_eq!(v["wallet_address"], wallet.as_str());
    assert!(state.sessions.contains_key(token_id), "the returned token must be a live session");
}

/// INF-4 (GAP-52): a signature over anything but the challenge is refused.
#[tokio::test]
async fn auth_verify_rejects_a_bad_signature() {
    let state = std::sync::Arc::new(default_state());
    let key = auth_test_key();
    let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let challenge = fetch_challenge(&state, &wallet).await;

    let (status, body) = verify_with(&state, &key, &challenge, "not the challenge").await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("signature invalid"), "wrong 400 arm: {body}");
    assert!(state.sessions.is_empty(), "a refused verify issues no session");
}

/// INF-4 (GAP-52): an expired challenge is refused, even when correctly signed. The
/// expiry is configured negative so the real challenge route issues one already expired.
#[tokio::test]
async fn auth_verify_rejects_an_expired_challenge() {
    let mut config = mercs_server::config::Config::default();
    config.server.challenge_expiry_secs = -1;
    let state = std::sync::Arc::new(mercs_server::state::AppState::new(std::sync::Arc::new(config)));
    let key = auth_test_key();
    let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let challenge = fetch_challenge(&state, &wallet).await;

    let (status, body) = verify_with(&state, &key, &challenge, &challenge).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("challenge expired"), "wrong 400 arm: {body}");
    assert!(state.sessions.is_empty(), "a refused verify issues no session");
}

/// INF-6c-2a (GAP-77): a third party's `challenge` for the same wallet no longer replaces
/// the victim's pending one.
#[tokio::test]
async fn auth_verify_survives_a_second_challenge_for_the_same_wallet() {
    let state = std::sync::Arc::new(default_state());
    let key = auth_test_key();
    let wallet = bs58::encode(key.verifying_key().to_bytes()).into_string();
    let text_a = fetch_challenge(&state, &wallet).await;
    let _text_b = fetch_challenge(&state, &wallet).await;

    let (status, body) = verify_with(&state, &key, &text_a, &text_a).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(state.sessions.len(), 1);
}

/// INF-6c-2a: a challenge issued to one wallet cannot be redeemed by another key. Seed 77
/// is a second signing key, in memory only, like 20.
#[tokio::test]
async fn auth_verify_refuses_a_challenge_issued_to_another_wallet() {
    let state = std::sync::Arc::new(default_state());
    let key_a = auth_test_key();
    let wallet_a = bs58::encode(key_a.verifying_key().to_bytes()).into_string();
    let text = fetch_challenge(&state, &wallet_a).await;

    let key_b = ed25519_dalek::SigningKey::from_bytes(&[77u8; 32]);
    let (status, body) = verify_with(&state, &key_b, &text, &text).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert!(state.sessions.is_empty(), "a refused verify issues no session");
}

#[tokio::test]
async fn mission_resolve_requires_a_session() {
    let state = default_state();
    let commander_id = seed_in_memory_commander(&state, [4u8; 32], |_| {}).await;
    let section = seed_section_for(&state, commander_id).await;
    let app = mercs_server::routes::router(std::sync::Arc::new(state));
    let (status, _) = post_mission(app, None, &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
}

/// Another wallet's Commander and an unknown id are indistinguishable.
#[tokio::test]
async fn mission_resolve_hides_another_wallets_commander() {
    let state = default_state();
    let theirs = seed_in_memory_commander(&state, [4u8; 32], |_| {}).await;
    let their_section = seed_section_for(&state, theirs).await;
    let my_token = insert_session(&state, [14u8; 32]);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (s1, b1) = post_mission(app.clone(), Some(&my_token), &mission_body(theirs, &[their_section])).await;
    let (s2, b2) = post_mission(app, Some(&my_token), &mission_body(uuid::Uuid::new_v4(), &[their_section])).await;
    assert_eq!(s1, axum::http::StatusCode::NOT_FOUND, "another wallet's Commander");
    assert_eq!(s2, axum::http::StatusCode::NOT_FOUND, "unknown Commander");
    assert_eq!(b1, b2, "the two 404 bodies must be byte-identical");
}

#[tokio::test]
async fn mission_resolve_rejects_a_client_commander() {
    use sim_engine::game_types::{Commander, CommanderPassiveBuffs, QualityGrade};
    let state = default_state();
    let wallet = [4u8; 32];
    let commander_id = seed_in_memory_commander(&state, wallet, |_| {}).await;
    let section = seed_section_for(&state, commander_id).await;
    let token = insert_session(&state, wallet);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let full = serde_json::to_value(Commander {
        id: "vance".into(), name: "Captain Vance".into(),
        rank: 5, skill: 10, success_aura: 999, quality_grade: QualityGrade::Basic,
        ability: String::new(), flavor_text: String::new(), stress_level: 0,
        is_kia: false, is_shattered: false, can_retreat: true,
        passive_buffs: CommanderPassiveBuffs { accuracy: 0, evasion: 0, damage_reduction: 0 },
        attached_unit_id: None,
    }).unwrap();

    for (label, commander) in [("full object", full), ("empty object", serde_json::json!({}))] {
        let mut body = mission_body(commander_id, &[section]);
        body["squad"]["commander"] = commander;
        let (status, _) = post_mission(app.clone(), Some(&token), &body).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{label} must be refused");
    }

    let mut body = mission_body(commander_id, &[section]);
    body["squad"]["commander"] = serde_json::Value::Null;
    let (status, bytes) = post_mission(app, Some(&token), &body).await;
    assert_eq!(status, axum::http::StatusCode::OK, "null means no client Commander: {}", String::from_utf8_lossy(&bytes));
}

#[tokio::test]
async fn mission_resolve_rejects_seed_override() {
    let state = default_state();
    let wallet = [4u8; 32];
    let commander_id = seed_in_memory_commander(&state, wallet, |_| {}).await;
    let section = seed_section_for(&state, commander_id).await;
    let token = insert_session(&state, wallet);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));
    let mut body = mission_body(commander_id, &[section]);
    body["seed_override"] = serde_json::json!(42);
    let (status, _) = post_mission(app, Some(&token), &body).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mission_resolve_refuses_kia_and_shattered() {
    let state = default_state();
    let wallet = [4u8; 32];
    let kia = seed_in_memory_commander(&state, wallet, |r| r.is_kia = true).await;
    let shattered = seed_in_memory_commander(&state, wallet, |r| r.is_shattered = true).await;
    let (kia_section, shattered_section) = (seed_section_for(&state, kia).await, seed_section_for(&state, shattered).await);
    let token = insert_session(&state, wallet);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));
    for (label, id, section) in [("KIA", kia, kia_section), ("Shattered", shattered, shattered_section)] {
        let (status, bytes) = post_mission(app.clone(), Some(&token), &mission_body(id, &[section])).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT, "{label}: {}", String::from_utf8_lossy(&bytes));
    }
}

#[tokio::test]
async fn mission_resolve_refuses_an_already_deployed_commander() {
    let state = default_state();
    let wallet = [4u8; 32];
    let commander_id = seed_in_memory_commander(&state, wallet, |_| {}).await;
    let section = seed_section_for(&state, commander_id).await;
    assert_eq!(state.commander_repo.start_deployment(&NewDeployment {
        deployment_id: uuid::Uuid::new_v4(),
        commander_id,
        session_id: uuid::Uuid::new_v4(),
        mission_id: "GUARD_DUTY".into(),
        started_at: chrono::Utc::now(),
        player_wallet: wallet.to_vec(),
        campaign_id: uuid::Uuid::nil(),
        section_ids: vec![],
    }).await.unwrap(), DeploymentStart::Started);
    let token = insert_session(&state, wallet);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));
    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(error_message(&bytes), "Commander is already deployed");
}

/// Test-only seam: forwards everything to `inner`, except that `start_deployment` first
/// Shatters the Commander through a real writer. This puts a Shattering close between
/// the route's first read and its deployment lock. Lives here, not in production code.
struct ShatterOnStart {
    inner: std::sync::Arc<dyn CommanderRepository + Send + Sync>,
}

#[async_trait::async_trait]
impl CommanderRepository for ShatterOnStart {
    async fn create_commander(&self, record: CommanderRecord) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.create_commander(record).await
    }
    async fn get_commander(&self, commander_id: uuid::Uuid) -> Result<Option<CommanderRecord>, mercs_server::repository::RepositoryError> {
        self.inner.get_commander(commander_id).await
    }
    async fn update_stress(&self, commander_id: uuid::Uuid, stress: i16) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.update_stress(commander_id, stress).await
    }
    async fn update_rank_and_xp(&self, commander_id: uuid::Uuid, rank: i16, xp: i32) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.update_rank_and_xp(commander_id, rank, xp).await
    }
    async fn set_veteran_trait(&self, commander_id: uuid::Uuid, veteran_trait: String) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.set_veteran_trait(commander_id, veteran_trait).await
    }
    async fn set_shattered(&self, commander_id: uuid::Uuid) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.set_shattered(commander_id).await
    }
    async fn set_kia(&self, commander_id: uuid::Uuid) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.set_kia(commander_id).await
    }
    async fn list_commanders_by_campaign(&self, campaign_id: uuid::Uuid) -> Result<Vec<CommanderRecord>, mercs_server::repository::RepositoryError> {
        self.inner.list_commanders_by_campaign(campaign_id).await
    }
    async fn list_commanders_for_member(&self, campaign_id: uuid::Uuid, player_wallet: &[u8]) -> Result<Vec<CommanderRecord>, mercs_server::repository::RepositoryError> {
        self.inner.list_commanders_for_member(campaign_id, player_wallet).await
    }
    async fn delete_commanders_by_campaign(&self, campaign_id: uuid::Uuid) -> Result<u64, mercs_server::repository::RepositoryError> {
        self.inner.delete_commanders_by_campaign(campaign_id).await
    }
    async fn start_deployment(&self, d: &NewDeployment) -> Result<DeploymentStart, mercs_server::repository::RepositoryError> {
        self.inner.update_stress(d.commander_id, 100).await?;
        self.inner.start_deployment(d).await
    }
    async fn abort_deployment(&self, deployment_id: uuid::Uuid) -> Result<(), mercs_server::repository::RepositoryError> {
        self.inner.abort_deployment(deployment_id).await
    }
    async fn resolve_deployment(
        &self,
        deployment_id: uuid::Uuid,
        commander_id: uuid::Uuid,
        kia: u32,
        tier: u8,
        ticker_contribution_pct: f64,
        outcome: &sim_engine::game_types::OutcomeType,
        cfg: &sim_engine::config::SimConfig,
    ) -> Result<mercs_server::repository::AppliedEffects, mercs_server::repository::RepositoryError> {
        self.inner.resolve_deployment(deployment_id, commander_id, kia, tier, ticker_contribution_pct, outcome, cfg).await
    }
    async fn list_deployments_by_commander(&self, commander_id: uuid::Uuid) -> Result<Vec<mercs_server::repository::DeploymentRecord>, mercs_server::repository::RepositoryError> {
        self.inner.list_deployments_by_commander(commander_id).await
    }
}

/// Shared body of 4g-2 test (a): the Commander is Shattered after the route's first
/// read, so only the re-read under the deployment lock can refuse them. Expects 409,
/// one `aborted` deployment, no logged session, and no effect beyond the Shatter.
async fn assert_shattered_after_first_read_is_refused(
    mut state: mercs_server::state::AppState,
    wallet: [u8; 32],
    commander_id: uuid::Uuid,
) {
    let seeded = state.commander_repo.get_commander(commander_id).await.unwrap().unwrap();
    assert!(!seeded.is_shattered, "precondition: the first read sees a deployable Commander");
    let section = seed_section_for(&state, commander_id).await;
    state.commander_repo = std::sync::Arc::new(ShatterOnStart { inner: state.commander_repo.clone() });
    let token = insert_session(&state, wallet);
    let commanders = state.commander_repo.clone();
    let logs = state.input_log_repo.clone();
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&bytes));
    assert!(String::from_utf8_lossy(&bytes).contains("Shattered"), "{}", String::from_utf8_lossy(&bytes));

    let deps = commanders.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].status, "aborted", "a refused re-read must abort the deployment");
    assert!(logs.get_session_config(&deps[0].session_id).await.unwrap().is_none(),
        "a refused deployment must never log");
    let rec = commanders.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!((rec.stress, rec.is_shattered, rec.xp, rec.rank), (100, true, seeded.xp, seeded.rank));
}

/// 4g-2 test (a), in-memory.
#[tokio::test]
async fn mission_resolve_refuses_a_commander_shattered_after_the_first_read_in_memory() {
    let state = default_state();
    let wallet = [4u8; 32];
    let commander_id = seed_in_memory_commander(&state, wallet, |r| r.stress = 20).await;
    assert_shattered_after_first_read_is_refused(state, wallet, commander_id).await;
}

/// 4g-2 test (a), Postgres.
#[tokio::test]
async fn mission_resolve_refuses_a_commander_shattered_after_the_first_read_postgres() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [19u8; 32];
    let (_campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 20).await;
    let state = pg_route_state(&pool);
    assert_shattered_after_first_read_is_refused(state, wallet, commander_id).await;
    clean_wallet_4g1(&pool, &wallet).await;
}

/// The log write is fallible on this route: a failure aborts the deployment, returns
/// 500, and leaves every Commander effect unapplied.
#[tokio::test]
async fn mission_resolve_aborts_with_500_when_the_log_write_fails() {
    let mut state = default_state();
    state.input_log_repo = std::sync::Arc::new(FailingInputLog { inner: state.input_log_repo.clone(), fail_writes: true });
    let wallet = [4u8; 32];
    let commander_id = seed_in_memory_commander(&state, wallet, |r| { r.stress = 30; r.xp = 88; }).await;
    let section = seed_section_for(&state, commander_id).await;
    let token = insert_session(&state, wallet);
    let commanders = state.commander_repo.clone();
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR, "{}", String::from_utf8_lossy(&bytes));

    let deps = commanders.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].status, "aborted", "a failed log write must abort the deployment");
    let rec = commanders.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!((rec.stress, rec.xp, rec.rank), (30, 88, 1), "no effect may land without its logged input");
}

/// The combat route's policy is unchanged: its log write stays fire-and-forget.
#[tokio::test]
async fn combat_resolve_still_succeeds_when_its_log_write_fails() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let mut state = default_state();
    state.input_log_repo = std::sync::Arc::new(FailingInputLog { inner: state.input_log_repo.clone(), fail_writes: true });
    let app = mercs_server::routes::router(std::sync::Arc::new(state));
    let req = Request::builder()
        .method("POST")
        .uri("/api/combat/resolve")
        .header("content-type", "application/json")
        .body(Body::from(alpha_vs_scout_payload().to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
}

/// 4g-1 end to end on Postgres: the input is logged, the effects are written in one
/// atomic call, the deployment is closed, and the build version reaches the log.
#[tokio::test]
async fn mission_resolve_end_to_end_writes_effects() {
    use sim_engine::game_types::{OutcomeType, UnitStatus};
    use mercs_server::commander_gen::{mission_tier, rank_for_xp, xp_award};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [16u8; 32];
    let (campaign_id, commander_id) = seed_commander_4g1(&pool, wallet, 20).await;
    let commanders = PostgresCommanderRepository::new(pool.clone());
    let logs = PostgresInputLogRepository::new(pool.clone());
    let record = commanders.get_commander(commander_id).await.unwrap().unwrap();

    let state = pg_route_state(&pool);
    let section = seed_section_for(&state, commander_id).await;
    let token = insert_session(&state, wallet);
    let sim_cfg = state.config.sim.clone();
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app.clone(), Some(&token), &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let sid_str = resp["session_id"].as_str().expect("session_id missing").to_string();
    let sid = uuid::Uuid::parse_str(&sid_str).unwrap();
    let report = &resp["report"];
    assert_eq!(report["score_breakdown"]["commander_bonus"], 0);
    assert_eq!(report["commander_name"].as_str(), Some(record.name.as_str()));

    // The log: header with the version and the real campaign, and the request body.
    let config = logs.get_session_config(&sid).await.unwrap().expect("no session_configs header");
    assert_eq!(config.build_version, "0.3.0", "R7 bump must reach the logged build_version");
    assert_eq!(config.campaign_id, campaign_id);
    let entries = logs.get_entries_by_session(&sid).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event_type, "session_start");
    let request = &entries[0].payload["request"];
    assert_eq!(request["commander_id"].as_str(), Some(commander_id.to_string().as_str()));
    assert!(request.get("seed_override").is_none(), "the logged request carries no seed_override");

    // The effects, recomputed here from the report with the same pure functions.
    let outcome: OutcomeType = serde_json::from_value(report["outcome"].clone()).unwrap();
    let units: Vec<sim_engine::game_types::UnitBattleResult> = serde_json::from_value(report["unit_results"].clone()).unwrap();
    let kia = units.iter().filter(|u| matches!(u.final_status, UnitStatus::Kia)).count() as u32;
    let difficulty = sim_engine::missions::missions()["GUARD_DUTY"].difficulty;
    let want_stress = sim_engine::resolver::stress_after_casualties(
        sim_engine::resolver::stress_after_deployment(20, &sim_cfg), kia, &sim_cfg);
    let want_xp = record.xp + xp_award(mission_tier(difficulty), &outcome);
    let after = commanders.get_commander(commander_id).await.unwrap().unwrap();
    assert_eq!(after.stress, i16::from(want_stress));
    assert_eq!(after.xp, want_xp);
    assert_eq!(after.rank, rank_for_xp(want_xp));

    let deps = commanders.list_deployments_by_commander(commander_id).await.unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].status, "resolved");
    assert_eq!(deps[0].session_id, sid);
    assert_eq!(deps[0].outcome.as_deref(), Some(outcome_db_str(&outcome)));
    assert!(entries[0].payload["timestamp"].is_string());

    // The death (4e-1): the response reports it, and the record and card queue agree.
    // The seed is the route's, so this checks whichever branch the roll took.
    let died = matches!(outcome, OutcomeType::Wipeout);
    assert_eq!(resp["commander"]["kia"].as_bool(), Some(died), "commander.kia must equal outcome == Wipeout");
    assert!(resp["commander"]["glorious"].is_boolean());
    // BP-2b-1: the block's totals and deltas, asserted after the cleanup below.
    let block = resp["commander"].clone();
    assert_eq!(after.is_kia, died);
    let cards: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mint_pending WHERE player_wallet = $1")
        .bind(&wallet[..])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cards, if died { 1 } else { 0 });

    // The response's session_id reaches the integrity route.
    let req = axum::http::Request::builder()
        .method("GET")
        .uri(format!("/api/session/{sid_str}/integrity"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["log_hash"].as_str().is_some_and(|h| !h.is_empty()), "integrity digest missing");

    // Cleanup: the immutable log rows, then the wallet's records.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE session_id = $1").bind(sid).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM session_configs WHERE session_id = $1").bind(sid).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    clean_wallet_4g1(&pool, &wallet).await;

    // BP-2b-1: the commander block carries the totals and the deltas, computed from the
    // row `resolve_deployment` locked (here equal to `record`, read before the mission).
    let keys: std::collections::BTreeSet<&str> = block.as_object().expect("commander is an object").keys().map(String::as_str).collect();
    assert_eq!(keys, ["glorious", "kia", "new_rank", "old_rank", "stress", "stress_change", "xp", "xp_gained"].into_iter().collect());
    assert_eq!(block["stress"], serde_json::json!(after.stress));
    assert_eq!(block["xp"], serde_json::json!(after.xp));
    assert_eq!(block["old_rank"], serde_json::json!(record.rank));
    assert_eq!(block["new_rank"], serde_json::json!(after.rank));
    assert_eq!(block["stress_change"], serde_json::json!(after.stress - record.stress));
    assert_eq!(block["xp_gained"], serde_json::json!(after.xp - record.xp));
}

// ── INF-5: GAP-57, the boot migrator ─────────────────────────────────────────

/// Runs `m` against the test DB with a fake applied migration (99999) that no binary
/// embeds, inside a transaction that is rolled back. Locking is off: `run_direct` returns
/// `VersionMissing` before its `unlock()`, and a session advisory lock left on a pooled
/// connection would block the next `test_pool()` migrate forever.
async fn run_with_fake_applied_version(
    pool: &sqlx::PgPool,
    m: &sqlx::migrate::Migrator,
) -> Result<(), sqlx::migrate::MigrateError> {
    let mut tx = pool.begin().await.expect("begin");
    // Runtime query: `_sqlx_migrations` is sqlx's own table, so no `.sqlx/` cache file.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (99999, 'gap57-fake', TRUE, '\\x00'::bytea, 0)",
    )
    .execute(&mut *tx)
    .await
    .expect("insert fake applied migration");
    let r = m.run_direct(&mut *tx).await;
    tx.rollback().await.expect("rollback");
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM _sqlx_migrations WHERE version = 99999")
        .fetch_one(pool)
        .await
        .expect("count fake row");
    assert_eq!(left, 0, "the fake applied migration must not survive the rollback");
    r
}

#[tokio::test]
async fn boot_migrator_tolerates_missing_applied_version() {
    let Some(pool) = test_pool().await else {
        eprintln!("TEST_DATABASE_URL not set — skipping");
        return;
    };
    let mut m = mercs_server::boot_migrator();
    m.set_locking(false);
    let r = run_with_fake_applied_version(&pool, &m).await;
    assert!(r.is_ok(), "the boot migrator must ignore an applied version it doesn't embed: {r:?}");
}

#[tokio::test]
async fn strict_migrator_rejects_missing_applied_version() {
    let Some(pool) = test_pool().await else {
        eprintln!("TEST_DATABASE_URL not set — skipping");
        return;
    };
    let mut m = mercs_server::boot_migrator();
    m.set_ignore_missing(false);
    m.set_locking(false);
    let r = run_with_fake_applied_version(&pool, &m).await;
    assert!(
        matches!(r, Err(sqlx::migrate::MigrateError::VersionMissing(99999))),
        "with ignore_missing off the same migrator must refuse: {r:?}"
    );
}

// ── INF-5: GAP-53, poisoned timer members ────────────────────────────────────

#[tokio::test]
async fn get_due_timers_reports_poisoned_members() {
    use redis::AsyncCommands;
    let Some(url) = std::env::var("TEST_REDIS_URL").ok() else {
        eprintln!("TEST_REDIS_URL not set — skipping Redis poisoned-timer test");
        return;
    };
    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mut conn = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");
    let repo = RedisTimerRepository::new(conn.clone());

    let sector_id = uuid::Uuid::new_v4();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(60)).timestamp() as f64;
    let good_id = uuid::Uuid::new_v4();
    let bad_uuid = format!("gap53-not-a-uuid-{}", uuid::Uuid::new_v4());
    let bad_json = uuid::Uuid::new_v4().to_string();
    let orphan = uuid::Uuid::new_v4().to_string();
    let sector_zset = format!("sector:{sector_id}:timers");

    repo.schedule_timer(DeploymentTimer {
        timer_id:      good_id,
        player_wallet: WalletAddress([9u8; 32]),
        sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      chrono::Utc::now() - chrono::Duration::seconds(60),
    }).await.expect("schedule good timer");
    conn.zadd::<_, _, _, ()>("timers:global", &bad_uuid, past).await.expect("ZADD non-UUID");
    conn.set::<_, _, ()>(format!("timer:{bad_json}"), "{not json").await.expect("SET bad payload");
    conn.set::<_, _, ()>(format!("timer:{bad_json}:sector_id"), sector_id.to_string()).await.expect("SET bad reverse");
    conn.zadd::<_, _, _, ()>(&sector_zset, &bad_json, past).await.expect("ZADD bad sector");
    conn.zadd::<_, _, _, ()>("timers:global", &bad_json, past).await.expect("ZADD bad payload");
    conn.zadd::<_, _, _, ()>("timers:global", &orphan, past).await.expect("ZADD orphan");

    let scan = repo.get_due_timers(chrono::Utc::now()).await.expect("one bad member must not fail the scan");
    assert!(scan.due.iter().any(|t| t.timer_id == good_id), "the good timer is still due");
    let ours = [bad_uuid.as_str(), bad_json.as_str(), orphan.as_str()];
    for m in ours {
        assert!(scan.poisoned.iter().any(|p| p.member == m), "{m} must be reported as poisoned");
    }
    assert!(!scan.due.iter().any(|t| ours.contains(&t.timer_id.to_string().as_str())));

    for m in ours {
        repo.quarantine_timer(m).await.expect("quarantine");
    }
    for m in ours {
        let score: Option<f64> = conn.zscore("timers:global", m).await.expect("ZSCORE");
        assert!(score.is_none(), "{m} must leave timers:global");
        let q: bool = conn.sismember("timers:quarantine", m).await.expect("SISMEMBER");
        assert!(q, "{m} must be recorded in timers:quarantine");
    }
    let kept: bool = conn.exists(format!("timer:{bad_json}")).await.expect("EXISTS");
    assert!(kept, "a poisoned payload is kept for hand recovery");
    let in_sector: Option<f64> = conn.zscore(&sector_zset, &bad_json).await.expect("ZSCORE sector");
    assert!(in_sector.is_none(), "the reverse lookup removes the member from its sector zset");

    // Cleanup: this test's keys and quarantine members only.
    repo.cancel_timer(good_id).await.ok();
    conn.del::<_, ()>(vec![format!("timer:{bad_json}"), format!("timer:{bad_json}:sector_id"), sector_zset]).await.ok();
    for m in ours {
        conn.zrem::<_, _, ()>("timers:global", m).await.ok();
        conn.srem::<_, _, ()>("timers:quarantine", m).await.ok();
    }
}

#[tokio::test]
async fn quarantine_timer_errs_when_the_reverse_lookup_fails() {
    use redis::AsyncCommands;
    let Some(url) = std::env::var("TEST_REDIS_URL").ok() else {
        eprintln!("TEST_REDIS_URL not set — skipping Redis quarantine-lookup-error test");
        return;
    };
    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mut conn = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");
    let repo = RedisTimerRepository::new(conn.clone());

    let member = format!("f3-wrongtype-{}", uuid::Uuid::new_v4());
    let reverse = format!("timer:{member}:sector_id");
    let past = (chrono::Utc::now() - chrono::Duration::seconds(60)).timestamp() as f64;
    // A set where a string is expected: the reverse-lookup GET fails with WRONGTYPE.
    conn.sadd::<_, _, ()>(&reverse, "x").await.expect("SADD wrong-type reverse key");
    conn.zadd::<_, _, _, ()>("timers:global", &member, past).await.expect("ZADD member");

    let result = repo.quarantine_timer(&member).await;
    assert!(result.is_err(), "a failed reverse lookup must return Err, got {result:?}");
    let score: Option<f64> = conn.zscore("timers:global", &member).await.expect("ZSCORE");
    assert!(score.is_some(), "nothing is written before the lookup succeeds: the member stays in timers:global");
    let q: bool = conn.sismember("timers:quarantine", &member).await.expect("SISMEMBER");
    assert!(!q, "the member must not be quarantined when the lookup failed");

    // Cleanup: this test's key and members only; never DEL timers:quarantine.
    conn.del::<_, ()>(&reverse).await.ok();
    conn.zrem::<_, _, ()>("timers:global", &member).await.ok();
    conn.srem::<_, _, ()>("timers:quarantine", &member).await.ok();
}

#[tokio::test]
async fn quarantine_size_counts_quarantined_members() {
    use redis::AsyncCommands;
    use mercs_server::repository::timer::quarantine_size;
    let Some(url) = std::env::var("TEST_REDIS_URL").ok() else {
        eprintln!("TEST_REDIS_URL not set — skipping Redis quarantine-size test");
        return;
    };
    let client = redis::Client::open(url).expect("Invalid TEST_REDIS_URL");
    let mut conn = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");
    let repo = RedisTimerRepository::new(conn.clone());

    let member = format!("boot-quarantine-{}", uuid::Uuid::new_v4());
    let past = (chrono::Utc::now() - chrono::Duration::seconds(60)).timestamp() as f64;
    conn.zadd::<_, _, _, ()>("timers:global", &member, past).await.expect("ZADD member");

    // timers:quarantine is a global key other tests write too, so assert deltas only.
    let before = quarantine_size(&mut conn).await.expect("SCARD before");
    repo.quarantine_timer(&member).await.expect("quarantine");
    let after = quarantine_size(&mut conn).await.expect("SCARD after");
    assert_eq!(after, before + 1, "one quarantined member adds one");
    conn.srem::<_, _, ()>("timers:quarantine", &member).await.expect("SREM");
    let restored = quarantine_size(&mut conn).await.expect("SCARD restored");
    assert_eq!(restored, before, "removing it restores the count");

    // Cleanup: this test's member only (quarantine already removed it from timers:global).
    conn.zrem::<_, _, ()>("timers:global", &member).await.ok();
}

#[tokio::test]
async fn poisoned_member_does_not_stall_a_real_arrival() {
    use std::sync::Arc;
    use mercs_server::config::Config;
    use mercs_server::convoy_expiry::{poll_expiry_once, ExpiryWarns};
    use mercs_server::state::AppState;
    use redis::AsyncCommands;

    let Some(pool) = test_pool().await else {
        eprintln!("TEST_DATABASE_URL not set — skipping");
        return;
    };
    let Some(redis_url) = std::env::var("TEST_REDIS_URL").ok() else {
        eprintln!("TEST_REDIS_URL not set — skipping");
        return;
    };
    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let mut conn = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");

    let convoy_repo = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    let timer_repo  = Arc::new(RedisTimerRepository::new(conn.clone()));
    let sector_repo = Arc::new(RedisSectorStateRepository::new(conn.clone()));

    let convoy_id   = uuid::Uuid::new_v4();
    let sector_id   = uuid::Uuid::new_v4();
    let campaign_id = uuid::Uuid::new_v4();
    sector_repo.upsert_sector(SectorState {
        sector_id,
        campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
        terrain:             std::collections::HashMap::new(),
        magma_veins:         vec![],
        hex_occupancy:       std::collections::HashMap::new(),
    }).await.expect("upsert_sector");
    // No account: convoy_records.owner_wallet has no FK.
    convoy_repo.create_convoy(ConvoyDbRecord {
        convoy_id,
        sector_id,
        owner_wallet:  vec![7u8; 32],
        origin_q:      0,
        origin_r:      0,
        destination_q: 1,
        destination_r: 0,
        route:         serde_json::json!([{"q": 0, "r": 0}, {"q": 1, "r": 0}]),
        arrival_time:  chrono::Utc::now() - chrono::Duration::seconds(1),
        vehicle_class: "BehemothMegaTruck".to_string(),
        cargo:         serde_json::json!({}),
        in_transit:    true,
        created_at:    chrono::Utc::now(),
    }).await.expect("create_convoy");
    timer_repo.schedule_timer(DeploymentTimer {
        timer_id:      convoy_id,
        player_wallet: WalletAddress([7u8; 32]),
        sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      chrono::Utc::now() - chrono::Duration::seconds(1),
    }).await.expect("schedule_timer");
    let poisoned = format!("gap53-not-a-uuid-{}", uuid::Uuid::new_v4());
    let past = (chrono::Utc::now() - chrono::Duration::seconds(60)).timestamp() as f64;
    conn.zadd::<_, _, _, ()>("timers:global", &poisoned, past).await.expect("ZADD poisoned");

    let mut state = AppState::new(Arc::new(Config::default()));
    state.convoy_repo    = convoy_repo.clone();
    state.sector_repo    = sector_repo.clone();
    state.timer_repo     = timer_repo.clone();
    state.input_log_repo = Arc::new(PostgresInputLogRepository::new(pool.clone()));
    poll_expiry_once(&state, &mut ExpiryWarns::new()).await;

    let landed = convoy_repo.get_convoy(convoy_id).await.expect("get_convoy").expect("convoy exists");
    let left: Option<f64> = conn.zscore("timers:global", &poisoned).await.expect("ZSCORE");

    // Cleanup before asserting, so a failure leaves no residue.
    let cid = convoy_id.to_string();
    conn.zrem::<_, _, ()>("timers:global", &poisoned).await.ok();
    conn.srem::<_, _, ()>("timers:quarantine", &poisoned).await.ok();
    conn.del::<_, ()>(vec![format!("timer:{cid}"), format!("timer:{cid}:sector_id")]).await.ok();
    conn.zrem::<_, _, ()>("timers:global", &cid).await.ok();
    conn.zrem::<_, _, ()>(format!("sector:{sector_id}:timers"), &cid).await.ok();
    conn.del::<_, ()>(vec![format!("sector:{sector_id}:hex_state"), format!("sector:{sector_id}:player_presence")]).await.ok();
    conn.srem::<_, _, ()>("sectors:all", sector_id.to_string()).await.ok();
    sqlx::query("DELETE FROM convoy_records WHERE convoy_id = $1").bind(convoy_id).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(campaign_id).execute(&pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();

    assert!(!landed.in_transit, "a poisoned member must not stall a real arrival");
    assert!(left.is_none(), "the poisoned member must leave timers:global");
}

// ── INF-5: GAP-44, convoy dispatch and arrival ───────────────────────────────

/// A Postgres + Redis app state with one campaign, its sector blob, and a signed-in
/// session for wallet 25 (session only: dispatch writes no account-keyed row).
struct ConvoyFixture {
    pool:        sqlx::PgPool,
    conn:        redis::aio::ConnectionManager,
    state:       std::sync::Arc<mercs_server::state::AppState>,
    campaign_id: uuid::Uuid,
    sector_id:   uuid::Uuid,
    token_id:    String,
}

async fn convoy_fixture() -> Option<ConvoyFixture> {
    use std::sync::Arc;
    let pool = test_pool().await?;
    let redis_url = std::env::var("TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).expect("Invalid TEST_REDIS_URL");
    let conn = redis::aio::ConnectionManager::new(client).await.expect("Failed to connect to Redis");

    let mut state = pg_route_state(&pool);
    state.convoy_repo    = Arc::new(PostgresConvoyRepository::new(pool.clone()));
    state.sector_repo    = Arc::new(RedisSectorStateRepository::new(conn.clone()));
    state.timer_repo     = Arc::new(RedisTimerRepository::new(conn.clone()));

    let wallet_b58 = bs58::encode([25u8; 32]).into_string();
    let token_id = format!("inf5-convoy-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().timestamp();
    state.sessions.insert(token_id.clone(), shared::SessionToken {
        token_id:       token_id.clone(),
        account_id:     wallet_b58.clone(),
        wallet_address: wallet_b58,
        issued_at:      now,
        expires_at:     now + 7200,
    });

    // An Active campaign that wallet 25 is a member of (GAP-74: dispatch refuses a
    // non-member and a campaign that isn't Active).
    let campaigns = PostgresCampaignRepository::new(pool.clone());
    let campaign_id = seed_account_and_campaign(&pool, [25u8; 32], 25_i64).await;
    assert!(
        campaigns.activate_campaign(campaign_id, chrono::Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
            .await.expect("activate_campaign"),
        "the fixture campaign must activate"
    );
    let campaign = campaigns.get_campaign(campaign_id).await.expect("get_campaign").expect("seeded campaign");
    state.sector_repo.upsert_sector(SectorState {
        sector_id:           campaign.sector_id,
        campaign_id:         campaign.campaign_id,
        occupation_status:   OccupationStatus::Neutral,
        owner:               None,
        deployed_unit_count: 0,
        terrain:             std::collections::HashMap::new(),
        magma_veins:         vec![],
        hex_occupancy:       std::collections::HashMap::new(),
    }).await.expect("upsert_sector");

    Some(ConvoyFixture {
        pool,
        conn,
        state: Arc::new(state),
        campaign_id: campaign.campaign_id,
        sector_id: campaign.sector_id,
        token_id,
    })
}

/// POSTs a dispatch from (3,-2) to (5,3) and returns the new convoy's id.
async fn dispatch_via_route(f: &ConvoyFixture) -> uuid::Uuid {
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;
    let body = serde_json::json!({
        "origin_q": 3, "origin_r": -2,
        "destination_q": 5, "destination_r": 3,
        "vehicle_class": "BehemothMegaTruck",
        "cargo": {"magma_ore": 5}
    });
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/campaign/{}/convoy/dispatch", f.campaign_id))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", f.token_id))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = mercs_server::routes::router(std::sync::Arc::clone(&f.state)).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "dispatch must return 201");
    let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["convoy_id"].as_str().unwrap().parse().unwrap()
}

async fn clean_convoy_fixture(f: &ConvoyFixture, convoy_id: uuid::Uuid) {
    use redis::AsyncCommands;
    let mut conn = f.conn.clone();
    let cid = convoy_id.to_string();
    conn.del::<_, ()>(vec![format!("timer:{cid}"), format!("timer:{cid}:sector_id")]).await.ok();
    conn.zrem::<_, _, ()>("timers:global", &cid).await.ok();
    conn.zrem::<_, _, ()>(format!("sector:{}:timers", f.sector_id), &cid).await.ok();
    conn.del::<_, ()>(vec![format!("sector:{}:hex_state", f.sector_id), format!("sector:{}:player_presence", f.sector_id)]).await.ok();
    conn.srem::<_, _, ()>("sectors:all", f.sector_id.to_string()).await.ok();
    sqlx::query("DELETE FROM convoy_records WHERE convoy_id = $1").bind(convoy_id).execute(&f.pool).await.ok();
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&f.pool).await.ok();
    sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(f.campaign_id).execute(&f.pool).await.ok();
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&f.pool).await.ok();
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1").bind(f.campaign_id).execute(&f.pool).await.ok();
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1").bind([25u8; 32].to_vec()).execute(&f.pool).await.ok();
}

#[tokio::test]
async fn convoy_dispatch_logs_full_request_with_convoy() {
    let Some(f) = convoy_fixture().await else {
        eprintln!("TEST_DATABASE_URL or TEST_REDIS_URL not set — skipping");
        return;
    };
    let convoy_id = dispatch_via_route(&f).await;

    let convoy = PostgresConvoyRepository::new(f.pool.clone()).get_convoy(convoy_id).await.expect("get_convoy");
    let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT payload FROM input_logs WHERE campaign_id = $1 AND event_type = 'convoy_dispatched'",
    )
    .bind(f.campaign_id)
    .fetch_all(&f.pool)
    .await
    .expect("read convoy_dispatched");
    clean_convoy_fixture(&f, convoy_id).await;

    let convoy = convoy.expect("the convoy row exists");
    assert_eq!(rows.len(), 1, "exactly one convoy_dispatched row");
    let p = rows[0].0.as_object().expect("payload is an object");
    let mut keys: Vec<&str> = p.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["arrival_time", "cargo", "convoy_id", "destination_q", "destination_r", "origin_q", "origin_r", "vehicle_class"],
        "the payload carries exactly the 8 keys"
    );
    assert_eq!(p["convoy_id"], serde_json::json!(convoy_id));
    assert_eq!((p["origin_q"].as_i64(), p["origin_r"].as_i64()), (Some(3), Some(-2)));
    assert_eq!((p["destination_q"].as_i64(), p["destination_r"].as_i64()), (Some(5), Some(3)));
    assert_eq!(p["vehicle_class"], "BehemothMegaTruck");
    assert_eq!(p["cargo"], serde_json::json!({"magma_ore": 5}));
    let logged: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(p["arrival_time"].clone()).expect("arrival_time is a timestamp");
    // Dispatch truncates arrival_time to µs, the precision Postgres stores, so the log
    // and the row agree exactly (Reviewer INF-5 F5).
    assert_eq!(logged, convoy.arrival_time, "logged arrival_time matches the row exactly");
}

#[tokio::test]
async fn convoy_arrival_is_logged_once() {
    use mercs_server::convoy_expiry::{poll_expiry_once, ExpiryWarns};
    let Some(f) = convoy_fixture().await else {
        eprintln!("TEST_DATABASE_URL or TEST_REDIS_URL not set — skipping");
        return;
    };
    let convoy_id = dispatch_via_route(&f).await;
    // Make it due now (same id: schedule_timer overwrites the payload and both scores).
    let due = DeploymentTimer {
        timer_id:      convoy_id,
        player_wallet: WalletAddress([25u8; 32]),
        sector_id:     f.sector_id,
        timer_type:    TimerType::ConvoyArrival,
        fires_at:      chrono::Utc::now() - chrono::Duration::seconds(1),
    };
    f.state.timer_repo.schedule_timer(due.clone()).await.expect("make due");
    let mut warns = ExpiryWarns::new();
    poll_expiry_once(&f.state, &mut warns).await;

    // A failed cancel leaves the timer due: simulate it, and poll again.
    f.state.timer_repo.schedule_timer(due).await.expect("re-schedule (failed cancel)");
    poll_expiry_once(&f.state, &mut warns).await;

    let (arrived,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM input_logs WHERE campaign_id = $1 AND event_type = 'convoy_arrived' AND payload->>'convoy_id' = $2",
    )
    .bind(f.campaign_id)
    .bind(convoy_id.to_string())
    .fetch_one(&f.pool)
    .await
    .expect("count convoy_arrived");
    let convoy = PostgresConvoyRepository::new(f.pool.clone()).get_convoy(convoy_id).await.expect("get_convoy");
    clean_convoy_fixture(&f, convoy_id).await;

    assert_eq!(arrived, 1, "a retried arrival must not log convoy_arrived twice");
    assert!(!convoy.expect("convoy exists").in_transit, "the convoy stays landed");
}

// ── Brick 7c-1: crew_records (0024) and CrewRepository ──────────────────────

/// The only wallet the 7c-1 tests use.
const CREW_WALLET: [u8; 32] = [32u8; 32];

/// Pre-cleans wallet 32 and seeds its account and a campaign.
async fn seed_crew_fixture(pool: &sqlx::PgPool, map_seed: i64) -> uuid::Uuid {
    clean_wallet_4g1(pool, &CREW_WALLET).await;
    seed_account_and_campaign(pool, CREW_WALLET, map_seed).await
}

/// Asserts `err` is a CHECK violation (23514) of `constraint`.
fn assert_check_violation(err: &sqlx::Error, constraint: &str) {
    let db = err.as_database_error().unwrap_or_else(|| panic!("not a database error: {err}"));
    assert_eq!(db.code().as_deref(), Some("23514"), "expected a check violation, got {err}");
    assert_eq!(db.constraint(), Some(constraint));
}

/// 7c-1 — a `new_hire` crew round-trips through `create_crew`, `get_crew` and
/// `list_crews_by_campaign`.
#[tokio::test]
async fn postgres_crew_repository_round_trips() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let campaign_id = seed_crew_fixture(&pool, 0x7C11_i64).await;
    let repo = PostgresCrewRepository::new(pool.clone());

    let r = CrewRecord::new_hire(campaign_id, CREW_WALLET.to_vec());
    repo.create_crew(r.clone()).await.expect("create_crew failed");

    let got = repo.get_crew(r.crew_id).await.expect("get_crew failed").expect("stored crew");
    let listed = repo.list_crews_by_campaign(campaign_id).await.expect("list failed");
    let missing = repo.get_crew(uuid::Uuid::new_v4()).await.expect("get_crew failed");
    clean_wallet_4g1(&pool, &CREW_WALLET).await;

    for (path, c) in [("get", &got), ("list", listed.first().expect("one listed crew"))] {
        assert_eq!(c.crew_id, r.crew_id, "{path}");
        assert_eq!(c.campaign_id, campaign_id, "{path}");
        assert_eq!(c.player_wallet, CREW_WALLET.to_vec(), "{path}");
        assert_eq!(c.headcount, 8, "{path}");
        assert_eq!(c.xp, 0, "{path}");
        assert_eq!(c.morale, 60, "{path}");
    }
    assert_eq!(listed.len(), 1, "one crew in this campaign");
    assert!(missing.is_none());
}

/// 7c-1 DoD — morale clamps to 0–100 on create. `-5` and `150` would hit
/// `crew_records_morale_range` without the clamp.
#[tokio::test]
async fn postgres_create_crew_clamps_morale_at_both_bounds() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let campaign_id = seed_crew_fixture(&pool, 0x7C12_i64).await;
    let repo = PostgresCrewRepository::new(pool.clone());

    let mut stored = Vec::new();
    for (given, want) in [(-5i16, 0i16), (150, 100), (0, 0), (100, 100)] {
        let mut r = CrewRecord::new_hire(campaign_id, CREW_WALLET.to_vec());
        r.morale = given;
        let created = repo.create_crew(r.clone()).await;
        let got = repo.get_crew(r.crew_id).await.ok().flatten().map(|c| c.morale);
        stored.push((given, want, created, got));
    }
    clean_wallet_4g1(&pool, &CREW_WALLET).await;

    for (given, want, created, got) in stored {
        assert!(created.is_ok(), "morale {given}: create_crew must clamp, not fail: {created:?}");
        assert_eq!(got, Some(want), "morale {given} must be stored as {want}");
    }
}

/// 7c-1 — the SQL defaults are the canon hire: 8 heads, 0 XP, morale 60 (ODQ-40).
#[tokio::test]
async fn crew_records_defaults_are_the_canon_hire() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let campaign_id = seed_crew_fixture(&pool, 0x7C13_i64).await;
    let crew_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO crew_records (crew_id, campaign_id, player_wallet) VALUES ($1, $2, $3)")
        .bind(crew_id)
        .bind(campaign_id)
        .bind(CREW_WALLET.to_vec())
        .execute(&pool)
        .await
        .expect("owner-only insert failed");
    let row: (i16, i32, i16) = sqlx::query_as("SELECT headcount, xp, morale FROM crew_records WHERE crew_id = $1")
        .bind(crew_id)
        .fetch_one(&pool)
        .await
        .expect("read back failed");
    clean_wallet_4g1(&pool, &CREW_WALLET).await;

    assert_eq!(row, (8, 0, 60), "defaults (headcount, xp, morale)");
}

/// 7c-1 — `crew_records_headcount_range`: 9 and -1 are refused; 0 (a wipe) and 8 store.
#[tokio::test]
async fn crew_records_refuses_headcount_outside_zero_to_eight() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let campaign_id = seed_crew_fixture(&pool, 0x7C14_i64).await;

    let mut refusals = Vec::new();
    for headcount in [9i16, -1] {
        let res = sqlx::query("INSERT INTO crew_records (crew_id, campaign_id, player_wallet, headcount) VALUES ($1, $2, $3, $4)")
            .bind(uuid::Uuid::new_v4())
            .bind(campaign_id)
            .bind(CREW_WALLET.to_vec())
            .bind(headcount)
            .execute(&pool)
            .await;
        refusals.push((headcount, res));
    }
    let repo = PostgresCrewRepository::new(pool.clone());
    let mut accepted = Vec::new();
    for headcount in [0i16, 8] {
        let mut r = CrewRecord::new_hire(campaign_id, CREW_WALLET.to_vec());
        r.headcount = headcount;
        let created = repo.create_crew(r.clone()).await;
        let got = repo.get_crew(r.crew_id).await.ok().flatten().map(|c| c.headcount);
        accepted.push((headcount, created, got));
    }
    clean_wallet_4g1(&pool, &CREW_WALLET).await;

    for (headcount, res) in refusals {
        let err = res.expect_err(&format!("headcount {headcount} must violate crew_records_headcount_range"));
        assert_check_violation(&err, "crew_records_headcount_range");
    }
    for (headcount, created, got) in accepted {
        assert!(created.is_ok(), "headcount {headcount} must store: {created:?}");
        assert_eq!(got, Some(headcount));
    }
}

/// 7c-1 — `crew_records_morale_range` backs the clamp: a raw insert (no clamp) of 101 or -1
/// is refused.
#[tokio::test]
async fn crew_records_refuses_raw_morale_outside_zero_to_hundred() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let campaign_id = seed_crew_fixture(&pool, 0x7C15_i64).await;

    let mut results = Vec::new();
    for morale in [101i16, -1] {
        let res = sqlx::query("INSERT INTO crew_records (crew_id, campaign_id, player_wallet, morale) VALUES ($1, $2, $3, $4)")
            .bind(uuid::Uuid::new_v4())
            .bind(campaign_id)
            .bind(CREW_WALLET.to_vec())
            .bind(morale)
            .execute(&pool)
            .await;
        results.push((morale, res));
    }
    clean_wallet_4g1(&pool, &CREW_WALLET).await;
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("campaign cleanup failed");

    for (morale, res) in results {
        let err = res.expect_err(&format!("morale {morale} must violate crew_records_morale_range"));
        assert_check_violation(&err, "crew_records_morale_range");
    }
}

/// 7c-1 — deleting the campaign deletes its crews (`ON DELETE CASCADE`).
#[tokio::test]
async fn crew_records_cascade_with_their_campaign() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let campaign_id = seed_crew_fixture(&pool, 0x7C16_i64).await;
    let crew_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO crew_records (crew_id, campaign_id, player_wallet) VALUES ($1, $2, $3)")
        .bind(crew_id)
        .bind(campaign_id)
        .bind(CREW_WALLET.to_vec())
        .execute(&pool)
        .await
        .expect("insert failed");
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&pool)
        .await
        .expect("campaign delete failed");
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM crew_records WHERE crew_id = $1")
        .bind(crew_id)
        .fetch_one(&pool)
        .await
        .expect("count failed");
    clean_wallet_4g1(&pool, &CREW_WALLET).await;

    assert_eq!(left, 0, "the crew must cascade with its campaign");
}

// ── Brick 7b-1: campaign cleanup through membership; section_records owner (0025) ──

/// The only wallet the 7b-1 tests use.
const SECTION_WALLET: [u8; 32] = [33u8; 32];

/// 7b-1 (Reviewer 7c-1 F1) — a campaign the wallet reaches only through its membership
/// row (no roster row at all) is still collected and deleted by `clean_wallet_4g1`.
#[tokio::test]
async fn clean_wallet_4g1_finds_a_campaign_through_membership() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, SECTION_WALLET, 0x7B11_i64).await;

    clean_wallet_4g1(&pool, &SECTION_WALLET).await;
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id)
        .fetch_one(&pool)
        .await
        .expect("count failed");
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await.expect("campaign cleanup failed");
    sqlx::query("DELETE FROM player_accounts WHERE wallet_address = $1")
        .bind(SECTION_WALLET.to_vec()).execute(&pool).await.ok();

    assert_eq!(
        left, 0,
        "clean_wallet_4g1 left campaign {campaign_id} behind: a campaign reachable only through membership must be collected"
    );
}

/// A wallet-33 Section with an owner and the given Commander.
fn owned_section(campaign_id: uuid::Uuid, player_wallet: Vec<u8>) -> SectionRecord {
    SectionRecord {
        section_id:    uuid::Uuid::new_v4(),
        campaign_id,
        player_wallet,
        name:          "Bravo Section".into(),
        headcount:     8,
        loadout:       serde_json::json!({}),
        xp:            0,
        created_at:    chrono::Utc::now(),
        updated_at:    chrono::Utc::now(),
    }
}

/// 7b-1 — `section_records_headcount_range` (`0025`) stores 0 and 8 and refuses 9 and −1.
#[tokio::test]
async fn section_records_headcount_check_stores_0_and_8_refuses_9_and_minus_1() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, SECTION_WALLET, 0x7B12_i64).await;

    let mut results = Vec::new();
    for headcount in [0i16, 8, 9, -1] {
        let res = sqlx::query(
            "INSERT INTO section_records (section_id, campaign_id, player_wallet, name, headcount)
             VALUES ($1, $2, $3, 'Charlie Section', $4)"
        )
        .bind(uuid::Uuid::new_v4())
        .bind(campaign_id)
        .bind(SECTION_WALLET.to_vec())
        .bind(headcount)
        .execute(&pool)
        .await;
        results.push((headcount, res));
    }
    let stored: Vec<i16> = sqlx::query_scalar("SELECT headcount FROM section_records WHERE campaign_id = $1 ORDER BY headcount")
        .bind(campaign_id)
        .fetch_all(&pool)
        .await
        .expect("read stored headcounts");
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;

    assert_eq!(stored, vec![0i16, 8], "0 and 8 must be stored, nothing else");
    for (headcount, res) in results {
        match headcount {
            0 | 8 => { res.unwrap_or_else(|e| panic!("headcount {headcount} must be stored: {e}")); }
            _ => {
                let err = res.expect_err(&format!("headcount {headcount} must violate section_records_headcount_range"));
                assert_check_violation(&err, "section_records_headcount_range");
            }
        }
    }
}

/// 7b-1 — the owner round-trips through `create_section` and both reads.
#[tokio::test]
async fn postgres_section_repository_round_trips_the_owner() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, SECTION_WALLET, 0x7B13_i64).await;

    let repo = PostgresSectionRepository::new(pool.clone());
    let r = owned_section(campaign_id, SECTION_WALLET.to_vec());
    let created = repo.create_section(r.clone()).await;
    let got = repo.get_section(r.section_id).await;
    let by_campaign = repo.list_sections_by_campaign(campaign_id).await;
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;

    created.expect("create_section failed");
    let got = got.expect("get_section failed").expect("stored section");
    assert_eq!(got.player_wallet, vec![33u8; 32]);
    let by_campaign = by_campaign.expect("list_sections_by_campaign failed");
    assert_eq!(by_campaign.len(), 1);
    assert_eq!(by_campaign[0].player_wallet, vec![33u8; 32]);
}

/// 7b-2 — `0026` makes the owner `NOT NULL`: a raw insert with a null owner is refused
/// (`23502`, naming the column) and nothing is written. Replaces 7b-1's `None` refusal,
/// which the `Vec<u8>` field makes unrepresentable.
#[tokio::test]
async fn section_records_refuses_a_null_owner() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, SECTION_WALLET, 0x7B14_i64).await;

    let section_id = uuid::Uuid::new_v4();
    let res = sqlx::query(
        "INSERT INTO section_records (section_id, campaign_id, player_wallet, name) VALUES ($1, $2, NULL, 'Echo Section')"
    )
    .bind(section_id)
    .bind(campaign_id)
    .execute(&pool)
    .await;
    let (stored,): (i64,) = sqlx::query_as("SELECT count(*) FROM section_records WHERE section_id = $1")
        .bind(section_id)
        .fetch_one(&pool)
        .await
        .expect("count failed");
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;

    let err = res.expect_err("a null owner must be refused");
    let db = err.as_database_error().unwrap_or_else(|| panic!("not a database error: {err}"));
    assert_eq!(db.code().as_deref(), Some("23502"), "expected a not-null violation, got {err}");
    let pg = db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>().expect("a Postgres error");
    assert_eq!(pg.column(), Some("player_wallet"));
    assert_eq!(stored, 0, "a refused section must not be written");
}

/// 7b-1, restated in 5a-1 — a member's Section blocks deleting that membership
/// (`section_records_membership_fkey`, `0030`, `NO ACTION`). This test used to assert that
/// the Section's owner FK (`section_records_player_wallet_fkey`, `RESTRICT`) blocks deleting
/// the account. Since `0030` a Section implies a membership, and the membership's own account
/// `RESTRICT` (`0007`) fires first, so that FK is defence in depth and can no longer be
/// observed by name (5a-1 spec check E3).
#[tokio::test]
async fn a_members_section_restricts_deleting_the_membership() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, SECTION_WALLET, 0x7B15_i64).await;
    PostgresSectionRepository::new(pool.clone())
        .create_section(owned_section(campaign_id, SECTION_WALLET.to_vec()))
        .await
        .expect("create_section failed");

    let res = sqlx::query("DELETE FROM player_campaign_membership WHERE wallet_address = $1")
        .bind(SECTION_WALLET.to_vec())
        .execute(&pool)
        .await;
    // The campaign delete cascades the Section and the membership together.
    clean_wallet_4g1(&pool, &SECTION_WALLET).await;

    assert_fk_violation(res, "section_records_membership_fkey");
}

// ── Brick 7b-2: per-mission Sections on deployment_sections (GAP-50, GAP-66) ─

/// The 7b-2 tests' owner and, for the other-owner cases, the second wallet.
const DEPLOY_WALLET: [u8; 32] = [34u8; 32];
const OTHER_WALLET: [u8; 32] = [35u8; 32];

/// 7b-2 #4 — a missing `section_ids` key, an empty list and a repeated id are each the
/// route's own `400` (JSON error body), not axum's plain-text `422`.
#[tokio::test]
async fn mission_route_refuses_section_ids_missing_empty_or_duplicate() {
    let state = default_state();
    let commander_id = seed_in_memory_commander(&state, DEPLOY_WALLET, |_| {}).await;
    let section = seed_section_for(&state, commander_id).await;
    let token = insert_session(&state, DEPLOY_WALLET);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let mut missing = mission_body(commander_id, &[]);
    missing.as_object_mut().unwrap().remove("section_ids");
    let cases = [
        ("missing", missing),
        ("empty", mission_body(commander_id, &[])),
        ("duplicate", mission_body(commander_id, &[section, section])),
    ];
    for (label, body) in cases {
        let (status, bytes) = post_mission(app.clone(), Some(&token), &body).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{label}: {}", String::from_utf8_lossy(&bytes));
        assert!(!error_message(&bytes).is_empty(), "{label}: the route's JSON error body");
    }
}

/// 7b-2 #5 (GAP-50) — a Rank 1 Commander may take 2 Sections, not 3. The `422` carries
/// the route's JSON error body.
#[tokio::test]
async fn mission_route_refuses_rank_1_with_three_sections() {
    let state = default_state();
    let commander_id = seed_in_memory_commander(&state, DEPLOY_WALLET, |r| r.rank = 1).await;
    let mut s = Vec::new();
    for _ in 0..3 {
        s.push(seed_section_for(&state, commander_id).await);
    }
    let token = insert_session(&state, DEPLOY_WALLET);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app.clone(), Some(&token), &mission_body(commander_id, &s)).await;
    assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    assert!(error_message(&bytes).contains("at most 2 Sections"), "{}", String::from_utf8_lossy(&bytes));

    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &s[..2])).await;
    assert_ne!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY, "2 Sections is within Rank 1's cap: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(status, axum::http::StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
}

/// 7b-2 #6 — an unknown Section, another wallet's and one from another campaign are all
/// `404` with byte-identical bodies.
#[tokio::test]
async fn mission_route_hides_foreign_and_unknown_sections() {
    let state = default_state();
    let commander_id = seed_in_memory_commander(&state, DEPLOY_WALLET, |_| {}).await;
    let campaign_id = state.commander_repo.get_commander(commander_id).await.unwrap().unwrap().campaign_id;
    let theirs = seed_section(&state, OTHER_WALLET, campaign_id).await;
    let elsewhere = seed_section(&state, DEPLOY_WALLET, uuid::Uuid::new_v4()).await;
    let token = insert_session(&state, DEPLOY_WALLET);
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let mut bodies = Vec::new();
    for (label, section) in [("unknown", uuid::Uuid::new_v4()), ("another wallet's", theirs), ("another campaign's", elsewhere)] {
        let (status, bytes) = post_mission(app.clone(), Some(&token), &mission_body(commander_id, &[section])).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{label}: {}", String::from_utf8_lossy(&bytes));
        bodies.push(bytes);
    }
    assert_eq!(bodies[0], bodies[1], "unknown vs another wallet's");
    assert_eq!(bodies[0], bodies[2], "unknown vs another campaign's");
}

/// 7b-2 #7 — a Section held by another Commander's active deployment is a `409` with the
/// Section body, not the Commander-busy one.
#[tokio::test]
async fn mission_route_refuses_a_section_already_deployed() {
    let state = default_state();
    let campaign_id = seed_active_campaign(&state).await;
    let holder = seed_in_memory_commander(&state, DEPLOY_WALLET, |r| r.campaign_id = campaign_id).await;
    let commander_id = seed_in_memory_commander(&state, DEPLOY_WALLET, |r| r.campaign_id = campaign_id).await;
    let section = seed_section(&state, DEPLOY_WALLET, campaign_id).await;
    let held = state.commander_repo.start_deployment(&NewDeployment {
        deployment_id: uuid::Uuid::new_v4(),
        commander_id: holder,
        session_id: uuid::Uuid::new_v4(),
        mission_id: "GUARD_DUTY".into(),
        started_at: chrono::Utc::now(),
        player_wallet: DEPLOY_WALLET.to_vec(),
        campaign_id,
        section_ids: vec![section],
    }).await.unwrap();
    assert_eq!(held, DeploymentStart::Started);
    let token = insert_session(&state, DEPLOY_WALLET);
    let commanders = state.commander_repo.clone();
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(error_message(&bytes), "A Section is already deployed");
    assert!(commanders.list_deployments_by_commander(commander_id).await.unwrap().is_empty(),
        "the refused start must write no deployment");
}

/// 7b-2 #8 — the selection is logged beside `request`, in request order (not sorted), and
/// `request` keeps its pre-7b-2 shape.
#[tokio::test]
async fn mission_route_logs_the_selection_in_request_order() {
    let state = default_state();
    let commander_id = seed_in_memory_commander(&state, DEPLOY_WALLET, |r| r.rank = 1).await;
    let mut sent = vec![seed_section_for(&state, commander_id).await, seed_section_for(&state, commander_id).await];
    sent.sort();
    sent.reverse(); // descending, so request order differs from the sorted insert order
    let token = insert_session(&state, DEPLOY_WALLET);
    let logs = state.input_log_repo.clone();
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &sent)).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let sid = uuid::Uuid::parse_str(resp["session_id"].as_str().unwrap()).unwrap();
    let entries = logs.get_entries_by_session(&sid).await.unwrap();
    assert_eq!(entries.len(), 1);
    let payload = &entries[0].payload;
    let logged: Vec<uuid::Uuid> = serde_json::from_value(payload["selection"]["section_ids"].clone()).expect("selection.section_ids");
    assert_eq!(logged, sent, "the log keeps request order");
    assert!(payload["request"].get("section_ids").is_none(), "payload.request keeps its pre-7b-2 shape");
    assert_eq!(payload["request"]["commander_id"].as_str(), Some(commander_id.to_string().as_str()));
}

// ── Brick 5a-3b: S3, and the first real-player mission end to end ────────────

/// 5a-3b S3 — a Commander whose campaign is `Pending` is refused `409`, and the refusal
/// comes before `start_deployment`, so no deployment row is opened.
#[tokio::test]
async fn mission_resolve_refuses_a_pending_campaign() {
    let state = default_state();
    let pending = state.campaign_repo
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 1 })
        .await
        .expect("create_campaign failed")
        .campaign_id;
    let commander_id = seed_in_memory_commander(&state, DEPLOY_WALLET, |r| r.campaign_id = pending).await;
    let section = seed_section(&state, DEPLOY_WALLET, pending).await;
    let token = insert_session(&state, DEPLOY_WALLET);
    let commanders = state.commander_repo.clone();
    let app = mercs_server::routes::router(std::sync::Arc::new(state));

    let (status, bytes) = post_mission(app, Some(&token), &mission_body(commander_id, &[section])).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(error_message(&bytes), "campaign is not active");
    assert!(commanders.list_deployments_by_commander(commander_id).await.unwrap().is_empty(),
        "a refused mission must open no deployment row");
}

/// 5a-3b E1 — join (which launches the campaign since BP-1) → a mission with the starter
/// Commander and starter Section → `200`, all on Postgres (`sector_repo` stays in memory, so no Redis). The
/// battle's outcome comes from the route's seed, so only its invariants are asserted.
#[tokio::test]
async fn join_launch_and_first_mission_end_to_end() {
    use sim_engine::game_types::OutcomeType;
    use axum::{body::Body, http::{Request, StatusCode}};

    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let wallet = [62u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await; // the join's pick is global
    seed_account(&pool, wallet).await;

    let mut state = pg_route_state(&pool);
    std::sync::Arc::make_mut(&mut state.config).server.join_enabled = true; // 5a-3c: joins are off by default
    let state = std::sync::Arc::new(state);
    let token = insert_session(&state, wallet);

    // 1. Join.
    let join_req = Request::builder()
        .method("POST")
        .uri("/api/campaign/join")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (join_status, join_body) = send(&state, join_req).await;
    let joined: serde_json::Value = serde_json::from_str(&join_body).unwrap_or(serde_json::Value::Null);
    let uuid_of = |key: &str| joined[key].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
    let (campaign_id, commander_id, section_id) = (uuid_of("campaign_id"), uuid_of("commander_id"), uuid_of("section_id"));

    // 2. The join launched the campaign (BP-1): collect its state and sector now.
    let launched = match campaign_id {
        Some(c) => PostgresCampaignRepository::new(pool.clone()).get_campaign(c).await.expect("get_campaign"),
        None => None,
    };
    let sector = match &launched {
        Some(c) => state.sector_repo.get_sector(c.sector_id).await.expect("get_sector"),
        None => None,
    };

    // 3. The first mission, straight after the join, with the starter Commander and Section.
    let (mission_status, mission_bytes) = match (commander_id, section_id) {
        (Some(cmd), Some(sec)) => {
            let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
            let (s, b) = post_mission(app, Some(&token), &mission_body(cmd, &[sec])).await;
            (Some(s), b)
        }
        _ => (None, Vec::new()),
    };
    let resp: serde_json::Value = serde_json::from_slice(&mission_bytes).unwrap_or(serde_json::Value::Null);
    let sid = resp["session_id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());

    // Collect.
    let commanders = PostgresCommanderRepository::new(pool.clone());
    let deps = match commander_id {
        Some(c) => commanders.list_deployments_by_commander(c).await.expect("list deployments"),
        None => Vec::new(),
    };
    let after = match commander_id {
        Some(c) => commanders.get_commander(c).await.expect("get_commander"),
        None => None,
    };
    let logged_campaign = match sid {
        Some(s) => PostgresInputLogRepository::new(pool.clone())
            .get_session_config(&s).await.expect("get_session_config").map(|c| c.campaign_id),
        None => None,
    };
    let mut starters = Vec::new();
    for table in ["commander_records", "foreman_records", "section_records", "crew_records"] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE player_wallet = $1 AND acquired_via = 'starter'"
        ))
        .bind(wallet.to_vec())
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("{table} starter count failed: {e}"));
        starters.push((table, n));
    }

    // Clean: the immutable log rows (campaign_started and the mission session), then the wallet.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    if let Some(c) = campaign_id {
        sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.ok();
    }
    if let Some(s) = sid {
        sqlx::query("DELETE FROM input_logs WHERE session_id = $1").bind(s).execute(&pool).await.ok();
    }
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    if let Some(s) = sid {
        sqlx::query("ALTER TABLE session_configs DISABLE TRIGGER ALL").execute(&pool).await.ok();
        sqlx::query("DELETE FROM session_configs WHERE session_id = $1").bind(s).execute(&pool).await.ok();
        sqlx::query("ALTER TABLE session_configs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    }
    clean_wallet_4g1(&pool, &wallet).await;

    // Assert.
    assert_eq!(join_status, StatusCode::CREATED, "join: {join_body}");
    let campaign_id = campaign_id.expect("campaign_id in the join body");
    assert_eq!(launched.map(|c| c.state), Some(CampaignLifecycle::Active), "the join launched the campaign");
    assert!(sector.is_some(), "the launch wrote the sector");
    assert_eq!(mission_status, Some(StatusCode::OK), "mission: {}", String::from_utf8_lossy(&mission_bytes));
    assert_eq!(deps.len(), 1, "one deployment");
    assert_eq!(deps[0].status, "resolved");
    assert_eq!(Some(deps[0].session_id), sid);
    assert_eq!(logged_campaign, Some(campaign_id), "session_configs.campaign_id is the joined campaign");
    let outcome: OutcomeType = serde_json::from_value(resp["report"]["outcome"].clone()).expect("report.outcome");
    let died = matches!(outcome, OutcomeType::Wipeout);
    assert_eq!(resp["commander"]["kia"].as_bool(), Some(died), "commander.kia must equal outcome == Wipeout");
    assert_eq!(after.expect("the starter Commander").is_kia, died);
    for (table, n) in starters {
        assert_eq!(n, 1, "{table}: exactly one starter row for the joiner");
    }
}

/// 7b-2 #9 — an `f7a1ff3`-shape mission payload (no `selection`) and a 7b-2 one both
/// deserialise their `request` into `MissionResolveRequest`.
#[tokio::test]
async fn pre_7b2_mission_log_still_deserialises() {
    use mercs_server::api_types::MissionResolveRequest;
    let commander_id = "0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f";
    let request = serde_json::json!({
        "squad": { "units": [mission_unit_json()] },
        "mission_id": "GUARD_DUTY",
        "commander_id": commander_id,
    });
    let old = serde_json::json!({ "request": request.clone(), "timestamp": "2026-10-17T12:00:00+00:00" });
    let new = serde_json::json!({
        "request": request,
        "timestamp": "2026-10-18T12:00:00+00:00",
        "selection": { "section_ids": ["5c1d0e2f-3a4b-4c5d-8e6f-7a8b9c0d1e2f"] },
    });
    for (label, payload) in [("f7a1ff3", old), ("7b-2", new)] {
        let req: MissionResolveRequest = serde_json::from_value(payload["request"].clone())
            .unwrap_or_else(|e| panic!("{label} payload.request must deserialise: {e}"));
        assert_eq!(req.commander_id.to_string(), commander_id, "{label}");
        assert_eq!(req.mission_id, "GUARD_DUTY", "{label}");
        assert_eq!(req.squad.units.len(), 1, "{label}");
    }
}

/// The Postgres fixture for 7b-2's FK tests: wallet 34 owns campaigns `c1` and `c2`,
/// Commanders `a` (with active deployment `d`, no Sections) and `a2` in `c1`, Section
/// `s34` in `c1` and `s34b` in `c2`. Wallet 35 owns Section `s35` in `c1`.
struct FkFixture {
    c1: uuid::Uuid,
    a: uuid::Uuid,
    a2: uuid::Uuid,
    d: uuid::Uuid,
    s34: uuid::Uuid,
    s34b: uuid::Uuid,
    s35: uuid::Uuid,
}

async fn pg_commander(pool: &sqlx::PgPool, wallet: [u8; 32], campaign_id: uuid::Uuid, seed: u32) -> uuid::Uuid {
    let rec = mercs_server::commander_gen::generate_commander(wallet.to_vec(), campaign_id, seed);
    let id = rec.commander_id;
    PostgresCommanderRepository::new(pool.clone()).create_commander(rec).await.expect("create_commander failed");
    id
}

async fn pg_section(pool: &sqlx::PgPool, wallet: [u8; 32], campaign_id: uuid::Uuid) -> uuid::Uuid {
    let r = owned_section(campaign_id, wallet.to_vec());
    let id = r.section_id;
    PostgresSectionRepository::new(pool.clone()).create_section(r).await.expect("create_section failed");
    id
}

fn pg_deployment(commander_id: uuid::Uuid, campaign_id: uuid::Uuid, section_ids: Vec<uuid::Uuid>, started_at: chrono::DateTime<chrono::Utc>) -> NewDeployment {
    NewDeployment {
        deployment_id: uuid::Uuid::new_v4(),
        commander_id,
        session_id: uuid::Uuid::new_v4(),
        mission_id: "GUARD_DUTY".into(),
        started_at,
        player_wallet: DEPLOY_WALLET.to_vec(),
        campaign_id,
        section_ids,
    }
}

async fn clean_7b2(pool: &sqlx::PgPool) {
    clean_wallet_4g1(pool, &DEPLOY_WALLET).await;
    clean_wallet_4g1(pool, &OTHER_WALLET).await;
}

async fn fk_fixture(pool: &sqlx::PgPool) -> FkFixture {
    clean_7b2(pool).await;
    let c1 = seed_account_and_campaign(pool, DEPLOY_WALLET, 0x7B21_i64).await;
    let c2 = seed_account_and_campaign(pool, DEPLOY_WALLET, 0x7B22_i64).await;
    seed_account_and_campaign(pool, OTHER_WALLET, 0x7B23_i64).await;
    let a = pg_commander(pool, DEPLOY_WALLET, c1, 42).await;
    let a2 = pg_commander(pool, DEPLOY_WALLET, c1, 43).await;
    let dep = pg_deployment(a, c1, vec![], chrono::Utc::now());
    let started = PostgresCommanderRepository::new(pool.clone()).start_deployment(&dep).await.expect("start_deployment failed");
    assert_eq!(started, DeploymentStart::Started);
    // 5a-1 (0030): wallet 35's Section in c1 needs wallet 35's membership of c1.
    PostgresMembershipRepository::new(pool.clone())
        .add_member(&OTHER_WALLET, c1, None, None)
        .await
        .expect("membership seed failed");
    FkFixture {
        c1, a, a2, d: dep.deployment_id,
        s34: pg_section(pool, DEPLOY_WALLET, c1).await,
        s34b: pg_section(pool, DEPLOY_WALLET, c2).await,
        s35: pg_section(pool, OTHER_WALLET, c1).await,
    }
}

/// A raw `deployment_sections` insert, as a forged or buggy writer would make it.
async fn raw_join_insert(pool: &sqlx::PgPool, d: uuid::Uuid, section: uuid::Uuid, commander: uuid::Uuid, wallet: [u8; 32], campaign: uuid::Uuid) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query("INSERT INTO deployment_sections (deployment_id, section_id, commander_id, player_wallet, campaign_id) VALUES ($1, $2, $3, $4, $5)")
        .bind(d).bind(section).bind(commander).bind(wallet.to_vec()).bind(campaign)
        .execute(pool)
        .await
}

fn assert_fk_violation(res: Result<sqlx::postgres::PgQueryResult, sqlx::Error>, constraint: &str) {
    let err = res.expect_err(&format!("the insert must violate {constraint}"));
    let db = err.as_database_error().unwrap_or_else(|| panic!("not a database error: {err}"));
    assert_eq!(db.code().as_deref(), Some("23503"), "expected a foreign-key violation, got {err}");
    assert_eq!(db.constraint(), Some(constraint));
}

/// 7b-2 #10 (GAP-66) — another wallet's Section, with the deployment pair and the
/// Commander triple satisfied.
#[tokio::test]
async fn deployment_sections_refuses_another_wallets_section() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let f = fk_fixture(&pool).await;
    let res = raw_join_insert(&pool, f.d, f.s35, f.a, DEPLOY_WALLET, f.c1).await;
    clean_7b2(&pool).await;
    assert_fk_violation(res, "deployment_sections_section_fkey");
}

/// 7b-2 #11 (GAP-66) — the same wallet's Section from another campaign.
#[tokio::test]
async fn deployment_sections_refuses_another_campaigns_section() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let f = fk_fixture(&pool).await;
    let res = raw_join_insert(&pool, f.d, f.s34b, f.a, DEPLOY_WALLET, f.c1).await;
    clean_7b2(&pool).await;
    assert_fk_violation(res, "deployment_sections_section_fkey");
}

/// 7b-2 #12 (GAP-66) — the Section triple and the deployment pair hold (wallet 35's
/// Section, named as wallet 35's), but the Commander is not wallet 35's.
#[tokio::test]
async fn deployment_sections_refuses_a_commander_of_another_owner() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let f = fk_fixture(&pool).await;
    let res = raw_join_insert(&pool, f.d, f.s35, f.a, OTHER_WALLET, f.c1).await;
    clean_7b2(&pool).await;
    assert_fk_violation(res, "deployment_sections_commander_fkey");
}

/// 7b-2 #13 (GAP-66) — the Commander and Section triples hold, but the deployment is not
/// this Commander's.
#[tokio::test]
async fn deployment_sections_refuses_a_mismatched_deployment_pair() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let f = fk_fixture(&pool).await;
    let res = raw_join_insert(&pool, f.d, f.s34, f.a2, DEPLOY_WALLET, f.c1).await;
    clean_7b2(&pool).await;
    assert_fk_violation(res, "deployment_sections_deployment_fkey");
}

/// Pre-cleans, then seeds wallet 34 with a campaign, two Commanders and two Sections.
/// Returns (campaign, commander a, commander b, section 1, section 2).
async fn two_commanders_two_sections(pool: &sqlx::PgPool) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    clean_7b2(pool).await;
    let c = seed_account_and_campaign(pool, DEPLOY_WALLET, 0x7B24_i64).await;
    let a = pg_commander(pool, DEPLOY_WALLET, c, 42).await;
    let b = pg_commander(pool, DEPLOY_WALLET, c, 43).await;
    let s1 = pg_section(pool, DEPLOY_WALLET, c).await;
    let s2 = pg_section(pool, DEPLOY_WALLET, c).await;
    (c, a, b, s1, s2)
}

/// Whether the (deployment, Section) join row has been released.
async fn released(pool: &sqlx::PgPool, deployment_id: uuid::Uuid, section_id: uuid::Uuid) -> bool {
    sqlx::query_scalar("SELECT released_at IS NOT NULL FROM deployment_sections WHERE deployment_id = $1 AND section_id = $2")
        .bind(deployment_id).bind(section_id)
        .fetch_one(pool).await.expect("join row read failed")
}

/// 7b-2 #14 — a busy Section refuses the whole start: no deployment row and no join row
/// for the second Commander, whose other Section stays free for its next start.
#[tokio::test]
async fn postgres_start_deployment_refuses_a_busy_section_and_rolls_back() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let (c, a, b, s1, s2) = two_commanders_two_sections(&pool).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let now = chrono::Utc::now();
    let first = repo.start_deployment(&pg_deployment(a, c, vec![s1], now)).await;
    let busy = repo.start_deployment(&pg_deployment(b, c, vec![s2, s1], now)).await;
    let b_rows_after_refusal = repo.list_deployments_by_commander(b).await;
    let (s2_rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM deployment_sections WHERE section_id = $1")
        .bind(s2).fetch_one(&pool).await.expect("count failed");
    let retry = repo.start_deployment(&pg_deployment(b, c, vec![s2], now)).await;
    clean_7b2(&pool).await;

    assert_eq!(first.expect("first start failed"), DeploymentStart::Started);
    assert_eq!(busy.expect("busy start failed"), DeploymentStart::SectionBusy);
    assert!(b_rows_after_refusal.expect("list failed").is_empty(), "the refused start must roll back its deployment row");
    assert_eq!(s2_rows, 0, "the refused start must roll back its join rows");
    assert_eq!(retry.expect("retry failed"), DeploymentStart::Started, "the free Section starts once the busy one is dropped");
}

/// 7b-2 #15 — the stale abort (same Commander) releases the Section, which starts again.
#[tokio::test]
async fn postgres_section_redeploys_after_stale_abort() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let (c, a, _b, s1, _s2) = two_commanders_two_sections(&pool).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let now = chrono::Utc::now();
    let stale = pg_deployment(a, c, vec![s1], now - chrono::Duration::minutes(6));
    let first = repo.start_deployment(&stale).await;
    let again = repo.start_deployment(&pg_deployment(a, c, vec![s1], now)).await;
    let was_released = released(&pool, stale.deployment_id, s1).await;
    clean_7b2(&pool).await;

    assert_eq!(first.expect("stale start failed"), DeploymentStart::Started);
    assert_eq!(again.expect("restart failed"), DeploymentStart::Started, "the stale abort must release the Section");
    assert!(was_released, "the stale deployment's join row must be released");
}

/// 7b-2 #16 — `abort_deployment` releases the Section, which another Commander then takes.
#[tokio::test]
async fn postgres_section_redeploys_after_abort() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let (c, a, b, s1, _s2) = two_commanders_two_sections(&pool).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let now = chrono::Utc::now();
    let d = pg_deployment(a, c, vec![s1], now);
    let first = repo.start_deployment(&d).await;
    let aborted = repo.abort_deployment(d.deployment_id).await;
    let was_released = released(&pool, d.deployment_id, s1).await;
    let again = repo.start_deployment(&pg_deployment(b, c, vec![s1], now)).await;
    clean_7b2(&pool).await;

    assert_eq!(first.expect("first start failed"), DeploymentStart::Started);
    aborted.expect("abort failed");
    assert!(was_released, "the abort must release the join row");
    assert_eq!(again.expect("restart failed"), DeploymentStart::Started, "the aborted deployment's Section must start again");
}

/// 7b-2 #17 — `resolve_deployment`'s close releases the Section.
#[tokio::test]
async fn postgres_section_redeploys_after_resolve() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let (c, a, b, s1, _s2) = two_commanders_two_sections(&pool).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let now = chrono::Utc::now();
    let d = pg_deployment(a, c, vec![s1], now);
    let first = repo.start_deployment(&d).await;
    let cfg = sim_engine::config::SimConfig::default();
    let resolved = repo.resolve_deployment(d.deployment_id, a, 0, 1, 0.0, &sim_engine::game_types::OutcomeType::Success, &cfg).await;
    let was_released = released(&pool, d.deployment_id, s1).await;
    let again = repo.start_deployment(&pg_deployment(b, c, vec![s1], now)).await;
    clean_7b2(&pool).await;

    assert_eq!(first.expect("first start failed"), DeploymentStart::Started);
    resolved.expect("resolve failed");
    assert!(was_released, "the resolve close must release the join row");
    assert_eq!(again.expect("restart failed"), DeploymentStart::Started, "the resolved deployment's Section must start again");
}

/// 7b-2 #18 (R4) — Commander A's stale deployment holds S; Commander B starts with S, and
/// the stale rule aborts A's deployment although it is not B's.
#[tokio::test]
async fn postgres_stale_section_holder_is_aborted_for_another_commander() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let (c, a, b, s1, _s2) = two_commanders_two_sections(&pool).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let now = chrono::Utc::now();
    let first = repo.start_deployment(&pg_deployment(a, c, vec![s1], now - chrono::Duration::minutes(6))).await;
    let other = repo.start_deployment(&pg_deployment(b, c, vec![s1], now)).await;
    let a_rows = repo.list_deployments_by_commander(a).await;
    clean_7b2(&pool).await;

    assert_eq!(first.expect("stale start failed"), DeploymentStart::Started);
    assert_eq!(other.expect("other start failed"), DeploymentStart::Started, "a stale holder must not block another Commander");
    let a_rows = a_rows.expect("list failed");
    assert_eq!(a_rows.len(), 1);
    assert_eq!(a_rows[0].status, "aborted", "the stale holder must be aborted");
}

/// 7b-2 #19 — the trigger releases on any close, including a raw status write as `.prev`
/// (f7a1ff3) makes it, which knows nothing of deployment_sections.
#[tokio::test]
async fn trigger_releases_sections_on_a_raw_status_update() {
    let pool = match test_pool().await {
        Some(p) => p,
        None => { eprintln!("TEST_DATABASE_URL not set — skipping live DB integration test"); return; }
    };
    let (c, a, _b, s1, _s2) = two_commanders_two_sections(&pool).await;
    let repo = PostgresCommanderRepository::new(pool.clone());
    let d = pg_deployment(a, c, vec![s1], chrono::Utc::now());
    let first = repo.start_deployment(&d).await;
    let held_before = released(&pool, d.deployment_id, s1).await;
    let raw = sqlx::query("UPDATE deployment_records SET status = 'aborted', closed_at = now() WHERE deployment_id = $1 AND status = 'active'")
        .bind(d.deployment_id)
        .execute(&pool)
        .await;
    let was_released = released(&pool, d.deployment_id, s1).await;
    clean_7b2(&pool).await;

    assert_eq!(first.expect("start failed"), DeploymentStart::Started);
    assert!(!held_before, "precondition: the join row is active");
    assert_eq!(raw.expect("raw close failed").rows_affected(), 1);
    assert!(was_released, "the trigger must release the Section on a raw close");
}

// ── Brick 7d-1: augmentee_records (0029) ─────────────────────────────────────

/// The 7d-1 tests' wallet, and "another wallet" for the owner-integrity tests.
const AUG_WALLET: [u8; 32] = [36u8; 32];
const AUG_OTHER: [u8; 32] = [37u8; 32];

/// A stored Section owned by `wallet` in `campaign_id`.
async fn aug_section(pool: &sqlx::PgPool, wallet: [u8; 32], campaign_id: uuid::Uuid) -> uuid::Uuid {
    let r = owned_section(campaign_id, wallet.to_vec());
    PostgresSectionRepository::new(pool.clone()).create_section(r.clone()).await.expect("create_section failed");
    r.section_id
}

/// A stored Mining Crew owned by `wallet` in `campaign_id`.
async fn aug_crew(pool: &sqlx::PgPool, wallet: [u8; 32], campaign_id: uuid::Uuid) -> uuid::Uuid {
    let r = CrewRecord::new_hire(campaign_id, wallet.to_vec());
    PostgresCrewRepository::new(pool.clone()).create_crew(r.clone()).await.expect("create_crew failed");
    r.crew_id
}

/// A generated augmentee; the SKU must fit the owner.
fn aug_record(campaign_id: uuid::Uuid, owner: AugmenteeOwner, slot: i16, augment: Augment) -> AugmenteeRecord {
    mercs_server::augmentee_gen::generate_augmentee(
        AUG_WALLET.to_vec(), campaign_id, owner, slot, CommanderOrigin::HumanCorporate, augment, FittingSource::Church, 36,
    )
}

/// A raw insert, so a refusal surfaces as the database error rather than `Internal`.
async fn raw_augmentee(
    pool: &sqlx::PgPool,
    wallet: [u8; 32],
    campaign_id: uuid::Uuid,
    section_id: Option<uuid::Uuid>,
    crew_id: Option<uuid::Uuid>,
    slot: i16,
    augment: &str,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO augmentee_records (augmentee_id, campaign_id, player_wallet, section_id, crew_id, slot,
                                        name, callsign, augment, fitting_source, prng_seed_state)
         VALUES ($1, $2, $3, $4, $5, $6, 'Kira Drek', 'Rivet', $7, 'Secular', 0)"
    )
    .bind(uuid::Uuid::new_v4())
    .bind(campaign_id)
    .bind(wallet.to_vec())
    .bind(section_id)
    .bind(crew_id)
    .bind(slot)
    .bind(augment)
    .execute(pool)
    .await
}

fn assert_unique_violation(res: Result<sqlx::postgres::PgQueryResult, sqlx::Error>, constraint: &str) {
    let err = res.expect_err(&format!("the insert must violate {constraint}"));
    let db = err.as_database_error().unwrap_or_else(|| panic!("not a database error: {err}"));
    assert_eq!(db.code().as_deref(), Some("23505"), "expected a unique violation, got {err}");
    assert_eq!(db.constraint(), Some(constraint));
}

/// 7d-1 D1 — a Section and a Crew augmentee round-trip through create, get and both lists.
#[tokio::test]
async fn augmentee_records_round_trip() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D11_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let crew = aug_crew(&pool, AUG_WALLET, campaign_id).await;
    let repo = PostgresAugmenteeRepository::new(pool.clone());

    let s1 = aug_record(campaign_id, AugmenteeOwner::Section(section), 1, Augment::IntegralGunArm);
    let s2 = aug_record(campaign_id, AugmenteeOwner::Section(section), 2, Augment::GloomSightOcularsTactical);
    let c1 = aug_record(campaign_id, AugmenteeOwner::Crew(crew), 1, Augment::LoadBearerFrame);
    let mut created = Vec::new();
    for r in [&s1, &s2, &c1] {
        created.push(repo.create_augmentee(r.clone()).await);
    }
    let got = repo.get_augmentee(s1.augmentee_id).await;
    let missing = repo.get_augmentee(uuid::Uuid::new_v4()).await;
    let by_section = repo.list_augmentees_by_owner(&AugmenteeOwner::Section(section)).await;
    let by_crew = repo.list_augmentees_by_owner(&AugmenteeOwner::Crew(crew)).await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    for c in created {
        c.expect("create_augmentee failed");
    }
    let got = got.expect("get_augmentee failed");
    assert_eq!(got.owner, AugmenteeOwner::Section(section));
    assert_eq!((got.slot, got.augment.as_str(), got.fitting_source.as_str()), (1, "Integral Gun-Arm", "Church"));
    assert_eq!((got.name.as_str(), got.callsign.as_str()), (s1.name.as_str(), s1.callsign.as_str()));
    assert_eq!((got.casualty_class, got.fate_at, got.prng_seed_state), (None, None, 36));
    assert!(matches!(missing, Err(RepositoryError::NotFound)), "an unknown id is NotFound: {missing:?}");
    let mut section_ids: Vec<_> = by_section.expect("list by Section failed").iter().map(|r| r.augmentee_id).collect();
    section_ids.sort();
    let mut want = vec![s1.augmentee_id, s2.augmentee_id];
    want.sort();
    assert_eq!(section_ids, want);
    let crew_rows = by_crew.expect("list by Crew failed");
    assert_eq!(crew_rows.len(), 1);
    assert_eq!((crew_rows[0].augmentee_id, crew_rows[0].owner), (c1.augmentee_id, AugmenteeOwner::Crew(crew)));
}

/// 7d-1 D2 — a held slot refuses a second serving holder; a KIA frees it.
#[tokio::test]
async fn augmentee_slot_is_refused_while_held_and_freed_by_death() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D12_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let repo = PostgresAugmenteeRepository::new(pool.clone());
    let holder = aug_record(campaign_id, AugmenteeOwner::Section(section), 1, Augment::DopplerCochlea);
    let created = repo.create_augmentee(holder.clone()).await;
    let second = raw_augmentee(&pool, AUG_WALLET, campaign_id, Some(section), None, 1, "Doppler Cochlea").await;
    let fate = repo.record_fate(holder.augmentee_id, CasualtyClass::Kia).await;
    let after = raw_augmentee(&pool, AUG_WALLET, campaign_id, Some(section), None, 1, "Doppler Cochlea").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    created.expect("create_augmentee failed");
    assert_unique_violation(second, "augmentee_records_section_slot_held");
    fate.expect("record_fate(KIA) failed");
    after.expect("a KIA holder must free the slot");
}

/// 7d-1 D3 — a Wounded holder keeps the slot.
#[tokio::test]
async fn augmentee_wounded_holder_keeps_the_slot() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D13_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let repo = PostgresAugmenteeRepository::new(pool.clone());
    let holder = aug_record(campaign_id, AugmenteeOwner::Section(section), 1, Augment::DopplerCochlea);
    let created = repo.create_augmentee(holder.clone()).await;
    let fate = repo.record_fate(holder.augmentee_id, CasualtyClass::Wounded).await;
    let second = raw_augmentee(&pool, AUG_WALLET, campaign_id, Some(section), None, 1, "Doppler Cochlea").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    created.expect("create_augmentee failed");
    fate.expect("record_fate(Wounded) failed");
    assert_unique_violation(second, "augmentee_records_section_slot_held");
}

/// 7d-1 D4 — an industrial SKU on a Section.
#[tokio::test]
async fn augmentee_records_refuse_an_industrial_augment_on_a_section() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D14_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let res = raw_augmentee(&pool, AUG_WALLET, campaign_id, Some(section), None, 1, "Geophone Cochlea").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    assert_check_violation(&res.expect_err("an industrial SKU must not fit a Section"), "augmentee_records_combat_augment_canon");
}

/// 7d-1 D5 — a combat SKU on a Crew.
#[tokio::test]
async fn augmentee_records_refuse_a_combat_augment_on_a_crew() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D15_i64).await;
    let crew = aug_crew(&pool, AUG_WALLET, campaign_id).await;
    let res = raw_augmentee(&pool, AUG_WALLET, campaign_id, None, Some(crew), 1, "Integral Gun-Arm").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    assert_check_violation(&res.expect_err("a combat SKU must not fit a Crew"), "augmentee_records_industrial_augment_canon");
}

/// 7d-1 D6 — zero owners, two owners and slot 3.
#[tokio::test]
async fn augmentee_records_refuse_bad_owner_count_and_slot() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D16_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let crew = aug_crew(&pool, AUG_WALLET, campaign_id).await;
    // Two owners can't keep the other CHECKs satisfied: no SKU is in both catalogues, so a
    // two-owner row always fails one *_augment_canon too, and Postgres tests CHECKs in name
    // order, so `industrial_augment_canon` (before `one_owner`) is the name it reports.
    let none = raw_augmentee(&pool, AUG_WALLET, campaign_id, None, None, 1, "Integral Gun-Arm").await;
    let both = raw_augmentee(&pool, AUG_WALLET, campaign_id, Some(section), Some(crew), 1, "Integral Gun-Arm").await;
    let slot3 = raw_augmentee(&pool, AUG_WALLET, campaign_id, Some(section), None, 3, "Integral Gun-Arm").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    assert_check_violation(&none.expect_err("zero owners must be refused"), "augmentee_records_one_owner");
    assert_check_violation(&both.expect_err("two owners must be refused"), "augmentee_records_industrial_augment_canon");
    assert_check_violation(&slot3.expect_err("slot 3 must be refused"), "augmentee_records_slot_range");
}

/// 7d-1 D7 — wallet 37's augmentee on wallet 36's Section.
#[tokio::test]
async fn augmentee_records_refuse_another_wallets_section() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    clean_wallet_4g1(&pool, &AUG_OTHER).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D17_i64).await;
    seed_account_and_campaign(&pool, AUG_OTHER, 0x7D18_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let res = raw_augmentee(&pool, AUG_OTHER, campaign_id, Some(section), None, 1, "Integral Gun-Arm").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    clean_wallet_4g1(&pool, &AUG_OTHER).await;

    assert_fk_violation(res, "augmentee_records_section_fkey");
}

/// 7d-1 D8 — wallet 37's augmentee on wallet 36's Crew.
#[tokio::test]
async fn augmentee_records_refuse_another_wallets_crew() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    clean_wallet_4g1(&pool, &AUG_OTHER).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D19_i64).await;
    seed_account_and_campaign(&pool, AUG_OTHER, 0x7D1A_i64).await;
    let crew = aug_crew(&pool, AUG_WALLET, campaign_id).await;
    let res = raw_augmentee(&pool, AUG_OTHER, campaign_id, None, Some(crew), 1, "Load-Bearer Frame").await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    clean_wallet_4g1(&pool, &AUG_OTHER).await;

    assert_fk_violation(res, "augmentee_records_crew_fkey");
}

/// 7d-1 D9 — `record_fate` on Postgres: a terminal or unknown augmentee is `NotFound`;
/// Wounded can still die, and `fate_at` is set.
#[tokio::test]
async fn augmentee_record_fate_is_final_once_terminal() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D1B_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let repo = PostgresAugmenteeRepository::new(pool.clone());
    let dead = aug_record(campaign_id, AugmenteeOwner::Section(section), 1, Augment::IntegralGunArm);
    let hurt = aug_record(campaign_id, AugmenteeOwner::Section(section), 2, Augment::ThermalLanceArm);
    repo.create_augmentee(dead.clone()).await.expect("create dead");
    repo.create_augmentee(hurt.clone()).await.expect("create hurt");
    let kia = repo.record_fate(dead.augmentee_id, CasualtyClass::Kia).await;
    let refate = repo.record_fate(dead.augmentee_id, CasualtyClass::Mia).await;
    let unknown = repo.record_fate(uuid::Uuid::new_v4(), CasualtyClass::Kia).await;
    let wounded = repo.record_fate(hurt.augmentee_id, CasualtyClass::Wounded).await;
    let then_kia = repo.record_fate(hurt.augmentee_id, CasualtyClass::Kia).await;
    let dead_row = repo.get_augmentee(dead.augmentee_id).await;
    let hurt_row = repo.get_augmentee(hurt.augmentee_id).await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    kia.expect("record_fate(KIA) failed");
    assert!(matches!(refate, Err(RepositoryError::NotFound)), "a KIA must not be re-fated: {refate:?}");
    assert!(matches!(unknown, Err(RepositoryError::NotFound)), "an unknown id is NotFound: {unknown:?}");
    wounded.expect("record_fate(Wounded) failed");
    then_kia.expect("a Wounded augmentee can still die");
    let dead_row = dead_row.expect("get dead");
    assert_eq!(dead_row.casualty_class.as_deref(), Some("KIA"), "the refused MIA must not overwrite KIA");
    assert!(dead_row.fate_at.is_some());
    let hurt_row = hurt_row.expect("get hurt");
    assert_eq!(hurt_row.casualty_class.as_deref(), Some("KIA"));
    assert!(hurt_row.fate_at.is_some(), "fate_at must be set");
}

/// 7d-1 D10 — a wipe is a state, never a `DELETE`: the augmentee RESTRICTs its Section.
#[tokio::test]
async fn a_section_with_an_augmentee_cannot_be_deleted() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D1C_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let created = PostgresAugmenteeRepository::new(pool.clone())
        .create_augmentee(aug_record(campaign_id, AugmenteeOwner::Section(section), 1, Augment::IntegralGunArm))
        .await;
    let res = sqlx::query("DELETE FROM section_records WHERE section_id = $1").bind(section).execute(&pool).await;
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    created.expect("create_augmentee failed");
    assert_fk_violation(res, "augmentee_records_section_fkey");
}

/// 7d-1 D11 — a campaign delete cascades through its Sections, Crews and augmentees, and
/// (since 5a-1) through the membership that `0030`'s `NO ACTION` FKs tie them to: the
/// campaign is deleted directly, and the check runs at the end of the statement.
#[tokio::test]
async fn a_campaign_delete_cascades_through_augmentees() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &AUG_WALLET).await;
    let campaign_id = seed_account_and_campaign(&pool, AUG_WALLET, 0x7D1D_i64).await;
    let section = aug_section(&pool, AUG_WALLET, campaign_id).await;
    let crew = aug_crew(&pool, AUG_WALLET, campaign_id).await;
    let repo = PostgresAugmenteeRepository::new(pool.clone());
    let a = repo.create_augmentee(aug_record(campaign_id, AugmenteeOwner::Section(section), 1, Augment::IntegralGunArm)).await;
    let b = repo.create_augmentee(aug_record(campaign_id, AugmenteeOwner::Crew(crew), 1, Augment::GeophoneCochlea)).await;
    let deleted = sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(&pool).await;
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM augmentee_records WHERE campaign_id = $1")
        .bind(campaign_id).fetch_one(&pool).await.expect("count failed");
    clean_wallet_4g1(&pool, &AUG_WALLET).await;

    a.expect("create Section augmentee");
    b.expect("create Crew augmentee");
    assert_eq!(deleted.expect("the campaign delete must cascade").rows_affected(), 1);
    assert_eq!(left, 0, "the campaign's augmentees must cascade with it");
}

// ── Brick 5a-1: the join schema (0030) ───────────────────────────────────────

/// The starter-index tests' wallet (one campaign per test).
const STARTER_WALLET: [u8; 32] = [38u8; 32];
/// The membership-FK tests' wallet (one campaign A per test, plus a campaign B it isn't in).
const NON_MEMBER_WALLET: [u8; 32] = [39u8; 32];
/// The Postgres missing-Grade test's wallet.
const GRADE_WALLET: [u8; 32] = [40u8; 32];
/// The duplicate-membership probe's wallet.
const DUP_MEMBER_WALLET: [u8; 32] = [41u8; 32];

/// Marks `first` then `second` as starters in `table` (keyed by `id_col`), with raw
/// unchecked SQL (no `.sqlx` file). Returns the second UPDATE's result; the first must
/// succeed, since one starter per (campaign, wallet) is allowed.
async fn mark_two_starters(
    pool: &sqlx::PgPool,
    table: &str,
    id_col: &str,
    first: uuid::Uuid,
    second: uuid::Uuid,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    let sql = format!("UPDATE {table} SET acquired_via = 'starter' WHERE {id_col} = $1");
    let one = sqlx::query(&sql).bind(first).execute(pool).await
        .unwrap_or_else(|e| panic!("{table}: the first starter must be accepted: {e}"));
    assert_eq!(one.rows_affected(), 1, "{table}: the first starter row must exist");
    sqlx::query(&sql).bind(second).execute(pool).await
}

/// 5a-1 #1 — a second starter Commander for one (campaign, wallet) is refused.
#[tokio::test]
async fn second_starter_refused_commander() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;
    let c = seed_account_and_campaign(&pool, STARTER_WALLET, 0x5A11_i64).await;
    let a = pg_commander(&pool, STARTER_WALLET, c, 42).await;
    let b = pg_commander(&pool, STARTER_WALLET, c, 43).await;
    let res = mark_two_starters(&pool, "commander_records", "commander_id", a, b).await;
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;

    assert_unique_violation(res, "commander_records_one_starter");
}

/// 5a-1 #2 — a second starter Foreman for one (campaign, wallet) is refused.
#[tokio::test]
async fn second_starter_refused_foreman() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;
    let c = seed_account_and_campaign(&pool, STARTER_WALLET, 0x5A12_i64).await;
    let repo = PostgresForemanRepository::new(pool.clone());
    let a = hand_built_foreman(STARTER_WALLET, c);
    let b = hand_built_foreman(STARTER_WALLET, c);
    repo.create_foreman(a.clone()).await.expect("create_foreman a failed");
    repo.create_foreman(b.clone()).await.expect("create_foreman b failed");
    let res = mark_two_starters(&pool, "foreman_records", "foreman_id", a.foreman_id, b.foreman_id).await;
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;

    assert_unique_violation(res, "foreman_records_one_starter");
}

/// 5a-1 #3 — a second starter Section for one (campaign, wallet) is refused.
#[tokio::test]
async fn second_starter_refused_section() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;
    let c = seed_account_and_campaign(&pool, STARTER_WALLET, 0x5A13_i64).await;
    let a = pg_section(&pool, STARTER_WALLET, c).await;
    let b = pg_section(&pool, STARTER_WALLET, c).await;
    let res = mark_two_starters(&pool, "section_records", "section_id", a, b).await;
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;

    assert_unique_violation(res, "section_records_one_starter");
}

/// 5a-1 #4 — a second starter Crew for one (campaign, wallet) is refused.
#[tokio::test]
async fn second_starter_refused_crew() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;
    let c = seed_account_and_campaign(&pool, STARTER_WALLET, 0x5A14_i64).await;
    let a = aug_crew(&pool, STARTER_WALLET, c).await;
    let b = aug_crew(&pool, STARTER_WALLET, c).await;
    let res = mark_two_starters(&pool, "crew_records", "crew_id", a, b).await;
    clean_wallet_4g1(&pool, &STARTER_WALLET).await;

    assert_unique_violation(res, "crew_records_one_starter");
}

/// Moves `id` (a fresh row with no referrers, spec check E5) in `table` into a brand-new
/// campaign B that the owner is not a member of, with raw unchecked SQL. Returns the
/// UPDATE's result and B, which `clean_wallet_4g1` can't find, so the caller deletes it.
async fn move_to_non_member_campaign(
    pool: &sqlx::PgPool,
    table: &str,
    id_col: &str,
    id: uuid::Uuid,
    map_seed: i64,
) -> (Result<sqlx::postgres::PgQueryResult, sqlx::Error>, uuid::Uuid) {
    let b = PostgresCampaignRepository::new(pool.clone())
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed })
        .await
        .expect("create campaign B failed")
        .campaign_id;
    let res = sqlx::query(&format!("UPDATE {table} SET campaign_id = $1 WHERE {id_col} = $2"))
        .bind(b).bind(id).execute(pool).await;
    (res, b)
}

async fn delete_campaign(pool: &sqlx::PgPool, campaign_id: uuid::Uuid) {
    sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = $1")
        .bind(campaign_id).execute(pool).await.expect("campaign B cleanup failed");
}

/// 5a-1 #5 — a Commander for a non-member of its campaign is refused.
#[tokio::test]
async fn non_member_row_refused_commander() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    let a = seed_account_and_campaign(&pool, NON_MEMBER_WALLET, 0x5A15_i64).await;
    let id = pg_commander(&pool, NON_MEMBER_WALLET, a, 42).await;
    let (res, b) = move_to_non_member_campaign(&pool, "commander_records", "commander_id", id, 0x5A25_i64).await;
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    delete_campaign(&pool, b).await;

    assert_fk_violation(res, "commander_records_membership_fkey");
}

/// 5a-1 #6 — a Foreman for a non-member of its campaign is refused.
#[tokio::test]
async fn non_member_row_refused_foreman() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    let a = seed_account_and_campaign(&pool, NON_MEMBER_WALLET, 0x5A16_i64).await;
    let r = hand_built_foreman(NON_MEMBER_WALLET, a);
    PostgresForemanRepository::new(pool.clone()).create_foreman(r.clone()).await.expect("create_foreman failed");
    let (res, b) = move_to_non_member_campaign(&pool, "foreman_records", "foreman_id", r.foreman_id, 0x5A26_i64).await;
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    delete_campaign(&pool, b).await;

    assert_fk_violation(res, "foreman_records_membership_fkey");
}

/// 5a-1 #7 — a Section for a non-member of its campaign is refused.
#[tokio::test]
async fn non_member_row_refused_section() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    let a = seed_account_and_campaign(&pool, NON_MEMBER_WALLET, 0x5A17_i64).await;
    let id = pg_section(&pool, NON_MEMBER_WALLET, a).await;
    let (res, b) = move_to_non_member_campaign(&pool, "section_records", "section_id", id, 0x5A27_i64).await;
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    delete_campaign(&pool, b).await;

    assert_fk_violation(res, "section_records_membership_fkey");
}

/// 5a-1 #8 — a Mining Crew for a non-member of its campaign is refused.
#[tokio::test]
async fn non_member_row_refused_crew() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    let a = seed_account_and_campaign(&pool, NON_MEMBER_WALLET, 0x5A18_i64).await;
    let id = aug_crew(&pool, NON_MEMBER_WALLET, a).await;
    let (res, b) = move_to_non_member_campaign(&pool, "crew_records", "crew_id", id, 0x5A28_i64).await;
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    delete_campaign(&pool, b).await;

    assert_fk_violation(res, "crew_records_membership_fkey");
}

/// 5a-1 #9 — an Advisor for a non-member of its campaign is refused.
#[tokio::test]
async fn non_member_row_refused_advisor() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    let a = seed_account_and_campaign(&pool, NON_MEMBER_WALLET, 0x5A19_i64).await;
    let r = hand_built_advisor(NON_MEMBER_WALLET, a, "Director of Overland Logistics");
    PostgresAdvisorRepository::new(pool.clone()).create_advisor(r.clone()).await.expect("create_advisor failed");
    let (res, b) = move_to_non_member_campaign(&pool, "advisor_records", "advisor_id", r.advisor_id, 0x5A29_i64).await;
    clean_wallet_4g1(&pool, &NON_MEMBER_WALLET).await;
    delete_campaign(&pool, b).await;

    assert_fk_violation(res, "advisor_records_membership_fkey");
}

/// A generated Commander for `wallet` in `campaign_id` with its Grade removed.
fn gradeless_commander(wallet: [u8; 32], campaign_id: uuid::Uuid) -> CommanderRecord {
    let mut r = mercs_server::commander_gen::generate_commander(wallet.to_vec(), campaign_id, 42);
    assert!(r.commander_grade.is_some(), "the generator always grades; the test removes it");
    r.commander_grade = None;
    r
}

fn assert_grade_refused(res: Result<(), RepositoryError>) {
    match res {
        Err(RepositoryError::Internal(msg)) => assert!(
            msg.contains("commander_grade is required"),
            "unexpected refusal text: {msg}"
        ),
        other => panic!("a None Grade must be refused with Internal, got {other:?}"),
    }
}

/// 5a-1 #10 (GAP-61 step 1) — Postgres refuses a Commander with no Grade, and writes nothing.
/// The member pair satisfies the FK, so only the refusal can stop the insert.
#[tokio::test]
async fn create_commander_refuses_missing_grade_postgres() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &GRADE_WALLET).await;
    let c = seed_account_and_campaign(&pool, GRADE_WALLET, 0x5A1A_i64).await;
    let r = gradeless_commander(GRADE_WALLET, c);
    let id = r.commander_id;
    let res = PostgresCommanderRepository::new(pool.clone()).create_commander(r).await;
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM commander_records WHERE commander_id = $1")
        .bind(id).fetch_one(&pool).await.expect("count failed");
    clean_wallet_4g1(&pool, &GRADE_WALLET).await;

    assert_grade_refused(res);
    assert_eq!(rows, 0, "a refused Commander must not be written");
}

/// 5a-1 #11 (GAP-61 step 1) — the in-memory impl refuses a Commander with no Grade, and
/// stores nothing.
#[tokio::test]
async fn create_commander_refuses_missing_grade_in_memory() {
    let repo = InMemoryCommanderRepository::new();
    let r = gradeless_commander(GRADE_WALLET, uuid::Uuid::new_v4());
    let id = r.commander_id;
    assert_grade_refused(repo.create_commander(r).await);
    assert!(repo.get_commander(id).await.expect("get_commander failed").is_none(), "a refused Commander must not be stored");
}

/// 5a-1 #12 (Reviewer 5a-0 F2) — `RepositoryError` hides SQLSTATEs, so this raw probe pins
/// that a duplicate membership is still the database's refusal, by name.
#[tokio::test]
async fn duplicate_membership_names_uq_player_campaign() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    clean_wallet_4g1(&pool, &DUP_MEMBER_WALLET).await;
    let c = seed_account_and_campaign(&pool, DUP_MEMBER_WALLET, 0x5A1B_i64).await;
    let res = sqlx::query("INSERT INTO player_campaign_membership (wallet_address, campaign_id) VALUES ($1, $2)")
        .bind(DUP_MEMBER_WALLET.to_vec()).bind(c).execute(&pool).await;
    clean_wallet_4g1(&pool, &DUP_MEMBER_WALLET).await;

    assert_unique_violation(res, "uq_player_campaign");
}

// ── 5a-2b: the campaign join ─────────────────────────────────────────────────

use mercs_server::repository::{
    CampaignJoinRepository, JoinOutcome, JoinSeeds, LaunchStart, PauseHook, PostgresCampaignJoinRepository,
    StarterIds, JOIN_ADVISORY_KEY,
};
use mercs_server::campaign_init::LAUNCH_MAX_MEMBERS;

const JOIN_SEEDS: JoinSeeds = JoinSeeds { commander: 0x1111_1111, foreman: 0x2222_2222, map: 0x3333_3333 };

/// The account row the join's precondition needs, and nothing else (no campaign, no membership).
async fn seed_account(pool: &sqlx::PgPool, wallet: [u8; 32]) {
    sqlx::query(
        "INSERT INTO player_accounts (wallet_address, trust_standing, gcn_balance)
         VALUES ($1, 0, 0) ON CONFLICT (wallet_address) DO NOTHING"
    )
    .bind(wallet.to_vec())
    .execute(pool)
    .await
    .expect("player_accounts seed failed");
}

async fn pending_campaign_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM campaign_instances WHERE state = 'Pending'")
        .fetch_one(pool).await.expect("pending count failed")
}

/// The join's pick is global, so a leftover Pending campaign would capture the test: a dirty
/// test DB voids the run loudly.
async fn assert_no_pending_campaign(pool: &sqlx::PgPool) {
    assert_eq!(pending_campaign_count(pool).await, 0, "dirty test DB: a Pending campaign exists before this test (void run)");
}

async fn wallet_rows(pool: &sqlx::PgPool, table: &str, wallet: [u8; 32]) -> i64 {
    try_wallet_rows(pool, table, wallet).await
        .unwrap_or_else(|e| panic!("{table} count failed: {e}"))
}

/// `wallet_rows` without the panic, for a collect phase that must reach its cleanup.
async fn try_wallet_rows(pool: &sqlx::PgPool, table: &str, wallet: [u8; 32]) -> Result<i64, sqlx::Error> {
    let col = if table == "player_campaign_membership" { "wallet_address" } else { "player_wallet" };
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {col} = $1"))
        .bind(wallet.to_vec()).fetch_one(pool).await
}

async fn campaign_members(pool: &sqlx::PgPool, campaign_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM player_campaign_membership WHERE campaign_id = $1")
        .bind(campaign_id).fetch_one(pool).await.expect("member count failed")
}

async fn acquired_via_of(pool: &sqlx::PgPool, table: &str, id_col: &str, id: uuid::Uuid) -> String {
    sqlx::query_scalar(&format!("SELECT acquired_via FROM {table} WHERE {id_col} = $1"))
        .bind(id).fetch_one(pool).await
        .unwrap_or_else(|e| panic!("{table} acquired_via read failed: {e}"))
}

fn joined_ids(o: JoinOutcome) -> StarterIds {
    match o {
        JoinOutcome::Joined(ids) => ids,
        other => panic!("expected Joined, got {other:?}"),
    }
}

/// 5a-2b P1 — a join writes one of each starter, owned by the joiner, with the canon values.
#[tokio::test]
async fn campaign_join_writes_one_of_each_starter() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [42u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    seed_account(&pool, wallet).await;

    let ids = joined_ids(PostgresCampaignJoinRepository::new(pool.clone()).join_campaign(&wallet, 2, JOIN_SEEDS).await.unwrap());

    // Read everything, clean, then assert: a failed assertion must not leave a Pending
    // campaign behind for the next join test's dirty-DB guard.
    let mut counts = Vec::new();
    for t in ["commander_records", "foreman_records", "section_records", "crew_records"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {t} WHERE campaign_id = $1 AND player_wallet = $2"))
            .bind(ids.campaign_id).bind(wallet.to_vec()).fetch_one(&pool).await.unwrap();
        counts.push((t, n));
    }
    let c = PostgresCommanderRepository::new(pool.clone()).get_commander(ids.commander_id).await.unwrap();
    let f = PostgresForemanRepository::new(pool.clone()).get_foreman(ids.foreman_id).await.unwrap();
    let s = PostgresSectionRepository::new(pool.clone()).get_section(ids.section_id).await.unwrap();
    let crew = PostgresCrewRepository::new(pool.clone()).get_crew(ids.crew_id).await.unwrap();
    let m = PostgresMembershipRepository::new(pool.clone()).get_membership(&wallet, ids.campaign_id).await.unwrap();
    let camp = PostgresCampaignRepository::new(pool.clone()).get_campaign(ids.campaign_id).await.unwrap();
    clean_wallet_4g1(&pool, &wallet).await;

    for (t, n) in counts {
        assert_eq!(n, 1, "{t}: one starter row");
    }
    let c = c.expect("commander");
    assert_eq!((c.rank, c.xp, c.stress), (1, 0, 0));
    assert!(c.commander_grade.is_some());
    assert_eq!(c.prng_seed_state, 0x1111_1111);
    assert_eq!(c.player_wallet, wallet.to_vec());
    let f = f.expect("foreman");
    assert_eq!(f.prng_seed_state, 0x2222_2222);
    assert_ne!(c.prng_seed_state, f.prng_seed_state, "the two seeds differ");
    assert_eq!(f.player_wallet, wallet.to_vec());
    let s = s.expect("section");
    assert_eq!(s.name, "1st Section");
    assert_eq!(s.headcount, 8);
    assert_eq!(s.loadout, serde_json::json!({}));
    assert_eq!(s.player_wallet, wallet.to_vec());
    let crew = crew.expect("crew");
    assert_eq!((crew.headcount, crew.xp, crew.morale), (8, 0, 60));
    assert_eq!(crew.player_wallet, wallet.to_vec());

    let m = m.expect("membership");
    assert_eq!((m.gateway_hex_q, m.gateway_hex_r), (None, None));
    let camp = camp.expect("campaign");
    assert_eq!(camp.state, CampaignLifecycle::Pending);
    assert_eq!(camp.map_seed, 0x3333_3333);
}

/// 5a-2b P2 (Reviewer 5a-1 CF1) — each starter's `acquired_via` reaches the column as
/// `'starter'`, and a `create_*` row as `'hired'`.
#[tokio::test]
async fn campaign_join_writes_acquired_via_starter() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [43u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    seed_account(&pool, wallet).await;

    let ids = joined_ids(PostgresCampaignJoinRepository::new(pool.clone()).join_campaign(&wallet, 2, JOIN_SEEDS).await.unwrap());
    let commander = acquired_via_of(&pool, "commander_records", "commander_id", ids.commander_id).await;
    let foreman = acquired_via_of(&pool, "foreman_records", "foreman_id", ids.foreman_id).await;
    let section = acquired_via_of(&pool, "section_records", "section_id", ids.section_id).await;
    let crew = acquired_via_of(&pool, "crew_records", "crew_id", ids.crew_id).await;

    let hired = CrewRecord::new_hire(ids.campaign_id, wallet.to_vec());
    let hired_id = hired.crew_id;
    PostgresCrewRepository::new(pool.clone()).create_crew(hired).await.unwrap();
    let hired = acquired_via_of(&pool, "crew_records", "crew_id", hired_id).await;
    // Clean before asserting, so a failure leaves no Pending campaign behind.
    clean_wallet_4g1(&pool, &wallet).await;

    assert_eq!(commander, "starter", "commander acquired_via");
    assert_eq!(foreman, "starter", "foreman acquired_via");
    assert_eq!(section, "starter", "section acquired_via");
    assert_eq!(crew, "starter", "crew acquired_via");
    assert_eq!(hired, "hired", "hired crew acquired_via");
}

/// 5a-2b P3 — a repeat join returns `AlreadyMember` with the same ids and writes nothing.
#[tokio::test]
async fn campaign_join_repeat_returns_already_member() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [44u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    seed_account(&pool, wallet).await;
    let repo = PostgresCampaignJoinRepository::new(pool.clone());

    let ids = joined_ids(repo.join_campaign(&wallet, 2, JOIN_SEEDS).await.unwrap());
    let again = repo.join_campaign(&wallet, 2, JOIN_SEEDS).await;
    let campaigns = pending_campaign_count(&pool).await;
    let mut rows = Vec::new();
    for t in ["player_campaign_membership", "commander_records", "foreman_records", "section_records", "crew_records"] {
        rows.push((t, wallet_rows(&pool, t, wallet).await));
    }
    // Clean before asserting, so a failure leaves no Pending campaign behind.
    clean_wallet_4g1(&pool, &wallet).await;

    assert_eq!(again.expect("repeat join"), JoinOutcome::AlreadyMember(ids));
    assert_eq!(campaigns, 1, "one campaign");
    for (t, n) in rows {
        assert_eq!(n, 1, "{t}: one row");
    }
}

/// BP-1 (ODQ-55 (a)) — on Postgres, with 999 live campaigns (the literal) A's join creates
/// the 1,000th; B's join is refused and rolls back, account row included; A again is still
/// `AlreadyMember`.
///
/// Order-sensitive: the zero-live guard is **global** (any leaked live campaign, from any
/// test on any branch, voids it). Nothing between the seed and the cleanup can panic. If a
/// run is ever killed in between, clean by hand: `DELETE FROM campaign_instances WHERE state
/// IN ('Pending','Active','Ending') AND map_seed BETWEEN 1 AND 999 AND campaign_id NOT IN
/// (SELECT campaign_id FROM player_campaign_membership)`.
#[tokio::test]
async fn pg_join_refuses_a_new_campaign_at_the_demo_cap() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let (a, b) = ([78u8; 32], [79u8; 32]);
    clean_wallet_4g1(&pool, &a).await;
    clean_wallet_4g1(&pool, &b).await;
    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM campaign_instances WHERE state IN ('Pending','Active','Ending')")
        .fetch_one(&pool).await.expect("live count failed");
    assert_eq!(live, 0, "dirty test DB: live campaigns exist before this test (void run)");

    let seeded: Vec<uuid::Uuid> = sqlx::query_scalar(
        "INSERT INTO campaign_instances (sector_id, map_seed, state) \
         SELECT gen_random_uuid(), g, 'Active' FROM generate_series(1, 999) g RETURNING campaign_id",
    )
    .fetch_all(&pool)
    .await
    .expect("seed 999 Active campaigns");
    let repo = PostgresCampaignJoinRepository::new(pool.clone());

    // Collect.
    let first = repo.join_campaign(&a, 1, JOIN_SEEDS).await;
    let refused = repo.join_campaign(&b, 1, JOIN_SEEDS).await;
    let again = repo.join_campaign(&a, 1, JOIN_SEEDS).await;
    let mut b_rows: Vec<(&str, Result<i64, sqlx::Error>)> = Vec::new();
    for t in ["player_accounts", "player_campaign_membership", "commander_records", "foreman_records", "section_records", "crew_records"] {
        let n = if t == "player_accounts" {
            sqlx::query_scalar("SELECT count(*) FROM player_accounts WHERE wallet_address = $1")
                .bind(b.to_vec()).fetch_one(&pool).await
        } else {
            try_wallet_rows(&pool, t, b).await
        };
        b_rows.push((t, n));
    }

    // Clean: the seeded campaigns by id, then both wallets, whatever the seed delete did.
    let seed_clean = sqlx::query("DELETE FROM campaign_instances WHERE campaign_id = ANY($1)")
        .bind(&seeded).execute(&pool).await;
    clean_wallet_4g1(&pool, &a).await;
    clean_wallet_4g1(&pool, &b).await;

    // Assert.
    seed_clean.expect("seed cleanup failed");
    assert_eq!(seeded.len(), 999);
    let ids = joined_ids(first.expect("A's join"));
    assert_eq!(refused.expect("B's join"), JoinOutcome::DemoFull);
    for (t, n) in b_rows {
        assert_eq!(n.unwrap_or_else(|e| panic!("{t} count failed: {e}")), 0, "{t}: B wrote nothing");
    }
    assert_eq!(again.expect("A's repeat join"), JoinOutcome::AlreadyMember(ids), "AlreadyMember still passes when full");
}

/// 5a-2b P4 — three wallets at cap 2 fill two campaigns.
#[tokio::test]
async fn campaign_join_fills_campaigns_up_to_cap() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallets = [[45u8; 32], [46u8; 32], [47u8; 32]];
    for w in &wallets { clean_wallet_4g1(&pool, w).await; }
    assert_no_pending_campaign(&pool).await;
    for w in wallets { seed_account(&pool, w).await; }
    let repo = PostgresCampaignJoinRepository::new(pool.clone());

    let mut outcomes = Vec::new();
    for w in &wallets {
        outcomes.push(repo.join_campaign(w, 2, JOIN_SEEDS).await);
    }
    let campaigns = pending_campaign_count(&pool).await;

    for w in &wallets { clean_wallet_4g1(&pool, w).await; }
    let ids: Vec<StarterIds> = outcomes.into_iter().map(|o| joined_ids(o.expect("join"))).collect();
    assert_eq!(ids[0].campaign_id, ids[1].campaign_id, "the first two share a campaign");
    assert_ne!(ids[2].campaign_id, ids[0].campaign_id, "the third gets a new campaign");
    assert_eq!(campaigns, 2, "two campaigns in all");
}

/// 5a-2b P5 — at cap 1, a join lands in an existing empty Pending campaign (guards
/// `count(m.membership_id)`: `count(*)` counts an empty campaign as 1).
#[tokio::test]
async fn campaign_join_picks_an_empty_pending_campaign() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [48u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    seed_account(&pool, wallet).await;
    let e = PostgresCampaignRepository::new(pool.clone())
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 0x5A2B_i64 })
        .await.unwrap().campaign_id;

    let ids = joined_ids(PostgresCampaignJoinRepository::new(pool.clone()).join_campaign(&wallet, 1, JOIN_SEEDS).await.unwrap());
    let pending = pending_campaign_count(&pool).await;

    clean_wallet_4g1(&pool, &wallet).await;
    delete_campaign(&pool, e).await;
    assert_eq!(ids.campaign_id, e, "joined the existing empty campaign");
    assert_eq!(pending, 1, "no second campaign");
}

/// 5a-2b P6 — the oldest Pending campaign is picked, and two campaigns created in one
/// transaction (same `now()`) tie-break on `campaign_id`.
#[tokio::test]
async fn campaign_join_picks_the_oldest_pending_campaign() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallets = [[49u8; 32], [50u8; 32], [51u8; 32]];
    for w in &wallets { clean_wallet_4g1(&pool, w).await; }
    assert_no_pending_campaign(&pool).await;
    for w in wallets { seed_account(&pool, w).await; }

    let o = PostgresCampaignRepository::new(pool.clone())
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 0x0_i64 })
        .await.unwrap().campaign_id;
    let mut tx = pool.begin().await.unwrap();
    let mut tied = Vec::new();
    for seed in [1_i64, 2] {
        let id: uuid::Uuid = sqlx::query_scalar("INSERT INTO campaign_instances (sector_id, map_seed) VALUES ($1, $2) RETURNING campaign_id")
            .bind(uuid::Uuid::new_v4()).bind(seed).fetch_one(&mut *tx).await.unwrap();
        tied.push(id);
    }
    tx.commit().await.unwrap();
    let first_tied = *tied.iter().min().unwrap();

    let repo = PostgresCampaignJoinRepository::new(pool.clone());
    let mut outcomes = Vec::new();
    for w in &wallets {
        outcomes.push(repo.join_campaign(w, 2, JOIN_SEEDS).await);
    }

    for w in &wallets { clean_wallet_4g1(&pool, w).await; }
    for c in [o, tied[0], tied[1]] { delete_campaign(&pool, c).await; }
    let ids: Vec<StarterIds> = outcomes.into_iter().map(|o| joined_ids(o.expect("join"))).collect();
    assert_eq!(ids[0].campaign_id, o, "the first joiner takes the oldest campaign");
    assert_eq!(ids[1].campaign_id, o, "the second joiner fills it");
    assert_eq!(ids[2].campaign_id, first_tied, "a created_at tie goes to the smaller campaign_id");
}

/// 5a-2b P7 — a member with no starter set is an integrity error, not `AlreadyMember`.
#[tokio::test]
async fn campaign_join_member_without_starter_set_is_internal() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [52u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    seed_account_and_campaign(&pool, wallet, 0x5A2B7_i64).await;

    let res = PostgresCampaignJoinRepository::new(pool.clone()).join_campaign(&wallet, 2, JOIN_SEEDS).await;
    clean_wallet_4g1(&pool, &wallet).await;
    match res {
        Err(RepositoryError::Internal(m)) => assert!(m.contains("no starter set"), "unexpected text: {m}"),
        other => panic!("expected Internal, got {other:?}"),
    }
}

/// A pause hook: each call reports "parked", then waits until the gate opens. Once open,
/// every later call passes straight through.
fn join_pause_hook() -> (PauseHook, tokio::sync::mpsc::UnboundedReceiver<()>, tokio::sync::watch::Sender<bool>) {
    let (parked_tx, parked_rx) = tokio::sync::mpsc::unbounded_channel();
    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let hook: PauseHook = std::sync::Arc::new(move || {
        let parked = parked_tx.clone();
        let mut gate = gate_rx.clone();
        Box::pin(async move {
            let _ = parked.send(());
            let _ = gate.wait_for(|open| *open).await;
        })
    });
    (hook, parked_rx, gate_tx)
}

/// Waits (≤ 10 s) until B is held or done: B parked at its own seam, B waiting on the join
/// key in `pg_locks` (filtered by `JOIN_ADVISORY_KEY`, Reviewer 5a-2b F2), or B finished. The
/// third exit lets a no-lock negative reach the test's own assertions instead of the deadline.
async fn wait_until_b_is_held<T>(
    wide: &sqlx::PgPool,
    parked: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    b: &tokio::task::JoinHandle<T>,
) {
    use std::time::Duration;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if parked.try_recv().is_ok() || b.is_finished() { return; }
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks
             WHERE locktype = 'advisory' AND objsubid = 1 AND NOT granted
               AND classid = (($1::bigint >> 32) & 4294967295)::oid
               AND objid = ($1::bigint & 4294967295)::oid"
        ).bind(JOIN_ADVISORY_KEY).fetch_one(wide).await.expect("pg_locks poll failed");
        if waiting == 1 { return; }
        assert!(tokio::time::Instant::now() < deadline, "B neither parked, waited on the lock, nor finished");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Runs A and B through the seam protocol (Section 5 doc): A parks; B either parks at its own
/// seam or waits on the join lock; then the gate opens. Returns both results.
async fn race_joins(
    wide: &sqlx::PgPool,
    a: [u8; 32],
    b: [u8; 32],
    cap: i64,
) -> (Result<JoinOutcome, RepositoryError>, Result<JoinOutcome, RepositoryError>) {
    use std::time::Duration;
    let (hook, mut parked, gate) = join_pause_hook();
    let repo = std::sync::Arc::new(PostgresCampaignJoinRepository::with_pause_seam(wide.clone(), hook));

    let ra = repo.clone();
    let task_a = tokio::spawn(async move { ra.join_campaign(&a, cap, JOIN_SEEDS).await });
    tokio::time::timeout(Duration::from_secs(10), parked.recv())
        .await.expect("A never parked").expect("A never parked");

    let rb = repo.clone();
    let task_b = tokio::spawn(async move { rb.join_campaign(&b, cap, JOIN_SEEDS).await });
    wait_until_b_is_held(wide, &mut parked, &task_b).await;

    gate.send(true).expect("open the gate");
    let ra = task_a.await.expect("A panicked");
    let rb = task_b.await.expect("B panicked");
    (ra, rb)
}

async fn wide_pool() -> sqlx::PgPool {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    PgPoolOptions::new().max_connections(4).connect(&url).await.expect("wide pool")
}

/// 5a-2b P8 (seam (a)) — one wallet joining twice at once gets one starter set: one call
/// joins, the other is `AlreadyMember`, and both are `Ok`.
#[tokio::test]
async fn concurrent_joins_by_one_wallet_yield_one_starter_set() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [53u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;
    seed_account(&pool, wallet).await;
    // Both racers must pick the same row, so the campaign exists before either starts.
    let e = PostgresCampaignRepository::new(pool.clone())
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 0x5A2B8_i64 })
        .await.unwrap().campaign_id;
    let wide = wide_pool().await;

    let (ra, rb) = race_joins(&wide, wallet, wallet, 2).await;
    let rows: Vec<i64> = {
        let mut v = Vec::new();
        for t in ["player_campaign_membership", "commander_records", "foreman_records", "section_records", "crew_records"] {
            v.push(wallet_rows(&pool, t, wallet).await);
        }
        v
    };
    clean_wallet_4g1(&pool, &wallet).await;
    delete_campaign(&pool, e).await;

    let (ra, rb) = match (ra, rb) {
        (Ok(a), Ok(b)) => (a, b),
        other => panic!("both calls Ok: {other:?}"),
    };
    let (j, am) = match (ra, rb) {
        (JoinOutcome::Joined(j), JoinOutcome::AlreadyMember(am)) | (JoinOutcome::AlreadyMember(am), JoinOutcome::Joined(j)) => (j, am),
        other => panic!("one Joined and one AlreadyMember: {other:?}"),
    };
    assert_eq!(j, am, "the same starter set");
    assert_eq!(rows, vec![1, 1, 1, 1, 1], "one membership and one row per roster table");
}

/// 5a-2b P9 (seam (b)) — two wallets racing for the last seat at cap 2: no campaign goes over
/// cap.
#[tokio::test]
async fn concurrent_joins_for_the_last_seat_respect_the_cap() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let (first, a, b) = ([54u8; 32], [55u8; 32], [56u8; 32]);
    for w in [first, a, b] { clean_wallet_4g1(&pool, &w).await; }
    assert_no_pending_campaign(&pool).await;
    for w in [first, a, b] { seed_account(&pool, w).await; }
    let wide = wide_pool().await;

    let seated = joined_ids(PostgresCampaignJoinRepository::new(pool.clone()).join_campaign(&first, 2, JOIN_SEEDS).await.unwrap());
    let (ra, rb) = race_joins(&wide, a, b, 2).await;

    let mut campaigns = vec![seated.campaign_id];
    for r in [&ra, &rb] {
        if let Ok(JoinOutcome::Joined(ids) | JoinOutcome::AlreadyMember(ids)) = r { campaigns.push(ids.campaign_id); }
    }
    campaigns.sort();
    campaigns.dedup();
    let mut members = Vec::new();
    for c in &campaigns { members.push(campaign_members(&pool, *c).await); }
    for w in [first, a, b] { clean_wallet_4g1(&pool, &w).await; }

    let (ra, rb) = match (ra, rb) {
        (Ok(a), Ok(b)) => (a, b),
        other => panic!("both calls Ok: {other:?}"),
    };
    assert!(matches!(ra, JoinOutcome::Joined(_)) && matches!(rb, JoinOutcome::Joined(_)), "both Joined: {ra:?} / {rb:?}");
    assert!(members.iter().all(|&n| n <= 2), "campaign over cap: {members:?}");
    assert_eq!(campaigns.len(), 2, "two campaigns in all");
}

/// 5a-2b P10, recast at INF-6c-2b-1 (GAP-79): a join with no account row creates it.
#[tokio::test]
async fn campaign_join_creates_a_missing_account() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [57u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await;

    let res = PostgresCampaignJoinRepository::new(pool.clone()).join_campaign(&wallet, 2, JOIN_SEEDS).await;
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM player_accounts WHERE wallet_address = $1")
        .bind(wallet.to_vec())
        .fetch_one(&pool)
        .await
        .expect("account count failed");
    clean_wallet_4g1(&pool, &wallet).await;
    assert!(matches!(res, Ok(JoinOutcome::Joined(_))), "expected Joined, got {res:?}");
    assert_eq!(accounts, 1, "the join created exactly one account row");
}

// ── 5a-3a: the launch under the join lock ────────────────────────────────────

/// 5a-3a T1 (Reviewer 5a-2b F1/CF1) — a launch parked after its guard, before its activation,
/// holds the join lock: a racing join waits, then lands in a **different** campaign, and the
/// launched campaign keeps exactly the members the launch read.
#[tokio::test]
async fn launch_serialises_against_a_racing_join() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let (member, joiner) = ([58u8; 32], [59u8; 32]);
    for w in [member, joiner] { clean_wallet_4g1(&pool, &w).await; }
    assert_no_pending_campaign(&pool).await;
    let c = seed_account_and_campaign(&pool, member, 0x5A3A1_i64).await;
    seed_account(&pool, joiner).await;
    let wide = wide_pool().await;
    let ends_at = chrono::Utc::now() + chrono::Duration::days(90);
    let tickers = serde_json::json!({ "MILITARY_DOMINANCE": 0.0 });

    let (hook, mut parked, gate) = join_pause_hook();
    let launcher = std::sync::Arc::new(PostgresCampaignJoinRepository::with_pause_seam(wide.clone(), hook));
    let task_a = tokio::spawn(async move { launcher.begin_launch(c, LAUNCH_MAX_MEMBERS, ends_at, tickers).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), parked.recv())
        .await.expect("the launch never parked").expect("the launch never parked");

    let joining = PostgresCampaignJoinRepository::new(wide.clone());
    let task_b = tokio::spawn(async move { joining.join_campaign(&joiner, 2, JOIN_SEEDS).await });
    wait_until_b_is_held(&wide, &mut parked, &task_b).await;
    gate.send(true).expect("open the gate");
    let launch = task_a.await.expect("the launch panicked");
    let join = task_b.await.expect("the join panicked");

    // Collect, clean, then assert.
    let launched = PostgresCampaignRepository::new(pool.clone()).get_campaign(c).await.expect("get_campaign");
    let launched_members = campaign_members(&pool, c).await;
    let joiner_campaign: Option<uuid::Uuid> = match &join {
        Ok(JoinOutcome::Joined(ids) | JoinOutcome::AlreadyMember(ids)) => Some(ids.campaign_id),
        _ => None,
    };
    let joiner_hex = match joiner_campaign {
        Some(jc) => PostgresMembershipRepository::new(pool.clone()).get_membership(&joiner, jc).await
            .expect("get_membership").map(|m| (m.gateway_hex_q, m.gateway_hex_r)),
        None => None,
    };
    for w in [member, joiner] { clean_wallet_4g1(&pool, &w).await; }

    match launch.expect("begin_launch") {
        LaunchStart::Started { members, .. } => assert_eq!(members.len(), 1, "the launch read one member"),
        other => panic!("expected Started, got {other:?}"),
    }
    let ids = match join {
        Ok(JoinOutcome::Joined(ids)) => ids,
        other => panic!("expected Ok(Joined), got {other:?}"),
    };
    assert_ne!(ids.campaign_id, c, "joiner in a different campaign");
    assert_eq!(launched.expect("launched campaign").state, CampaignLifecycle::Active);
    assert_eq!(launched_members, 1, "the launched campaign has exactly the member the launch read");
    assert_eq!(joiner_hex, Some((None, None)), "the joiner's hex is NULL");
}

// ── 5a-4: the alpha KIA replacement ──────────────────────────────────────────

use mercs_server::repository::ReplacementOutcome;

/// Pre-cleans `wallet`, then seeds its account, an `Active` campaign (activated at once, so the
/// join's pick never sees it), its membership and `n` generated Commanders. Returns
/// (campaign_id, commander ids).
async fn seed_active_member(pool: &sqlx::PgPool, wallet: [u8; 32], map_seed: i64, n: u32) -> (uuid::Uuid, Vec<uuid::Uuid>) {
    clean_wallet_4g1(pool, &wallet).await;
    let c = seed_account_and_campaign(pool, wallet, map_seed).await;
    let activated = PostgresCampaignRepository::new(pool.clone())
        .activate_campaign(c, chrono::Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
        .await
        .expect("activate_campaign failed");
    assert!(activated, "the seeded campaign must activate");
    let mut ids = Vec::new();
    for i in 0..n {
        ids.push(pg_commander(pool, wallet, c, 0x5A40 + i).await);
    }
    (c, ids)
}

/// Sets a boolean flag (`is_kia` or `is_shattered`) by direct unchecked SQL (test-only).
async fn set_commander_flag(pool: &sqlx::PgPool, commander_id: uuid::Uuid, column: &str) {
    let r = sqlx::query(&format!("UPDATE commander_records SET {column} = TRUE WHERE commander_id = $1"))
        .bind(commander_id).execute(pool).await
        .unwrap_or_else(|e| panic!("set {column} failed: {e}"));
    assert_eq!(r.rows_affected(), 1, "set {column}: the Commander must exist");
}

/// The member's rows with `acquired_via = 'replacement'`.
async fn replacement_rows(pool: &sqlx::PgPool, wallet: [u8; 32]) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM commander_records WHERE player_wallet = $1 AND acquired_via = 'replacement'")
        .bind(wallet.to_vec()).fetch_one(pool).await.expect("replacement count failed")
}

const CLAIM_SEED: u32 = 0x5A4_0001;

/// 5a-4 P1 — an all-KIA member gets one Rank 1 replacement, written as `'replacement'`.
#[tokio::test]
async fn claim_replacement_grants_one_to_an_all_kia_member() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [63u8; 32];
    let (c, ids) = seed_active_member(&pool, wallet, 0x5A41_i64, 1).await;
    set_commander_flag(&pool, ids[0], "is_kia").await;

    let res = PostgresCampaignJoinRepository::new(pool.clone()).claim_replacement(c, &wallet, CLAIM_SEED).await;
    let (rec, via) = match &res {
        Ok(ReplacementOutcome::Granted(id)) => (
            PostgresCommanderRepository::new(pool.clone()).get_commander(*id).await.expect("get_commander"),
            Some(acquired_via_of(&pool, "commander_records", "commander_id", *id).await),
        ),
        _ => (None, None),
    };
    clean_wallet_4g1(&pool, &wallet).await;

    assert!(matches!(res, Ok(ReplacementOutcome::Granted(_))), "expected Granted, got {res:?}");
    let rec = rec.expect("the replacement row");
    assert_eq!(via.as_deref(), Some("replacement"));
    assert_eq!((rec.rank, rec.xp, rec.stress, rec.is_kia, rec.is_shattered), (1, 0, 0, false, false));
    assert!(rec.commander_grade.is_some(), "a replacement carries its Grade");
    assert_eq!((rec.player_wallet, rec.campaign_id, rec.prng_seed_state), (wallet.to_vec(), c, CLAIM_SEED as i64));
}

/// 5a-4 P2 — per wipe: once the replacement dies, the member may claim again.
#[tokio::test]
async fn claim_replacement_again_after_the_replacement_dies() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [64u8; 32];
    let (c, ids) = seed_active_member(&pool, wallet, 0x5A42_i64, 1).await;
    set_commander_flag(&pool, ids[0], "is_kia").await;
    let repo = PostgresCampaignJoinRepository::new(pool.clone());

    let first = repo.claim_replacement(c, &wallet, CLAIM_SEED).await;
    if let Ok(ReplacementOutcome::Granted(id)) = &first {
        set_commander_flag(&pool, *id, "is_kia").await;
    }
    let second = repo.claim_replacement(c, &wallet, CLAIM_SEED + 1).await;
    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;

    let (a, b) = match (first, second) {
        (Ok(ReplacementOutcome::Granted(a)), Ok(ReplacementOutcome::Granted(b))) => (a, b),
        other => panic!("expected two Granted, got {other:?}"),
    };
    assert_ne!(a, b);
    assert_eq!(rows, 2, "two replacements, one of them dead");
}

/// 5a-4 P3 — a usable Commander blocks the claim.
#[tokio::test]
async fn claim_replacement_refuses_a_living_commander() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [65u8; 32];
    let (c, _) = seed_active_member(&pool, wallet, 0x5A43_i64, 1).await;
    let res = PostgresCampaignJoinRepository::new(pool.clone()).claim_replacement(c, &wallet, CLAIM_SEED).await;
    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;
    assert!(matches!(res, Ok(ReplacementOutcome::HasUsable)), "expected HasUsable, got {res:?}");
    assert_eq!(rows, 0);
}

/// 5a-4 P4, flipped by 5a-5 — a Shattered Commander is not usable (ODQ-46: Phase 1 has no
/// Rehabilitation), so a member whose every Commander is Shattered gets a replacement.
#[tokio::test]
async fn claim_replacement_grants_when_every_commander_is_shattered() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [66u8; 32];
    let (c, ids) = seed_active_member(&pool, wallet, 0x5A44_i64, 1).await;
    set_commander_flag(&pool, ids[0], "is_shattered").await;
    let res = PostgresCampaignJoinRepository::new(pool.clone()).claim_replacement(c, &wallet, CLAIM_SEED).await;
    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;
    assert!(matches!(res, Ok(ReplacementOutcome::Granted(_))), "expected Granted, got {res:?}");
    assert_eq!(rows, 1);
}

/// 5a-4 P5 — a live replacement blocks a second claim.
#[tokio::test]
async fn claim_replacement_refuses_while_the_replacement_lives() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [67u8; 32];
    let (c, ids) = seed_active_member(&pool, wallet, 0x5A45_i64, 1).await;
    set_commander_flag(&pool, ids[0], "is_kia").await;
    let repo = PostgresCampaignJoinRepository::new(pool.clone());
    let first = repo.claim_replacement(c, &wallet, CLAIM_SEED).await;
    let second = repo.claim_replacement(c, &wallet, CLAIM_SEED + 1).await;
    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;
    assert!(matches!(first, Ok(ReplacementOutcome::Granted(_))), "expected Granted, got {first:?}");
    assert!(matches!(second, Ok(ReplacementOutcome::HasUsable)), "expected HasUsable, got {second:?}");
    assert_eq!(rows, 1);
}

/// 5a-4 P6 — the in-transaction membership miss is `NotMember` (the route's second `403`).
#[tokio::test]
async fn claim_replacement_non_member_is_not_member() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [68u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    seed_account(&pool, wallet).await;
    let campaigns = PostgresCampaignRepository::new(pool.clone());
    let c = campaigns
        .create_campaign(&NewCampaignInstance { sector_id: uuid::Uuid::new_v4(), map_seed: 0x5A46_i64 })
        .await.expect("create campaign").campaign_id;
    let activated = campaigns
        .activate_campaign(c, chrono::Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
        .await.expect("activate_campaign");

    let res = PostgresCampaignJoinRepository::new(pool.clone()).claim_replacement(c, &wallet, CLAIM_SEED).await;
    let rows = replacement_rows(&pool, wallet).await;
    // No roster or membership row names this campaign, so clean_wallet_4g1 can't find it.
    delete_campaign(&pool, c).await;
    clean_wallet_4g1(&pool, &wallet).await;

    assert!(activated, "the campaign must activate");
    assert!(matches!(res, Ok(ReplacementOutcome::NotMember)), "expected NotMember, got {res:?}");
    assert_eq!(rows, 0);
}

/// 5a-4 P7 — a member with no Commander at all is an integrity error, and nothing is granted.
/// Collect → clean → assert, so a granted row (under N3a) is cleaned too.
#[tokio::test]
async fn claim_replacement_zero_commanders_is_internal() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [69u8; 32];
    let (c, _) = seed_active_member(&pool, wallet, 0x5A47_i64, 0).await;
    let res = PostgresCampaignJoinRepository::new(pool.clone()).claim_replacement(c, &wallet, CLAIM_SEED).await;
    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;
    match res {
        Err(RepositoryError::Internal(m)) => assert!(m.contains("no Commanders"), "unexpected text: {m}"),
        other => panic!("expected Internal, got {other:?}"),
    }
    assert_eq!(rows, 0, "nothing is granted");
}

/// Waits (≤ 10 s) until B is held or done: B parked at its own seam (the no-lock negative's
/// exit), B finished, or `pg_stat_activity` shows B waiting on a lock in the claim's
/// membership `FOR UPDATE`. A row-lock wait never shows in `pg_locks` as the join key, so
/// `wait_until_b_is_held` can't see it.
async fn wait_until_b_waits_on_the_membership_row<T>(
    wide: &sqlx::PgPool,
    parked: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    b: &tokio::task::JoinHandle<T>,
) {
    use std::time::Duration;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if parked.try_recv().is_ok() || b.is_finished() { return; }
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
             WHERE datname = current_database() AND pid <> pg_backend_pid()
               AND wait_event_type = 'Lock'
               AND query LIKE '%FROM player_campaign_membership%FOR UPDATE%'"
        ).fetch_one(wide).await.expect("pg_stat_activity poll failed");
        if waiting == 1 { return; }
        assert!(tokio::time::Instant::now() < deadline, "B neither parked, waited on the membership row, nor finished");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 5a-4 P8 — two concurrent claims by one all-KIA member yield exactly one replacement: A
/// parks after its eligibility read holding the membership row; B waits on the row (or, with
/// the lock removed, parks at its own seam); the gate releases every parked racer.
#[tokio::test]
async fn concurrent_replacement_claims_yield_one() {
    use std::time::Duration;
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [70u8; 32];
    let (c, ids) = seed_active_member(&pool, wallet, 0x5A48_i64, 1).await;
    set_commander_flag(&pool, ids[0], "is_kia").await;
    let wide = wide_pool().await;

    let (hook, mut parked, gate) = join_pause_hook();
    let repo = std::sync::Arc::new(PostgresCampaignJoinRepository::with_pause_seam(wide.clone(), hook));
    let ra = repo.clone();
    let task_a = tokio::spawn(async move { ra.claim_replacement(c, &wallet, CLAIM_SEED).await });
    tokio::time::timeout(Duration::from_secs(10), parked.recv())
        .await.expect("A never parked").expect("A never parked");
    let rb = repo.clone();
    let task_b = tokio::spawn(async move { rb.claim_replacement(c, &wallet, CLAIM_SEED + 1).await });
    wait_until_b_waits_on_the_membership_row(&wide, &mut parked, &task_b).await;
    gate.send(true).expect("open the gate");
    let ra = task_a.await.expect("A panicked");
    let rb = task_b.await.expect("B panicked");

    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;

    let (ra, rb) = match (ra, rb) {
        (Ok(a), Ok(b)) => (a, b),
        other => panic!("both calls Ok: {other:?}"),
    };
    assert!(
        matches!((ra, rb), (ReplacementOutcome::Granted(_), ReplacementOutcome::HasUsable) | (ReplacementOutcome::HasUsable, ReplacementOutcome::Granted(_))),
        "one Granted and one HasUsable: {ra:?} / {rb:?}"
    );
    assert_eq!(rows, 1, "exactly one replacement");
}

/// 5a-4 P9 — `commander_records_one_live_replacement` refuses a second living replacement
/// (direct `UPDATE`, unchecked SQL); once the first is KIA, the second is accepted.
#[tokio::test]
async fn second_live_replacement_refused_by_index() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [71u8; 32];
    let (_, ids) = seed_active_member(&pool, wallet, 0x5A49_i64, 2).await;
    let (a, b) = (ids[0], ids[1]);
    let sql = "UPDATE commander_records SET acquired_via = 'replacement' WHERE commander_id = $1";

    let first = sqlx::query(sql).bind(a).execute(&pool).await;
    let second = sqlx::query(sql).bind(b).execute(&pool).await;
    set_commander_flag(&pool, a, "is_kia").await;
    let after_death = sqlx::query(sql).bind(b).execute(&pool).await;
    clean_wallet_4g1(&pool, &wallet).await;

    assert_eq!(first.expect("the first replacement must be accepted").rows_affected(), 1);
    assert_unique_violation(second, "commander_records_one_live_replacement");
    assert_eq!(after_death.expect("a dead replacement leaves the index").rows_affected(), 1);
}

/// 5a-5 P11 — per wipe, Shattered included (ODQ-46): the original is KIA → `Granted`; the
/// replacement is then Shattered → `Granted` again, and the index accepts the second insert.
#[tokio::test]
async fn claim_replacement_grants_again_when_the_replacement_is_shattered() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [73u8; 32];
    let (c, ids) = seed_active_member(&pool, wallet, 0x5A51_i64, 1).await;
    set_commander_flag(&pool, ids[0], "is_kia").await;
    let repo = PostgresCampaignJoinRepository::new(pool.clone());

    let first = repo.claim_replacement(c, &wallet, CLAIM_SEED).await;
    if let Ok(ReplacementOutcome::Granted(id)) = &first {
        set_commander_flag(&pool, *id, "is_shattered").await;
    }
    let second = repo.claim_replacement(c, &wallet, CLAIM_SEED + 1).await;
    let rows = replacement_rows(&pool, wallet).await;
    clean_wallet_4g1(&pool, &wallet).await;

    let (a, b) = match (first, second) {
        (Ok(ReplacementOutcome::Granted(a)), Ok(ReplacementOutcome::Granted(b))) => (a, b),
        other => panic!("expected two Granted, got {other:?}"),
    };
    assert_ne!(a, b);
    assert_eq!(rows, 2, "two replacements, one of them Shattered");
}

/// 5a-5 P12 — a Shattered `'replacement'` row does not occupy
/// `commander_records_one_live_replacement` (0032): a usable `'replacement'` row beside it is
/// accepted (direct `UPDATE`s, unchecked SQL, P9's style). P9 pins the refusal of two usable rows.
#[tokio::test]
async fn shattered_replacement_does_not_occupy_the_index() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [74u8; 32];
    let (_, ids) = seed_active_member(&pool, wallet, 0x5A52_i64, 2).await;
    let (a, b) = (ids[0], ids[1]);
    let sql = "UPDATE commander_records SET acquired_via = 'replacement' WHERE commander_id = $1";

    set_commander_flag(&pool, a, "is_shattered").await;
    let first = sqlx::query(sql).bind(a).execute(&pool).await;
    let second = sqlx::query(sql).bind(b).execute(&pool).await;
    clean_wallet_4g1(&pool, &wallet).await;

    assert_eq!(first.expect("the Shattered replacement must be accepted").rows_affected(), 1);
    assert_eq!(second.expect("a usable replacement beside a Shattered one must be accepted").rows_affected(), 1);
}

/// The card's `acquired_via` for one Commander, `None` when the column is `NULL`. Unlike
/// `acquired_via_of` it does not panic on `NULL`, so a failing P13 still cleans first.
async fn card_acquired_via(pool: &sqlx::PgPool, commander_id: uuid::Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT acquired_via FROM card_mint_pending WHERE commander_id = $1")
        .bind(commander_id).fetch_one(pool).await
}

/// 5a-5 P13 (ODQ-47) — each death card copies its Commander's provenance: three Commanders
/// marked `'starter'`, `'replacement'` and `'hired'` (the fixture's own value) die by a mission
/// Wipeout through `resolve_deployment` (`mark_commander_kia`), and each card carries its value.
#[tokio::test]
async fn death_card_records_commander_provenance() {
    use sim_engine::game_types::OutcomeType;
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [75u8; 32];
    let (_, ids) = seed_active_member(&pool, wallet, 0x5A53_i64, 3).await;
    for (id, via) in [(ids[0], "starter"), (ids[1], "replacement")] {
        let r = sqlx::query("UPDATE commander_records SET acquired_via = $1 WHERE commander_id = $2")
            .bind(via).bind(id).execute(&pool).await.expect("provenance update failed");
        assert_eq!(r.rows_affected(), 1, "the Commander must exist");
    }
    let repo = PostgresCommanderRepository::new(pool.clone());
    let cfg = sim_engine::config::SimConfig::default();

    let mut killed = Vec::new();
    for id in &ids {
        let d = new_deployment_4g1(*id, chrono::Utc::now());
        let started = repo.start_deployment(&d).await;
        let resolved = repo.resolve_deployment(d.deployment_id, *id, 2, 1, 0.0, &OutcomeType::Wipeout, &cfg).await;
        killed.push((started, resolved.map(|e| e.is_kia)));
    }
    let mut cards = Vec::new();
    for id in &ids {
        cards.push(card_acquired_via(&pool, *id).await);
    }
    clean_wallet_4g1(&pool, &wallet).await;

    for (started, kia) in killed {
        assert_eq!(started.expect("start_deployment failed"), DeploymentStart::Started);
        assert!(kia.expect("resolve failed"), "a Wipeout kills the Commander");
    }
    let cards: Vec<Option<String>> = cards.into_iter().map(|c| c.expect("card read failed")).collect();
    assert_eq!(cards, vec![Some("starter".to_string()), Some("replacement".to_string()), Some("hired".to_string())]);
}

/// 5a-5 P14 — `card_mint_pending_acquired_via_canon` refuses an unknown value and accepts
/// `NULL` (a CHECK passes a `NULL` expression). Literals only.
#[tokio::test]
async fn card_acquired_via_check_refuses_unknown_and_accepts_null() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [76u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    seed_account(&pool, wallet).await;
    let insert = |via: Option<&'static str>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO card_mint_pending
                    (card_mint_id, player_wallet, campaign_id, commander_id, deployment_id, session_id, commander_name, rank, foil, acquired_via)
                 VALUES ($1, $2, $3, $4, $5, $6, 'P14', 1, FALSE, $7)"
            )
            .bind(uuid::Uuid::new_v4()).bind(wallet.to_vec()).bind(uuid::Uuid::new_v4())
            .bind(uuid::Uuid::new_v4()).bind(uuid::Uuid::new_v4()).bind(uuid::Uuid::new_v4())
            .bind(via)
            .execute(&pool).await
        }
    };
    let bogus = insert(Some("bogus")).await;
    let null = insert(None).await;
    clean_wallet_4g1(&pool, &wallet).await;

    assert_check_violation(&bogus.expect_err("'bogus' must be refused"), "card_mint_pending_acquired_via_canon");
    assert_eq!(null.expect("NULL must be accepted").rows_affected(), 1);
}

/// 5a-4 P10 — end to end on Postgres: a real join, the launch's state change, the starter
/// dies, and the route grants a `'replacement'` Commander (`201`).
#[tokio::test]
async fn replacement_route_end_to_end_on_postgres() {
    use axum::{body::Body, http::{Request, StatusCode}};
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [72u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await; // the join's pick is global
    seed_account(&pool, wallet).await;

    let mut state = pg_route_state(&pool);
    std::sync::Arc::make_mut(&mut state.config).server.join_enabled = true; // 5a-3c: joins are off by default
    let state = std::sync::Arc::new(state);
    let token = insert_session(&state, wallet);
    let post = |uri: String| Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let (join_status, join_body) = send(&state, post("/api/campaign/join".into())).await;
    let joined: serde_json::Value = serde_json::from_str(&join_body).unwrap_or(serde_json::Value::Null);
    let uuid_of = |v: &serde_json::Value, key: &str| v[key].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
    let (campaign_id, starter) = (uuid_of(&joined, "campaign_id"), uuid_of(&joined, "commander_id"));

    let mut claim = None;
    if let (Some(c), Some(s)) = (campaign_id, starter) {
        // BP-1: the join launched the campaign.
        let launched_state = PostgresCampaignRepository::new(pool.clone())
            .get_campaign(c)
            .await
            .expect("get_campaign")
            .map(|r| r.state);
        set_commander_flag(&pool, s, "is_kia").await;
        let (status, body) = send(&state, post(format!("/api/campaign/{c}/replacement-commander"))).await;
        let body: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let via = match uuid_of(&body, "commander_id") {
            Some(id) => Some(acquired_via_of(&pool, "commander_records", "commander_id", id).await),
            None => None,
        };
        claim = Some((launched_state, status, body, via));
    }
    let rows = replacement_rows(&pool, wallet).await;
    // Clean: the immutable campaign_started row the launch wrote (BP-1), then the wallet.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    if let Some(c) = campaign_id {
        sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.ok();
    }
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    clean_wallet_4g1(&pool, &wallet).await;

    assert_eq!(join_status, StatusCode::CREATED, "{join_body}");
    let (launched_state, status, body, via) = claim.expect("the join returned ids");
    assert_eq!(launched_state, Some(CampaignLifecycle::Active), "the join launched the campaign");
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(uuid_of(&body, "campaign_id"), campaign_id);
    assert_ne!(uuid_of(&body, "commander_id"), starter);
    assert_eq!(via.as_deref(), Some("replacement"));
    assert_eq!(rows, 1);
}

/// BP-1b — end to end on Postgres: a real join, the abandon (`200`), then a rejoin gets a
/// new campaign. The old one is `Archived`, keeps its membership, and its sector is gone.
#[tokio::test]
async fn abandon_route_end_to_end_on_postgres() {
    use axum::{body::Body, http::{Request, StatusCode}};
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let wallet = [80u8; 32];
    clean_wallet_4g1(&pool, &wallet).await;
    assert_no_pending_campaign(&pool).await; // the join's pick is global
    seed_account(&pool, wallet).await;

    let mut state = pg_route_state(&pool);
    std::sync::Arc::make_mut(&mut state.config).server.join_enabled = true; // 5a-3c: joins are off by default
    state.convoy_repo = std::sync::Arc::new(PostgresConvoyRepository::new(pool.clone()));
    let state = std::sync::Arc::new(state);
    let token = insert_session(&state, wallet);
    let post = |uri: String| Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let json_of = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null);
    let uuid_of = |v: &serde_json::Value, key: &str| v[key].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());

    // Collect.
    let (join_status, join_body) = send(&state, post("/api/campaign/join".into())).await;
    let old = uuid_of(&json_of(&join_body), "campaign_id");
    let mut collected = None;
    if let Some(c) = old {
        let (abandon_status, abandon_body) = send(&state, post(format!("/api/campaign/{c}/abandon"))).await;
        let old_row = PostgresCampaignRepository::new(pool.clone()).get_campaign(c).await;
        let members: Result<i64, sqlx::Error> =
            sqlx::query_scalar("SELECT count(*) FROM player_campaign_membership WHERE campaign_id = $1")
                .bind(c).fetch_one(&pool).await;
        let (rejoin_status, rejoin_body) = send(&state, post("/api/campaign/join".into())).await;
        collected = Some((abandon_status, abandon_body, old_row, members, rejoin_status, rejoin_body));
    }
    let fresh = collected.as_ref().and_then(|(.., body)| uuid_of(&json_of(body), "campaign_id"));

    // Clean: both campaigns' immutable campaign_started rows, then the wallet.
    sqlx::query("ALTER TABLE input_logs DISABLE TRIGGER ALL").execute(&pool).await.ok();
    for c in [old, fresh].into_iter().flatten() {
        sqlx::query("DELETE FROM input_logs WHERE campaign_id = $1").bind(c).execute(&pool).await.ok();
    }
    sqlx::query("ALTER TABLE input_logs ENABLE TRIGGER ALL").execute(&pool).await.ok();
    clean_wallet_4g1(&pool, &wallet).await;

    // Assert.
    assert_eq!(join_status, StatusCode::CREATED, "{join_body}");
    let c = old.expect("the join returned a campaign id");
    let (abandon_status, abandon_body, old_row, members, rejoin_status, rejoin_body) =
        collected.expect("the abandon ran");
    assert_eq!(abandon_status, StatusCode::OK, "{abandon_body}");
    assert_eq!(json_of(&abandon_body), serde_json::json!({"campaign_id": c}));
    let old_row = old_row.expect("get_campaign").expect("the old campaign exists");
    assert_eq!(old_row.state, CampaignLifecycle::Archived, "the abandon archived it");
    assert_eq!(members.expect("member count"), 1, "its membership is kept");
    assert!(state.sector_repo.get_sector(old_row.sector_id).await.expect("get_sector").is_none(), "its sector is deleted");
    assert_eq!(rejoin_status, StatusCode::CREATED, "{rejoin_body}");
    assert_ne!(fresh.expect("the rejoin returned a campaign id"), c, "a new campaign");
}

/// BP-1b — the convoy in-transit read keys on the sector, the wallet and the flag, and a
/// convoy more than five minutes overdue no longer counts. `convoy_records` has no FK
/// parents, so the rows need no account or campaign.
#[tokio::test]
async fn pg_has_convoy_in_transit_reads_sector_wallet_flag_and_arrival() {
    let Some(pool) = test_pool().await else { eprintln!("TEST_DATABASE_URL not set — skipping"); return; };
    let (w, other) = ([81u8; 32], [82u8; 32]);
    let repo = PostgresConvoyRepository::new(pool.clone());
    let (s1, s2, s3) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let now = chrono::Utc::now();
    let row = |sector_id: uuid::Uuid, in_transit: bool, arrival_time: chrono::DateTime<chrono::Utc>| ConvoyDbRecord {
        convoy_id: uuid::Uuid::new_v4(),
        sector_id,
        owner_wallet:  w.to_vec(),
        origin_q:      0,
        origin_r:      0,
        destination_q: 1,
        destination_r: 0,
        route:         serde_json::json!([{"q": 0, "r": 0}, {"q": 1, "r": 0}]),
        arrival_time,
        vehicle_class: "BehemothMegaTruck".to_string(),
        cargo:         serde_json::json!({}),
        in_transit,
        created_at:    now,
    };
    let rows = [
        row(s1, true, now + chrono::Duration::minutes(10)),
        row(s2, true, now - chrono::Duration::minutes(10)),
        row(s3, false, now + chrono::Duration::minutes(10)),
    ];
    let ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.convoy_id).collect();
    let mut seeded = Vec::new();
    for r in rows {
        seeded.push(repo.create_convoy(r).await);
    }

    // Collect.
    let s1_w = repo.has_convoy_in_transit(s1, &w).await;
    let s1_other = repo.has_convoy_in_transit(s1, &other).await;
    let empty_w = repo.has_convoy_in_transit(uuid::Uuid::new_v4(), &w).await;
    let s2_w = repo.has_convoy_in_transit(s2, &w).await;
    let s3_w = repo.has_convoy_in_transit(s3, &w).await;

    // Clean.
    let cleaned = sqlx::query("DELETE FROM convoy_records WHERE convoy_id = ANY($1)")
        .bind(&ids).execute(&pool).await;

    // Assert.
    cleaned.expect("convoy cleanup failed");
    for s in seeded {
        s.expect("create_convoy");
    }
    assert!(s1_w.expect("S1, W"), "in transit and due later: counts");
    assert!(!s1_other.expect("S1, OTHER"), "another wallet's sector query: no row");
    assert!(!empty_w.expect("empty sector, W"), "a sector with no convoys");
    assert!(!s2_w.expect("S2, W"), "ten minutes overdue: no longer counts");
    assert!(!s3_w.expect("S3, W"), "landed: no longer counts");
}

// ── BP-3b-1: the mission route's limits (in-memory; wallets 230–240, sessions only) ──

/// `n` copies of the test unit, an unknown Commander and one unknown Section: past the
/// body checks such a request answers 404 (Commander not found), so it never deploys.
fn mission_body_with_units(n: usize) -> serde_json::Value {
    let units: Vec<serde_json::Value> = (0..n).map(|_| mission_unit_json()).collect();
    serde_json::json!({
        "squad": { "units": units },
        "mission_id": "GUARD_DUTY",
        "commander_id": uuid::Uuid::new_v4(),
        "section_ids": [uuid::Uuid::new_v4()],
    })
}

/// A one-unit body exactly `len` bytes long (the unit's name is padded).
fn mission_body_of_len(len: usize) -> serde_json::Value {
    let mut body = mission_body_with_units(1);
    let base = body.to_string().len() - "Grunt".len();
    body["squad"]["units"][0]["name"] = serde_json::json!("x".repeat(len - base));
    assert_eq!(body.to_string().len(), len, "the padded body has the length asked for");
    body
}

/// Six missions a minute per wallet (a literal); the seventh is 429 whatever its body.
#[tokio::test]
async fn mission_resolve_per_wallet_limit() {
    let state = default_state();
    let token = insert_session(&state, [230u8; 32]);
    let state = std::sync::Arc::new(state);
    let mut statuses = Vec::new();
    for _ in 0..7 {
        let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
        statuses.push(post_mission(app, Some(&token), &mission_body_with_units(1)).await.0);
    }
    let mut expected = vec![axum::http::StatusCode::NOT_FOUND; 6];
    expected.push(axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(statuses, expected);
}

/// Sixty a minute per address (a literal): ten wallets spend six each, and an eleventh,
/// fresh wallet is refused by the address limit.
#[tokio::test]
async fn mission_resolve_per_ip_limit() {
    let state = default_state();
    let tokens: Vec<String> = (0..11u8).map(|i| insert_session(&state, [230 + i; 32])).collect();
    let state = std::sync::Arc::new(state);
    let mut statuses = Vec::new();
    for i in 0..61usize {
        let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
        statuses.push(post_mission(app, Some(&tokens[i / 6]), &mission_body_with_units(1)).await.0);
    }
    let mut expected = vec![axum::http::StatusCode::NOT_FOUND; 60];
    expected.push(axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(statuses, expected);
}

/// 64 KiB of body (65,536 bytes, a literal) is read; one byte more is axum's 413.
#[tokio::test]
async fn mission_resolve_body_over_the_cap_is_413() {
    let state = default_state();
    let token = insert_session(&state, [230u8; 32]);
    let state = std::sync::Arc::new(state);
    let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
    let (at_cap, _) = post_mission(app, Some(&token), &mission_body_of_len(65_536)).await;
    assert_eq!(at_cap, axum::http::StatusCode::NOT_FOUND, "a body at the cap is read");
    let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
    let (over, _) = post_mission(app, Some(&token), &mission_body_of_len(65_537)).await;
    assert_eq!(over, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
}

/// Canon: 8 units per Section. One Section carries 8 (past the check: 404), not 9 (400).
#[tokio::test]
async fn mission_resolve_rejects_more_units_than_its_sections_hold() {
    let state = default_state();
    let token = insert_session(&state, [230u8; 32]);
    let state = std::sync::Arc::new(state);
    let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
    let (eight, _) = post_mission(app, Some(&token), &mission_body_with_units(8)).await;
    assert_eq!(eight, axum::http::StatusCode::NOT_FOUND, "8 units for 1 Section pass the cap");
    let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
    let (nine, body) = post_mission(app, Some(&token), &mission_body_with_units(9)).await;
    assert_eq!(nine, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "A mission carries at most 8 units per Section");
}

/// The limits come before the body: with the wallet's six spent, an oversized body is
/// 429, not 413, so an over-cap body is counted like any other.
#[tokio::test]
async fn mission_resolve_counts_before_reading_the_body() {
    let state = default_state();
    let token = insert_session(&state, [230u8; 32]);
    let state = std::sync::Arc::new(state);
    for _ in 0..6 {
        let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
        post_mission(app, Some(&token), &mission_body_with_units(1)).await;
    }
    let app = mercs_server::routes::router(std::sync::Arc::clone(&state));
    let (status, _) = post_mission(app, Some(&token), &mission_body_of_len(65_537)).await;
    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);
}
