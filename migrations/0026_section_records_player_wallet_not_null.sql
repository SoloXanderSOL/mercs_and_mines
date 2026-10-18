-- Brick 7b-2: the contract step for 0025's owner column (Reviewer 7b-1 CF3).
-- Contract step; precondition .prev = f7a1ff3, whose only Section writer (create_section)
-- refuses None and has no production caller; production null owners = 0 (Director,
-- 2026-10-17). No NOT VALID and no backfill: SET NOT NULL validates every existing row and
-- fails this migration loudly on a null.
ALTER TABLE section_records ALTER COLUMN player_wallet SET NOT NULL;
