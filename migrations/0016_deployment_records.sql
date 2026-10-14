-- Brick 4g-1: one active deployment per Commander, enforced by the database
-- (Phase_1_Section_4_Commander_Records.md §4g items 5 and 10).
-- An aborted deployment never resolved, so it has no outcome: outcome is set exactly
-- when status = 'resolved', and closed_at exactly when status <> 'active'.
CREATE TABLE deployment_records (
    deployment_id UUID PRIMARY KEY,
    commander_id  UUID NOT NULL REFERENCES commander_records(commander_id) ON DELETE RESTRICT,
    session_id    UUID NOT NULL,   -- no FK: an aborted deployment's log header may not exist
    mission_id    TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','resolved','aborted')),
    outcome       TEXT CHECK (outcome IS NULL OR outcome IN ('Success','Defeat','TacticalRetreat','Wipeout')),
    started_at    TIMESTAMPTZ NOT NULL,
    closed_at     TIMESTAMPTZ,
    CHECK ((status = 'active')   = (closed_at IS NULL)),
    CHECK ((status = 'resolved') = (outcome IS NOT NULL))
);
CREATE UNIQUE INDEX deployment_records_one_active ON deployment_records (commander_id) WHERE status = 'active';

-- Validates every existing row: a stored stress outside 0..=100 aborts this migration.
ALTER TABLE commander_records ADD CONSTRAINT commander_records_stress_range CHECK (stress BETWEEN 0 AND 100);
