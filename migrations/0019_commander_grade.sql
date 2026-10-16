-- Brick 4a-2: Commander Grade (Commander_Grade_System.md §2-§3).
-- commander_grade holds the spawn Grade. It is persisted and never recomputed from the
-- traits: Story Event characters are hand-graded with any trait combination (§6). TQS
-- itself is never stored. The CHECK mirrors sim_engine::roster::CommanderGrade.
-- The death card carries the Grade too (§4c): card_mint_pending.commander_grade is copied
-- from the checked column, so it needs no CHECK of its own.
-- Expand-only (CLAUDE.md §4 rules 1 and 5): both columns are nullable, with nothing
-- dropped, renamed or retyped. The previous binary (8e74c74) names neither column, so it
-- writes NULL, and NULL passes the CHECK. The column is nullable (not NOT NULL) for that
-- rollback reason only; every Field Leader written by this binary carries a Grade.
-- The column comment corrects 0004's "required for deterministic replay" (ruling CF4):
-- per-seed generator output is not a contract.
ALTER TABLE commander_records ADD COLUMN commander_grade TEXT NULL
  CONSTRAINT commander_records_commander_grade_canon
  CHECK (commander_grade IN ('Standard Issue', 'Notable', 'Distinguished', 'Elite', 'Heroic'));

ALTER TABLE card_mint_pending ADD COLUMN commander_grade TEXT NULL;

COMMENT ON COLUMN commander_records.prng_seed_state IS 'Provenance only: the seed the traits were rolled from. Not a reproduction key; per-seed generator output is not a contract (4a-2).';
