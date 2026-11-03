-- Launch AR-086: store-enforced exact-duplicate key for concurrent fact additions.
-- `dedup_key` is `dedup::exact_dedup_key`: SHA-256 over a rule tag and the fields of the inserting
-- ingestion mode's exact-duplicate rule (the default path: memory type and normalized content,
-- consolidation v2: normalized content, subject and event time). Keys of different rules never
-- collide. It is written only by ingestion ADDs of active facts. Imports and updates (including
-- supersession and invalidation) leave it NULL, and rows that existed before this migration keep
-- NULL, so existing duplicates never violate the index and no backfill is needed. The partial
-- unique index lets at most one active fact (not deleted, superseded or invalid) per tenant hold
-- a key, and a delete frees it. Additive and nullable: existing rows read back unchanged.
ALTER TABLE facts ADD COLUMN IF NOT EXISTS dedup_key TEXT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uq_facts_tenant_dedup_key
    ON facts (org_id, user_id, dedup_key)
    WHERE dedup_key IS NOT NULL AND deleted_at IS NULL AND superseded_at IS NULL
        AND invalid_at IS NULL;
