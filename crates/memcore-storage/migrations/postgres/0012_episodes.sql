-- Phase 2 Step 6 (D61-D66): episodes, source-message-level memory. One row per message a client
-- sent while MEMCORE_EPISODES_ENABLED was on. `content` is stored after PII redaction when
-- redaction is on. `observed_at` is the client's message or session time, never the receipt time
-- (P2-20). `recorded_at` is when memcore stored the row. Deletes are hard deletes.
CREATE TABLE IF NOT EXISTS episodes (
    id UUID PRIMARY KEY,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NULL,
    message_id TEXT NULL,
    speaker TEXT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    observed_at TIMESTAMPTZ NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    embedded BOOLEAN NOT NULL DEFAULT FALSE,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_episodes_tenant_user
ON episodes (org_id, user_id);

CREATE INDEX IF NOT EXISTS idx_episodes_tenant_user_recorded_at
ON episodes (org_id, user_id, recorded_at);

-- Idempotency: a client message ID is stored once per organization, user and session (no session
-- counts as one session). Rows without a message ID are never deduplicated.
CREATE UNIQUE INDEX IF NOT EXISTS idx_episodes_client_message
ON episodes (org_id, user_id, COALESCE(session_id, ''), message_id)
WHERE message_id IS NOT NULL;
