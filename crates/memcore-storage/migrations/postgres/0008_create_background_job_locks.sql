CREATE TABLE IF NOT EXISTS background_job_locks (
    kind TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    locked_until TIMESTAMPTZ NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL,
    heartbeat_at TIMESTAMPTZ NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
