CREATE TABLE IF NOT EXISTS facts (
    id UUID PRIMARY KEY,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,

    memory_type TEXT NOT NULL,
    content TEXT NOT NULL,
    summary TEXT NULL,

    source TEXT NOT NULL,
    confidence DOUBLE PRECISION NOT NULL,
    importance DOUBLE PRECISION NOT NULL,

    valid_at TIMESTAMPTZ NULL,
    invalid_at TIMESTAMPTZ NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,

    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    deleted_at TIMESTAMPTZ NULL
);

CREATE INDEX IF NOT EXISTS idx_facts_tenant_user
ON facts (org_id, user_id);

CREATE INDEX IF NOT EXISTS idx_facts_type
ON facts (org_id, user_id, memory_type);

CREATE INDEX IF NOT EXISTS idx_facts_valid_time
ON facts (org_id, user_id, valid_at, invalid_at);

CREATE INDEX IF NOT EXISTS idx_facts_metadata
ON facts USING GIN (metadata);

CREATE INDEX IF NOT EXISTS idx_facts_not_deleted
ON facts (org_id, user_id)
WHERE deleted_at IS NULL;
