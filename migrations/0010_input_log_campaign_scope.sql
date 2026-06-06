ALTER TABLE input_logs
    ALTER COLUMN session_id DROP NOT NULL;

ALTER TABLE input_logs
    ADD COLUMN campaign_id UUID;

ALTER TABLE input_logs
    ADD CONSTRAINT input_logs_has_scope
    CHECK (session_id IS NOT NULL OR campaign_id IS NOT NULL);

CREATE INDEX ON input_logs (campaign_id, tick, seq)
    WHERE campaign_id IS NOT NULL;
