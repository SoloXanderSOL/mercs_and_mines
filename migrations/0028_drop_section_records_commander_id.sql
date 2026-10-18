-- Brick 7d-1: drop section_records.commander_id (Reviewer 7b-2 CF1).
-- Contract step; precondition .prev = 27f2598, whose code and .sqlx name the column
-- nowhere (Linux statement scan, 2026-10-18: 16 section_records literals and 7 cached
-- statements, none names it; no SELECT *). Commanders lead missions, not Sections (7b-2).
-- The column's only two dependents, section_records_commander_id_fkey and
-- idx_section_records_commander, drop with it. Production held 0 non-null values
-- (Director reading at 27f2598), so the drop discards nothing.
ALTER TABLE section_records DROP COLUMN commander_id;
