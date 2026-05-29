CREATE TYPE sector_state AS ENUM (
    'Pending',
    'Active',
    'Ending',
    'Ended',
    'Archived'
);

CREATE TABLE campaign_instances (
    campaign_id      UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    sector_id        UUID         NOT NULL,
    map_seed         BIGINT       NOT NULL,
    state            sector_state NOT NULL DEFAULT 'Pending',
    -- Ten victory tickers: { "TICKER_NAME": float_value }, values 0.0–100.0.
    -- Initialized to {}; apply_ticker_delta populates entries on first write.
    victory_tickers  JSONB        NOT NULL DEFAULT '{}',
    -- Nullable: set when Sector transitions Pending → Active
    started_at       TIMESTAMPTZ,
    -- Nullable: set at start as start + 90 days; may be overridden by early ticker completion
    ends_at          TIMESTAMPTZ,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT now()
);
