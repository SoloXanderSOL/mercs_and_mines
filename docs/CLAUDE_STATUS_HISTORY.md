# CLAUDE.md §1 history (moved 2026-10-28, ST-0b)

- **Section 1 (Database Layer):** 1a (Postgres) and 1b (Redis) complete. **1c-3 (the
  GAP-34 fix) built at `bb49b3c`:** `session_start` (header + full request + timestamp)
  is written to Postgres through `log_session_start` at all four session sites, and the WS
  retreat `player_input` through `append_entry`. `combat_end`, `combat_tick` and
  `collision_detected` are pruned. `SessionLogWriter`, `log_reader`, `LOG_DIR` and the
  `.ndjson` files are retired, and the combat AAR reads the input log repository. Left
  open: GAP-42 (retreat tick), GAP-43 (log ordering), GAP-44 (convoy dispatch). Arch
  Audit 4 complete. **Arch Audit 6's conclusion is superseded** (it missed GAP-34).
- **Section 2 (Campaign State Machine):** 2a and 2b complete. 2c and 2d deferred until
  after Sections 3–5. Arch Audits 3 and 5 complete.
- **Section 3 (Hex Map and Timers):** Complete — 3a-1, 3b-1, 3b-2, 3c-1, 3d-1, 3e-1.
- **Section 4 (Commander Records):** Complete (closed 2026-10-16; 4f deferred to 2c/2d). 4a-1, 4b-0, 4b-1, 4e-0 (state-based Wipeout, GAP-35 part 2) and 4c-1
  (stress persisted through `sim-engine`'s pure stress fns; `update_stress` sets Shattered at
  100; five mutators return `NotFound` on a miss) complete. **4a-2 complete (`f95969b`,
  `90b0cd2`, `322c53d`, 2026-10-16; Commander Grade):** the Commander_Grade_System §3 tables
  for both classes are data in `sim_engine::grade` (`commander_tqs`, `foreman_tqs`,
  `CommanderGrade::from_tqs`); the roster gains `CommanderGrade`, `ForemanSpecialization` and
  `ForemanFlaw` (Foremen are graded, with no generator until 7a-1). `generate_commander`
  stores the Grade in the nullable `commander_records.commander_grade` (migration 0019, with
  a CHECK); TQS is never stored, and the draw count stays 7. The death card copies the
  Grade (`card_mint_pending.commander_grade`). Per-seed generator output is not a contract
  (CF4). The sim `Commander.species` field and `Species` enum are gone; `is_nft` is dropped
  (0020). No R7 bump. **4d-2 complete (`1f87d98`, `291b4ba`, `a723ad6`, 2026-10-16;
  completes Section 4's brick table, with 4f deferred to 2c/2d):** `evaluate_fatal_flaw`
  takes `CommanderFlaw` and matches exhaustively on flaw × `StressTier` for all twelve flaws
  (ODQ-36 numbers; declarative, no draws; still no production caller, so no R7 bump). The
  first host brick that calls it inside a logged sim entry point bumps the version and logs
  the flaw and tier in `session_start`. `generate_commander` is band-first: a Grade band from
  `DEFAULT_GRADE_WEIGHTS` `[50, 30, 15, 4, 1]`, then a uniform pick from
  `sim_engine::grade::commander_combinations` (6 draws; all 12 flaws roll). `0021` adds the
  `fatal_flaw`/`specialization` CHECKs, and a two-way `pg_get_constraintdef` guard checks all
  five roster CHECKs. **4g-1 built
  (`1917f4c` storage + `21a76ab` route, 2026-10-14):** `POST /api/mission/resolve` requires
  an `AuthSession`, names a `commander_id` read from `commander_records` (404 for unknown or
  another wallet's; 409 for KIA, Shattered or already deployed; 400 for a non-null
  `squad.commander` or `seed_override`), and logs `session_start` through the fallible
  `InputLogRepository::start_session` **before** the roll. A record Commander adds 0 to the
  roll. Stress, XP, rank and the deployment close are one `resolve_deployment` transaction.
  `deployment_records` (migration 0016) holds one `active` row per Commander by partial
  unique index; a stale `active` row (> 5 min) is aborted on the next start.
  `commander_records.stress` has `CHECK (stress BETWEEN 0 AND 100)`. `mission_tier` is a
  Phase 0 bridge from mission difficulty. Workspace `0.3.0`. **4g-2 complete (`737939d`,
  2026-10-14; Reviewer 4g-1 finding 1, the lost update):** the route re-reads the Commander
  after `start_deployment` and refuses KIA/Shattered with 409 (deployment aborted) before
  logging; `resolve_deployment(…, kia, tier, outcome, cfg)` computes stress, XP and rank
  from a `SELECT … FOR UPDATE` row, refuses KIA (500), and returns `AppliedEffects`. No
  bump (R7 does not fire). **4d-2** was created by the 2026-10-13 scoping session.
  **4e-1 built (`c95e49d`, 2026-10-15; permadeath):** a mission `Wipeout` marks the
  Commander KIA and queues one `card_mint_pending` card (foil if Glorious) inside
  `resolve_deployment`'s locked transaction, before the close, through the private
  `mark_commander_kia` helper (no trait method). `card_mint_pending` (migration 0017) is an
  account-scoped snapshot whose only FK is the wallet, so it survives the §4f wipe.
  `is_survivor` (neither KIA nor MIA, ODQ-30) drives `classify_outcome` and `any_survivor`;
  `check_commander_permadeath` is deleted. `is_glorious_death` lives in `commander_gen`;
  the route passes a ticker contribution of `0.0` until §2c. The mission response gains
  `commander: {kia, glorious}`. No input-log entry, no R7 bump. Test cleanup order:
  `card_mint_pending` → `deployment_records` → `section_records` (in `ROSTER_TABLES` since
  7b-1) → `commander_records` → `foreman_records` (since 7a-1) → `advisor_records` (since
  7a-2) → `crew_records` (since 7c-1) → their campaigns → membership → `player_accounts`.
  `clean_wallet_4g1` is list-driven over `ROSTER_TABLES` and collects campaigns through the
  roster rows **and** the wallet's membership rows (since 7b-1). A test that seeds through
  `seed_account_and_campaign` is cleaned by `clean_wallet_4g1` even if every insert it
  makes is refused.
- **Section 5 (Multiplayer Sync):** split agreed 2026-10-18 (5a-0 → 5a-4, then INF-6, then
  5b–5d; the Section 5 doc's `## 5x`). **5a-0 complete (`cef3ab2`, 2026-10-18; no behaviour
  change):** `pub(crate) insert_{commander,foreman,section,crew,membership,campaign}(conn:
  &mut PgConnection, …)` hold each INSERT with its SQL literal moved byte for byte (a
  `.sqlx/` file name is the sha256 of the literal, indentation included), plus every clamp;
  the `create_*`/`add_member` methods acquire a connection and wrap them. `clamp_new_record`
  is `pub(super)` in `commander`, `foreman` and `crew`. `MembershipRepository` and
  `CampaignRepository` return `RepositoryError`; `InitError::SqlxError` is gone (its sites
  use `InitError::RepositoryError`). `apply_ticker_delta` on an unknown campaign is
  `NotFound`. `InMemoryCampaignRepository` is still all `unimplemented!()` (5a-2 builds a
  real one). **5a-1 complete (`96fb4ba`, `99f643e`, 2026-10-18; join schema):** migration
  `0030` adds `acquired_via TEXT NOT NULL DEFAULT 'hired'` to commander/foreman/section/crew
  records (`<t>_acquired_via_canon`: Commanders `starter|replacement|hired`, the rest
  `starter|hired`; guard rows), `<t>_one_starter` partial uniques on `(campaign_id,
  player_wallet) WHERE acquired_via = 'starter'`, and `<t>_membership_fkey` (`(campaign_id,
  player_wallet) → player_campaign_membership`, `ON DELETE NO ACTION`) on those four and
  `advisor_records`. `repository::AcquiredVia { Starter, Replacement, Hired }` is a `via`
  parameter on the four `insert_*` helpers, never a struct field; every `create_*` passes
  `Hired`, and nothing passes `Starter`/`Replacement` yet (5a-2/5a-4). GAP-61 step 1:
  `require_grade` refuses `commander_grade: None` first in `insert_commander` and the in-memory
  `create_commander`. A membership delete with roster rows left is now refused; a campaign
  delete still cascades everything. `section_records_player_wallet_fkey` is shadowed by
  membership's own account `RESTRICT` and can't be observed by name. Every Postgres roster
  fixture writes a membership. **5a-2a complete (`11aefb8`, `7f198f2`, `0b84e70`,
  2026-10-18):** `InMemoryCampaignRepository` is a real `DashMap` store mirroring the Postgres
  impl, miss paths included (`update_sector_state` miss → `Ok(())`; `activate_campaign` →
  `false` for a missing id or any non-`Pending` state; list unordered). Share it as
  `Arc<InMemoryCampaignRepository>`: no inner `Arc`, no `Clone`, no `Mutex`. `AppState::new`
  is unchanged (5a-3 rewires it). GAP-70: `assign_gateway_hexes` round-robins Solo overflow
  (`rim_indices[slot % len]`, no draw moves, no R7); the overflow `warn!` is gone.
  `delete_memberships_by_campaign` is deleted (trait and both impls, one `.sqlx` file).
  **5a-2b complete (`e0952a2`, `da5d9a0`, `b3711f8`, 2026-10-18; deployed):**
  `CampaignJoinRepository::join_campaign` writes the membership and the four starters
  (`AcquiredVia::Starter`) in one transaction under `JOIN_ADVISORY_KEY`. **5a-3a complete
  (`8473f14`, 2026-10-19):** `CampaignJoinRepository::begin_launch(campaign_id, max_members,
  ends_at, victory_tickers) -> LaunchStart { Started { campaign, members }, NotFound,
  NotPending, NoMembers, TooManyMembers(n) }` reads, guards and activates under the same key
  (Reviewer 5a-2b F1); `initialize_campaign` does map gen, hexes, Redis and `campaign_started`
  after its commit. `LAUNCH_MAX_MEMBERS = 1` (GAP-58, until 5b-2) → `409` "campaign has more
  than one member"; `InitError::WasAlreadyActive` is gone. `AppState.campaign_join_repo`
  (Postgres in `main.rs`); deployed with its docs commit `145338e`. **5a-3b complete
  (`4af1945`, 2026-10-19; the join's first production caller):** `POST /api/campaign/join`
  (`routes/campaign.rs`; `AuthSession`, no body) → `201` `Joined` / `200` `AlreadyMember` with
  `{campaign_id, commander_id, foreman_id, section_id, crew_id}` (built with `json!`;
  `StarterIds` has no `Serialize`); every join error → `500`. S3: `post_mission_resolve` answers
  `409` "campaign is not active" right after the `404` Commander, before `start_deployment`
  (an unlocked read). GAP-74: dispatch `404` (JSON) → `403` non-member → `409` not `Active`;
  the events socket `401` → `404` → `403` → `409` unless `Active` or `Ending`, before the
  upgrade, which it takes as `Result<WebSocketUpgrade, WebSocketUpgradeRejection>` so the
  checks run first (an admitted `oneshot` request answers `426`). `forbidden()` helper.
  `OneCampaign` is gone. A failed launch is retired to `Archived` (`admin.rs`). No migration,
  no `.sqlx` change, no R7. **5a-3c complete (`32199dc`, 2026-10-19; the join switch,
  Reviewer 5a-3b F1):** `ServerConfig::join_enabled` (default `false`) is read from
  `MERCS_JOIN_ENABLED` by `ServerConfig::from_lookup` (`parse_or_warn`: exactly `true` opens; anything else
  warns and stays closed; documented in `.env.example`). Order: `401` (`AuthSession`) →
  `503` `{"error":"campaign joins are closed"}` (`service_unavailable()`) → `429` per client
  IP → `429` per wallet (INF-6c-1) → wallet decode → join, so the `503` precedes every
  database read and `JOIN_ADVISORY_KEY`. **Off in production; the Director turns it on
  after all of INF-6 deploys.** A test that joins
  turns it on itself (the lib `joined_state()`; E1 via `Arc::make_mut(&mut state.config)`);
  `pg_route_state` leaves it off. Both bare dispatch `500`s go through `internal_error`
  (CF5). **5a-3b and 5a-3c deployed together (`ce8096e`, 2026-10-20).** **5a-4 built
  (`2caa944`, 2026-10-20; the alpha KIA replacement, ODQ-33):** `POST
  /api/campaign/:campaign_id/replacement-commander` (`routes/campaign.rs`; **not** behind the
  join switch): `401` → `429` per client IP → `429` per wallet (INF-6c-1) → `404` → `403` non-member (any membership row) → `409` not `Active`
  (read before the transaction) → `CampaignJoinRepository::claim_replacement(campaign_id,
  wallet, seed: u32) -> ReplacementOutcome { Granted(Uuid), NotMember, HasUsable }` → `201`
  `{campaign_id, commander_id}` / `403` / `409` "member has a usable Commander" / `500`. The
  Postgres claim is one transaction under the member's membership row (`SELECT … FOR UPDATE`,
  deliberate, never `JOIN_ADVISORY_KEY`), then one count/usable read, then `insert_commander`
  with `AcquiredVia::Replacement`. Zero Commanders → `Internal(NO_COMMANDERS)` → `500`;
  per wipe, not once per membership. The in-memory claim takes its own `claims` Mutex and
  does not model the index. Alpha-only: `TODO(phase-2-§10j)` ×2 (Phase 2 §10j removes them).
  Not a logged input, no R7. **5a-5 built (`4e104d1`, 2026-10-22; ODQ-46, ODQ-47):** a
  Commander is **usable** when it is neither KIA nor Shattered; the claim grants when none is
  usable (`HasUsable` otherwise), in both impls, with `TODO(phase-2-§14)` ×2 on the
  eligibility reads (the Shattered clause retires with §14 Rehabilitation). Migration `0032`
  re-cuts `commander_records_one_live_replacement` under the same name (`WHERE acquired_via =
  'replacement' AND NOT is_kia AND NOT is_shattered`) and adds the nullable
  `card_mint_pending.acquired_via` (`card_mint_pending_acquired_via_canon`, with a guard row),
  which `mark_commander_kia` copies from `c.acquired_via`. Postgres is its only oracle: the
  in-memory card carries `acquired_via: None`. **INF-6** (split 6a → 6b → 6c-1 →
  6c-2a → 6c-2b-1 → 6c-2b-2 → 6d; 6a through 6c-2b-2 deployed, 6d built; see INF-6a
  … INF-6d below), then the Breakpoint freeze: no §5b until 2026-11-18.
- **Section 6:** Not started.
- **Sections 7 (Personnel Entity Model) and 8 (Combat Engine to canon):** created
  2026-10-13 (ruling D2); both run before Section 6. Strike Mining and Base Building moved
  into Phase 2 as §13/§14 (ruling D1). **Section 7 complete (2026-10-18): 7a-1 built (2026-10-16;
  Mining Foreman).** The class-neutral generation helpers live in `field_leader_gen`, where
  `pick_band` refuses an overflowing `u32` sum, an all-zero vector and a non-zero weight on an
  empty band (Reviewer 4d-2 F1/F2). `generate_foreman` is band-first over
  `sim_engine::grade::foreman_combinations` with the same `DEFAULT_GRADE_WEIGHTS`.
  `sim_engine::foreman_flaw::evaluate_foreman_flaw` returns `FatalFlawEffect` (47 variants;
  ODQ-38 numbers; the three shared flaws route through the Commander fns, F3), with no
  production caller. Migration `0022` adds `foreman_records`, with five roster CHECKs under the
  per-table guard `roster_checks_match_the_roster_both_ways`. `ForemanRepository` has Postgres
  and in-memory impls and no `AppState` field (§5a adds it). **7a-2 complete (`0ceb184`,
  `fdfe413`, `e88518f`, 2026-10-17; Advisors):** the dead Phase 0 `AdvisorBoard =
  Vec<Commander>`, `retire_commander` and `COMMANDER_RETIRE_RANK` are deleted (retirement
  freezes a Field Leader's own record; its brick adds its own constant). `sim_engine::roster`
  gains `AdvisorSpecialization` (14) and `AdvisorQuirk` (12; identity only, GAP-64), plus
  `AdvisorBoard` (3, derived by an exhaustive `board()`, never stored) and `GenericBoard`
  (Mercenary/Mining, what a caller passes; `generic_pool` gives 4 and 7). The Research Board
  is never rolled generically. `advisor_gen::generate_advisor(wallet, campaign, board, seed)`
  makes seven draws (origin, spec, quirk, bias ×2, name ×2) with no band; rank 1, not rolled.
  Migration `0023` adds `advisor_records` (one `name`, `prng_seed_state`, no `updated_at`,
  four `_canon` CHECKs, campaign CASCADE). `AdvisorRepository` has Postgres and in-memory
  impls, no `AppState` field and no production caller. No R7 bump. **7c-1 complete
  (`f7a6558`, 2026-10-17; Mining Crews):** migration `0024` adds `crew_records` (owner
  columns as `0023`; `headcount` 8 under `crew_records_headcount_range` 0..8, a wipe is 0;
  `xp` 0, rank derived; `morale` 60 under `crew_records_morale_range` 0..100; no
  `foreman_id`, name, enum, `updated_at` or guard row). `CrewRecord::new_hire` sets the
  canon hire (8/0/60, no RNG). `create_crew` is the only write and clamps morale in both
  impls; the in-memory impl stores an out-of-range headcount and overwrites a duplicate id
  (documented). No `AppState` field, no production caller, no R7 bump. **7b-1 complete
  (`877adb9`, `64be6db`, 2026-10-17; Section owner, GAP-63):** `seed_account_and_campaign`
  writes a membership row and `clean_wallet_4g1` collects campaigns through it, so a
  refusal-only test can't leak its campaign (Reviewer 7c-1 F1; the test DB's 59 orphans were
  deleted once). Migration `0025` adds a **nullable** `section_records.player_wallet` (BYTEA,
  RESTRICT, backfilled from the Commander, `idx_section_records_player`; `NOT NULL` waits for
  a contract migration) and `section_records_headcount_range` (0..8). `SectionRecord` gains
  `player_wallet: Option<Vec<u8>>`; `create_section` refuses `None` first in both impls
  (`Internal`); the in-memory impl stores an out-of-range headcount. `update_headcount`
  unchanged. No production caller, no R7 bump. **7b-2 complete (`5ee4de7`, `4abf451`,
  2026-10-18; deployed at `27f2598`):** per-mission Sections on `deployment_sections`
  (`0027`), the rank cap, `player_wallet NOT NULL` (`0026`). **7d-1 complete (`63a9206`,
  `0fbd41a`, `2b61dc1`, 2026-10-18; Section 7 is complete):** `0028` drops
  `section_records.commander_id` (contract). `sim_engine::roster` gains `Augment` (11, with
  `catalogue()`), `FittingSource` (2) and `CasualtyClass` (4, a persistence level, not
  `UnitStatus`). `sim_engine::casualty`: `from_unit_status`, `chrome_fate` (per carrier,
  ODQ-43), `run_casualty_lottery` (convert first, then exactly `k` draws) and Equipment
  Recovery (`recovery_chance_percent` 75/40/15/0; `roll_equipment_recovery`, one draw per
  KIA, survivor-gated). Migration `0029` adds `augmentee_records` (exactly one owner;
  composite owner FKs `RESTRICT`; campaign `CASCADE`; the Chrome Ceiling as partial uniques;
  two guarded catalogue CHECKs) and `crew_records_owner_key`. `server::augmentee_gen` and
  `repository::augmentee` (Postgres + in-memory; `record_fate` is one conditional `UPDATE`).
  No production caller (8b-1 wires it), no `AppState` field, no R7 bump.
  `augmentee_records` is `ROSTER_TABLES[0]`. Section 8 not started.
- **INF-1 (infrastructure, GAP-37):** Complete — `tracing` subscriber installed, reward-mult
  config hardened, workspace version `0.2.0`.
- **INF-2 (infrastructure, GAP-38):** Complete — idempotent `mercs_server::init_tracing()`
  (ANSI only on a TTY) with an install test; `RepeatWarn` suppresses repeat warnings at the
  two background-task tick gates (INF-3b added per-item suppression); stress-penalty env
  keys warn on bad values; `logs/` untracked and gitignored.
- **INF-3a (infrastructure; Reviewer 4g-2 f1–3, GAP-46, Reviewer 4g-1 f4):** Built at
  `ed9a3a1` — `resolve_deployment` locks `FOR NO KEY UPDATE`; the integrity route answers
  404/500; no route serialises with `.unwrap()`; logged request payloads refuse the request
  before logging instead of logging `null` (WS: 500 before the upgrade). Regenerate `.sqlx/`
  with `cargo sqlx prepare --workspace -- --all-targets` (test-file queries live in the test
  target).
- **INF-3b (infrastructure; GAP-39, GAP-40, GAP-41):** Built at `8921bab` + `a90dc11` —
  `supervisor::supervise` runs both background tasks and restarts one after a panic or
  return, rate-limiting repeated fast exits (GAP-41); per-item failures in both poll loops
  are suppressed by `KeyedRepeatWarn` (one per failure site, evicted when the item leaves
  the due set), via `ExpiryWarns` / `LifecycleWarns` (GAP-39); the numeric env readers warn
  on an unparseable value, raw prints go through `tracing`, and `main` calls
  `try_init_tracing()` (GAP-40); `dispatch_convoy` logs its 500s and answers 401 for an
  undecodable token wallet; the AAR's missing-timestamp arm is a 500; the legacy
  `FULL_SUCCESS_REWARD_MULT` key is retired.
- **INF-4 (infrastructure; GAP-32, GAP-33, GAP-52, ODQ-20):** Built at `55ebb5c` +
  `49dd10a` — public `GET /api/version` → `{version, build_sha}` (`build_sha` from
  `option_env!("MERCS_BUILD_SHA")`, set by CI on the release build; `"unknown"` locally);
  the admin key is compared as SHA-256 digests with a XOR-fold (ODQ-20); `post_auth_verify`
  upserted the account before issuing the session (GAP-52; the upsert is gone since
  INF-6c-2b-1, GAP-79); `internal_error` is `#[track_caller]` and is the
  **only** `500` builder (since INF-6b): it journals the full message, an `error_id`
  (`Uuid::new_v4()`) and the call site, and returns exactly
  `{"error":"internal error","error_id":"<uuid>"}`. **No literal `500`:** every one is an
  `internal_error` call written at the site (never `.map_err(internal_error)`). The other
  response helpers: `forbidden()`, `service_unavailable()`, and `too_many_requests()`
  (`429 {"error":"rate limited"}` + `Retry-After`, INF-6c-1). CI runs on pushes to `main` and `dev` and on every pull request (GAP-32);
  deploy is still main-only. `.github/workflows/audit.yml` (INF-6d, GAP-81) also runs
  `cargo audit` weekly (Mondays 06:17 UTC) and on `workflow_dispatch`, from `main`'s copy;
  `release.yml` must never get a `schedule:` trigger (a scheduled run there would deploy). The droplet-side deploy is `deploy/remote_deploy.sh`: it reads
  the running build's SHA, swaps, restarts, polls `/api/version` until `build_sha` equals the
  commit, and rolls back with explicit checks. Its exit codes are a contract: **0** deployed,
  **1** failed with the rollback verified (by SHA, or by health only when the old SHA was
  unreadable), **2** rollback failed or impossible. `deploy/test_remote_deploy.sh` (13 stub
  scenarios since INF-6a) and `shellcheck` run in the `deploy-script` job on every push, and
  the deploy job needs it (GAP-33).
- **INF-6a (infrastructure; GAP-54, GAP-55, GAP-56 repo half; the gating audit, Director
  ruling (a) 2026-10-22):** Built at `7e42d82` (`spin` 0.9.8 → 0.9.9, `Cargo.lock` only,
  never compiled; cargo-audit cannot ignore a yanked crate) + `81fc1af` (audit job) +
  `0b5c2c6` (deploy hardening). Before it, `main` = `dev` = `d975249`, a lockfile-only
  commit (rustls 0.23.46, rustls-webpki 0.103.16, webpki-roots 1.0.10). **The `audit` CI job
  gates `deploy`** (`needs: [build, deploy-script, audit]`): cached cargo-audit 0.22.2 runs
  `cargo audit` against `.cargo/audit.toml` (two dated ignores, `rsa` and `anyhow`, since `5d55f92`; `[output] deny =
  ["unsound", "yanked"]`; `quiet = false` is required by 0.22.2). The advisory DB is fetched
  live, so a red run with no code change is expected; the escape hatch is a dated ignore
  entry in the same commit. CI uploads `app/` to `$APP_PATH/app.new/` (`--link-dest` on the
  live `app/`); the script swaps `app` → `app.prev`, `app.new` → `app` after the binary, and
  a rollback restores `app.prev/` too. Every `sudo` is `sudo -n`, behind a pre-swap
  `sudo -n -l` probe per command (exit 2, nothing touched). The deploy job has
  `concurrency: deploy-production` (the newest pending deploy wins). The unit is
  `mercs-and-mines`; the deploy-user secret is `DEV_SERVER_USER`. **Deployed and
  droplet-verified 2026-10-22 (`4c8fe40`).** The Director's GAP-56 host pass is done: CI
  deploys as `deploy`, and the service runs as `mercs`.
- **INF-6b (infrastructure; GAP-76, Reviewer 5a-3c F1, the boot bind warning):** Built at
  `2b479cf`. `internal_error` is the only `500` builder (generic body + `error_id`; see
  INF-4). The six former literal `500`s (`ws_combat.rs` ×2, `admin.rs` ×3,
  `post_combat_stream_start`) go through it, and the AAR's `400` no longer echoes serde's
  error over the logged payload (journalled with `warn!`). `ServerConfig::from_lookup(get)`
  (`from_env` = `from_lookup(env_lookup)`); `config.rs` no longer mutates the process
  environment. `config::public_bind_warning(&SocketAddr)` is `None` on loopback (IPv4-mapped
  included, via `to_canonical`); `main` journals it once on `listener.local_addr()`. Local
  dev with the repo `.env` (`0.0.0.0:3000`) sees the warning on purpose. The journal-capture
  pattern for a test is `routes/mod.rs`'s `Buf` + `tracing::subscriber::with_default`. No
  R7, no migration. **Deployed 2026-10-22 (`e73d0dd`).**
- **INF-6c-1 (infrastructure; client IP + rate limits; Reviewer 5a-3b F1, 5a-4 CF1, 5a-5 CF,
  Reviewer INF-6b F1):** Built at `7e1005d`, after `5d55f92` (`event-listener` 5.4.2, the
  RUSTSEC-2026-0221 ignore dropped). `main.rs` serves
  `into_make_service_with_connect_info::<SocketAddr>()` (the bind warning stays before it).
  `rate_limit::client_key(peer, &HeaderMap)` trusts `X-Real-IP` only from a **loopback
  peer** and never reads `X-Forwarded-For`; anything but exactly one parseable value →
  `ClientKey::Shared`, journalled once per `AppState` (`RateLimits::shared_warned`); IPv6 is
  keyed by /64. This depends on nginx's `proxy_set_header X-Real-IP $remote_addr` on
  `location /api/` (Security_Mandate §2i). `rate_limit::FixedWindow<K>` is a keyed
  fixed-window limiter: the window is anchored at a key's first hit, the map has a soft
  cap of 10⁴ swept on pressure, and a map still full refuses new keys (fail closed). **Never
  call `len()`/`retain()`/`contains_key()` on its `DashMap` while an `entry()` guard is
  alive** (deadlock). Five limits, all consts (60 s windows): per IP `challenge` + `verify`
  20 (one shared bucket), join 10, replacement 10; per wallet join 3, replacement 3 (none on
  sign-in, GAP-77). `AppState.rate_limits` is fresh per `AppState`. The four handlers take
  `Option<ConnectInfo<SocketAddr>>` + `HeaderMap`; `None` → `500` through `internal_error`
  (`client_ip`). Sign-in takes `Result<Json<_>, JsonRejection>`, so a malformed body is
  counted, then answered by axum unchanged. Tests give a router a peer with
  `routes::with_test_peer(router, TEST_PEER)` (`192.0.2.1:1`; `pub`, `#[doc(hidden)]`). T1
  pins `#[track_caller]` via `routes/mod.rs:{line}:`. No R7, no migration. **Deployed
  2026-10-22 (`e878837`).**
- **INF-6c-2a (infrastructure; sign-in hardening, GAP-77, the challenge memory leak):** Built
  at `a72d3e2`. `pending_challenges` is a `DashMap<[u8; 32], PendingChallenge>` keyed by
  `auth::challenge_key` (SHA-256 of the challenge text, which the client sends back
  verbatim); nothing parses the `Nonce:` line. `PendingChallenge` stores the
  `wallet_address` it was issued to, and **`verify` refuses (`400 "challenge mismatch"`) a
  request naming another wallet**: without that check, key B could redeem wallet A's
  challenge. `verify` still `remove`s on lookup whatever the outcome. `challenge` refuses a
  `wallet_address` that is not bs58 of 32 bytes (`auth::decode_wallet` → `400 "invalid
  wallet address"`) after the rate-limit check and before anything stores it. Both sign-in
  routes carry `DefaultBodyLimit::max(AUTH_BODY_LIMIT)` (4096) **per route**, never on the
  `Router`: the overflow is axum's own plain-text `413`, a `JsonRejection` after the
  limiter, so it is counted. `auth::run_auth_sweep_task` (a third `supervise`d task,
  `"auth_sweep"`, every `AUTH_SWEEP_INTERVAL_SECS` = 60) calls
  `sweep_expired_challenges` / `sweep_expired_sessions(map, now)`, which keep `expires_at
  >= now` (the boundary both refusals still accept). No cap or eviction yet (INF-6c-2b,
  ODQ-48). Auth signing-key seeds (in memory only): 20 and 77. No R7, no migration.
  **Deployed 2026-10-23 (`492ae98`).**
- **INF-6c-2b-1 (infrastructure; GAP-80, GAP-79 (a)):** Built at `276dea4`. **Sign-in
  verifies strictly** (`verify_wallet_signature` → `verify_strict`; no `Verifier` import),
  and `decode_wallet` refuses any address that is off-curve or a small-order (weak) key
  (`VerifyingKey::from_bytes` + `!is_weak()`), so `challenge` answers them `400 "invalid
  wallet address"`. The forged-signature tests use the identity key `[1, 0×31]` with
  signature `[1, 0×63]` (deterministic; the all-zero key is not). **Sign-in writes nothing
  to Postgres:** `verify` no longer upserts, and `AppState.account_repo` is gone. The join
  creates the `player_accounts` row (`postgres_account::insert_account_if_absent`, `ON
  CONFLICT DO NOTHING`, right after `lock_joins`). `verify`'s `500` arms: `client_ip` and
  token serialisation. The auth sweep task is covered by `auth_sweep_task_sweeps_both_maps`;
  **its `main.rs` spawn is guarded by a count gate only** (`tokio::spawn(supervise(` = 3).
  No R7, no migration. **Deployed 2026-10-23 (`7be446c`, with a `Cargo.lock`-only tokio
  1.54.1 bump for RUSTSEC-2026-0386).**
- **INF-6c-2b-2 (infrastructure; bounded sign-in lists, ODQ-48; Reviewer INF-6c-1 F1/F2):**
  Built at `45574d0`. It supersedes INF-6c-1's fail-closed cap and once-per-`AppState`
  warning. **The limiters fail open:** `RATE_LIMIT_MAP_CAP` = 10⁵; a new key at a map still
  full after the sweep is admitted **uncounted** (no `entry()`) and journalled through the
  limiter's own `overflow: RateWarn`, named by `FixedWindow::new(name, …)` (the five names
  are the `RateLimits` field names). A capped map sweeps at most once per
  `SWEEP_MIN_INTERVAL` (1 s, `SweepThrottle`; the first sweep always runs).
  **`rate_limit::RateWarn`** journals a repeating condition on its first hit, then at most
  once per `RATE_WARN_INTERVAL` (3600 s) with the count since the last line; `hit(now)`
  returns `Some(n)` to the caller that journals, and the caller owns the text. Eight
  instances from four construction sites: five limiter `overflow`s,
  `RateLimits.shared_warn` (replaces `shared_warned`), the session backstop and the
  challenge eviction. **No `Default` for `RateWarn` or `SweepThrottle`.** Test them on
  injected `Instant`s taken **after** `new()` (a `now` before `base` saturates to 0).
  **Sessions:** `AppState.session_index: auth::SessionIndex` (a `std` `Mutex` over
  `ClientKey` → `(token_id, expires_at)` in issue order) beside the unchanged `sessions`
  map. `auth::admit_session(state, wallet, key)` replaces `issue_session`: a key at
  `SESSION_QUOTA` (10) signs out its own oldest; otherwise, at `SESSION_CAP` (10⁵) live
  sessions, the sign-in is refused (`503 {"error":"sign-in is at capacity, try again
  later"}`, via `service_unavailable`) and nothing is evicted. `ClientKey::Shared` is never
  indexed (backstop only). **Lock order: index → `sessions` shards**; never take the index
  while a `sessions` guard is alive. Stale entries are pruned lazily, and the auth sweep
  task prunes the index after its `retain` (`SessionIndex::prune`). Sessions inserted
  directly by fixtures are unindexed: they count toward the backstop only.
  **Challenges:** `AppState.challenge_cap: auth::ChallengeCap` (`CHALLENGE_CAP` = 10⁵) is
  the only production writer of `pending_challenges`; at the cap it sweeps expired entries
  (throttled), then evicts an arbitrary one (soft cap). A test that needs a small cap swaps
  the `pub` field on `AppState::new(…)`'s value before `Arc::new` (no pre-fill). The
  quota-of-10 rationale assumes the default session and challenge durations; the caps stay
  consts. `AccountRepository` documents that only the join creates accounts (Reviewer
  INF-6c-2b-1 F1). No R7, no migration, no `.sqlx/` change. **Deployed 2026-10-23
  (`a374602`).**
- **INF-6d (infrastructure; WebSocket caps, GAP-82, GAP-81):** Built at `84d655f`
  (toolchain pin) + `53d4c72` (code + tests) + `de136e1` (`audit.yml`). **The toolchain is
  pinned to rustc 1.99.1 by `rust-toolchain.toml` (GAP-82),** so WSL, CI and production
  build on one compiler; a bump is its own commit with a full build, test run and audit.
  CI keeps `dtolnay/rust-toolchain@stable` and the file wins at the first cargo call (a
  `rustc --version` step logs the real compiler). Negatives still mutate conditions or
  constants and stay warning-free, so the 0-warnings gate holds. **Combat socket:** frames
  and messages over 4 KiB (`COMBAT_WS_MAX_MESSAGE`) end the session through the receive
  loop's error arm; the Close waits up to one `tick_stream_delay_ms` for the resolver's
  cancel check. The tick-serialisation arm warns and cancels the resolver before its
  `break`. **Convoy events socket:** 1 KiB cap (`CONVOY_WS_MAX_MESSAGE`); the handler
  `select!`s over its channel and the unsplit socket (no `split()`), so a client Close
  (answered on the next read: keep reading after `Message::Close`) or an over-cap frame
  ends it. The channel is bounded at `state::CONVOY_EVENT_CHANNEL` = 64, and the handler
  holds only a `WeakSender`. The producer clones the sender out of the map guard, then
  `deliver` `try_send`s: `Full` drops that sender and journals through
  `ExpiryWarns.events_full` (a `RateWarn`, now `Debug`); `Closed` is silent. Both removals
  are `remove_if(… same_channel)`, so a closing socket never takes a newer one's sender.
  The one-sender fan-out is still GAP-75 → 5b-2. A peer that stops reading but keeps TCP
  open can park its handler in `socket.send().await` (memory bounded at 64 events). Live
  socket tests serve through `routes::serve_on_loopback` (`cfg(test)`) and connect with
  the dev-dependencies `tokio-tungstenite` 0.24 + `futures-util` 0.3. No R7, no migration,
  no `.sqlx/` change. **Pushed and deployed 2026-10-25.**

**461/461 tests green (1 `shared` + 231 `mercs_server` lib + 2 `aar_determinism` + 156
`db_infrastructure` + 71 `sim_engine`; single-threaded, verified at `dbbcd84` on rustc
1.99.1). Workspace version `0.3.0`. `.sqlx/` holds 77 files; the latest migration is
`0032_usable_replacement_and_card_provenance`. `main` = `dev` = `dbbcd84` (INF-6d, deployed
2026-10-25; joins ON in production since that day).
Next: the Breakpoint freeze; no §5b until 2026-11-18. Demo work runs on the `breakpoint`
branch (`Wiki/GDD/Breakpoint_Demo_Plan.md`) and comes back here as reviewed keeper bricks
after the freeze.** Any brick that changes what `resolve_mission` produces for
a given seed bumps the workspace version in the same brick. 4b-1 reused `sim-engine`'s `OutcomeType` rather than
defining a new enum, and writes **no input-log entry** — the rank-up entry was removed
from scope. **ODQ-22 and ODQ-23 were ruled 2026-09-03 — engagement and mission outcomes
are separate levels, and the mission and streaming pipes are separate ledgers; see
`Wiki/GDD/Canon_Type_Reference.md` §9.** Read the full scope of the next brick and its
carried constraints in the Phase 1 document before writing a line.

**Mission outcome taxonomy (canon 2026-09-02):** `Success / Defeat / TacticalRetreat /
Wipeout`. `PartialSuccess` is retired; `FullSuccess` is now `Success`.

**GAP-35 part 1 landed in `9591331`.** `calc_rewards` no longer matches on the enum — it
keys on `is_success` and `margin` — and the resolver assigns the canon names. Reward
scaling is decoupled from the taxonomy by ruling: do not reintroduce a match on
`OutcomeType` in `calc_rewards`, or retiring a variant silently becomes a balance change.

**GAP-35 part 2 landed in `5bacbb7` (brick 4e-0) — outcomes are survival-based.**
`classify_outcome` runs after unit damage: **Success = objective completed AND at least
one unit alive.** Every unit KIA is `Wipeout`, **win or lose**, and pays nothing — no
credits, no ore, no loot (the loot roll still runs, so no draw moves). A failure with
survivors is `Defeat`. A zero-unit mission is rejected at the route (`400`). The roll
margin now sets only the reward multiplier (`DECISIVE_SUCCESS_REWARD_MULT`). Permadeath
reads `Wipeout`.

**`TacticalRetreat` is reserved and must never be assigned on the mission path.** Note
the qualifier: a player retreat input *does* exist on the streaming WS combat pipe
(`ClientCommand::Retreat` in `routes/ws_combat.rs`), and it emits
`shared::ws_events::CombatOutcome::Retreated` — a **different enum**, which also now
shares the bare variant name `Defeat` with `OutcomeType`. Always write
`OutcomeType::Defeat` fully qualified; never `use` the bare variant. These two points are
ODQ-23 and ODQ-22 respectively, both ruled 2026-09-03.
