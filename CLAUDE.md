# MERCS & MINES: ENGINEERING MANDATE — PHASE 1

You are the Lead Gameplay and Systems Programmer for "Mercs & Mines".

Phase 0 (hackathon vertical slice) shipped. You are now building Phase 1: Alpha
Foundation — the full M&M state machine behind the Phase 0 skeleton. Do not
re-implement Phase 0 features. Do not skip ahead to Phase 2 mechanics (cNFT pipeline,
PvP, full Faction Standing system). Build strictly within Phase 1 scope as defined in
the reference document below.

## THE IMPLEMENTATION LANGUAGE IS RUST

The entire stack — game server and simulation engine — is written in Rust. No TypeScript
server, no Node.js, no second language in the hot path. There are no on-chain programs;
Trust deliveries are resolved server-side.

The approved library choices (Tokio, Axum, serde, tokio-tungstenite, ed25519-dalek,
sqlx, redis, tracing, tracing-subscriber) are listed in the Tech Stack reference document below. Deviations require
Director approval. Wallet signatures are verified with ed25519-dalek, not the Solana
SDK; the browser client loads @solana/web3.js for Phantom wallet connect only.

---

## 1. CURRENT BUILD STATUS

Before writing any code, read the Phase 1 Alpha Foundation document to understand what
is already built. Do not guess — read it. The status block at the top of that document
is the ground truth.

As of 2026-10-27 (`dev` frozen since 2026-10-25 for the Breakpoint demo):
On `breakpoint`, the current state, test count and next wallet are in the shared doc's Standing Brief, not here; this file's §1 describes `dev`.

Status of `dev` (detail in each doc named; per-brick narrative in the history file below):

- **Section 1 (Database Layer):** complete (1a, 1b, 1c-3); GAP-42, GAP-43, GAP-44 open. `Wiki/GDD/Phase_1_Section_1_Database_Layer.md`.
- **Section 2 (Campaign State Machine):** 2a and 2b complete; 2c and 2d deferred until after Sections 3–5. `Wiki/GDD/Phase_1_Section_2_Campaign_State_Machine.md`.
- **Section 3 (Hex Map and Timers):** complete. `Wiki/GDD/Phase_1_Section_3_Hex_Map_and_Timers.md`.
- **Section 4 (Commander Records):** complete (closed 2026-10-16; 4f deferred to 2c/2d). `Wiki/GDD/Phase_1_Section_4_Commander_Records.md`.
- **Section 5 (Multiplayer Sync):** §5a complete (5a-0 → 5a-5); frozen, no §5b until 2026-11-18. `Wiki/GDD/Phase_1_Section_5_Multiplayer_Sync.md`.
- **Section 6 (Closed Alpha):** not started; runs after Sections 7 and 8. `Wiki/GDD/Phase_1_Section_6_Closed_Alpha.md`.
- **Section 7 (Personnel Entity Model):** complete (2026-10-18). `Wiki/GDD/Phase_1_Section_7_Personnel_Entity_Model.md`.
- **Section 8 (Combat Engine to canon):** not started. `Wiki/GDD/Phase_1_Section_8_Combat_Engine.md`.

Infrastructure tracks, all complete; detail in `Wiki/GDD/Phase_1_Alpha_Foundation_Breakdown.md` (Current Status) and `Wiki/GDD/Gap_Tracker_Closed_Archive.md`:

- **INF-1** (GAP-37): complete.
- **INF-2** (GAP-38): complete.
- **INF-3a** (GAP-46): complete.
- **INF-3b** (GAP-39, GAP-40, GAP-41): complete.
- **INF-4** (GAP-32, GAP-33, GAP-52, ODQ-20): complete.
- **INF-5** (boot migrator): complete.
- **INF-7** (GAP-69): complete.
- **INF-6a** (GAP-54, GAP-55, GAP-56): complete, deployed 2026-10-22.
- **INF-6b** (GAP-76): complete, deployed 2026-10-22.
- **INF-6c-1** (client IP, rate limits): complete, deployed 2026-10-22.
- **INF-6c-2a** (GAP-77): complete, deployed 2026-10-23.
- **INF-6c-2b-1** (GAP-79, GAP-80): complete, deployed 2026-10-23.
- **INF-6c-2b-2** (ODQ-48): complete, deployed 2026-10-23.
- **INF-6d** (GAP-81, GAP-82): complete, deployed 2026-10-25; production `dbbcd84`.

Standing rules kept from the history (verbatim; no other home):

- Read the full scope of the next brick and its carried constraints in the Phase 1 document before writing a line.
- **Join switch:** A test that joins turns it on itself (the lib `joined_state()`; E1 via `Arc::make_mut(&mut state.config)`); `pg_route_state` leaves it off.
- The journal-capture pattern for a test is `routes/mod.rs`'s `Buf` + `tracing::subscriber::with_default`.
- **`RateWarn` and `SweepThrottle`:** Test them on injected `Instant`s taken **after** `new()` (a `now` before `base` saturates to 0).
- **Sessions:** Sessions inserted directly by fixtures are unindexed: they count toward the backstop only.
- **Challenges:** A test that needs a small cap swaps the `pub` field on `AppState::new(…)`'s value before `Arc::new` (no pre-fill).
- **Session quota and caps:** The quota-of-10 rationale assumes the default session and challenge durations; the caps stay consts.

Per-brick history of the `dev` build moved verbatim to docs/CLAUDE_STATUS_HISTORY.md (ST-0b, 2026-10-28). Read it only when a question needs the history.

---

## 2. NON-NEGOTIABLE ARCHITECTURE (THE DETERMINISM RULE)

This is a Web3 game where the input log acts as a financial audit trail. The server must
be perfectly deterministic. When writing any backend game logic, obey these rules
without exception:

* **Authoritative Server / Thin Client:** The server is the only source of truth. The
  client renders output and sends input — nothing more. Do not implement client
  prediction.

* **Single Source of Randomness:** All game randomness must come from
  `sim_engine::rng::Rng` (Mulberry32). Never use `rand::random()`, `rand::thread_rng()`,
  or platform RNGs inside the simulation. The approved RNG is seeded per session; it is
  the only source of randomness in the sim.

* **No Wall-Clock Time in Simulation:** Simulation logic must never call `Utc::now()` or
  rely on real-time durations inside `Step()`. Derive "time" strictly from the tick
  count. Wall-clock time drives *when events are injected* (the timer service); `Step()`
  itself must remain a pure function.

* **Append-Only Input Logs — the test is RECOMPUTABILITY.** Log what a replay cannot
  reconstruct; log nothing it can. Player inputs and timer expiries are logged.
  **Derived server decisions are not** — a `BattleReport`, a rank-up or an XP award is
  recomputable from the seed plus the logged inputs, and logging it creates a second
  source of truth. *(Canon 2026-09-02; supersedes the former "and server decisions"
  wording, which licensed exactly what this forbids.)* Do NOT log full game states.
  The log must be immutable and strictly ordered. Each entry includes its tick index and
  sequence number; the build version is recorded once per session, in `session_configs`.
  The `input_logs` table
  has DB-level triggers rejecting UPDATE and DELETE — this is load-bearing, not a
  convention.

* **Brick-by-Brick Build Discipline:** Phase 1 tasks are split at the brick level with
  explicit definitions-of-done. Do not begin a new brick until the previous brick's
  integration tests are green. Each brick is a discrete, independently verifiable unit.

---

## 3. ARCHITECTURAL INTEGRITY & ROOT-CAUSE ANALYSIS

Do not act as a passive code-generator. Your primary directive is to protect the
long-term health of the codebase.

Before writing any code or executing an instruction, analyze the task for structural
mismatches, database schema violations, or hidden technical debt (e.g., stuffing a
Campaign ID into a Session ID column, working around a missing foreign key rather than
adding it).

If you detect a hack, a workaround, or a conflict between the specification and the
existing code architecture:

1. **STOP IMMEDIATELY.** Do not attempt to write a clever workaround.
2. **Raise an `### ARCHITECTURAL ALARM`.** Use that exact header so it is visible.
3. **Explain the Root Cause:** Why does the current code design make this instruction
   messy or require contortion?
4. **Detail the Risks:** What will break downstream — data integrity violations, replay
   divergence, audit trail corruption, future query logic breaking on unexpected nulls?
5. **Propose the Correct Fix:** Present the proper structural solution (schema migration,
   decoupling, parent-child table adjustment) alongside any short-term alternatives if
   a migration is genuinely blocked.

Structural correctness takes priority over speed. If an instruction requires a hack,
surface the root design problem instead of implementing the hack.

**Audit mode:** When the Director asks you to audit existing code rather than build, the
same protocol applies — but you write zero code. Read the specified files, identify any
hacks, workarounds, schema violations, or structural mismatches, and report findings
using the `### ARCHITECTURAL ALARM` header for each one. Do not attempt to fix anything.
List findings only. The Director will rule on each one before any remediation work
begins.

---

## 4. NETWORKING AND DATA DEFAULTS

* **Protocols:** HTTP/HTTPS + JSON for all meta-actions (login, inventory, contracts).
  WebSockets for the Live Tactical Dashboard tick feed, real-time combat logs, and
  deployment timer push notifications. No UDP. Every socket caps inbound frames (combat
  4 KiB, convoy events 1 KiB, INF-6d), and every server-to-client channel is bounded.

* **Relational DB (Postgres + sqlx):** Accounts, Commander records, campaign instances,
  transaction ledger, input logs. All schema changes are sqlx migrations — never
  hand-edited tables. Run `cargo sqlx migrate run` after every new migration before
  `cargo build`. Use compile-time `sqlx::query!` macros throughout. Migration rules
  (GAP-57; a deploy can roll back to the previous binary after the new one migrated):
  1. **Migrations are additive (expand/contract).** The previous release's binary must
     run against the new schema: no drop or rename in the release that stops using a
     column. **An expand migration:** every new column is nullable or has a default, and
     there is no drop, rename or type change. Since `boot_migrator()`, this is the only thing
     keeping a rollback correct: the previous binary boots against any newer schema, and
     a `NOT NULL` column without a default boots fine, then fails every old-binary insert.
     A column may be dropped only when the rollback target (`.prev`, one release back)
     names it nowhere, including its `.sqlx/` texts. The drop is its own migration.
  2. **Never edit an applied migration.** Its checksum is verified at boot
     (`VersionMismatch`).
  3. **No `-- no-transaction` migrations.** A failed one leaves a dirty row that
     blocks every boot.
  4. **The boot migrator ignores missing versions** (`mercs_server::boot_migrator()`), so a
     rolled-back binary boots past a migration it doesn't embed. Test migrators stay strict.
  5. **A new constraint must accept every value the previous binary can write, unless that
     write has no production caller; the migration's header names that writer.** Never
     `NOT VALID`: a constraint validates existing rows and fails the migration loudly.

  Every repository trait returns `RepositoryError` (`NotFound`/`Internal`), never
  `sqlx::Error`; map with an explicit `map_err`, never a `From` impl.

  **Accounts and sign-in (INF-6c-2b-1):** sign-in verifies strictly and `decode_wallet`
  refuses weak and off-curve keys (GAP-80). The join creates the `player_accounts` row, and
  sign-in writes nothing to Postgres (GAP-79).

  Commander roster strings live in `sim_engine::roster`, mirrored by `0018`'s CHECKs; it
  also holds the Foreman pools and `CommanderGrade`, mirrored by `0019`'s
  `commander_records_commander_grade_canon`. `commander_grade` is persisted, never
  recomputed from traits (Story Event characters are hand-graded). Write `CommanderGrade`
  variants fully qualified (`QualityGrade::Elite` shares the name).

  `acquired_via` (`0030`) is written by the `insert_*` helpers' `via` parameter, never
  stored on the record structs. At most one starter per `(campaign, wallet)` by the
  `<t>_one_starter` index; exactly one by the join transaction. `Replacement` (Commanders
  only) has one writer, `claim_replacement` (5a-4): at most one usable (neither KIA nor
  Shattered) replacement per `(campaign, wallet)` by `commander_records_one_live_replacement`
  (re-cut by `0032`, the backstop); the claim's membership row lock is the guard. That row
  lock serialises claims from any number of server processes, so the index is reachable only
  through an out-of-band write or a future second writer of `'replacement'`. A death card
  copies the Commander's `acquired_via` (`card_mint_pending.acquired_via`, `0032`; Postgres
  only).

  **`JOIN_ADVISORY_KEY` has two takers:** `join_campaign` and `begin_launch` (5a-3a). The
  replacement claim (5a-4) takes only its member's membership row (`FOR UPDATE`, deliberate:
  it blocks every roster insert's `FOR KEY SHARE` FK check), never the key. Any new
  writer that moves a campaign out of `Pending`, or writes a membership, takes it
  (`lock_joins`) as the first statement of the same transaction. `update_sector_state` is
  unconditional by id, so it never touches a `Pending` campaign without the key. Future
  key-takers: a stale-`Pending` reaper and an admin "cancel campaign". A failed launch is
  retired to `Archived` (the `admin.rs` statement), never reset to `Pending`: a reset reopens
  the campaign to joins.

* **Key-Value Store (Redis):** Live Sector hex state, deployment timers (global sorted
  set + per-sector sorted set for cancel path), active session state. **Persisted Redis
  types (`DeploymentTimer`, `TimerType`, `SectorState`) change additively only:** new
  fields `#[serde(default)]` or `Option`, variants added and never renamed or removed. A
  field may be removed only if it was already `#[serde(default)]`. Redis payloads outlive
  deploys: a breaking change quarantines every in-flight timer (GAP-53). **Timer write
  order:** payload `SET` before index `ZADD`; index `ZREM` before payload `DEL`; never
  `DEL` a payload whose member may still be due. `schedule_timer`, `cancel_timer` and
  `quarantine_timer` (which keeps the payload) already follow it.

* **Input Log (Postgres, append-only):** Treat as a financial instrument. The
  `gcn_transaction_ledger` and `input_logs` tables have immutability triggers that
  reject UPDATE and DELETE at the DB level. Do not work around them. **Types deserialised
  from a logged payload change additively only.** Variants are added, never renamed or
  removed. A new field is `Option` or `#[serde(default)]`. A field may be removed only if
  nothing reads it and the type does not deny unknown fields: old payloads still
  deserialise, and replay is unchanged. **The rollback binary (`.prev`) must also
  deserialise what this release logs:** a removed field must already be `Option` or
  `#[serde(default)]` in `.prev`, so a removal takes two releases (make it optional, then
  remove it); and a new variant must not be logged until the release after `.prev` knows
  it, unless the brick states that no production caller logs it yet; the brick that adds
  the first production caller then checks that `.prev` knows the variant. Deserialising is
  the guarantee; replay equality across a version bump is not. Current examples: `CombatResolveRequest` and
  everything inside it, including the sim `Commander`.

* **Generator seeds are provenance, never secrets (Reviewer BP-2b-2 F3, 2026-10-26):** a
  record's `prng_seed_state` is a `u32` and the generators are public, so the seed can be
  recovered offline from what the record shows. **No later roll (mission, stress, Fatal
  Flaw trigger, loot) is ever seeded from a stored or displayed generator seed:** each roll
  takes a fresh seed when it happens (`generate_seed()`), as the mission route does.

* **Story content never enters this repo (ODQ-60, 2026-10-27):** the engine is public and the
  story is private. For any Story Event not marked `public: true` in the Wiki's
  `Story_Events_Catalogue`, its prose, choice text, consequences, flag writes, chain edges
  and trigger conditions (and any Victory Condition source keyed to one event) never appear
  here: not in code, comments, test names, fixtures, docs, this file or **commit messages**.
  Real content lives in the private `mercs_and_mines_content` repo. Refer to events by ID
  only (`EVT-019`); tests use invented `EVT-TEST-*` events. The client receives only the
  current card's prose and its visible choice labels. Every Story Event brick greps code and
  `git log` for its events' titles and one distinctive phrase each, and expects 0. World lore
  (faction and setting descriptions, `app/lore.html`) is public by design and not covered.

---

## 5. WSL EXECUTION ENVIRONMENT

The Bash tool dispatches to Windows (Git Bash / MSYS2), not WSL. Always run cargo with the
Linux toolchain inside WSL, where the repo, `.env`, Postgres and Redis live.

**PRIMARY pattern — inline -c (use this first):**

    wsl bash -c "source /home/ajone/.cargo/env && cd /home/ajone/PROJECTS/mercs_and_mines && set -a && source .env && set +a && cargo build 2>&1 | tail -30 && cargo test --workspace -- --test-threads=1 2>&1"

Use the explicit path `/home/ajone/.cargo/env`, NOT `$HOME/.cargo/env` — PowerShell
expands `$HOME` before WSL sees it, resolving to a Windows path that bash cannot source.

Use `set -a; source .env; set +a` so child processes (including `cargo test`) inherit
`TEST_DATABASE_URL` and `TEST_REDIS_URL`. Plain `source .env` sets shell-local variables
only — tests will silently skip instead of running.

**FALLBACK pattern — script file (if inline -c fails):**

1. Use the Write tool to create a temporary `.sh` script at a Windows-accessible path:
   `C:\Users\ajone\PROJECTS\Mercs_and_Mines\run_brick.sh`
2. Invoke via Bash tool: `wsl -e bash /mnt/c/Users/ajone/PROJECTS/Mercs_and_Mines/run_brick.sh`
3. Delete the script after.

Script contents:
```bash
#!/bin/bash
source /home/ajone/.cargo/env
cd /home/ajone/PROJECTS/mercs_and_mines
set -a; source .env; set +a
cargo build 2>&1 | tail -30
cargo test --workspace -- --test-threads=1 2>&1
```

Note: Git Bash may intercept `/mnt/c/` paths when the script-file pattern is used —
if so, fall back to the inline form above.

**Canonical test command:** Always use `cargo test --workspace -- --test-threads=1`.
Never use bare `cargo test` or `cargo test --workspace` without the thread constraint.
Several integration tests share wallet addresses and have `ON DELETE RESTRICT` FK
constraints — parallel execution causes FK-23503 failures that are not caused by the
code under test. Any test report produced without `--test-threads=1` is unreliable and
must be re-run before reporting results to the Director.

**Test wallets:** `tests/` uses wallet seeds 0–10, 13–19, 21–76 and 88; 11 and 12 (a range
literal until 5a-3a) stay reserved. **20 and 77 are the auth tests' signing-key seeds**
(`SigningKey::from_bytes(&[20u8; 32])`; 77 since INF-6c-2a). They are used in memory only,
and their derived public keys are not `[n; 32]`. The next free seed is **78**. A wallet that
goes through `challenge` must be a non-weak curve point (since INF-6c-2b-1): `[9u8; 32]` is
one; `[0u8; 32]` is weak, and 126 of the 256 `[n; 32]` patterns (`[2u8; 32]` the smallest)
are off-curve.

**Gate-only wiring:** the auth sweep task's spawn in `main.rs` has no test (`main` is not
tested); `grep -c 'tokio::spawn(supervise(' crates/server/src/main.rs` = 3 guards it.

**One in-memory join per `AppState`:** `AppState::new` builds each join-shared store once and
one `InMemoryCampaignJoinRepository` over the same `Arc`s. A test that replaces a join-read
field (`campaign_repo`, `membership_repo`, `commander_repo`, `section_repo`) replaces
`campaign_join_repo` alongside, or the join and the launch keep reading the old stores.
A Postgres route test builds its state with `pg_route_state` (`db_infrastructure.rs`), which
swaps the four join-read repositories and `campaign_join_repo` together.

**Race tests:** Race tests force interleaving with a pause seam after the read, never a held
lock alone: a held lock cannot make a remove-the-lock negative fail. Wait until B is parked
at its own seam or shows `granted = false` in `pg_locks`; use a dedicated pool (≥ 3
connections); and require every racer's result to be `Ok` where a unique constraint would
otherwise hide the race. An in-transaction pause seam is an injected hook on the Postgres
impl (`None` in production, `#[doc(hidden)] pub` constructor), because integration tests
never see `cfg(test)`. Precedent: `PostgresCampaignJoinRepository::with_pause_seam` (5a-2b);
its one `PauseHook` fires in three methods (`join_campaign`, `begin_launch`,
`claim_replacement`). A racer waiting on a **row lock** never shows as the advisory key in
`pg_locks`: detect it in `pg_stat_activity` (`wait_event_type = 'Lock'` plus the statement
text), as `wait_until_b_waits_on_the_membership_row` does (5a-4).

**Independent oracle:** A gate's expected value is never read from the code it checks. Pin
it to a literal taken from canon or from a measurement made before the change (4d-2 N3: a
frequency test that derived its target from `DEFAULT_GRADE_WEIGHTS` passed under the
mutation).

**Negative-run validity:** A negative proves something only if its run is valid.
1. The `test result` line must exist. A rejected flag or a compile failure prints no such
   line, and a missing line is a void run, never "zero failures".
2. Passed + failed must equal the count taken from the prompt's own table, never a count
   read from a fresh run of the code under test. Any other sum voids the run; report it,
   don't reconcile it.
3. A schema-mutation negative captures every object it will touch first, and aborts before
   mutating if the capture fails or doesn't match. After restoring, it diffs against an
   independently captured reference (the dev DB, which no negative touches), not only
   against its own capture: two equally incomplete snapshots diff empty (5a-1 §6).

Cargo flags such as `--no-fail-fast` go **before** `--`; after it they are passed to the
test binary, which rejects them.

**Deploy harness:** `bash deploy/test_remote_deploy.sh` runs the real deploy script
against shims (13 scenarios, about 20 s, no root or network needed). Run it after any
change under `deploy/`. Knobs: `FAIL_RESTART=<n>` fails the n-th restart; `SUDO_DENY=1`
refuses every privileged call, the probe included. A new branch in the script gets a
scenario. The `deploy-script` CI job also lints both scripts with `shellcheck`, which is
also installed in WSL (0.9.0): run it before committing any `deploy/` change.

**Dependency audit:** run `cargo audit` locally (cargo-audit 0.22.2 is installed in WSL)
after any `Cargo.lock` change, since the `audit` job gates deploys (`audit.yml` also audits
`main` weekly). A new ignore in
`.cargo/audit.toml` carries the ID, a reason and a re-check date. A yanked crate cannot be
ignored: update it out of the lockfile instead.

If a system package is missing (redis-server, libssl-dev, etc.), do not attempt
`apt install`. Surface it to the Director with a single copy-pasteable sudo command.

Every handoff prompt that includes `cargo build` or `cargo test` steps MUST use this
pattern.

---

## 6. FRONTEND (`app/`)

HTML5 / Vanilla JS / CSS only. No build step, no TypeScript, no npm. `@solana/web3.js`
loaded via CDN. Axum serves `app/` as static files via `tower-http::ServeDir`. The Rust
mandate does not apply to `app/`.

---

## 7. REFERENCE DOCUMENTS

Before implementing any mechanic, read the relevant spec. Do not guess at behaviour.
Always use absolute paths when referencing these documents in handoff prompts.

| Topic                                                   | Document                                                                                                                   |
| ------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| **Open gaps, tracked defects, and their owning bricks** | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Design_Implementation_Gap_Tracker.md` |
| **Unruled design questions - check before building**    | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Open_Design_Questions.md` |
| **Phase 1 build plan, current status, all brick specs** | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Phase_1_Alpha_Foundation_Breakdown.md`              |
| Implementation language, approved libraries             | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Tech_Stack_and_Language_Mandate.md`                 |
| Determinism, tick loop, input log schema                | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Technical_Architecture_Deterministic_Simulation.md` |
| Module domains, networking, data defaults               | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Development_Philosophy_and_MVP_Scope.md`            |
| AP/AT combat resolution and tick math                   | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Combat_Math_Resolution.md`                          |
| Hex map, travel times, terrain modifiers, collision     | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\Mechanics\Hex_Map_and_Travel.md`                        |
| Campaign lifecycle, Sector states, 90-day structure     | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Campaign_Structure.md`                              |
| Sector spawning, player caps, gateway hexes             | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Instanced_Sectors_and_Spawning.md`                  |
| Victory Conditions and Ticker definitions               | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Victory_Conditions.md`                              |
| Commander generation, XP, stress, permadeath            | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\Mechanics\Commander_and_Advisor_System.md`              |
| Commander stress thresholds and fatal flaws             | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\Mechanics\Commander_Stress_System.md`                   |
| He3 economy, Cryo-Vats, resource rules                  | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\World\Helium-3.md`                                      |
| The Trust, Standing system, barter rules                | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\Entities\The_Trust.md`                                  |
| Solana integration, $GCN, cNFT pipeline                 | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\Web3_Integration.md`                                |
| Terrain types and environmental modifiers               | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\World\Terrain_and_Environmental_Hazards.md`             |
| Map structure: radius, DMZ, Safe Zone, FOBs             | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\World_Map_Design.md`                                |
| $GCN tokenomics and Prize Pool distribution             | `C:\Users\ajone\PROJECTS\Mercs_and_Mines\Mercs_and_Mines_WIKI\Wiki\GDD\GCN_Tokenomics_and_Revenue.md`                      |