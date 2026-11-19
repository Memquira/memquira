CREATE TABLE IF NOT EXISTS background_job_runs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NULL,
    duration_ms BIGINT NULL,
    error_code TEXT NULL,
    error_message TEXT NULL,
    metadata TEXT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_background_job_runs_started_at
ON background_job_runs(started_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_background_job_runs_kind_started_at
ON background_job_runs(kind, started_at DESC, id DESC);
