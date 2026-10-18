-- Brick 7d-1: named augmented individuals (Bespoke_Augmentation_System §2–3).
-- Expand-only (CLAUDE.md §4 rules 1 and 5): a new table plus a superkey. Neither rollback
-- candidate (27f2598 prod, f7a1ff3) names either; crew_records_owner_key is a superset of
-- the PK, so every existing row and every old-binary write satisfies it. The writers,
-- create_augmentee and record_fate, have no production caller.
-- One table, exactly one owner (a Section or a Mining Crew). Each owner FK is composite, to
-- the owner's (id, wallet, campaign) superkey, so an augmentee shares its owner's wallet and
-- campaign; under MATCH SIMPLE the FK whose owner column is null is skipped.
-- Rows are never deleted: a dead augmentee's row is the death notice's content (GAP-67), and
-- it goes with the campaign's CASCADE. A Section or Crew wiped to headcount 0 is a state,
-- never a DELETE; both owner FKs RESTRICT (Reviewer 7b-2 CF2).
-- The Chrome Ceiling: at most two slots per owner, each held by a serving (casualty_class
-- NULL) or Wounded augmentee; KIA, MIA and Deserted free the slot. Wounded keep their slot,
-- and no writer returns them to serving yet (GAP-68).
-- Division of labour: combat SKUs fit Sections, industrial SKUs fit Crews; the two
-- *_augment_canon CHECKs mirror sim_engine::roster::Augment split by catalogue().
ALTER TABLE crew_records ADD CONSTRAINT crew_records_owner_key UNIQUE (crew_id, player_wallet, campaign_id);

CREATE TABLE augmentee_records (
    augmentee_id     UUID        PRIMARY KEY,
    campaign_id      UUID        NOT NULL REFERENCES campaign_instances(campaign_id) ON DELETE CASCADE,
    player_wallet    BYTEA       NOT NULL REFERENCES player_accounts(wallet_address) ON DELETE RESTRICT,
    section_id       UUID,
    crew_id          UUID,
    slot             SMALLINT    NOT NULL,
    name             TEXT        NOT NULL,
    callsign         TEXT        NOT NULL,
    augment          TEXT        NOT NULL,
    fitting_source   TEXT        NOT NULL,
    casualty_class   TEXT,
    fate_at          TIMESTAMPTZ,
    prng_seed_state  BIGINT      NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT augmentee_records_one_owner  CHECK (num_nonnulls(section_id, crew_id) = 1),
    CONSTRAINT augmentee_records_slot_range CHECK (slot IN (1, 2)),
    CONSTRAINT augmentee_records_combat_augment_canon CHECK (section_id IS NULL OR augment IN (
        'Integral Gun-Arm', 'Thermal Lance-Arm', 'Coherent-Light Oculars', 'Cortex Intrusion Jack',
        'Myomer-Bundle Arms', 'Arrestor-Coil Legs', 'Doppler Cochlea', 'Gloom-Sight Oculars (Tactical)'
    )),
    CONSTRAINT augmentee_records_industrial_augment_canon CHECK (crew_id IS NULL OR augment IN (
        'Geophone Cochlea', 'Gloom-Sight Oculars (Industrial)', 'Load-Bearer Frame'
    )),
    CONSTRAINT augmentee_records_fitting_source_canon CHECK (fitting_source IN ('Church', 'Secular')),
    CONSTRAINT augmentee_records_casualty_class_canon CHECK (casualty_class IN ('KIA', 'Wounded', 'MIA', 'Deserted')),
    CONSTRAINT augmentee_records_section_fkey FOREIGN KEY (section_id, player_wallet, campaign_id)
        REFERENCES section_records (section_id, player_wallet, campaign_id) ON DELETE RESTRICT,
    CONSTRAINT augmentee_records_crew_fkey FOREIGN KEY (crew_id, player_wallet, campaign_id)
        REFERENCES crew_records (crew_id, player_wallet, campaign_id) ON DELETE RESTRICT
);

-- The Chrome Ceiling: a slot is held by a serving or Wounded augmentee; KIA, MIA and Deserted free it.
CREATE UNIQUE INDEX augmentee_records_section_slot_held ON augmentee_records (section_id, slot)
    WHERE section_id IS NOT NULL AND (casualty_class IS NULL OR casualty_class = 'Wounded');
CREATE UNIQUE INDEX augmentee_records_crew_slot_held ON augmentee_records (crew_id, slot)
    WHERE crew_id IS NOT NULL AND (casualty_class IS NULL OR casualty_class = 'Wounded');
CREATE INDEX idx_augmentee_records_campaign ON augmentee_records (campaign_id);
CREATE INDEX idx_augmentee_records_player   ON augmentee_records (player_wallet);
CREATE INDEX idx_augmentee_records_section  ON augmentee_records (section_id);
CREATE INDEX idx_augmentee_records_crew     ON augmentee_records (crew_id);
