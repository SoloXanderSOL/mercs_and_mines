CREATE TABLE player_accounts (
    wallet_address                BYTEA        PRIMARY KEY,
    gcn_balance                   BIGINT       NOT NULL DEFAULT 0,
    trust_standing                BIGINT       NOT NULL DEFAULT 0,
    founding_courtesy_claimed     BOOLEAN      NOT NULL DEFAULT FALSE,
    lifetime_missions_completed   INTEGER      NOT NULL DEFAULT 0,
    lifetime_campaigns_completed  INTEGER      NOT NULL DEFAULT 0,
    nft_trophy_refs               JSONB        NOT NULL DEFAULT '[]'::jsonb,
    created_at                    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                    TIMESTAMPTZ  NOT NULL DEFAULT now()
);
