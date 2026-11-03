CREATE TABLE IF NOT EXISTS memory_usage_snapshots (
    id UUID PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    total_users BIGINT NOT NULL,
    total_memories BIGINT NOT NULL,
    active_memories BIGINT NOT NULL,
    deleted_memories BIGINT NULL,
    captured_at TIMESTAMPTZ NOT NULL,
    metadata JSONB NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_usage_snapshots_org_captured
    ON memory_usage_snapshots (org_id, captured_at DESC, id DESC);
