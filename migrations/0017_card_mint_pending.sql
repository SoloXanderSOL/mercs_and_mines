-- Brick 4e-1 (§4e, resolution R2): the death-card outbox. Account-scoped snapshot:
-- only the wallet has an FK, so the row survives the §4f campaign wipe. No consumer yet (Phase 2 §6d).
CREATE TABLE card_mint_pending (
    card_mint_id    UUID        PRIMARY KEY,
    player_wallet   BYTEA       NOT NULL REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT,
    campaign_id     UUID        NOT NULL,          -- no FK: outlives the campaign wipe
    commander_id    UUID        NOT NULL UNIQUE,   -- no FK; a Commander dies once
    deployment_id   UUID        NOT NULL UNIQUE,   -- no FK; the fatal mission
    session_id      UUID        NOT NULL,          -- the mission's input log: the chronicle's source
    commander_name  TEXT        NOT NULL,
    rank            SMALLINT    NOT NULL,
    foil            BOOLEAN     NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_card_mint_pending_wallet ON card_mint_pending(player_wallet);
