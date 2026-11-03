-- Phase 2 full-text search over facts (design section 2.1).
-- A generated, weighted tsvector (subject A, content B, time expression C) with a GIN index, ranked
-- with ts_rank_cd. Being generated, it is filled for existing rows when the column is added and kept
-- current by Postgres itself, so no trigger or separate backfill is needed.
ALTER TABLE facts ADD COLUMN IF NOT EXISTS fts tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english'::regconfig, coalesce(subject, '')), 'A')
    || setweight(to_tsvector('english'::regconfig, coalesce(content, '')), 'B')
    || setweight(to_tsvector('english'::regconfig, coalesce(event_time_text, '')), 'C')
) STORED;

CREATE INDEX IF NOT EXISTS idx_facts_fts ON facts USING GIN (fts);
