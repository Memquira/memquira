CREATE TABLE IF NOT EXISTS api_keys (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    name TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    scopes TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT NULL
);

CREATE INDEX IF NOT EXISTS idx_api_keys_org
ON api_keys (org_id);

CREATE INDEX IF NOT EXISTS idx_api_keys_hash
ON api_keys (key_hash);

CREATE INDEX IF NOT EXISTS idx_api_keys_active
ON api_keys (org_id, revoked_at);
