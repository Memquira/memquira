-- Phase 2 full-text search over facts (design section 2.1).
-- An external-content FTS5 index over content, subject and the time expression, ranked with bm25().
-- Triggers keep it in step with every insert, update and hard delete of a fact. Soft-deleted facts
-- stay indexed and are filtered out by the query, exactly like every other fact read.
-- The final statement backfills rows written before this migration.
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    content,
    subject,
    event_time_text,
    content = 'facts',
    content_rowid = 'rowid',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS facts_fts_after_insert AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts (rowid, content, subject, event_time_text)
    VALUES (new.rowid, new.content, new.subject, new.event_time_text);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_after_delete AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts (facts_fts, rowid, content, subject, event_time_text)
    VALUES ('delete', old.rowid, old.content, old.subject, old.event_time_text);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_after_update AFTER UPDATE OF content, subject, event_time_text ON facts BEGIN
    INSERT INTO facts_fts (facts_fts, rowid, content, subject, event_time_text)
    VALUES ('delete', old.rowid, old.content, old.subject, old.event_time_text);
    INSERT INTO facts_fts (rowid, content, subject, event_time_text)
    VALUES (new.rowid, new.content, new.subject, new.event_time_text);
END;

INSERT INTO facts_fts (facts_fts) VALUES ('rebuild');
