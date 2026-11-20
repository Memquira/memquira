-- Phase 1 Part B: who a fact is about, when it happened, and which messages it came from.
-- Every column is nullable and additive, so rows written before this migration read back unchanged.
-- `observed_at` is also the anchor the event time was resolved against.
ALTER TABLE facts ADD COLUMN subject TEXT;
ALTER TABLE facts ADD COLUMN event_time_start TEXT;
ALTER TABLE facts ADD COLUMN event_time_end TEXT;
ALTER TABLE facts ADD COLUMN event_time_granularity TEXT;
ALTER TABLE facts ADD COLUMN event_time_text TEXT;
ALTER TABLE facts ADD COLUMN observed_at TEXT;
ALTER TABLE facts ADD COLUMN session_id TEXT;
-- JSON array of client message IDs.
ALTER TABLE facts ADD COLUMN source_message_ids TEXT;

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_subject
    ON facts (org_id, user_id, subject);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_event_time_start
    ON facts (org_id, user_id, event_time_start);
