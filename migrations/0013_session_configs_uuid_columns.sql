-- session_configs.campaign_id and sector_id were TEXT; campaign_instances.campaign_id
-- and input_logs.campaign_id are both UUID. Align the types.
-- TRUNCATE bypasses the row-level immutability triggers (pre-production data only).

TRUNCATE session_configs;

ALTER TABLE session_configs
    ALTER COLUMN campaign_id TYPE UUID USING campaign_id::uuid;

ALTER TABLE session_configs
    ALTER COLUMN sector_id TYPE UUID USING sector_id::uuid;
