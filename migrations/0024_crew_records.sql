-- Brick 7c-1: Mining Crew records, with Crew Morale persistence.
-- Expand-only (CLAUDE.md §4 rules 1 and 5): a new table. Neither rollback candidate
-- (f054caa prod, 719c8ee .prev) names it, so a hand restore boots against it. Its only
-- writer, create_crew, has no production caller yet.
-- A crew is the Mining equivalent of a Section: 8 heads, and a wipe is headcount = 0, with
-- no flag. Rank is derived from xp and never stored. Morale 60 is the ODQ-40 starting
-- value; the morale bands, events and strikes are Phase 2 §13.
-- No foreman_id: Foremen lead operations, not crews (ODQ-40 (b), amended 2026-10-17; the
-- per-operation link is GAP-66). No name, crew type, roster column or updated_at (no writer).
-- Neither constraint is _canon, so the roster drift guard does not see them.
CREATE TABLE crew_records (
    crew_id         UUID        PRIMARY KEY,
    campaign_id     UUID        NOT NULL REFERENCES campaign_instances(campaign_id) ON DELETE CASCADE,
    player_wallet   BYTEA       NOT NULL REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT,
    headcount       SMALLINT    NOT NULL DEFAULT 8,
    xp              INTEGER     NOT NULL DEFAULT 0,
    morale          SMALLINT    NOT NULL DEFAULT 60,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT crew_records_headcount_range CHECK (headcount BETWEEN 0 AND 8),
    CONSTRAINT crew_records_morale_range    CHECK (morale BETWEEN 0 AND 100)
);

CREATE INDEX idx_crew_records_campaign ON crew_records(campaign_id);
CREATE INDEX idx_crew_records_player   ON crew_records(player_wallet);
