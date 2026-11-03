ALTER TABLE background_job_runs
ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 1;

ALTER TABLE background_job_runs
ADD COLUMN max_attempts INTEGER NOT NULL DEFAULT 1;

ALTER TABLE background_job_runs
ADD COLUMN retried BOOLEAN NOT NULL DEFAULT FALSE;
