use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{Fact, TenantContext};
use sqlx::sqlite::SqlitePool;
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::sqlite::conversions::{
    StructureColumns, datetime_to_str, memory_source_to_str, memory_type_to_str, metadata_to_str,
    optional_datetime_from_str, optional_datetime_to_str, row_to_fact, structure_from_columns,
    structure_to_columns,
};
use memcore_core::ports::FactSearchQuery;
use memcore_core::ports::{
    DedupInsert, FactFulltextQuery, FactStore, RetentionPruneResult, ScoredFact, StoredFactState,
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

fn parse_row(row: &sqlx::sqlite::SqliteRow) -> MemcoreResult<Fact> {
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
    let column = |name: &'static str| -> MemcoreResult<Option<String>> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("row {name}"), error))
    };
    let structure = structure_from_columns(StructureColumns {
        subject: column("subject")?,
        event_time_start: column("event_time_start")?,
        event_time_end: column("event_time_end")?,
        event_time_granularity: column("event_time_granularity")?,
        event_time_text: column("event_time_text")?,
        observed_at: column("observed_at")?,
        session_id: column("session_id")?,
        source_message_ids: column("source_message_ids")?,
    })?;
    let mut fact = fact.with_structure(structure);
    fact.superseded_at = optional_datetime_from_str(column("superseded_at")?)?;
    fact.superseded_by = column("superseded_by")?
        .map(|id| Uuid::parse_str(&id).map_err(|error| storage_error("row superseded_by", error)))
        .transpose()?;
    Ok(fact)
}

#[derive(Clone, Debug)]
pub struct SqliteFactStore {
    pool: SqlitePool,
}

impl SqliteFactStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self::new(pool))
    }

    /// Returns a clone of the underlying SQLite connection pool.
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    async fn fetch_fact_row(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
        include_deleted: bool,
    ) -> MemcoreResult<Option<Fact>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by FROM facts WHERE id = ",
        );
        query.push_bind(fact_id.to_string());
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
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by, (deleted_at IS NOT NULL) AS is_deleted FROM facts WHERE id = ",
        );
        query.push_bind(fact_id.to_string());
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
        // SQLite returns the boolean expression as an INTEGER (0 / 1).
        let deleted = row
            .try_get::<i64, _>("is_deleted")
            .map_err(|error| storage_error("failed to read fact deletion state", error))?
            != 0;
        Ok(Some(StoredFactState {
            fact: parse_row(&row)?,
            deleted,
        }))
    }
}

/// The partial unique index of migration 15; the conflict target must match it.
const DEDUP_CONFLICT: &str = "ON CONFLICT (org_id, user_id, dedup_key) WHERE dedup_key IS NOT NULL AND deleted_at IS NULL AND superseded_at IS NULL AND invalid_at IS NULL DO NOTHING";

impl SqliteFactStore {
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
            "SELECT COUNT(1) FROM facts WHERE id = ? AND org_id = ? AND user_id = ?",
        )
        .bind(fact.id.to_string())
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

        let structure = structure_to_columns(&fact.structure)?;
        // Only a keyed (ingestion) insert names `dedup_key`; every other insert
        // (imports included) keeps the statement of the schema before migration 15.
        let (key_column, key_value, conflict) = match dedup_key {
            Some(_) => (", dedup_key", ", ?", DEDUP_CONFLICT),
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?{key_value})
            {conflict}
            "#
        );
        let mut query = sqlx::query(&sql)
            .bind(fact.id.to_string())
            .bind(&fact.org_id)
            .bind(&fact.user_id)
            .bind(memory_type_to_str(fact.memory_type))
            .bind(&fact.content)
            .bind(&fact.summary)
            .bind(memory_source_to_str(fact.source))
            .bind(fact.confidence)
            .bind(fact.importance)
            .bind(optional_datetime_to_str(fact.valid_at))
            .bind(optional_datetime_to_str(fact.invalid_at))
            .bind(datetime_to_str(fact.recorded_at))
            .bind(datetime_to_str(fact.updated_at))
            .bind(metadata_to_str(&fact.metadata)?)
            .bind(structure.subject)
            .bind(structure.event_time_start)
            .bind(structure.event_time_end)
            .bind(structure.event_time_granularity)
            .bind(structure.event_time_text)
            .bind(structure.observed_at)
            .bind(structure.session_id)
            .bind(structure.source_message_ids)
            .bind(optional_datetime_to_str(fact.superseded_at))
            .bind(fact.superseded_by.map(|id| id.to_string()));
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
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM facts WHERE org_id = ? AND user_id = ? AND dedup_key = ? AND deleted_at IS NULL AND superseded_at IS NULL AND invalid_at IS NULL LIMIT 1",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(dedup_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to read fact dedup key", error))?;
        id.map(|id| Uuid::parse_str(&id).map_err(|error| storage_error("invalid fact id", error)))
            .transpose()
    }
}

#[async_trait]
impl FactStore for SqliteFactStore {
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
        // A holder that is deleted between the conflict and the read-back frees the key: retry.
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

        let structure = structure_to_columns(&fact.structure)?;
        let result = sqlx::query(
            r#"
            UPDATE facts SET
                memory_type = ?,
                content = ?,
                summary = ?,
                source = ?,
                confidence = ?,
                importance = ?,
                valid_at = ?,
                invalid_at = ?,
                recorded_at = ?,
                updated_at = ?,
                metadata = ?,
                subject = ?,
                event_time_start = ?,
                event_time_end = ?,
                event_time_granularity = ?,
                event_time_text = ?,
                observed_at = ?,
                session_id = ?,
                source_message_ids = ?,
                superseded_at = ?,
                superseded_by = ?,
                dedup_key = NULL
            WHERE id = ? AND org_id = ? AND user_id = ? AND deleted_at IS NULL
            "#,
        )
        .bind(memory_type_to_str(fact.memory_type))
        .bind(&fact.content)
        .bind(&fact.summary)
        .bind(memory_source_to_str(fact.source))
        .bind(fact.confidence)
        .bind(fact.importance)
        .bind(optional_datetime_to_str(fact.valid_at))
        .bind(optional_datetime_to_str(fact.invalid_at))
        .bind(datetime_to_str(fact.recorded_at))
        .bind(datetime_to_str(fact.updated_at))
        .bind(metadata_to_str(&fact.metadata)?)
        .bind(structure.subject)
        .bind(structure.event_time_start)
        .bind(structure.event_time_end)
        .bind(structure.event_time_granularity)
        .bind(structure.event_time_text)
        .bind(structure.observed_at)
        .bind(structure.session_id)
        .bind(structure.source_message_ids)
        .bind(optional_datetime_to_str(fact.superseded_at))
        .bind(fact.superseded_by.map(|id| id.to_string()))
        .bind(fact.id.to_string())
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
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
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, user_id, memory_type, content, summary, source, confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, subject, event_time_start, event_time_end, event_time_granularity, event_time_text, observed_at, session_id, source_message_ids, superseded_at, superseded_by, (deleted_at IS NOT NULL) AS is_deleted FROM facts WHERE org_id = ",
        );
        query.push_bind(&tenant.org_id);
        query.push(" AND user_id = ");
        query.push_bind(&tenant.user_id);
        if let Some(after) = after {
            query.push(" AND id > ");
            query.push_bind(after.to_string());
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
            // SQLite returns the boolean expression as an INTEGER (0 / 1).
            let deleted = row
                .try_get::<i64, _>("is_deleted")
                .map_err(|error| storage_error("failed to read fact deletion state", error))?
                != 0;
            out.push(StoredFactState {
                fact: parse_row(row)?,
                deleted,
            });
        }
        Ok(out)
    }

    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool> {
        let mut query = QueryBuilder::<Sqlite>::new("SELECT id FROM facts WHERE id = ");
        query.push_bind(fact_id.to_string());
        query.push(" LIMIT 1");
        let row = query
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage_error("failed to check fact id", error))?;
        Ok(row.is_some())
    }

    async fn search_facts(&self, query: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        use crate::keyword_search::push_sqlite_fact_keyword_filter;
        use crate::pagination::{fetch_limit, push_sqlite_desc_cursor};

        let mut builder = QueryBuilder::<Sqlite>::new(
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
            push_sqlite_fact_keyword_filter(&mut builder, query_text);
        }

        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "updated_at", "id", cursor);
        }

        builder.push(" ORDER BY updated_at DESC, id DESC LIMIT ");
        builder.push_bind(
            i64::try_from(fetch_limit(query.limit)).map_err(|error| {
                storage_error("fact search limit out of range for sqlite", error)
            })?,
        );

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to search facts", error))?;

        rows.iter().map(parse_row).collect()
    }

    /// FTS5 `MATCH` over `facts_fts` (migration 0011), ranked by `bm25()` (lower is better, so the
    /// returned score is its negation).
    ///
    /// Equal scores are common: short facts sharing one matched word score identically. They are
    /// ordered by stable fields of the fact, not by its randomly generated id, so the same logical
    /// store returns the same order however it was built: content (binary), then source
    /// message ids, then subject. The id only separates facts identical in all three.
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>> {
        if query.query.is_empty() || query.limit == 0 {
            return Ok(Vec::new());
        }
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT f.id, f.org_id, f.user_id, f.memory_type, f.content, f.summary, f.source, f.confidence, f.importance, f.valid_at, f.invalid_at, f.recorded_at, f.updated_at, f.metadata, f.subject, f.event_time_start, f.event_time_end, f.event_time_granularity, f.event_time_text, f.observed_at, f.session_id, f.source_message_ids, f.superseded_at, f.superseded_by, bm25(facts_fts) AS fts_rank FROM facts_fts JOIN facts f ON f.rowid = facts_fts.rowid WHERE facts_fts MATCH ",
        );
        builder.push_bind(query.query.to_fts5_match());
        builder.push(" AND f.org_id = ");
        builder.push_bind(&query.tenant.org_id);
        builder.push(" AND f.user_id = ");
        builder.push_bind(&query.tenant.user_id);
        builder.push(" AND f.deleted_at IS NULL");
        if let Some(memory_types) = &query.memory_types
            && !memory_types.is_empty()
        {
            builder.push(" AND f.memory_type IN (");
            let mut separated = builder.separated(", ");
            for memory_type in memory_types {
                separated.push_bind(memory_type_to_str(*memory_type));
            }
            separated.push_unseparated(") ");
        }
        builder.push(
            " ORDER BY fts_rank ASC, f.content ASC, COALESCE(f.source_message_ids, '') ASC, \
             COALESCE(f.subject, '') ASC, f.id ASC LIMIT ",
        );
        builder.push_bind(i64::try_from(query.limit).map_err(|error| {
            storage_error("full-text search limit out of range for sqlite", error)
        })?);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to run full-text fact search", error))?;
        rows.iter()
            .map(|row| {
                let rank: f64 = row
                    .try_get("fts_rank")
                    .map_err(|error| storage_error("row fts_rank", error))?;
                Ok(ScoredFact {
                    fact: parse_row(row)?,
                    score: -rank as f32,
                })
            })
            .collect()
    }

    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        let deleted_at = datetime_to_str(Utc::now());
        let result = sqlx::query(
            "UPDATE facts SET deleted_at = ? WHERE id = ? AND org_id = ? AND user_id = ? AND deleted_at IS NULL",
        )
        .bind(deleted_at)
        .bind(fact_id.to_string())
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
        sqlx::query("DELETE FROM facts WHERE org_id = ? AND user_id = ?")
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
        let cutoff_str = datetime_to_str(cutoff);

        let rows = sqlx::query(
            "SELECT id FROM facts WHERE org_id = ? AND user_id = ? AND deleted_at IS NULL AND updated_at < ?",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(&cutoff_str)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list facts for retention", error))?;

        let fact_ids: Vec<Uuid> = rows
            .iter()
            .map(|row| {
                let id_str: String = row
                    .try_get("id")
                    .map_err(|error| storage_error("row id", error))?;
                Uuid::parse_str(&id_str).map_err(|error| storage_error("invalid fact id", error))
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

        let deleted_at = datetime_to_str(Utc::now());
        let result = sqlx::query(
            "UPDATE facts SET deleted_at = ? WHERE org_id = ? AND user_id = ? AND deleted_at IS NULL AND updated_at < ?",
        )
        .bind(deleted_at)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(cutoff_str)
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
            "SELECT COUNT(*) as count FROM facts WHERE org_id = ? AND deleted_at IS NULL",
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
            "SELECT COUNT(*) as count FROM facts WHERE org_id = ? AND deleted_at IS NULL \
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
            "SELECT COUNT(*) as count FROM facts WHERE org_id = ? AND user_id = ? AND deleted_at IS NULL",
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
            "SELECT COUNT(DISTINCT user_id) as count FROM facts WHERE org_id = ? AND deleted_at IS NULL",
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
        use crate::sqlite::conversions::datetime_to_str;

        let fetch = i64::try_from(fetch_limit(query.limit)).map_err(|error| {
            storage_error("org users list limit out of range for sqlite", error)
        })?;

        let rows = if let Some(cursor) = &query.cursor {
            let sort_value = datetime_to_str(cursor.last_sort_value);
            sqlx::query(
                r#"
                SELECT user_id, COUNT(*) as memory_count, MAX(updated_at) as last_memory_at
                FROM facts
                WHERE org_id = ? AND deleted_at IS NULL
                GROUP BY user_id
                HAVING (MAX(updated_at) < ? OR (MAX(updated_at) = ? AND user_id < ?))
                ORDER BY last_memory_at DESC, user_id DESC
                LIMIT ?
                "#,
            )
            .bind(&query.org_id)
            .bind(&sort_value)
            .bind(&sort_value)
            .bind(&cursor.last_id)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                r#"
                SELECT user_id, COUNT(*) as memory_count, MAX(updated_at) as last_memory_at
                FROM facts
                WHERE org_id = ? AND deleted_at IS NULL
                GROUP BY user_id
                ORDER BY last_memory_at DESC, user_id DESC
                LIMIT ?
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
                let last_memory_at_str: Option<String> = row
                    .try_get("last_memory_at")
                    .map_err(|error| storage_error("row last_memory_at", error))?;
                let last_memory_at = match last_memory_at_str {
                    Some(value) => Some(
                        crate::sqlite::conversions::datetime_from_str(&value)
                            .map_err(|error| storage_error("invalid last_memory_at", error))?,
                    ),
                    None => None,
                };

                Ok(OrgUserSummary {
                    user_id,
                    memory_count: memory_count as usize,
                    last_memory_at,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use memcore_core::{MemorySource, MemoryType, TenantContext};
    use serde_json::json;
    use uuid::Uuid;

    use super::SqliteFactStore;
    use memcore_core::ports::{FactSearchQuery, FactStore};

    async fn test_store() -> SqliteFactStore {
        SqliteFactStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite store should connect")
    }

    fn tenant(org_id: &str, user_id: &str) -> TenantContext {
        TenantContext::new(org_id, user_id).expect("tenant should be valid")
    }

    fn sample_fact(
        org_id: &str,
        user_id: &str,
        content: &str,
        memory_type: MemoryType,
        metadata: serde_json::Value,
        valid_at: Option<chrono::DateTime<Utc>>,
        invalid_at: Option<chrono::DateTime<Utc>>,
    ) -> memcore_core::Fact {
        let now = Utc::now();
        memcore_core::Fact::new(
            Uuid::new_v4(),
            org_id,
            user_id,
            memory_type,
            content,
            Some("summary".to_string()),
            MemorySource::UserMessage,
            0.9,
            0.8,
            valid_at,
            invalid_at,
            now,
            now,
            metadata,
        )
        .expect("fact should be valid")
    }

    #[tokio::test]
    async fn insert_and_get_fact() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let fact = sample_fact(
            "org_a",
            "user_a",
            "learning rust",
            MemoryType::Skill,
            json!({}),
            None,
            None,
        );

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");

        let fetched = store
            .get_fact(&tenant, fact.id)
            .await
            .expect("get should succeed")
            .expect("fact should exist");

        assert_eq!(fetched.content, "learning rust");
    }

    #[tokio::test]
    async fn update_fact() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let mut fact = sample_fact(
            "org_a",
            "user_a",
            "original",
            MemoryType::Profile,
            json!({}),
            None,
            None,
        );

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");

        fact.content = "updated content".to_string();
        store
            .update_fact(&tenant, fact.clone())
            .await
            .expect("update should succeed");

        let fetched = store
            .get_fact(&tenant, fact.id)
            .await
            .expect("get should succeed")
            .expect("fact should exist");
        assert_eq!(fetched.content, "updated content");
    }

    #[tokio::test]
    async fn search_facts_by_tenant() {
        let store = test_store().await;
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_b", "user_b");

        store
            .insert_fact(
                &tenant_a,
                sample_fact(
                    "org_a",
                    "user_a",
                    "rust backend",
                    MemoryType::Skill,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert should succeed");
        store
            .insert_fact(
                &tenant_b,
                sample_fact(
                    "org_b",
                    "user_b",
                    "rust backend",
                    MemoryType::Skill,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert should succeed");

        let results = store
            .search_facts(FactSearchQuery {
                tenant: tenant_a,
                memory_types: None,
                query_text: Some("rust".to_string()),
                limit: 10,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn search_facts_by_keyword_is_case_insensitive() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        store
            .insert_fact(
                &tenant,
                sample_fact(
                    "org_a",
                    "user_a",
                    "RUST programming",
                    MemoryType::Skill,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert");

        let results = store
            .search_facts(FactSearchQuery {
                tenant,
                memory_types: None,
                query_text: Some("rust".to_string()),
                limit: 10,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("search");

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_facts_by_memory_type() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");

        store
            .insert_fact(
                &tenant,
                sample_fact(
                    "org_a",
                    "user_a",
                    "skill fact",
                    MemoryType::Skill,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert should succeed");
        store
            .insert_fact(
                &tenant,
                sample_fact(
                    "org_a",
                    "user_a",
                    "profile fact",
                    MemoryType::Profile,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert should succeed");

        let results = store
            .search_facts(FactSearchQuery {
                tenant,
                memory_types: Some(vec![MemoryType::Profile]),
                query_text: None,
                limit: 10,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Profile);
    }

    #[tokio::test]
    async fn tenant_isolation_prevents_cross_tenant_reads() {
        let store = test_store().await;
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");
        let fact = sample_fact(
            "org_a",
            "user_a",
            "private",
            MemoryType::Profile,
            json!({}),
            None,
            None,
        );

        store
            .insert_fact(&tenant_a, fact.clone())
            .await
            .expect("insert should succeed");

        let cross = store
            .get_fact(&tenant_b, fact.id)
            .await
            .expect("get should succeed");
        assert!(cross.is_none());
    }

    #[tokio::test]
    async fn soft_delete_hides_fact_from_normal_search() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let fact = sample_fact(
            "org_a",
            "user_a",
            "delete me",
            MemoryType::Task,
            json!({}),
            None,
            None,
        );

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");
        store
            .soft_delete_fact(&tenant, fact.id)
            .await
            .expect("soft delete should succeed");

        assert!(
            store
                .get_fact(&tenant, fact.id)
                .await
                .expect("get should succeed")
                .is_none()
        );

        let results = store
            .search_facts(FactSearchQuery::new(tenant.clone(), 10))
            .await
            .expect("search should succeed");
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn include_deleted_returns_soft_deleted_fact() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let fact = sample_fact(
            "org_a",
            "user_a",
            "deleted fact",
            MemoryType::Task,
            json!({}),
            None,
            None,
        );

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");
        store
            .soft_delete_fact(&tenant, fact.id)
            .await
            .expect("soft delete should succeed");

        let results = store
            .search_facts(FactSearchQuery {
                tenant,
                memory_types: None,
                query_text: None,
                limit: 10,
                cursor: None,
                include_deleted: true,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, fact.id);
    }

    #[tokio::test]
    async fn delete_user_data_removes_only_target_user() {
        let store = test_store().await;
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");

        store
            .insert_fact(
                &tenant_a,
                sample_fact(
                    "org_a",
                    "user_a",
                    "user a",
                    MemoryType::Profile,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert should succeed");
        store
            .insert_fact(
                &tenant_b,
                sample_fact(
                    "org_a",
                    "user_b",
                    "user b",
                    MemoryType::Profile,
                    json!({}),
                    None,
                    None,
                ),
            )
            .await
            .expect("insert should succeed");

        store
            .delete_user_data(&tenant_a)
            .await
            .expect("delete should succeed");

        assert!(
            store
                .search_facts(FactSearchQuery::new(tenant_a, 10))
                .await
                .expect("search")
                .is_empty()
        );
        assert_eq!(
            store
                .search_facts(FactSearchQuery::new(tenant_b, 10))
                .await
                .expect("search")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn metadata_round_trip_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let fact = sample_fact(
            "org_a",
            "user_a",
            "metadata test",
            MemoryType::System,
            json!({ "topic": "rust", "level": 2 }),
            None,
            None,
        );

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");

        let fetched = store
            .get_fact(&tenant, fact.id)
            .await
            .expect("get should succeed")
            .expect("fact should exist");
        assert_eq!(fetched.metadata["topic"], "rust");
        assert_eq!(fetched.metadata["level"], 2);
    }

    #[tokio::test]
    async fn valid_and_invalid_at_round_trip_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let valid_at = Utc::now() - Duration::days(2);
        let invalid_at = Utc::now() - Duration::days(1);
        let fact = sample_fact(
            "org_a",
            "user_a",
            "temporal fact",
            MemoryType::Entity,
            json!({}),
            Some(valid_at),
            Some(invalid_at),
        );

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");

        let fetched = store
            .get_fact(&tenant, fact.id)
            .await
            .expect("get should succeed")
            .expect("fact should exist");

        assert_eq!(fetched.valid_at, Some(valid_at));
        assert_eq!(fetched.invalid_at, Some(invalid_at));
    }

    #[tokio::test]
    async fn org_admin_counts_exclude_other_org_and_deleted_facts() {
        let store = test_store().await;
        let tenant_a = tenant("org_sqlite_admin", "user_a");
        let tenant_b = tenant("org_sqlite_admin", "user_b");
        let other_org = tenant("org_other", "user_x");

        let fact_a = sample_fact(
            "org_sqlite_admin",
            "user_a",
            "active",
            MemoryType::Profile,
            json!({}),
            None,
            None,
        );
        let fact_deleted = sample_fact(
            "org_sqlite_admin",
            "user_a",
            "deleted",
            MemoryType::Profile,
            json!({}),
            None,
            None,
        );
        let fact_b = sample_fact(
            "org_sqlite_admin",
            "user_b",
            "active b",
            MemoryType::Profile,
            json!({}),
            None,
            None,
        );
        let fact_other = sample_fact(
            "org_other",
            "user_x",
            "other org",
            MemoryType::Profile,
            json!({}),
            None,
            None,
        );

        store
            .insert_fact(&tenant_a, fact_a.clone())
            .await
            .expect("insert");
        store
            .insert_fact(&tenant_a, fact_deleted.clone())
            .await
            .expect("insert");
        store
            .soft_delete_fact(&tenant_a, fact_deleted.id)
            .await
            .expect("soft delete");
        store.insert_fact(&tenant_b, fact_b).await.expect("insert");
        store
            .insert_fact(&other_org, fact_other)
            .await
            .expect("insert");

        assert_eq!(
            store
                .count_facts_by_org("org_sqlite_admin")
                .await
                .expect("count facts"),
            2
        );
        assert_eq!(
            store
                .count_users_by_org("org_sqlite_admin")
                .await
                .expect("count users"),
            2
        );
        assert_eq!(
            store
                .count_facts_by_user(&tenant_a)
                .await
                .expect("count user facts"),
            1
        );

        let users = store
            .list_users_by_org(memcore_core::ports::OrgUserListQuery {
                org_id: "org_sqlite_admin".to_string(),
                limit: 10,
                cursor: None,
            })
            .await
            .expect("list users");
        assert_eq!(users.len(), 2);
        let user_a = users
            .iter()
            .find(|user| user.user_id == "user_a")
            .expect("user_a");
        assert_eq!(user_a.memory_count, 1);
        assert_eq!(user_a.last_memory_at, Some(fact_a.updated_at));
    }
}
