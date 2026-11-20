CREATE TABLE IF NOT EXISTS org_plan_configs (
    org_id TEXT PRIMARY KEY NOT NULL,
    tier TEXT NOT NULL,
    max_users_per_org INTEGER NULL,
    max_memories_per_user INTEGER NULL,
    max_memories_per_org INTEGER NULL,
    daily_provider_request_limit INTEGER NULL,
    daily_provider_token_limit INTEGER NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    metadata TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
