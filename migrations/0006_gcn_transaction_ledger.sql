CREATE TABLE gcn_transaction_ledger (
    entry_id        UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    wallet_address  BYTEA       NOT NULL REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT,
    delta           BIGINT      NOT NULL,
    balance_after   BIGINT      NOT NULL,
    entry_type      TEXT        NOT NULL,
    session_id      UUID,
    memo            TEXT,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX gcn_ledger_wallet_idx ON gcn_transaction_ledger (wallet_address, recorded_at);

-- Immutability trigger: this is a financial record. No UPDATE or DELETE, ever.
CREATE OR REPLACE FUNCTION gcn_ledger_immutable()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'gcn_transaction_ledger is append-only: % is not permitted', TG_OP;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER gcn_ledger_no_update
    BEFORE UPDATE ON gcn_transaction_ledger
    FOR EACH ROW EXECUTE FUNCTION gcn_ledger_immutable();

CREATE TRIGGER gcn_ledger_no_delete
    BEFORE DELETE ON gcn_transaction_ledger
    FOR EACH ROW EXECUTE FUNCTION gcn_ledger_immutable();
