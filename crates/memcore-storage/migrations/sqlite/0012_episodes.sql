-- Phase 2 Step 6 (D61-D66): episodes, source-message-level memory. One row per message a client
-- sent while MEMCORE_EPISODES_ENABLED was on. `content` is stored after PII redaction when
-- redaction is on. `observed_at` is the client's message or session time, never the receipt time
-- (P2-20). `recorded_at` is when memcore stored the row. Deletes are hard deletes.
CREATE TABLE IF NOT EXISTS episodes (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT,
    message_id TEXT,
    speaker TEXT,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    observed_at TEXT,
    recorded_at TEXT NOT NULL,
    embedded INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL
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
