-- 0030 — the join schema (Phase 1 §5a, brick 5a-1; ruling S2).
--
-- Adds:
--   * acquired_via on the four starter-bearing roster tables (commander, foreman,
--     section, crew): the provenance of a row. 'starter' (issued free at join),
--     'replacement' (Commanders only: the alpha KIA replacement, ODQ-33) or 'hired'
--     (everything else, so it is the default). Written by the server's insert helpers,
--     mirrored by repository::AcquiredVia; never carried on the record structs.
--   * <t>_acquired_via_canon CHECKs, guarded by roster_checks_match_the_roster_both_ways.
--   * <t>_one_starter partial unique indexes: at most one starter per (campaign, wallet),
--     which with uq_player_campaign means exactly one per membership.
--   * <t>_membership_fkey on those four and advisor_records: a roster row's owner must be
--     a member of its campaign. Augmentees are covered through their owner FKs
--     (augmentee_records_section_fkey / _crew_fkey). ON DELETE NO ACTION, checked at end of
--     statement, so a campaign delete that cascades both the membership and the roster
--     rows succeeds, while deleting a membership that still owns roster rows is refused.
--
-- Expand-only (code CLAUDE.md §4 rule 1): one column with a constant default, so the
-- previous binary's INSERTs (which do not name it) store 'hired'; CHECKs that accept
-- 'hired'; FKs and partial indexes that bind only production writers, and there are none.
--
-- Rule 5: no released binary writes any of these five tables in production.
--   git grep -nwE 'create_(commander|foreman|section|crew|advisor)|insert_(commander|foreman|section|crew)' <sha> -- crates/server/src
-- excluding repository/ and the definitions, prints nothing at 9194c0f or at 698efb9.
--
-- Backfill: 0 roster rows in all five tables in dev, test and production (production:
-- Director, 2026-10-18, at 9194c0f; 0 total and 0 non-member rows). No NOT VALID: every
-- constraint validates the existing rows.

ALTER TABLE commander_records ADD COLUMN acquired_via TEXT NOT NULL DEFAULT 'hired';
ALTER TABLE foreman_records   ADD COLUMN acquired_via TEXT NOT NULL DEFAULT 'hired';
ALTER TABLE section_records   ADD COLUMN acquired_via TEXT NOT NULL DEFAULT 'hired';
ALTER TABLE crew_records      ADD COLUMN acquired_via TEXT NOT NULL DEFAULT 'hired';

ALTER TABLE commander_records ADD CONSTRAINT commander_records_acquired_via_canon
    CHECK (acquired_via IN ('starter', 'replacement', 'hired'));
ALTER TABLE foreman_records ADD CONSTRAINT foreman_records_acquired_via_canon
    CHECK (acquired_via IN ('starter', 'hired'));
ALTER TABLE section_records ADD CONSTRAINT section_records_acquired_via_canon
    CHECK (acquired_via IN ('starter', 'hired'));
ALTER TABLE crew_records ADD CONSTRAINT crew_records_acquired_via_canon
    CHECK (acquired_via IN ('starter', 'hired'));

CREATE UNIQUE INDEX commander_records_one_starter ON commander_records (campaign_id, player_wallet)
    WHERE acquired_via = 'starter';
CREATE UNIQUE INDEX foreman_records_one_starter ON foreman_records (campaign_id, player_wallet)
    WHERE acquired_via = 'starter';
CREATE UNIQUE INDEX section_records_one_starter ON section_records (campaign_id, player_wallet)
    WHERE acquired_via = 'starter';
CREATE UNIQUE INDEX crew_records_one_starter ON crew_records (campaign_id, player_wallet)
    WHERE acquired_via = 'starter';

ALTER TABLE commander_records ADD CONSTRAINT commander_records_membership_fkey
    FOREIGN KEY (campaign_id, player_wallet)
    REFERENCES player_campaign_membership (campaign_id, wallet_address) ON DELETE NO ACTION;
ALTER TABLE foreman_records ADD CONSTRAINT foreman_records_membership_fkey
    FOREIGN KEY (campaign_id, player_wallet)
    REFERENCES player_campaign_membership (campaign_id, wallet_address) ON DELETE NO ACTION;
ALTER TABLE section_records ADD CONSTRAINT section_records_membership_fkey
    FOREIGN KEY (campaign_id, player_wallet)
    REFERENCES player_campaign_membership (campaign_id, wallet_address) ON DELETE NO ACTION;
ALTER TABLE crew_records ADD CONSTRAINT crew_records_membership_fkey
    FOREIGN KEY (campaign_id, player_wallet)
    REFERENCES player_campaign_membership (campaign_id, wallet_address) ON DELETE NO ACTION;
ALTER TABLE advisor_records ADD CONSTRAINT advisor_records_membership_fkey
    FOREIGN KEY (campaign_id, player_wallet)
    REFERENCES player_campaign_membership (campaign_id, wallet_address) ON DELETE NO ACTION;
