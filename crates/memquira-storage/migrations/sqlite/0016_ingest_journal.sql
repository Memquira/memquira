-- Memcore V2.1: the write-ahead journal of in-flight ingestion requests (memcore_core::ports::
-- ingest_journal). An add request records each fact operation here before applying it and deletes
-- its rows when it commits (just before it answers success), so the tables only ever hold requests
-- that are in flight or being rolled back (after an error, a cancellation or a crash). `payload` is
-- the step as JSON: an update or supersession keeps the target's before-image (fact text), and
-- forget-user deletes the user's rows. `fact_id` / `related_fact_id` are the fact ids the step
-- writes, for the in-flight lookup. Times are Unix milliseconds (exact ordering for the stale
-- sweep). Additive: no existing table changes.
CREATE TABLE IF NOT EXISTS ingest_operations (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('in_progress', 'rolling_back')),
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ingest_operations_updated
ON ingest_operations (updated_at_ms);

CREATE INDEX IF NOT EXISTS idx_ingest_operations_tenant
ON ingest_operations (org_id, user_id);

CREATE TABLE IF NOT EXISTS ingest_operation_steps (
    operation_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    fact_id TEXT NOT NULL,
    related_fact_id TEXT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (operation_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_ingest_operation_steps_fact
ON ingest_operation_steps (fact_id);

CREATE INDEX IF NOT EXISTS idx_ingest_operation_steps_related_fact
ON ingest_operation_steps (related_fact_id);
