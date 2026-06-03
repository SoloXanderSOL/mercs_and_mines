CREATE TABLE session_configs (
    session_id     UUID        PRIMARY KEY,
    seed           BIGINT      NOT NULL,
    build_version  TEXT        NOT NULL,
    sector_id      TEXT        NOT NULL,
    campaign_id    TEXT        NOT NULL,
    sector_tier    TEXT        NOT NULL,
    ruleset        TEXT        NOT NULL,
    recorded_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
