CREATE TABLE section_records (
    section_id      UUID        PRIMARY KEY,
    campaign_id     UUID        NOT NULL REFERENCES campaign_instances(campaign_id) ON DELETE CASCADE,
    commander_id    UUID        REFERENCES commander_records(commander_id) ON DELETE SET NULL,
    name            TEXT        NOT NULL,
    headcount       SMALLINT    NOT NULL DEFAULT 8,   -- max 8 for a Rank 1 Section
    loadout         JSONB       NOT NULL DEFAULT '{}',
    xp              INTEGER     NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_section_records_campaign   ON section_records(campaign_id);
CREATE INDEX idx_section_records_commander  ON section_records(commander_id);
