-- Brick 4d-2 (CF3): pin commander_records.fatal_flaw and specialization to the canonical
-- Commander roster. The strings mirror sim_engine::roster (CommanderFlaw,
-- CommanderSpecialization); a two-way drift guard compares pg_get_constraintdef with the
-- roster, and a write test sends every roster value through these constraints.
-- Expand-only (CLAUDE.md §4 rules 1 and 5): two CHECKs, no column added, dropped, renamed
-- or retyped. Every value the previous binary (0d93283) can write passes: its generator
-- rolls 3 canonical flaws and the 12 canonical specializations. Its only writer of these
-- columns, create_commander, has no production caller (test callers only), so no other
-- value reaches production. The test database held 0 commander rows at the 4d-2 spec
-- check. Plain ADD CONSTRAINT validates existing rows.
ALTER TABLE commander_records ADD CONSTRAINT commander_records_fatal_flaw_canon
  CHECK (fatal_flaw IN ('Embezzler', 'Bloodthirsty', 'Cowardly', 'Paranoid', 'Cult of Bob',
    'The Informant', 'Glory Hound', 'The Gambler', 'Chrome-Chaser', 'Martinet',
    'Squeak-Touched', 'Doomsayer'));
ALTER TABLE commander_records ADD CONSTRAINT commander_records_specialization_canon
  CHECK (specialization IN ('Trench-Sweeper', 'Armor-Adept', 'Night-Stalker', 'Cannoneer',
    'Marksman''s Eye', 'Breacher', 'Quartermaster''s Ghost', 'Corpseman''s Touch',
    'Suppression Doctrine', 'Veteran''s Instinct', 'Anti-Armor Adept', 'Battlefield Engineer'));
