-- 0032 — usable replacements and death-card provenance (Phase 1 §5a, brick 5a-5; ODQ-46, ODQ-47;
-- Reviewer 5a-4 F1, F2, F3).
--
-- 1. commander_records_one_live_replacement is re-cut under the same name. "Live" now means
--    usable: neither KIA nor Shattered (ODQ-46). A Shattered replacement no longer occupies the
--    index, so the member's next replacement is accepted. Keeping the name keeps the tests and
--    code CLAUDE.md valid and owes no later contract migration.
--    The guard is the claim's membership row lock (FOR UPDATE). It is a Postgres row lock, so it
--    serialises claims from any number of server processes. This index is reachable only through
--    an out-of-band write or a future second writer of 'replacement'. (This corrects 0031's
--    header, which is not edited.)
--
-- 2. card_mint_pending.acquired_via is copied from the dead Commander's row by
--    mark_commander_kia (ODQ-47), so a later consumer can tell a free replacement's card apart.
--    0019 left card_mint_pending.commander_grade without a CHECK because it is copied from a
--    checked column. This column departs from that on purpose: the CHECK carries a guard row
--    (roster_check_expectations) that ties it to AcquiredVia::ALL, so a new variant can't drift
--    between the two tables. There is no OR ... IS NULL: a CHECK passes when its expression is NULL.
--
-- Expand-only (code CLAUDE.md §4 rule 1): the new column is nullable with no default; the index
-- drop removes no column or data, and no code names the index (only tests). No NOT VALID, no
-- CONCURRENTLY, no -- no-transaction (rule 3). DROP INDEX holds ACCESS EXCLUSIVE until this
-- migration's transaction commits, so the re-cut leaves no window.
--
-- Rule 5, against .prev = 4eb1b1b (5a-4):
--   - it names no card acquired_via, so it writes NULL, which the CHECK passes;
--   - its claim inserts only when no Commander is non-KIA, so no row of the member's is in the
--     narrower index when it writes;
--   - its other Commander writers only turn KIA or Shattered on, which removes index rows;
--   - the new predicate selects a subset of 0031's rows, so it refuses only what 0031 refused.
--
-- Backfill: 0 card_mint_pending rows and 0 rows with acquired_via = 'replacement' in production
-- (Director, 2026-10-22), dev and test (spec check, 2026-10-22).

DROP INDEX commander_records_one_live_replacement;
CREATE UNIQUE INDEX commander_records_one_live_replacement ON commander_records (campaign_id, player_wallet)
    WHERE acquired_via = 'replacement' AND NOT is_kia AND NOT is_shattered;
ALTER TABLE card_mint_pending ADD COLUMN acquired_via TEXT;
ALTER TABLE card_mint_pending ADD CONSTRAINT card_mint_pending_acquired_via_canon
    CHECK (acquired_via IN ('starter', 'replacement', 'hired'));
