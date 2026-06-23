CREATE TABLE convoy_records (
    convoy_id       UUID         PRIMARY KEY,
    sector_id       UUID         NOT NULL,
    owner_wallet    BYTEA        NOT NULL,
    origin_q        INT          NOT NULL,
    origin_r        INT          NOT NULL,
    destination_q   INT          NOT NULL,
    destination_r   INT          NOT NULL,
    route           JSONB        NOT NULL,
    arrival_time    TIMESTAMPTZ  NOT NULL,
    vehicle_class   TEXT         NOT NULL,
    cargo           JSONB        NOT NULL DEFAULT '{}',
    in_transit      BOOL         NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now()
);
