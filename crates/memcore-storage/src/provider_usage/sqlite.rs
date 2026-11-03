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
use sqlx::sqlite::SqlitePool;
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::pagination::push_sqlite_desc_cursor;
use crate::sqlite::{datetime_from_str, datetime_to_str};

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

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

fn i64_to_bool(value: i64) -> bool {
    value != 0
}

fn optional_metadata_to_str(value: &Option<Value>) -> MemcoreResult<Option<String>> {
    match value {
        Some(metadata) => Ok(Some(serde_json::to_string(metadata).map_err(|error| {
            storage_error("serialize provider usage metadata", error)
        })?)),
        None => Ok(None),
    }
}

fn optional_metadata_from_str(value: Option<String>) -> MemcoreResult<Option<Value>> {
    match value {
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => Ok(Some(serde_json::from_str(&raw).map_err(|error| {
            storage_error("deserialize provider usage metadata", error)
        })?)),
        None => Ok(None),
    }
}

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> MemcoreResult<ProviderUsageEventRecord> {
    Ok(ProviderUsageEventRecord {
        id: Uuid::parse_str(
            row.try_get::<String, _>("id")
                .map_err(|error| storage_error("row id", error))?
                .as_str(),
        )
        .map_err(|error| storage_error("row id uuid", error))?,
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
        fallback_used: i64_to_bool(
            row.try_get("fallback_used")
                .map_err(|error| storage_error("row fallback_used", error))?,
        ),
        circuit_blocked: i64_to_bool(
            row.try_get("circuit_blocked")
                .map_err(|error| storage_error("row circuit_blocked", error))?,
        ),
        timed_out: i64_to_bool(
            row.try_get("timed_out")
                .map_err(|error| storage_error("row timed_out", error))?,
        ),
        estimated_cost_usd: row.try_get("estimated_cost_usd").ok(),
        metadata: optional_metadata_from_str(
            row.try_get::<Option<String>, _>("metadata").ok().flatten(),
        )?,
        created_at: datetime_from_str(
            row.try_get::<String, _>("created_at")
                .map_err(|error| storage_error("row created_at", error))?
                .as_str(),
        )?,
    })
}

fn push_usage_filters(builder: &mut QueryBuilder<Sqlite>, query: &ProviderUsageQuery) {
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
        builder.push_bind(datetime_to_str(created_after));
    }
    if let Some(created_before) = query.created_before {
        builder.push(" AND created_at < ");
        builder.push_bind(datetime_to_str(created_before));
    }
}

fn push_daily_usage_filters(builder: &mut QueryBuilder<Sqlite>, query: &ProviderUsageDailyQuery) {
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
    builder.push_bind(datetime_to_str(query.created_after));
    builder.push(" AND created_at < ");
    builder.push_bind(datetime_to_str(query.created_before));
}

fn row_to_daily_bucket(row: &sqlx::sqlite::SqliteRow) -> MemcoreResult<ProviderUsageDailyBucket> {
    let date = NaiveDate::parse_from_str(
        row.try_get::<String, _>("usage_date")
            .map_err(|error| storage_error("daily usage date", error))?
            .as_str(),
        "%Y-%m-%d",
    )
    .map_err(|error| storage_error("parse daily usage date", error))?;

    Ok(ProviderUsageDailyBucket {
        date,
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
pub struct SqliteProviderUsageStore {
    pool: SqlitePool,
}

impl SqliteProviderUsageStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self::new(pool))
    }

    async fn query_summary(
        &self,
        query: &ProviderUsageQuery,
    ) -> MemcoreResult<ProviderUsagePersistedSummary> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                COUNT(*) AS total_requests,
                COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0) AS total_successes,
                COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0) AS total_errors,
                COALESCE(SUM(retry_count), 0) AS total_retries,
                COALESCE(SUM(CASE WHEN fallback_used = 1 THEN 1 ELSE 0 END), 0) AS total_fallbacks,
                COALESCE(SUM(CASE WHEN circuit_blocked = 1 THEN 1 ELSE 0 END), 0) AS total_circuit_blocks,
                COALESCE(SUM(CASE WHEN timed_out = 1 THEN 1 ELSE 0 END), 0) AS total_timeouts,
                COALESCE(SUM(input_tokens), 0) AS total_input_tokens,
                COALESCE(SUM(output_tokens), 0) AS total_output_tokens,
                COALESCE(SUM(total_tokens), 0) AS total_tokens,
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

        let total_cost: Option<f64> = row.try_get("total_estimated_cost_usd").ok();
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
            total_estimated_cost_usd: total_cost,
        })
    }
}

#[async_trait]
impl ProviderUsageStore for SqliteProviderUsageStore {
    async fn record_usage_event(&self, event: ProviderUsageEventRecord) -> MemcoreResult<()> {
        sqlx::query(
            r#"
            INSERT INTO provider_usage_events (
                id, org_id, user_id, provider_name, model_name, capability, operation_name,
                status, input_tokens, output_tokens, total_tokens, retry_count, fallback_used,
                circuit_blocked, timed_out, estimated_cost_usd, metadata, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(event.id.to_string())
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
        .bind(bool_to_i64(event.fallback_used))
        .bind(bool_to_i64(event.circuit_blocked))
        .bind(bool_to_i64(event.timed_out))
        .bind(event.estimated_cost_usd)
        .bind(optional_metadata_to_str(&event.metadata)?)
        .bind(datetime_to_str(event.created_at))
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("insert provider usage event", error))?;

        Ok(())
    }

    async fn query_usage(
        &self,
        query: ProviderUsageQuery,
    ) -> MemcoreResult<ProviderUsageQueryResult> {
        use crate::pagination::fetch_limit;

        let limit = validate_provider_usage_limit(query.limit)?;
        let summary = self.query_summary(&query).await?;

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, user_id, provider_name, model_name, capability, operation_name, status, input_tokens, output_tokens, total_tokens, retry_count, fallback_used, circuit_blocked, timed_out, estimated_cost_usd, metadata, created_at FROM provider_usage_events",
        );
        push_usage_filters(&mut builder, &query);

        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "created_at", "id", cursor);
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
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                substr(created_at, 1, 10) AS usage_date,
                COUNT(*) AS total_requests,
                COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0) AS total_successes,
                COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0) AS total_errors,
                COALESCE(SUM(retry_count), 0) AS total_retries,
                COALESCE(SUM(CASE WHEN fallback_used = 1 THEN 1 ELSE 0 END), 0) AS total_fallbacks,
                COALESCE(SUM(CASE WHEN circuit_blocked = 1 THEN 1 ELSE 0 END), 0) AS total_circuit_blocks,
                COALESCE(SUM(CASE WHEN timed_out = 1 THEN 1 ELSE 0 END), 0) AS total_timeouts,
                COALESCE(SUM(input_tokens), 0) AS total_input_tokens,
                COALESCE(SUM(output_tokens), 0) AS total_output_tokens,
                COALESCE(SUM(total_tokens), 0) AS total_tokens,
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
        let cutoff_str = datetime_to_str(cutoff);

        if dry_run {
            let row = sqlx::query(
                "SELECT COUNT(*) as count FROM provider_usage_events WHERE org_id = ? AND created_at < ?",
            )
            .bind(org_id)
            .bind(cutoff_str)
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
            sqlx::query("DELETE FROM provider_usage_events WHERE org_id = ? AND created_at < ?")
                .bind(org_id)
                .bind(cutoff_str)
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use memcore_core::ports::ProviderUsageQuery;

    async fn test_store() -> SqliteProviderUsageStore {
        SqliteProviderUsageStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite provider usage store")
    }

    fn sample_event(
        org_id: &str,
        user_id: Option<&str>,
        created_at: DateTime<Utc>,
    ) -> ProviderUsageEventRecord {
        ProviderUsageEventRecord {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            user_id: user_id.map(str::to_string),
            provider_name: "mock".to_string(),
            model_name: Some("mock-llm".to_string()),
            capability: ProviderUsageCapability::Llm,
            operation_name: "llm_extract_facts".to_string(),
            status: ProviderCallStatus::Success,
            input_tokens: Some(100),
            output_tokens: Some(20),
            total_tokens: Some(120),
            retry_count: 0,
            fallback_used: false,
            circuit_blocked: false,
            timed_out: false,
            estimated_cost_usd: Some(0.001),
            metadata: None,
            created_at,
        }
    }

    #[tokio::test]
    async fn migration_and_record_work() {
        let store = test_store().await;
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        store
            .record_usage_event(sample_event("org_sqlite", Some("user_a"), ts))
            .await
            .expect("record");

        let result = store
            .query_usage(ProviderUsageQuery::new("org_sqlite", 10))
            .await
            .expect("query");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.summary.total_requests, 1);
        assert!(
            !serde_json::to_string(&result.events[0])
                .expect("json")
                .contains("prompt")
        );
    }

    #[tokio::test]
    async fn org_isolation_and_date_filters_work() {
        let store = test_store().await;
        let early = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mid = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        store
            .record_usage_event(sample_event("org_a", Some("user_a"), early))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event("org_b", Some("user_b"), mid))
            .await
            .expect("record");

        let filtered = store
            .query_usage(ProviderUsageQuery {
                created_after: Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()),
                ..ProviderUsageQuery::new("org_a", 10)
            })
            .await
            .expect("query");
        assert_eq!(filtered.events.len(), 0);

        let org_a = store
            .query_usage(ProviderUsageQuery::new("org_a", 10))
            .await
            .expect("org_a");
        assert_eq!(org_a.events.len(), 1);
    }

    #[tokio::test]
    async fn dry_run_counts_old_usage_events_without_deleting() {
        let store = test_store().await;
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let cutoff = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        store
            .record_usage_event(sample_event("org_ret", Some("user_a"), old))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event("org_ret", Some("user_b"), recent))
            .await
            .expect("record");

        let matched = store
            .delete_usage_events_older_than("org_ret", cutoff, true)
            .await
            .expect("dry-run");
        assert_eq!(matched, 1);

        let result = store
            .query_usage(ProviderUsageQuery::new("org_ret", 10))
            .await
            .expect("query");
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.summary.total_requests, 2);
    }

    #[tokio::test]
    async fn non_dry_run_deletes_old_events_and_updates_summary() {
        let store = test_store().await;
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let cutoff = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        store
            .record_usage_event(sample_event("org_del", Some("user_a"), old))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event("org_del", Some("user_b"), recent))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event("org_other", Some("user_c"), old))
            .await
            .expect("record");

        let deleted = store
            .delete_usage_events_older_than("org_del", cutoff, false)
            .await
            .expect("delete");
        assert_eq!(deleted, 1);

        let org_del = store
            .query_usage(ProviderUsageQuery::new("org_del", 10))
            .await
            .expect("org_del");
        assert_eq!(org_del.events.len(), 1);
        assert_eq!(org_del.summary.total_requests, 1);

        let org_other = store
            .query_usage(ProviderUsageQuery::new("org_other", 10))
            .await
            .expect("org_other");
        assert_eq!(org_other.events.len(), 1);
    }

    #[tokio::test]
    async fn cutoff_uses_created_at_sqlite() {
        let store = test_store().await;
        let before = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let at_cutoff = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap();

        for ts in [before, at_cutoff, after] {
            store
                .record_usage_event(sample_event("org_cut", None, ts))
                .await
                .expect("record");
        }

        let matched = store
            .delete_usage_events_older_than("org_cut", at_cutoff, true)
            .await
            .expect("count");
        assert_eq!(matched, 1);
    }
}
