-- 0031 — the alpha replacement's backstop (Phase 1 §5a, brick 5a-4; ODQ-33; Reviewer 5a-1 CF5).
--
-- Adds commander_records_one_live_replacement: at most one living replacement Commander per
-- (campaign, wallet). Living means NOT is_kia; a Shattered replacement is still living.
-- resolve_deployment's KIA write (mark_commander_kia) takes a dead replacement out of the
-- index, so the member's next replacement is accepted. The guard is the claim's membership
-- row lock (FOR UPDATE); this index is the backstop, reachable only across two server
-- processes or by an out-of-band write, and the claim answers its 23505 with a 500.
--
-- Expand-only (code CLAUDE.md §4 rule 1): one index on existing columns; no column, drop,
-- rename or type change. A plain CREATE UNIQUE INDEX, not CONCURRENTLY, so it runs inside the
-- migration's transaction (rule 3: no -- no-transaction).
--
-- Rule 5: no released binary writes acquired_via = 'replacement'.
--   git grep -nw 'AcquiredVia::Replacement' <sha> -- crates/server/src
-- prints nothing at ce8096e (production) or at 145338e and b3711f8 (rollback candidates).
--
-- Backfill: 0 rows with acquired_via = 'replacement' in dev and test (2026-10-20, at ce8096e),
-- and none can exist in production, which has no writer for the value. The build validates
-- every existing row and fails the migration loudly on a duplicate.

CREATE UNIQUE INDEX commander_records_one_live_replacement ON commander_records (campaign_id, player_wallet)
    WHERE acquired_via = 'replacement' AND NOT is_kia;
