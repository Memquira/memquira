-- Phase 1 Part B: who a fact is about, when it happened, and which messages it came from.
-- Every column is nullable and additive, so rows written before this migration read back unchanged.
-- `observed_at` is also the anchor the event time was resolved against.
ALTER TABLE facts ADD COLUMN IF NOT EXISTS subject TEXT NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS event_time_start TIMESTAMPTZ NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS event_time_end TIMESTAMPTZ NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS event_time_granularity TEXT NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS event_time_text TEXT NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS observed_at TIMESTAMPTZ NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS session_id TEXT NULL;
ALTER TABLE facts ADD COLUMN IF NOT EXISTS source_message_ids JSONB NULL;

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_subject
    ON facts (org_id, user_id, subject);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_event_time_start
    ON facts (org_id, user_id, event_time_start);
