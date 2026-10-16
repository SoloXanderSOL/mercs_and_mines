-- Brick 7a-1: Mining Foreman records, with roster CHECKs from day one.
-- The roster strings mirror sim_engine::roster (CommanderOrigin, FactionBias,
-- ForemanSpecialization, ForemanFlaw, CommanderGrade); a two-way drift guard compares
-- pg_get_constraintdef with the roster per table.
-- Expand-only (CLAUDE.md §4 rules 1 and 5): a new table. Neither rollback candidate
-- (0d93283, cc272b1) names it, so a hand restore boots against it. foreman_grade is NOT
-- NULL because no older binary writes this table. Its only writer, create_foreman, has no
-- production caller yet (§5a's join adds one). No is_starter (§5a), no veteran_trait
-- (Foremen get Field Citations, Phase 2), no rank CHECK (mirrors commander_records).
CREATE TABLE foreman_records (
    foreman_id      UUID        PRIMARY KEY,
    campaign_id     UUID        NOT NULL REFERENCES campaign_instances(campaign_id) ON DELETE CASCADE,
    player_wallet   BYTEA       NOT NULL REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT,
    name            TEXT        NOT NULL,
    rank            SMALLINT    NOT NULL DEFAULT 1,
    xp              INTEGER     NOT NULL DEFAULT 0,
    stress          SMALLINT    NOT NULL DEFAULT 0,
    origin          TEXT        NOT NULL,
    faction_bias    TEXT        NOT NULL,
    specialization  TEXT        NOT NULL,
    fatal_flaw      TEXT        NOT NULL,
    foreman_grade   TEXT        NOT NULL,
    is_shattered    BOOLEAN     NOT NULL DEFAULT FALSE,
    is_kia          BOOLEAN     NOT NULL DEFAULT FALSE,
    prng_seed_state BIGINT      NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT foreman_records_stress_range CHECK (stress BETWEEN 0 AND 100),
    CONSTRAINT foreman_records_origin_canon
      CHECK (origin IN ('Human (Corporate)', 'Human (Underhive)', 'Duck', 'Bearocrat', 'Science Ape')),
    CONSTRAINT foreman_records_faction_bias_canon
      CHECK (faction_bias IN ('G.U.N. Loyalist', 'Capitalist Partisan', 'Union Agitator',
        'Squeaking Prophet Sympathiser', 'Void-Caller (Liberationist)', 'Void-Caller (Abolitionist)',
        'Void-Caller (Collaborationist)', 'Jump Lane Restorer', 'Cyberkin Initiate',
        'Emperor Bob Devotee (Way of the Road)', 'Emperor Bob Devotee (Crowned Fist)', 'Trust Asset')),
    CONSTRAINT foreman_records_specialization_canon
      CHECK (specialization IN ('Deep-Driller', 'Shoring-Expert', 'Labor Relations',
        'Tectonic Whisperer', 'Black Market Auger', 'Convoy Hardener', 'Ore Grader',
        'Salvage Operator', 'Coolant-Jockey', 'Rush-Driller', 'Vein-Reader', 'Magma Tapper',
        'Crew Psychologist')),
    CONSTRAINT foreman_records_fatal_flaw_canon
      CHECK (fatal_flaw IN ('Superstitious', 'Doomsayer', 'Cult of Bob', 'Squeak-Touched',
        'Waste-Not', 'Cave-Shy', 'Prospector''s Itch', 'Deep-Ear', 'High-Grader', 'Seam-Fever')),
    CONSTRAINT foreman_records_foreman_grade_canon
      CHECK (foreman_grade IN ('Standard Issue', 'Notable', 'Distinguished', 'Elite', 'Heroic'))
);

CREATE INDEX idx_foreman_records_campaign ON foreman_records(campaign_id);
CREATE INDEX idx_foreman_records_player   ON foreman_records(player_wallet);

COMMENT ON COLUMN foreman_records.prng_seed_state IS 'Provenance only: the seed the traits were rolled from. Not a reproduction key; per-seed generator output is not a contract (4a-2).';
