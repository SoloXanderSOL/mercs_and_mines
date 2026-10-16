-- Brick 4a-2: contract step for the retired commander_records.is_nft (Commanders are never
-- NFTs, Soulbound_NFT_Policy). Brick B (cb9312d) stopped using it and kept the column by
-- ruling (expand/contract); this release drops it, as its own migration.
-- Legal under CLAUDE.md §4 rule 1: the rollback target is the running release, 8e74c74,
-- which names is_nft nowhere outside migrations/ (code and .sqlx/ texts:
-- `git grep is_nft 8e74c74 -- . ':!migrations'` = 0). The deploy keeps exactly one
-- previous binary (deploy/remote_deploy.sh: one PREV path, `rm -f` then `cp -a`), so
-- 8e74c74 is the only binary that can boot against this schema after a rollback.
ALTER TABLE commander_records DROP COLUMN is_nft;
