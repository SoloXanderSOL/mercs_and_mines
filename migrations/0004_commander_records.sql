CREATE TABLE commander_records (
    commander_id        UUID            PRIMARY KEY,
    campaign_id         UUID            NOT NULL REFERENCES campaign_instances(campaign_id) ON DELETE CASCADE,
    player_wallet       BYTEA           NOT NULL REFERENCES player_accounts(wallet_address),
    name                TEXT            NOT NULL,
    rank                SMALLINT        NOT NULL DEFAULT 1,
    xp                  INTEGER         NOT NULL DEFAULT 0,
    stress              SMALLINT        NOT NULL DEFAULT 0,  -- 0–100 integer percentage points
    origin              TEXT            NOT NULL,
    faction_bias        TEXT            NOT NULL,            -- hidden alignment; stored, never shown to player directly
    specialization      TEXT            NOT NULL,
    fatal_flaw          TEXT            NOT NULL,
    veteran_trait       TEXT,                                -- NULL until rank 3 unlock
    is_shattered        BOOLEAN         NOT NULL DEFAULT FALSE,
    is_kia              BOOLEAN         NOT NULL DEFAULT FALSE,
    is_nft              BOOLEAN         NOT NULL DEFAULT FALSE,
    prng_seed_state     BIGINT          NOT NULL,            -- seed state used for trait roll; required for deterministic replay
    created_at          TIMESTAMPTZ     NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ     NOT NULL DEFAULT now()
);

CREATE INDEX idx_commander_records_campaign ON commander_records(campaign_id);
CREATE INDEX idx_commander_records_player   ON commander_records(player_wallet);
