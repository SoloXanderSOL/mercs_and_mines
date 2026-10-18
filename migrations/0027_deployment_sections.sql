-- Brick 7b-2: per-mission Sections (GAP-50, GAP-66).
-- Expand step; each UNIQUE is a superset of its table's PK, so it cannot fail on existing
-- rows or reject a .prev write; .prev (f7a1ff3) never names deployment_sections, and its
-- deployment closes release Sections through the trigger.
-- deployment_sections records which Sections a deployment sends. It carries all three
-- ownership facts, every column NOT NULL, so the three composite FKs hold the deployment,
-- its Commander and its Sections to one owner and one campaign. A Section is in at most
-- one active deployment (deployment_sections_one_active). The trigger releases a
-- deployment's Sections in the same transaction as whatever closes it.
ALTER TABLE deployment_records ADD CONSTRAINT deployment_records_id_commander_key UNIQUE (deployment_id, commander_id);
ALTER TABLE commander_records  ADD CONSTRAINT commander_records_owner_key UNIQUE (commander_id, player_wallet, campaign_id);
ALTER TABLE section_records    ADD CONSTRAINT section_records_owner_key   UNIQUE (section_id, player_wallet, campaign_id);

CREATE TABLE deployment_sections (
    deployment_id UUID        NOT NULL,
    section_id    UUID        NOT NULL,
    commander_id  UUID        NOT NULL,
    player_wallet BYTEA       NOT NULL,
    campaign_id   UUID        NOT NULL,
    released_at   TIMESTAMPTZ,
    PRIMARY KEY (deployment_id, section_id),
    CONSTRAINT deployment_sections_deployment_fkey FOREIGN KEY (deployment_id, commander_id)
        REFERENCES deployment_records (deployment_id, commander_id) ON DELETE RESTRICT,
    CONSTRAINT deployment_sections_commander_fkey FOREIGN KEY (commander_id, player_wallet, campaign_id)
        REFERENCES commander_records (commander_id, player_wallet, campaign_id) ON DELETE RESTRICT,
    CONSTRAINT deployment_sections_section_fkey FOREIGN KEY (section_id, player_wallet, campaign_id)
        REFERENCES section_records (section_id, player_wallet, campaign_id) ON DELETE RESTRICT
);
CREATE UNIQUE INDEX deployment_sections_one_active ON deployment_sections (section_id) WHERE released_at IS NULL;

CREATE FUNCTION release_deployment_sections() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE deployment_sections SET released_at = now()
     WHERE deployment_id = NEW.deployment_id AND released_at IS NULL;
    RETURN NULL;
END $$;
CREATE TRIGGER deployment_records_release_sections
    AFTER UPDATE OF status ON deployment_records
    FOR EACH ROW WHEN (OLD.status = 'active' AND NEW.status <> 'active')
    EXECUTE FUNCTION release_deployment_sections();
