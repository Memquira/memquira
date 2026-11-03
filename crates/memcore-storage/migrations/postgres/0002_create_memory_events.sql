CREATE TABLE IF NOT EXISTS memory_events (
    id UUID PRIMARY KEY,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    fact_id UUID NULL,

    operation TEXT NOT NULL,

    input_text TEXT NULL,
    previous_content TEXT NULL,
    new_content TEXT NULL,

    provider_name TEXT NULL,
    model_name TEXT NULL,

    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_events_tenant
ON memory_events (org_id, user_id);

CREATE INDEX IF NOT EXISTS idx_memory_events_fact
ON memory_events (org_id, user_id, fact_id);

CREATE INDEX IF NOT EXISTS idx_memory_events_operation
ON memory_events (org_id, user_id, operation);

CREATE INDEX IF NOT EXISTS idx_memory_events_created_at
ON memory_events (org_id, user_id, created_at);
