use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ProviderUsageDailyBucket;
use memcore_core::pagination::{PageCursor, build_page};
use memcore_core::ports::{
    ProviderCallStatus, ProviderUsageCapability, ProviderUsageDailyQuery, ProviderUsageEventRecord,
    ProviderUsagePersistedSummary, ProviderUsageQuery, ProviderUsageQueryResult,
    ProviderUsageStore, validate_provider_usage_limit,
};
use serde_json::Value;
use sqlx::postgres::PgPool;
use sqlx::{Postgres, QueryBuilder, Row};

use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn capability_to_str(value: ProviderUsageCapability) -> &'static str {
    match value {
        ProviderUsageCapability::Llm => "llm",
        ProviderUsageCapability::Embedding => "embedding",
        ProviderUsageCapability::Summarization => "summarization",
    }
}

fn capability_from_str(value: &str) -> MemcoreResult<ProviderUsageCapability> {
    match value {
        "llm" => Ok(ProviderUsageCapability::Llm),
        "embedding" => Ok(ProviderUsageCapability::Embedding),
        "summarization" => Ok(ProviderUsageCapability::Summarization),
        _ => Err(MemcoreError::StorageError(format!(
            "invalid provider usage capability: {value}"
        ))),
    }
}

fn status_to_str(value: ProviderCallStatus) -> &'static str {
    match value {
        ProviderCallStatus::Success => "success",
        ProviderCallStatus::Error => "error",
    }
}

fn status_from_str(value: &str) -> MemcoreResult<ProviderCallStatus> {
    match value {
        "success" => Ok(ProviderCallStatus::Success),
        "error" => Ok(ProviderCallStatus::Error),
        _ => Err(MemcoreError::StorageError(format!(
            "invalid provider usage status: {value}"
        ))),
    }
}

fn row_to_event(row: &sqlx::postgres::PgRow) -> MemcoreResult<ProviderUsageEventRecord> {
    Ok(ProviderUsageEventRecord {
        id: row
            .try_get("id")
            .map_err(|error| storage_error("row id", error))?,
        org_id: row
            .try_get("org_id")
            .map_err(|error| storage_error("row org_id", error))?,
        user_id: row.try_get("user_id").ok(),
        provider_name: row
            .try_get("provider_name")
            .map_err(|error| storage_error("row provider_name", error))?,
        model_name: row.try_get("model_name").ok(),
        capability: capability_from_str(
            row.try_get::<String, _>("capability")
                .map_err(|error| storage_error("row capability", error))?
                .as_str(),
        )?,
        operation_name: row
            .try_get("operation_name")
            .map_err(|error| storage_error("row operation_name", error))?,
        status: status_from_str(
            row.try_get::<String, _>("status")
                .map_err(|error| storage_error("row status", error))?
                .as_str(),
        )?,
        input_tokens: row
            .try_get::<Option<i64>, _>("input_tokens")
            .ok()
            .flatten()
            .map(|value| value as u64),
        output_tokens: row
            .try_get::<Option<i64>, _>("output_tokens")
            .ok()
            .flatten()
            .map(|value| value as u64),
        total_tokens: row
            .try_get::<Option<i64>, _>("total_tokens")
            .ok()
            .flatten()
            .map(|value| value as u64),
        retry_count: row
            .try_get::<i64, _>("retry_count")
            .map_err(|error| storage_error("row retry_count", error))? as u64,
        fallback_used: row
            .try_get("fallback_used")
            .map_err(|error| storage_error("row fallback_used", error))?,
        circuit_blocked: row
            .try_get("circuit_blocked")
            .map_err(|error| storage_error("row circuit_blocked", error))?,
        timed_out: row
            .try_get("timed_out")
            .map_err(|error| storage_error("row timed_out", error))?,
        estimated_cost_usd: row.try_get("estimated_cost_usd").ok(),
        metadata: row.try_get::<Option<Value>, _>("metadata").ok().flatten(),
        created_at: row
            .try_get("created_at")
            .map_err(|error| storage_error("row created_at", error))?,
    })
}

fn push_usage_filters(builder: &mut QueryBuilder<Postgres>, query: &ProviderUsageQuery) {
    builder.push(" WHERE org_id = ");
    builder.push_bind(query.org_id.clone());

    if let Some(user_id) = &query.user_id {
        builder.push(" AND user_id = ");
        builder.push_bind(user_id.clone());
    }
    if let Some(provider_name) = &query.provider_name {
        builder.push(" AND provider_name = ");
        builder.push_bind(provider_name.clone());
    }
    if let Some(model_name) = &query.model_name {
        builder.push(" AND model_name = ");
        builder.push_bind(model_name.clone());
    }
    if let Some(capability) = query.capability {
        builder.push(" AND capability = ");
        builder.push_bind(capability_to_str(capability));
    }
    if let Some(operation_name) = &query.operation_name {
        builder.push(" AND operation_name = ");
        builder.push_bind(operation_name.clone());
    }
    if let Some(created_after) = query.created_after {
        builder.push(" AND created_at >= ");
        builder.push_bind(created_after);
    }
    if let Some(created_before) = query.created_before {
        builder.push(" AND created_at < ");
        builder.push_bind(created_before);
    }
}

fn push_daily_usage_filters(builder: &mut QueryBuilder<Postgres>, query: &ProviderUsageDailyQuery) {
    builder.push(" WHERE org_id = ");
    builder.push_bind(query.org_id.clone());

    if let Some(provider_name) = &query.provider_name {
        builder.push(" AND provider_name = ");
        builder.push_bind(provider_name.clone());
    }
    if let Some(model_name) = &query.model_name {
        builder.push(" AND model_name = ");
        builder.push_bind(model_name.clone());
    }
    if let Some(capability) = query.capability {
        builder.push(" AND capability = ");
        builder.push_bind(capability_to_str(capability));
    }
    builder.push(" AND created_at >= ");
    builder.push_bind(query.created_after);
    builder.push(" AND created_at < ");
    builder.push_bind(query.created_before);
}

fn row_to_daily_bucket(row: &sqlx::postgres::PgRow) -> MemcoreResult<ProviderUsageDailyBucket> {
    Ok(ProviderUsageDailyBucket {
        date: row
            .try_get::<NaiveDate, _>("usage_date")
            .map_err(|error| storage_error("daily usage_date", error))?,
        total_requests: row
            .try_get::<i64, _>("total_requests")
            .map_err(|error| storage_error("daily total_requests", error))?
            as u64,
        total_successes: row
            .try_get::<i64, _>("total_successes")
            .map_err(|error| storage_error("daily total_successes", error))?
            as u64,
        total_errors: row
            .try_get::<i64, _>("total_errors")
            .map_err(|error| storage_error("daily total_errors", error))?
            as u64,
        total_retries: row
            .try_get::<i64, _>("total_retries")
            .map_err(|error| storage_error("daily total_retries", error))?
            as u64,
        total_fallbacks: row
            .try_get::<i64, _>("total_fallbacks")
            .map_err(|error| storage_error("daily total_fallbacks", error))?
            as u64,
        total_circuit_blocks: row
            .try_get::<i64, _>("total_circuit_blocks")
            .map_err(|error| storage_error("daily total_circuit_blocks", error))?
            as u64,
        total_timeouts: row
            .try_get::<i64, _>("total_timeouts")
            .map_err(|error| storage_error("daily total_timeouts", error))?
            as u64,
        total_input_tokens: row
            .try_get::<i64, _>("total_input_tokens")
            .map_err(|error| storage_error("daily total_input_tokens", error))?
            as u64,
        total_output_tokens: row
            .try_get::<i64, _>("total_output_tokens")
            .map_err(|error| storage_error("daily total_output_tokens", error))?
            as u64,
        total_tokens: row
            .try_get::<i64, _>("total_tokens")
            .map_err(|error| storage_error("daily total_tokens", error))?
            as u64,
        total_estimated_cost_usd: row.try_get("total_estimated_cost_usd").ok(),
    })
}

#[derive(Clone, Debug)]
pub struct PostgresProviderUsageStore {
    pool: PgPool,
}

impl PostgresProviderUsageStore {
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

    // Every aggregate is cast to BIGINT: PostgreSQL returns NUMERIC for SUM over BIGINT/INTEGER
    // columns, which does not decode as i64 (it made every summary, and so every write with
    // persistence and quotas on, fail with STORAGE_ERROR). Same casts as `query_usage_daily`.
    async fn query_summary(
        &self,
        query: &ProviderUsageQuery,
    ) -> MemcoreResult<ProviderUsagePersistedSummary> {
        let mut builder = QueryBuilder::<Postgres>::new(
            r#"
            SELECT
                COUNT(*)::bigint AS total_requests,
                COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0)::bigint AS total_successes,
                COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0)::bigint AS total_errors,
                COALESCE(SUM(retry_count), 0)::bigint AS total_retries,
                COALESCE(SUM(CASE WHEN fallback_used THEN 1 ELSE 0 END), 0)::bigint AS total_fallbacks,
                COALESCE(SUM(CASE WHEN circuit_blocked THEN 1 ELSE 0 END), 0)::bigint AS total_circuit_blocks,
                COALESCE(SUM(CASE WHEN timed_out THEN 1 ELSE 0 END), 0)::bigint AS total_timeouts,
                COALESCE(SUM(input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                SUM(estimated_cost_usd) AS total_estimated_cost_usd
            FROM provider_usage_events
            "#,
        );
        push_usage_filters(&mut builder, query);

        let row = builder
            .build()
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("query provider usage summary", error))?;

        Ok(ProviderUsagePersistedSummary {
            total_requests: row
                .try_get::<i64, _>("total_requests")
                .map_err(|error| storage_error("summary total_requests", error))?
                as u64,
            total_successes: row
                .try_get::<i64, _>("total_successes")
                .map_err(|error| storage_error("summary total_successes", error))?
                as u64,
            total_errors: row
                .try_get::<i64, _>("total_errors")
                .map_err(|error| storage_error("summary total_errors", error))?
                as u64,
            total_retries: row
                .try_get::<i64, _>("total_retries")
                .map_err(|error| storage_error("summary total_retries", error))?
                as u64,
            total_fallbacks: row
                .try_get::<i64, _>("total_fallbacks")
                .map_err(|error| storage_error("summary total_fallbacks", error))?
                as u64,
            total_circuit_blocks: row
                .try_get::<i64, _>("total_circuit_blocks")
                .map_err(|error| storage_error("summary total_circuit_blocks", error))?
                as u64,
            total_timeouts: row
                .try_get::<i64, _>("total_timeouts")
                .map_err(|error| storage_error("summary total_timeouts", error))?
                as u64,
            total_input_tokens: row
                .try_get::<i64, _>("total_input_tokens")
                .map_err(|error| storage_error("summary total_input_tokens", error))?
                as u64,
            total_output_tokens: row
                .try_get::<i64, _>("total_output_tokens")
                .map_err(|error| storage_error("summary total_output_tokens", error))?
                as u64,
            total_tokens: row
                .try_get::<i64, _>("total_tokens")
                .map_err(|error| storage_error("summary total_tokens", error))?
                as u64,
            total_estimated_cost_usd: row.try_get("total_estimated_cost_usd").ok(),
        })
    }
}

#[async_trait]
impl ProviderUsageStore for PostgresProviderUsageStore {
    async fn record_usage_event(&self, event: ProviderUsageEventRecord) -> MemcoreResult<()> {
        sqlx::query(
            r#"
            INSERT INTO provider_usage_events (
                id, org_id, user_id, provider_name, model_name, capability, operation_name,
                status, input_tokens, output_tokens, total_tokens, retry_count, fallback_used,
                circuit_blocked, timed_out, estimated_cost_usd, metadata, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
            "#,
        )
        .bind(event.id)
        .bind(&event.org_id)
        .bind(event.user_id.as_deref())
        .bind(&event.provider_name)
        .bind(event.model_name.as_deref())
        .bind(capability_to_str(event.capability))
        .bind(&event.operation_name)
        .bind(status_to_str(event.status))
        .bind(event.input_tokens.map(|value| value as i64))
        .bind(event.output_tokens.map(|value| value as i64))
        .bind(event.total_tokens.map(|value| value as i64))
        .bind(event.retry_count as i64)
        .bind(event.fallback_used)
        .bind(event.circuit_blocked)
        .bind(event.timed_out)
        .bind(event.estimated_cost_usd)
        .bind(event.metadata.as_ref())
        .bind(event.created_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("insert provider usage event", error))?;

        Ok(())
    }

    async fn query_usage(
        &self,
        query: ProviderUsageQuery,
    ) -> MemcoreResult<ProviderUsageQueryResult> {
        let limit = validate_provider_usage_limit(query.limit)?;
        let summary = self.query_summary(&query).await?;

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, user_id, provider_name, model_name, capability, operation_name, status, input_tokens, output_tokens, total_tokens, retry_count, fallback_used, circuit_blocked, timed_out, estimated_cost_usd, metadata, created_at FROM provider_usage_events",
        );
        push_usage_filters(&mut builder, &query);

        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "created_at", "id", cursor);
        }

        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(fetch_limit(limit) as i64);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("query provider usage events", error))?;

        let events: Vec<ProviderUsageEventRecord> = rows
            .iter()
            .map(row_to_event)
            .collect::<MemcoreResult<Vec<_>>>()?;

        let page = build_page(events, limit, |event| PageCursor {
            last_id: event.id.to_string(),
            last_sort_value: event.created_at,
        })?;

        Ok(ProviderUsageQueryResult {
            events: page.items,
            next_cursor: page.next_cursor,
            summary,
        })
    }

    async fn query_usage_daily(
        &self,
        query: ProviderUsageDailyQuery,
    ) -> MemcoreResult<Vec<ProviderUsageDailyBucket>> {
        let mut builder = QueryBuilder::<Postgres>::new(
            r#"
            SELECT
                DATE(created_at AT TIME ZONE 'UTC') AS usage_date,
                COUNT(*)::bigint AS total_requests,
                COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0)::bigint AS total_successes,
                COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0)::bigint AS total_errors,
                COALESCE(SUM(retry_count), 0)::bigint AS total_retries,
                COALESCE(SUM(CASE WHEN fallback_used THEN 1 ELSE 0 END), 0)::bigint AS total_fallbacks,
                COALESCE(SUM(CASE WHEN circuit_blocked THEN 1 ELSE 0 END), 0)::bigint AS total_circuit_blocks,
                COALESCE(SUM(CASE WHEN timed_out THEN 1 ELSE 0 END), 0)::bigint AS total_timeouts,
                COALESCE(SUM(input_tokens), 0)::bigint AS total_input_tokens,
                COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
                COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                SUM(estimated_cost_usd) AS total_estimated_cost_usd
            FROM provider_usage_events
            "#,
        );
        push_daily_usage_filters(&mut builder, &query);
        builder.push(" GROUP BY usage_date ORDER BY usage_date ASC");

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("query daily provider usage", error))?;

        rows.iter().map(row_to_daily_bucket).collect()
    }

    async fn delete_usage_events_older_than(
        &self,
        org_id: &str,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize> {
        if dry_run {
            let row = sqlx::query(
                "SELECT COUNT(*)::bigint AS count FROM provider_usage_events WHERE org_id = $1 AND created_at < $2",
            )
            .bind(org_id)
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                storage_error("failed to count provider usage events for retention", error)
            })?;

            let count: i64 = row
                .try_get("count")
                .map_err(|error| storage_error("row count", error))?;
            return Ok(count as usize);
        }

        let result =
            sqlx::query("DELETE FROM provider_usage_events WHERE org_id = $1 AND created_at < $2")
                .bind(org_id)
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map_err(|error| {
                    storage_error(
                        "failed to delete provider usage events for retention",
                        error,
                    )
                })?;

        Ok(result.rows_affected() as usize)
    }
}
