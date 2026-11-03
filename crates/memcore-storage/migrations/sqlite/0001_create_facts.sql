CREATE TABLE IF NOT EXISTS facts (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    memory_type TEXT NOT NULL,
    content TEXT NOT NULL,
    summary TEXT,
    source TEXT NOT NULL,
    confidence REAL NOT NULL,
    importance REAL NOT NULL,
    valid_at TEXT,
    invalid_at TEXT,
    recorded_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    metadata TEXT NOT NULL,
    deleted_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user
    ON facts (org_id, user_id);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_type
    ON facts (org_id, user_id, memory_type);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_deleted
    ON facts (org_id, user_id, deleted_at);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user_valid_at
    ON facts (org_id, user_id, valid_at);
