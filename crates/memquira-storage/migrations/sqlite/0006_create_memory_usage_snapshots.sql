CREATE TABLE IF NOT EXISTS memory_usage_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    total_users INTEGER NOT NULL,
    total_memories INTEGER NOT NULL,
    active_memories INTEGER NOT NULL,
    deleted_memories INTEGER NULL,
    captured_at TEXT NOT NULL,
    metadata TEXT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_usage_snapshots_org_captured
    ON memory_usage_snapshots (org_id, captured_at DESC, id DESC);
