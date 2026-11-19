CREATE TABLE IF NOT EXISTS provider_usage_events (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NULL,
    provider_name TEXT NOT NULL,
    model_name TEXT NULL,
    capability TEXT NOT NULL,
    operation_name TEXT NOT NULL,
    status TEXT NOT NULL,
    input_tokens INTEGER NULL,
    output_tokens INTEGER NULL,
    total_tokens INTEGER NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    fallback_used INTEGER NOT NULL DEFAULT 0,
    circuit_blocked INTEGER NOT NULL DEFAULT 0,
    timed_out INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd REAL NULL,
    metadata TEXT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_created
    ON provider_usage_events (org_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_user
    ON provider_usage_events (org_id, user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_provider
    ON provider_usage_events (org_id, provider_name, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_capability
    ON provider_usage_events (org_id, capability, created_at DESC);
