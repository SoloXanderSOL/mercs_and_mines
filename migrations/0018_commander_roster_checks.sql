-- Brick B: pin commander_records.origin and faction_bias to the canonical Commander roster.
-- The strings mirror sim_engine::roster (CommanderOrigin, FactionBias); a drift-guard test
-- writes every roster value through these constraints.
-- Expand-only by exemption: the previous binary's only writer of these columns,
-- create_commander, has no production caller (test callers only), so no value it can
-- write reaches production. Plain ADD CONSTRAINT validates existing rows.
-- is_nft is kept by ruling (expand/contract, 2026-10-15); a later migration contracts it.
ALTER TABLE commander_records ADD CONSTRAINT commander_records_origin_canon
  CHECK (origin IN ('Human (Corporate)', 'Human (Underhive)', 'Duck', 'Bearocrat', 'Science Ape'));
ALTER TABLE commander_records ADD CONSTRAINT commander_records_faction_bias_canon
  CHECK (faction_bias IN ('G.U.N. Loyalist', 'Capitalist Partisan', 'Union Agitator',
    'Squeaking Prophet Sympathiser', 'Void-Caller (Liberationist)', 'Void-Caller (Abolitionist)',
    'Void-Caller (Collaborationist)', 'Jump Lane Restorer', 'Cyberkin Initiate',
    'Emperor Bob Devotee (Way of the Road)', 'Emperor Bob Devotee (Crowned Fist)', 'Trust Asset'));
