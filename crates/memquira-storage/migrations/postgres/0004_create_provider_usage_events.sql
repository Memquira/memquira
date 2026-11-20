CREATE TABLE IF NOT EXISTS provider_usage_events (
    id UUID PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NULL,
    provider_name TEXT NOT NULL,
    model_name TEXT NULL,
    capability TEXT NOT NULL,
    operation_name TEXT NOT NULL,
    status TEXT NOT NULL,
    input_tokens BIGINT NULL,
    output_tokens BIGINT NULL,
    total_tokens BIGINT NULL,
    retry_count BIGINT NOT NULL DEFAULT 0,
    fallback_used BOOLEAN NOT NULL DEFAULT FALSE,
    circuit_blocked BOOLEAN NOT NULL DEFAULT FALSE,
    timed_out BOOLEAN NOT NULL DEFAULT FALSE,
    estimated_cost_usd DOUBLE PRECISION NULL,
    metadata JSONB NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_created
    ON provider_usage_events (org_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_user
    ON provider_usage_events (org_id, user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_provider
    ON provider_usage_events (org_id, provider_name, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_provider_usage_events_org_capability
    ON provider_usage_events (org_id, capability, created_at DESC);
