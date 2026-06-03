CREATE TABLE input_logs (
    log_id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id      UUID        NOT NULL,
    tick            BIGINT      NOT NULL,
    seq             BIGINT      NOT NULL,
    event_type      TEXT        NOT NULL,
    player_id       TEXT,
    payload         JSONB,
    narrative_event TEXT,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ON input_logs (session_id, tick, seq);

CREATE OR REPLACE FUNCTION input_logs_immutable()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'input_logs rows are immutable';
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER no_update_input_logs
    BEFORE UPDATE ON input_logs
    FOR EACH ROW EXECUTE FUNCTION input_logs_immutable();

CREATE TRIGGER no_delete_input_logs
    BEFORE DELETE ON input_logs
    FOR EACH ROW EXECUTE FUNCTION input_logs_immutable();
