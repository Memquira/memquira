use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{MemoryEvent, TenantContext};
use sqlx::postgres::PgPool;
use sqlx::{Postgres, QueryBuilder, Row};

use memcore_core::ports::{MemoryEventQuery, MemoryEventStore, OrgMemoryEventQuery};

use super::conversions::{memory_event_operation_to_str, row_to_memory_event};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn ensure_event_tenant(event: &MemoryEvent, tenant: &TenantContext) -> MemcoreResult<()> {
    if event.org_id == tenant.org_id && event.user_id == tenant.user_id {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn parse_event_row(row: &sqlx::postgres::PgRow) -> MemcoreResult<MemoryEvent> {
    row_to_memory_event(
        row.try_get("id")
            .map_err(|error| storage_error("row id", error))?,
        row.try_get("org_id")
            .map_err(|error| storage_error("row org_id", error))?,
        row.try_get("user_id")
            .map_err(|error| storage_error("row user_id", error))?,
        row.try_get("fact_id")
            .map_err(|error| storage_error("row fact_id", error))?,
        row.try_get("operation")
            .map_err(|error| storage_error("row operation", error))?,
        row.try_get("input_text")
            .map_err(|error| storage_error("row input_text", error))?,
        row.try_get("previous_content")
            .map_err(|error| storage_error("row previous_content", error))?,
        row.try_get("new_content")
            .map_err(|error| storage_error("row new_content", error))?,
        row.try_get("provider_name")
            .map_err(|error| storage_error("row provider_name", error))?,
        row.try_get("model_name")
            .map_err(|error| storage_error("row model_name", error))?,
        row.try_get("metadata")
            .map_err(|error| storage_error("row metadata", error))?,
        row.try_get("created_at")
            .map_err(|error| storage_error("row created_at", error))?,
    )
}

#[derive(Clone, Debug)]
pub struct PostgresMemoryEventStore {
    pool: PgPool,
}

impl PostgresMemoryEventStore {
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
}

#[async_trait]
impl MemoryEventStore for PostgresMemoryEventStore {
    async fn record_event(
        &self,
        tenant: &TenantContext,
        event: MemoryEvent,
    ) -> MemcoreResult<MemoryEvent> {
        ensure_event_tenant(&event, tenant)?;

        sqlx::query(
            r#"
            INSERT INTO memory_events (
                id, org_id, user_id, fact_id, operation,
                input_text, previous_content, new_content,
                provider_name, model_name, metadata, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#,
        )
        .bind(event.id)
        .bind(&event.org_id)
        .bind(&event.user_id)
        .bind(event.fact_id)
        .bind(memory_event_operation_to_str(event.operation))
        .bind(&event.input_text)
        .bind(&event.previous_content)
        .bind(&event.new_content)
        .bind(&event.provider_name)
        .bind(&event.model_name)
        .bind(&event.metadata)
        .bind(event.created_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to insert memory event", error))?;

        Ok(event)
    }

    async fn list_events(&self, query: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        use crate::keyword_search::push_postgres_event_keyword_filter;
        use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, fact_id, operation, input_text, previous_content, new_content, provider_name, model_name, metadata, created_at FROM memory_events WHERE org_id = ",
        );
        builder.push_bind(query.tenant.org_id.clone());
        builder.push(" AND user_id = ");
        builder.push_bind(query.tenant.user_id.clone());

        if let Some(fact_id) = query.fact_id {
            builder.push(" AND fact_id = ");
            builder.push_bind(fact_id);
        }

        if let Some(operation) = query.operation {
            builder.push(" AND operation = ");
            builder.push_bind(memory_event_operation_to_str(operation));
        }

        if let Some(created_after) = query.created_after {
            builder.push(" AND created_at >= ");
            builder.push_bind(created_after);
        }

        if let Some(created_before) = query.created_before {
            builder.push(" AND created_at < ");
            builder.push_bind(created_before);
        }

        if let Some(query_text) = &query.query_text {
            push_postgres_event_keyword_filter(&mut builder, query_text, false);
        }

        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "created_at", "id", cursor);
        }

        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(
            i64::try_from(fetch_limit(query.limit)).map_err(|error| {
                storage_error("event list limit out of range for postgres", error)
            })?,
        );

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to list memory events", error))?;

        rows.iter().map(parse_event_row).collect()
    }

    async fn list_events_by_org(
        &self,
        query: OrgMemoryEventQuery,
    ) -> MemcoreResult<Vec<MemoryEvent>> {
        use crate::keyword_search::push_postgres_event_keyword_filter;
        use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, fact_id, operation, input_text, previous_content, new_content, provider_name, model_name, metadata, created_at FROM memory_events WHERE org_id = ",
        );
        builder.push_bind(query.org_id.clone());

        if let Some(user_id) = &query.user_id {
            builder.push(" AND user_id = ");
            builder.push_bind(user_id.clone());
        }

        if let Some(fact_id) = query.fact_id {
            builder.push(" AND fact_id = ");
            builder.push_bind(fact_id);
        }

        if let Some(operation) = query.operation {
            builder.push(" AND operation = ");
            builder.push_bind(memory_event_operation_to_str(operation));
        }

        if let Some(created_after) = query.created_after {
            builder.push(" AND created_at >= ");
            builder.push_bind(created_after);
        }

        if let Some(created_before) = query.created_before {
            builder.push(" AND created_at < ");
            builder.push_bind(created_before);
        }

        if let Some(query_text) = &query.query_text {
            push_postgres_event_keyword_filter(&mut builder, query_text, true);
        }

        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "created_at", "id", cursor);
        }

        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(i64::try_from(fetch_limit(query.limit)).map_err(|error| {
            storage_error("org event list limit out of range for postgres", error)
        })?);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to list org memory events", error))?;

        rows.iter().map(parse_event_row).collect()
    }

    async fn delete_events_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize> {
        if dry_run {
            let row = sqlx::query(
                "SELECT COUNT(*)::bigint AS count FROM memory_events WHERE org_id = $1 AND user_id = $2 AND created_at < $3",
            )
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count events for retention", error))?;

            let count: i64 = row
                .try_get("count")
                .map_err(|error| storage_error("row count", error))?;
            return Ok(count as usize);
        }

        let result = sqlx::query(
            "DELETE FROM memory_events WHERE org_id = $1 AND user_id = $2 AND created_at < $3",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to delete events for retention", error))?;

        Ok(result.rows_affected() as usize)
    }

    async fn count_events_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let row =
            sqlx::query("SELECT COUNT(*)::bigint AS count FROM memory_events WHERE org_id = $1")
                .bind(org_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| storage_error("failed to count events by org", error))?;

        let count: i64 = row
            .try_get("count")
            .map_err(|error| storage_error("row count", error))?;
        Ok(count as usize)
    }

    async fn erase_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let result = sqlx::query(
            "DELETE FROM memory_events WHERE org_id = $1 AND user_id = $2 AND NOT (operation = 'forget_user' AND fact_id IS NULL AND input_text IS NULL AND previous_content IS NULL AND new_content IS NULL AND model_name IS NULL AND (provider_name IS NULL OR provider_name = 'maintenance') AND metadata = '{\"deleted\":true}'::jsonb)",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to erase user audit events", error))?;
        Ok(result.rows_affected() as usize)
    }

    async fn count_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS count FROM memory_events WHERE org_id = $1 AND user_id = $2 AND NOT (operation = 'forget_user' AND fact_id IS NULL AND input_text IS NULL AND previous_content IS NULL AND new_content IS NULL AND model_name IS NULL AND (provider_name IS NULL OR provider_name = 'maintenance') AND metadata = '{\"deleted\":true}'::jsonb)",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to count user audit events", error))?;
        let count: i64 = row
            .try_get("count")
            .map_err(|error| storage_error("row count", error))?;
        Ok(count as usize)
    }
}
