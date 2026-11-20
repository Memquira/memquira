-- Phase 3 Step 6 (design 3.3, D104, D126): the entity layer. Written only while
-- MEMCORE_ENTITIES_ENABLED is on (default off, D108). Every row belongs to one organization and
-- user, and entities never resolve across users. No foreign keys, so fact lifecycle and import
-- ordering stay independent. Deletes are hard deletes.
--
-- `aliases` is a JSON array of alias records (name, how it was added, when, and the name
-- similarity for a merge), so a merge stays reversible. `is_speaker` marks a person named as a
-- message speaker (authoritative, never merged with another speaker). `name_embedding` is the JSON
-- embedding of the canonical name used for candidate matching, with the embedding model and
-- dimension that produced it (D132: embeddings are never compared across models or dimensions).
CREATE TABLE IF NOT EXISTS entities (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    canonical_name TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    aliases TEXT NOT NULL,
    summary TEXT,
    is_speaker INTEGER NOT NULL DEFAULT 0,
    name_embedding TEXT,
    name_embedding_model TEXT,
    name_embedding_dimensions INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entities_tenant_user_created_at
    ON entities (org_id, user_id, created_at);

-- A fact's link to an entity, with the role the entity plays in it and the name the fact used.
CREATE TABLE IF NOT EXISTS fact_entities (
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    fact_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    role TEXT NOT NULL,
    mention TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (fact_id, entity_id, role)
);

CREATE INDEX IF NOT EXISTS idx_fact_entities_tenant_user_entity
    ON fact_entities (org_id, user_id, entity_id);

CREATE INDEX IF NOT EXISTS idx_fact_entities_tenant_user_fact
    ON fact_entities (org_id, user_id, fact_id);

-- A relation between two entities stated by a fact, valid as long as the fact is.
CREATE TABLE IF NOT EXISTS entity_relations (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    source_entity_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    target_entity_id TEXT NOT NULL,
    valid_at TEXT,
    invalid_at TEXT,
    source_fact_id TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entity_relations_tenant_user_source
    ON entity_relations (org_id, user_id, source_entity_id);

CREATE INDEX IF NOT EXISTS idx_entity_relations_tenant_user_target
    ON entity_relations (org_id, user_id, target_entity_id);

CREATE INDEX IF NOT EXISTS idx_entity_relations_tenant_user_fact
    ON entity_relations (org_id, user_id, source_fact_id);
