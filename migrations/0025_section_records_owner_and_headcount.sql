-- Brick 7b-1: a Section's owner column (GAP-63) and its headcount CHECK.
-- Expand-only (CLAUDE.md §4 rules 1 and 5): a nullable column, an index and a CHECK; no
-- drop, rename or type change.
-- The writers are create_section and update_headcount, which have no production caller at
-- this release or at .prev (2a4fcb3), and production holds 0 section_records rows (Director,
-- 2026-10-17), so the CHECK accepts every value the previous binary can write.
-- player_wallet is nullable because .prev's INSERT omits it; a later contract migration sets
-- NOT NULL once no binary writes a null. It is backfilled from the Section's Commander; a
-- Section with no Commander stays null. Owner columns match 0022–0024 (BYTEA, RESTRICT).
-- Neither constraint is _canon, so the roster drift guard does not see them.
ALTER TABLE section_records
    ADD COLUMN player_wallet BYTEA REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT;

UPDATE section_records s
   SET player_wallet = c.player_wallet
  FROM commander_records c
 WHERE c.commander_id = s.commander_id;

CREATE INDEX idx_section_records_player ON section_records(player_wallet);

ALTER TABLE section_records
    ADD CONSTRAINT section_records_headcount_range CHECK (headcount BETWEEN 0 AND 8);
