-- Consistency fix: make the ON DELETE RESTRICT explicit on commander_records.player_wallet.
-- player_campaign_membership.wallet_address already uses explicit ON DELETE RESTRICT on
-- its parallel FK to player_accounts. Postgres NO ACTION (the previous implicit default)
-- is functionally identical for non-deferred constraints, so this is documentation-only.

ALTER TABLE commander_records
    DROP CONSTRAINT commander_records_player_wallet_fkey,
    ADD CONSTRAINT commander_records_player_wallet_fkey
        FOREIGN KEY (player_wallet)
        REFERENCES player_accounts(wallet_address)
        ON DELETE RESTRICT;
