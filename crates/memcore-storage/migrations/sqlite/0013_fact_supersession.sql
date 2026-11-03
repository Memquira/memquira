-- Phase 3 (owner decision D124): explicit supersession state for consolidation.
-- `superseded_at` is memcore's bookkeeping time for marking a fact no longer active because it was
-- superseded or invalidated, and it is NOT a real-world event time. `superseded_by` is the replacement
-- fact, when one was stored (provenance only, never the activity marker, and no foreign key, so
-- lifecycle and import ordering stay safe). A fact is active only when `deleted_at`, `invalid_at`
-- and `superseded_at` are all NULL. Both columns are nullable and additive: existing rows read back
-- unchanged, and no backfill is needed.
ALTER TABLE facts ADD COLUMN superseded_at TEXT;
ALTER TABLE facts ADD COLUMN superseded_by TEXT;

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_superseded_at
    ON facts (org_id, user_id, superseded_at);
