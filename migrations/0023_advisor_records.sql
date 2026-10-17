-- Brick 7a-2: Advisor records, with roster CHECKs from day one.
-- The roster strings mirror sim_engine::roster (CommanderOrigin, FactionBias,
-- AdvisorSpecialization, AdvisorQuirk); the two-way drift guard compares
-- pg_get_constraintdef with the roster per table.
-- Expand-only (CLAUDE.md §4 rules 1 and 5): a new table. Neither rollback candidate
-- (719c8ee prod, cc272b1 .prev) names it, so a hand restore boots against it. The NOT NULL
-- columns without defaults are safe because no older binary writes this table. Its only
-- writer, create_advisor, has no production caller yet.
-- Advisors are ungraded and have no Stress or flaw track (ODQ-39): no grade, stress, flaw,
-- XP, seat or is_starter column. rank is Skill Level, inert and not rolled. No updated_at
-- (no writer updates an Advisor yet). No rank CHECK (mirrors commander_records). The Board
-- is derived from specialization, never stored; specialization holds all 14 roles so a
-- hand-placed Research Advisor can be stored later.
CREATE TABLE advisor_records (
    advisor_id      UUID        PRIMARY KEY,
    campaign_id     UUID        NOT NULL REFERENCES campaign_instances(campaign_id) ON DELETE CASCADE,
    player_wallet   BYTEA       NOT NULL REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT,
    name            TEXT        NOT NULL,
    rank            SMALLINT    NOT NULL DEFAULT 1,
    origin          TEXT        NOT NULL,
    faction_bias    TEXT        NOT NULL,
    specialization  TEXT        NOT NULL,
    quirk           TEXT        NOT NULL,
    prng_seed_state BIGINT      NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT advisor_records_origin_canon
      CHECK (origin IN ('Human (Corporate)', 'Human (Underhive)', 'Duck', 'Bearocrat', 'Science Ape')),
    CONSTRAINT advisor_records_faction_bias_canon
      CHECK (faction_bias IN ('G.U.N. Loyalist', 'Capitalist Partisan', 'Union Agitator',
        'Squeaking Prophet Sympathiser', 'Void-Caller (Liberationist)', 'Void-Caller (Abolitionist)',
        'Void-Caller (Collaborationist)', 'Jump Lane Restorer', 'Cyberkin Initiate',
        'Emperor Bob Devotee (Way of the Road)', 'Emperor Bob Devotee (Crowned Fist)', 'Trust Asset')),
    CONSTRAINT advisor_records_specialization_canon
      CHECK (specialization IN ('Intelligence Broker (Spymaster)', 'Operations Officer (COO)',
        'The Financial Patron', 'CFO (Mercenary Focus)', 'CFO (Mining Focus)',
        'VP of Tectonic Risk & Planetology', 'Director of Heavy Engineering',
        'Director of Asset Protection (Corp-Sec)', 'VP of Labor & Personnel (Anti-Union)',
        'VP of Labor & Personnel (Pro-Union)', 'Director of Overland Logistics',
        'Chief Science Officer (CSO)', 'Head of Reverse Engineering', 'Director of Applied Innovation')),
    CONSTRAINT advisor_records_quirk_canon
      CHECK (quirk IN ('The Restructurer', 'Nepotist', 'Micromanager', 'Golden Parachute',
        'Leaks to the Press', 'Creative Accountant', 'The Consultant', 'Empire Builder',
        'Conference Circuit', 'Rubber Stamp', 'Headhunted', 'Vanity Metrics'))
);

CREATE INDEX idx_advisor_records_campaign ON advisor_records(campaign_id);
CREATE INDEX idx_advisor_records_player   ON advisor_records(player_wallet);

COMMENT ON COLUMN advisor_records.prng_seed_state IS 'Provenance only: the seed the traits were rolled from. Not a reproduction key; per-seed generator output is not a contract (4a-2).';
