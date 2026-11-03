-- Memcore V2.1 closure: receipts of add requests that succeeded, keyed by a fingerprint of the
-- request's stable identity (client message ids and content, session and metadata), so a retry of
-- the same request after a lost response returns the recorded result instead of extracting again.
-- `result` is JSON with counts and fact ids only (no fact text). Written in the same transaction as
-- the journal commit, purged after a bounded time by the recovery sweep, erased by forget-user.
-- Times are Unix milliseconds. Additive: no existing table changes.
CREATE TABLE IF NOT EXISTS ingest_receipts (
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    operation_id UUID NULL,
    committed_at_ms BIGINT NOT NULL,
    result TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id, request_key)
);

CREATE INDEX IF NOT EXISTS idx_ingest_receipts_committed
ON ingest_receipts (committed_at_ms);
