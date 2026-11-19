CREATE TABLE IF NOT EXISTS org_plan_configs (
    org_id TEXT PRIMARY KEY NOT NULL,
    tier TEXT NOT NULL,
    max_users_per_org BIGINT NULL,
    max_memories_per_user BIGINT NULL,
    max_memories_per_org BIGINT NULL,
    daily_provider_request_limit BIGINT NULL,
    daily_provider_token_limit BIGINT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    metadata JSONB NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
