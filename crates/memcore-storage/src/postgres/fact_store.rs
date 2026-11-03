use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{Fact, TenantContext};
use sqlx::postgres::PgPool;
use sqlx::{Postgres, QueryBuilder, Row};
use uuid::Uuid;

use memcore_core::ports::{
    DedupInsert, FactFulltextQuery, FactSearchQuery, FactStore, RetentionPruneResult, ScoredFact,
    StoredFactState,
};

/// The partial unique index of migration 15; the conflict target must match it.
const DEDUP_CONFLICT: &str = "ON CONFLICT (org_id, user_id, dedup_key) WHERE dedup_key IS NOT NULL AND deleted_at IS NULL AND superseded_at IS NULL AND invalid_at IS NULL DO NOTHING";

use super::conversions::{
    StructureColumns, memory_source_to_str, memory_type_to_str, row_to_fact,
    structure_from_columns, structure_to_columns,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn ensure_fact_tenant(fact: &Fact, tenant: &TenantContext) -> MemcoreResult<()> {
    if fact.org_id == tenant.org_id && fact.user_id == tenant.user_id {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn parse_row(row: &sqlx::postgres::PgRow) -> MemcoreResult<Fact> {
    let fact = row_to_fact(
        row.try_get("id")
            .map_err(|error| storage_error("row id", error))?,
        row.try_get("org_id")
            .map_err(|error| storage_error("row org_id", error))?,
        row.try_get("user_id")
            .map_err(|error| storage_error("row user_id", error))?,
        row.try_get("memory_type")
            .map_err(|error| storage_error("row memory_type", error))?,
        row.try_get("content")
            .map_err(|error| storage_error("row content", error))?,
        row.try_get("summary")
            .map_err(|error| storage_error("row summary", error))?,
        row.try_get("source")
            .map_err(|error| storage_error("row source", error))?,
        row.try_get("confidence")
            .map_err(|error| storage_error("row confidence", error))?,
        row.try_get("importance")
            .map_err(|error| storage_error("row importance", error))?,
        row.try_get("valid_at")
            .map_err(|error| storage_error("row valid_at", error))?,
        row.try_get("invalid_at")
            .map_err(|error| storage_error("row invalid_at", error))?,
        row.try_get("recorded_at")
            .map_err(|error| storage_error("row recorded_at", error))?,
        row.try_get("updated_at")
            .map_err(|error| storage_error("row updated_at", error))?,
        row.try_get("metadata")
            .map_err(|error| storage_error("row metadata", error))?,
    )?;
    let structure = structure_from_columns(StructureColumns {
        subject: row
            .try_get("subject")
            .map_err(|error| storage_error("row subject", error))?,
        event_time_start: row
            .try_get("event_time_start")
            .map_err(|error| storage_error("row event_time_start", error))?,
        event_time_end: row
            .try_get("event_time_end")
            .map_err(|error| storage_error("row event_time_end", error))?,
        event_time_granularity: row
            .try_get("event_time_granularity")
            .map_err(|error| storage_error("row event_time_granularity", error))?,
        event_time_text: row
            .try_get("event_time_text")
            .map_err(|error| storage_error("row event_time_text", error))?,
        observed_at: row
            .try_get("observed_at")
            .map_err(|error| storage_error("row observed_at", error))?,
        session_id: row
            .try_get("session_id")
            .map_err(|error| storage_error("row session_id", error))?,
        source_message_ids: row
            .try_get("source_message_ids")
            .map_err(|error| storage_error("row source_message_ids", error))?,
    })?;
    let mut fact = fact.with_structure(structure);
    fact.superseded_at = row
        .try_get("superseded_at")
        .map_err(|error| storage_error("row superseded_at", error))?;
    fact.superseded_by = row
        .try_get("superseded_by")
        .map_err(|error| storage_error("row superseded_by", error))?;
    Ok(fact)
}

#[derive(Clone, Debug)]
pub struct PostgresFactStore {
    pool: PgPool,
}

impl PostgresFactStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|error| storage_error("failed to connect postgres database", error))?;

        crate::migrations::postgres::run_postgres_migrations(&pool).await?;

        Ok(Self::new(pool))
    }

    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    async fn fetch_fact_row(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
        include_deleted: bool,
    ) -> MemcoreResult<Option<Fact>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by FROM facts WHERE id = ",
        );
        query.push_bind(fact_id);
        query.push(" AND org_id = ");
        query.push_bind(&tenant.org_id);
        query.push(" AND user_id = ");
        query.push_bind(&tenant.user_id);

        if !include_deleted {
            query.push(" AND deleted_at IS NULL");
        }

        let row = query
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage_error("failed to fetch fact", error))?;

        row.as_ref().map(parse_row).transpose()
    }

    /// One tenant-scoped row regardless of soft deletion, with its state.
    async fn fetch_fact_state(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by, (deleted_at IS NOT NULL) AS is_deleted FROM facts WHERE id = ",
        );
        query.push_bind(fact_id);
        query.push(" AND org_id = ");
        query.push_bind(&tenant.org_id);
        query.push(" AND user_id = ");
        query.push_bind(&tenant.user_id);

        let Some(row) = query
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage_error("failed to fetch fact", error))?
        else {
            return Ok(None);
        };
        let deleted: bool = row
            .try_get("is_deleted")
            .map_err(|error| storage_error("failed to read fact deletion state", error))?;
        Ok(Some(StoredFactState {
            fact: parse_row(&row)?,
            deleted,
        }))
    }
}

impl PostgresFactStore {
    /// Inserts the row (with `dedup_key`, or NULL); returns the number of rows written (0 = an
    /// active fact of the tenant already holds the key).
    async fn insert_row(
        &self,
        tenant: &TenantContext,
        fact: &Fact,
        dedup_key: Option<&str>,
    ) -> MemcoreResult<u64> {
        ensure_fact_tenant(fact, tenant)?;

        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(1) FROM facts WHERE id = $1 AND org_id = $2 AND user_id = $3",
        )
        .bind(fact.id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to check existing fact", error))?;

        if exists > 0 {
            return Err(MemcoreError::Conflict(format!(
                "fact already exists: {}",
                fact.id
            )));
        }

        let structure = structure_to_columns(&fact.structure);
        // Only a keyed (ingestion) insert names `dedup_key`; every other insert
        // (imports included) keeps the statement of the schema before migration 15.
        let (key_column, key_value, conflict) = match dedup_key {
            Some(_) => (", dedup_key", ", $25", DEDUP_CONFLICT),
            None => ("", "", ""),
        };
        let sql = format!(
            r#"
            INSERT INTO facts (
                id, org_id, user_id, memory_type, content, summary, source,
                confidence, importance, valid_at, invalid_at, recorded_at, updated_at,
                metadata, deleted_at,
                subject, event_time_start, event_time_end, event_time_granularity,
                event_time_text, observed_at, session_id, source_message_ids,
                superseded_at, superseded_by{key_column}
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NULL,
                $15, $16, $17, $18, $19, $20, $21, $22, $23, $24{key_value}
            )
            {conflict}
            "#
        );
        let mut query = sqlx::query(&sql)
            .bind(fact.id)
            .bind(&fact.org_id)
            .bind(&fact.user_id)
            .bind(memory_type_to_str(fact.memory_type))
            .bind(&fact.content)
            .bind(&fact.summary)
            .bind(memory_source_to_str(fact.source))
            .bind(fact.confidence)
            .bind(fact.importance)
            .bind(fact.valid_at)
            .bind(fact.invalid_at)
            .bind(fact.recorded_at)
            .bind(fact.updated_at)
            .bind(&fact.metadata)
            .bind(structure.subject)
            .bind(structure.event_time_start)
            .bind(structure.event_time_end)
            .bind(structure.event_time_granularity)
            .bind(structure.event_time_text)
            .bind(structure.observed_at)
            .bind(structure.session_id)
            .bind(structure.source_message_ids)
            .bind(fact.superseded_at)
            .bind(fact.superseded_by);
        if let Some(key) = dedup_key {
            query = query.bind(key);
        }
        let result = query
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to insert fact", error))?;

        Ok(result.rows_affected())
    }

    async fn fact_holding_dedup_key(
        &self,
        tenant: &TenantContext,
        dedup_key: &str,
    ) -> MemcoreResult<Option<Uuid>> {
        sqlx::query_scalar(
            "SELECT id FROM facts WHERE org_id = $1 AND user_id = $2 AND dedup_key = $3 AND deleted_at IS NULL AND superseded_at IS NULL AND invalid_at IS NULL LIMIT 1",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(dedup_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to read fact dedup key", error))
    }
}

#[async_trait]
impl FactStore for PostgresFactStore {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.insert_row(tenant, &fact, None).await?;
        Ok(fact)
    }

    async fn insert_fact_deduplicated(
        &self,
        tenant: &TenantContext,
        fact: Fact,
        dedup_key: &str,
    ) -> MemcoreResult<DedupInsert> {
        // Under READ COMMITTED the conflict waits for a concurrent holder to commit or roll back;
        // the read-back then sees the committed holder. A holder deleted in between frees the key.
        for _ in 0..3 {
            if self.insert_row(tenant, &fact, Some(dedup_key)).await? > 0 {
                return Ok(DedupInsert::Inserted(fact));
            }
            if let Some(existing_fact_id) = self.fact_holding_dedup_key(tenant, dedup_key).await? {
                return Ok(DedupInsert::Duplicate { existing_fact_id });
            }
        }
        Err(MemcoreError::Conflict(
            "fact dedup key contended; retry the request".to_string(),
        ))
    }

    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        ensure_fact_tenant(&fact, tenant)?;

        let structure = structure_to_columns(&fact.structure);
        let result = sqlx::query(
            r#"
            UPDATE facts SET
                memory_type = $1,
                content = $2,
                summary = $3,
                source = $4,
                confidence = $5,
                importance = $6,
                valid_at = $7,
                invalid_at = $8,
                recorded_at = $9,
                updated_at = $10,
                metadata = $11,
                subject = $15,
                event_time_start = $16,
                event_time_end = $17,
                event_time_granularity = $18,
                event_time_text = $19,
                observed_at = $20,
                session_id = $21,
                source_message_ids = $22,
                superseded_at = $23,
                superseded_by = $24,
                dedup_key = NULL
            WHERE id = $12 AND org_id = $13 AND user_id = $14 AND deleted_at IS NULL
            "#,
        )
        .bind(memory_type_to_str(fact.memory_type))
        .bind(&fact.content)
        .bind(&fact.summary)
        .bind(memory_source_to_str(fact.source))
        .bind(fact.confidence)
        .bind(fact.importance)
        .bind(fact.valid_at)
        .bind(fact.invalid_at)
        .bind(fact.recorded_at)
        .bind(fact.updated_at)
        .bind(&fact.metadata)
        .bind(fact.id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(structure.subject)
        .bind(structure.event_time_start)
        .bind(structure.event_time_end)
        .bind(structure.event_time_granularity)
        .bind(structure.event_time_text)
        .bind(structure.observed_at)
        .bind(structure.session_id)
        .bind(structure.source_message_ids)
        .bind(fact.superseded_at)
        .bind(fact.superseded_by)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to update fact", error))?;

        if result.rows_affected() == 0 {
            return Err(MemcoreError::NotFound(format!(
                "fact not found: {}",
                fact.id
            )));
        }

        Ok(fact)
    }

    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
        self.fetch_fact_row(tenant, fact_id, false).await
    }

    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.fetch_fact_state(tenant, fact_id).await
    }

    async fn list_fact_states(
        &self,
        tenant: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by, (deleted_at IS NOT NULL) AS is_deleted FROM facts WHERE org_id = ",
        );
        query.push_bind(&tenant.org_id);
        query.push(" AND user_id = ");
        query.push_bind(&tenant.user_id);
        if let Some(after) = after {
            query.push(" AND id > ");
            query.push_bind(after);
        }
        query.push(" ORDER BY id ASC LIMIT ");
        query.push_bind(
            i64::try_from(limit)
                .map_err(|error| storage_error("fact state page limit out of range", error))?,
        );
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to list fact states", error))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let deleted: bool = row
                .try_get("is_deleted")
                .map_err(|error| storage_error("failed to read fact deletion state", error))?;
            out.push(StoredFactState {
                fact: parse_row(row)?,
                deleted,
            });
        }
        Ok(out)
    }

    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool> {
        let mut query = QueryBuilder::<Postgres>::new("SELECT id FROM facts WHERE id = ");
        query.push_bind(fact_id);
        query.push(" LIMIT 1");
        let row = query
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage_error("failed to check fact id", error))?;
        Ok(row.is_some())
    }

    async fn search_facts(&self, query: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        use crate::keyword_search::push_postgres_fact_keyword_filter;
        use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by FROM facts WHERE org_id = ",
        );
        builder.push_bind(&query.tenant.org_id);
        builder.push(" AND user_id = ");
        builder.push_bind(&query.tenant.user_id);

        if !query.include_deleted {
            builder.push(" AND deleted_at IS NULL");
        }

        if let Some(memory_types) = &query.memory_types
            && !memory_types.is_empty()
        {
            builder.push(" AND memory_type IN (");
            let mut separated = builder.separated(", ");
            for memory_type in memory_types {
                separated.push_bind(memory_type_to_str(*memory_type));
            }
            separated.push_unseparated(") ");
        }

        if let Some(query_text) = &query.query_text {
            push_postgres_fact_keyword_filter(&mut builder, query_text);
        }

        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "updated_at", "id", cursor);
        }

        builder.push(" ORDER BY updated_at DESC, id DESC LIMIT ");
        builder.push_bind(i64::try_from(fetch_limit(query.limit)).map_err(|error| {
            storage_error("fact search limit out of range for postgres", error)
        })?);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to search facts", error))?;

        rows.iter().map(parse_row).collect()
    }

    /// `websearch_to_tsquery` over the weighted `fts` column (migration 0011), ranked by
    /// `ts_rank_cd`. Terms are letters and digits only (`FulltextQuery`), joined with `or`.
    ///
    /// Ties follow the same stable order as SQLite: content in byte order (`COLLATE "C"`,
    /// matching SQLite's binary collation), then source message ids, then subject; the id only
    /// separates facts identical in all three.
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>> {
        if query.query.is_empty() || query.limit == 0 {
            return Ok(Vec::new());
        }
        let websearch = query.query.to_websearch();
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by, ts_rank_cd(fts, websearch_to_tsquery('english', ",
        );
        builder.push_bind(websearch.clone());
        builder.push(")) AS fts_rank FROM facts WHERE org_id = ");
        builder.push_bind(&query.tenant.org_id);
        builder.push(" AND user_id = ");
        builder.push_bind(&query.tenant.user_id);
        builder.push(" AND deleted_at IS NULL AND fts @@ websearch_to_tsquery('english', ");
        builder.push_bind(websearch);
        builder.push(")");
        if let Some(memory_types) = &query.memory_types
            && !memory_types.is_empty()
        {
            builder.push(" AND memory_type IN (");
            let mut separated = builder.separated(", ");
            for memory_type in memory_types {
                separated.push_bind(memory_type_to_str(*memory_type));
            }
            separated.push_unseparated(") ");
        }
        builder.push(
            " ORDER BY fts_rank DESC, content COLLATE \"C\" ASC, \
             COALESCE(source_message_ids::text, '') COLLATE \"C\" ASC, \
             COALESCE(subject, '') COLLATE \"C\" ASC, id ASC LIMIT ",
        );
        builder.push_bind(i64::try_from(query.limit).map_err(|error| {
            storage_error("full-text search limit out of range for postgres", error)
        })?);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to run full-text fact search", error))?;
        rows.iter()
            .map(|row| {
                let rank: f32 = row
                    .try_get("fts_rank")
                    .map_err(|error| storage_error("row fts_rank", error))?;
                Ok(ScoredFact {
                    fact: parse_row(row)?,
                    score: rank,
                })
            })
            .collect()
    }

    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        let deleted_at = Utc::now();
        let result = sqlx::query(
            "UPDATE facts SET deleted_at = $1 WHERE id = $2 AND org_id = $3 AND user_id = $4 AND deleted_at IS NULL",
        )
        .bind(deleted_at)
        .bind(fact_id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to soft delete fact", error))?;

        if result.rows_affected() == 0 {
            return Err(MemcoreError::NotFound(format!("fact not found: {fact_id}")));
        }

        Ok(())
    }

    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        sqlx::query("DELETE FROM facts WHERE org_id = $1 AND user_id = $2")
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to delete user data", error))?;

        Ok(())
    }

    async fn delete_facts_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        let rows = sqlx::query(
            "SELECT id FROM facts WHERE org_id = $1 AND user_id = $2 AND deleted_at IS NULL AND updated_at < $3",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list facts for retention", error))?;

        let fact_ids: Vec<Uuid> = rows
            .iter()
            .map(|row| {
                row.try_get("id")
                    .map_err(|error| storage_error("row id", error))
            })
            .collect::<Result<Vec<_>, _>>()?;

        if dry_run {
            return Ok(RetentionPruneResult {
                count: fact_ids.len(),
                fact_ids: Vec::new(),
            });
        }

        if fact_ids.is_empty() {
            return Ok(RetentionPruneResult {
                count: 0,
                fact_ids: Vec::new(),
            });
        }

        let deleted_at = Utc::now();
        let result = sqlx::query(
            "UPDATE facts SET deleted_at = $1 WHERE org_id = $2 AND user_id = $3 AND deleted_at IS NULL AND updated_at < $4",
        )
        .bind(deleted_at)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to soft delete facts for retention", error))?;

        Ok(RetentionPruneResult {
            count: result.rows_affected() as usize,
            fact_ids,
        })
    }

    async fn count_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS count FROM facts WHERE org_id = $1 AND deleted_at IS NULL",
        )
        .bind(org_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to count facts by org", error))?;

        let count: i64 = row
            .try_get("count")
            .map_err(|error| storage_error("row count", error))?;
        Ok(count as usize)
    }

    async fn count_active_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS count FROM facts WHERE org_id = $1 AND deleted_at IS NULL \
             AND invalid_at IS NULL AND superseded_at IS NULL",
        )
        .bind(org_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to count active facts by org", error))?;

        let count: i64 = row
            .try_get("count")
            .map_err(|error| storage_error("row count", error))?;
        Ok(count as usize)
    }

    async fn count_facts_by_user(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS count FROM facts WHERE org_id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to count facts by user", error))?;

        let count: i64 = row
            .try_get("count")
            .map_err(|error| storage_error("row count", error))?;
        Ok(count as usize)
    }

    async fn count_users_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT user_id)::bigint AS count FROM facts WHERE org_id = $1 AND deleted_at IS NULL",
        )
        .bind(org_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to count users by org", error))?;

        let count: i64 = row
            .try_get("count")
            .map_err(|error| storage_error("row count", error))?;
        Ok(count as usize)
    }

    async fn list_users_by_org(
        &self,
        query: memcore_core::ports::OrgUserListQuery,
    ) -> MemcoreResult<Vec<memcore_core::ports::OrgUserSummary>> {
        use memcore_core::ports::OrgUserSummary;

        use crate::pagination::fetch_limit;

        let fetch = i64::try_from(fetch_limit(query.limit)).map_err(|error| {
            storage_error("org users list limit out of range for postgres", error)
        })?;

        let rows = if let Some(cursor) = &query.cursor {
            sqlx::query(
                r#"
                SELECT user_id, COUNT(*)::bigint AS memory_count, MAX(updated_at) AS last_memory_at
                FROM facts
                WHERE org_id = $1 AND deleted_at IS NULL
                GROUP BY user_id
                HAVING (MAX(updated_at) < $2 OR (MAX(updated_at) = $2 AND user_id < $3))
                ORDER BY MAX(updated_at) DESC, user_id DESC
                LIMIT $4
                "#,
            )
            .bind(&query.org_id)
            .bind(cursor.last_sort_value)
            .bind(cursor.last_sort_value)
            .bind(&cursor.last_id)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                r#"
                SELECT user_id, COUNT(*)::bigint AS memory_count, MAX(updated_at) AS last_memory_at
                FROM facts
                WHERE org_id = $1 AND deleted_at IS NULL
                GROUP BY user_id
                ORDER BY MAX(updated_at) DESC, user_id DESC
                LIMIT $2
                "#,
            )
            .bind(&query.org_id)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| storage_error("failed to list users by org", error))?;

        rows.iter()
            .map(|row| {
                let user_id: String = row
                    .try_get("user_id")
                    .map_err(|error| storage_error("row user_id", error))?;
                let memory_count: i64 = row
                    .try_get("memory_count")
                    .map_err(|error| storage_error("row memory_count", error))?;
                let last_memory_at: Option<DateTime<Utc>> = row
                    .try_get("last_memory_at")
                    .map_err(|error| storage_error("row last_memory_at", error))?;

                Ok(OrgUserSummary {
                    user_id,
                    memory_count: memory_count as usize,
                    last_memory_at,
                })
            })
            .collect()
    }
}
