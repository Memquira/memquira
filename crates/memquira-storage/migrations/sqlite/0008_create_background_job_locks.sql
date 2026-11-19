CREATE TABLE IF NOT EXISTS background_job_locks (
    kind TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    locked_until TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    heartbeat_at TEXT NULL,
    updated_at TEXT NOT NULL
);
