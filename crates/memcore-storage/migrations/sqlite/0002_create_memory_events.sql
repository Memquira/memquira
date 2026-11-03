CREATE TABLE IF NOT EXISTS memory_events (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    fact_id TEXT NULL,
    operation TEXT NOT NULL,
    input_text TEXT NULL,
    previous_content TEXT NULL,
    new_content TEXT NULL,
    provider_name TEXT NULL,
    model_name TEXT NULL,
    metadata TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_events_tenant
    ON memory_events (org_id, user_id);

CREATE INDEX IF NOT EXISTS idx_memory_events_fact
    ON memory_events (org_id, user_id, fact_id);

CREATE INDEX IF NOT EXISTS idx_memory_events_operation
    ON memory_events (org_id, user_id, operation);

CREATE INDEX IF NOT EXISTS idx_memory_events_created_at
    ON memory_events (org_id, user_id, created_at);
