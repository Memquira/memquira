CREATE TABLE IF NOT EXISTS background_job_runs (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ NULL,
    duration_ms BIGINT NULL,
    error_code TEXT NULL,
    error_message TEXT NULL,
    metadata JSONB NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_background_job_runs_started_at
ON background_job_runs(started_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_background_job_runs_kind_started_at
ON background_job_runs(kind, started_at DESC, id DESC);
