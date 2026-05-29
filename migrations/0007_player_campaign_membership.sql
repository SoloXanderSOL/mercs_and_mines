CREATE TABLE player_campaign_membership (
    membership_id       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    wallet_address      BYTEA NOT NULL
                            REFERENCES player_accounts(wallet_address)
                            ON DELETE RESTRICT,
    campaign_id         UUID NOT NULL
                            REFERENCES campaign_instances(campaign_id)
                            ON DELETE CASCADE,
    spawn_hex_q         INT,
    spawn_hex_r         INT,
    joined_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    is_active           BOOLEAN NOT NULL DEFAULT TRUE,
    CONSTRAINT uq_player_campaign UNIQUE (wallet_address, campaign_id)
);

CREATE INDEX idx_membership_campaign ON player_campaign_membership (campaign_id);
CREATE INDEX idx_membership_wallet   ON player_campaign_membership (wallet_address);
