use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{MemoryEvent, TenantContext};
use sqlx::sqlite::SqlitePool;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::sqlite::conversions::{
    datetime_to_str, memory_event_operation_to_str, metadata_to_str, optional_uuid_to_str,
    row_to_memory_event,
};
use memcore_core::ports::{MemoryEventQuery, MemoryEventStore, OrgMemoryEventQuery};

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

fn parse_event_row(row: &sqlx::sqlite::SqliteRow) -> MemcoreResult<MemoryEvent> {
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
pub struct SqliteMemoryEventStore {
    pool: SqlitePool,
}

impl SqliteMemoryEventStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self::new(pool))
    }
}

#[async_trait]
impl MemoryEventStore for SqliteMemoryEventStore {
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(event.id.to_string())
        .bind(&event.org_id)
        .bind(&event.user_id)
        .bind(optional_uuid_to_str(event.fact_id))
        .bind(memory_event_operation_to_str(event.operation))
        .bind(&event.input_text)
        .bind(&event.previous_content)
        .bind(&event.new_content)
        .bind(&event.provider_name)
        .bind(&event.model_name)
        .bind(metadata_to_str(&event.metadata)?)
        .bind(datetime_to_str(event.created_at))
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to insert memory event", error))?;

        Ok(event)
    }

    async fn list_events(&self, query: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        use crate::keyword_search::push_sqlite_event_keyword_filter;
        use crate::pagination::{fetch_limit, push_sqlite_desc_cursor};

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, user_id, fact_id, operation, input_text, previous_content, new_content, provider_name, model_name, metadata, created_at FROM memory_events WHERE org_id = ",
        );
        builder.push_bind(query.tenant.org_id.clone());
        builder.push(" AND user_id = ");
        builder.push_bind(query.tenant.user_id.clone());

        if let Some(fact_id) = query.fact_id {
            builder.push(" AND fact_id = ");
            builder.push_bind(fact_id.to_string());
        }

        if let Some(operation) = query.operation {
            builder.push(" AND operation = ");
            builder.push_bind(memory_event_operation_to_str(operation));
        }

        if let Some(created_after) = query.created_after {
            builder.push(" AND created_at >= ");
            builder.push_bind(datetime_to_str(created_after));
        }

        if let Some(created_before) = query.created_before {
            builder.push(" AND created_at < ");
            builder.push_bind(datetime_to_str(created_before));
        }

        if let Some(query_text) = &query.query_text {
            push_sqlite_event_keyword_filter(&mut builder, query_text, false);
        }

        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "created_at", "id", cursor);
        }

        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(
            i64::try_from(fetch_limit(query.limit)).map_err(|error| {
                storage_error("event list limit out of range for sqlite", error)
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
        use crate::keyword_search::push_sqlite_event_keyword_filter;
        use crate::pagination::{fetch_limit, push_sqlite_desc_cursor};

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, user_id, fact_id, operation, input_text, previous_content, new_content, provider_name, model_name, metadata, created_at FROM memory_events WHERE org_id = ",
        );
        builder.push_bind(query.org_id.clone());

        if let Some(user_id) = &query.user_id {
            builder.push(" AND user_id = ");
            builder.push_bind(user_id.clone());
        }

        if let Some(fact_id) = query.fact_id {
            builder.push(" AND fact_id = ");
            builder.push_bind(fact_id.to_string());
        }

        if let Some(operation) = query.operation {
            builder.push(" AND operation = ");
            builder.push_bind(memory_event_operation_to_str(operation));
        }

        if let Some(created_after) = query.created_after {
            builder.push(" AND created_at >= ");
            builder.push_bind(datetime_to_str(created_after));
        }

        if let Some(created_before) = query.created_before {
            builder.push(" AND created_at < ");
            builder.push_bind(datetime_to_str(created_before));
        }

        if let Some(query_text) = &query.query_text {
            push_sqlite_event_keyword_filter(&mut builder, query_text, true);
        }

        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "created_at", "id", cursor);
        }

        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(i64::try_from(fetch_limit(query.limit)).map_err(|error| {
            storage_error("org event list limit out of range for sqlite", error)
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
        let cutoff_str = datetime_to_str(cutoff);

        if dry_run {
            let row = sqlx::query(
                "SELECT COUNT(*) as count FROM memory_events WHERE org_id = ? AND user_id = ? AND created_at < ?",
            )
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(cutoff_str)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count events for retention", error))?;

            let count: i64 = row
                .try_get("count")
                .map_err(|error| storage_error("row count", error))?;
            return Ok(count as usize);
        }

        let result = sqlx::query(
            "DELETE FROM memory_events WHERE org_id = ? AND user_id = ? AND created_at < ?",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(cutoff_str)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to delete events for retention", error))?;

        Ok(result.rows_affected() as usize)
    }

    async fn count_events_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM memory_events WHERE org_id = ?")
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
            "DELETE FROM memory_events WHERE org_id = ? AND user_id = ? AND NOT (operation = 'forget_user' AND fact_id IS NULL AND input_text IS NULL AND previous_content IS NULL AND new_content IS NULL AND model_name IS NULL AND (provider_name IS NULL OR provider_name = 'maintenance') AND metadata = '{\"deleted\":true}')",
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
            "SELECT COUNT(*) as count FROM memory_events WHERE org_id = ? AND user_id = ? AND NOT (operation = 'forget_user' AND fact_id IS NULL AND input_text IS NULL AND previous_content IS NULL AND new_content IS NULL AND model_name IS NULL AND (provider_name IS NULL OR provider_name = 'maintenance') AND metadata = '{\"deleted\":true}')",
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use chrono::{TimeZone, Utc};
    use memcore_common::MemcoreError;
    use memcore_core::{
        MemoryEvent, MemoryEventOperation, PageCursor, TenantContext, build_page, decode_cursor,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::SqliteMemoryEventStore;
    use crate::traits::MemoryEventStore;
    use memcore_core::ports::{MemoryEventQuery, OrgMemoryEventQuery};

    async fn test_store() -> SqliteMemoryEventStore {
        SqliteMemoryEventStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite event store should connect")
    }

    fn tenant(org_id: &str, user_id: &str) -> TenantContext {
        TenantContext::new(org_id, user_id).expect("tenant should be valid")
    }

    fn sample_event(
        org_id: &str,
        user_id: &str,
        fact_id: Option<Uuid>,
        operation: MemoryEventOperation,
        metadata: serde_json::Value,
    ) -> MemoryEvent {
        MemoryEvent::new(
            org_id,
            user_id,
            fact_id,
            operation,
            Some("previous".to_string()),
            Some("new".to_string()),
            Some("mock".to_string()),
            Some("mock-llm".to_string()),
            metadata,
        )
    }

    #[tokio::test]
    async fn record_event_stores_event() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let fact_id = Uuid::new_v4();
        let event = sample_event(
            "org_a",
            "user_a",
            Some(fact_id),
            MemoryEventOperation::Add,
            json!({ "source": "test" }),
        );

        store
            .record_event(&tenant, event.clone())
            .await
            .expect("record should succeed");

        let listed = store
            .list_events(MemoryEventQuery::new(tenant, 10))
            .await
            .expect("list should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, event.id);
        assert_eq!(listed[0].fact_id, Some(fact_id));
    }

    #[tokio::test]
    async fn list_events_returns_tenant_events() {
        let store = test_store().await;
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_b", "user_b");

        store
            .record_event(
                &tenant_a,
                sample_event(
                    "org_a",
                    "user_a",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");
        store
            .record_event(
                &tenant_b,
                sample_event(
                    "org_b",
                    "user_b",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");

        let listed_a = store
            .list_events(MemoryEventQuery::new(tenant_a, 10))
            .await
            .expect("list should succeed");
        assert_eq!(listed_a.len(), 1);
        assert_eq!(listed_a[0].org_id, "org_a");

        let listed_b = store
            .list_events(MemoryEventQuery::new(tenant_b, 10))
            .await
            .expect("list should succeed");
        assert_eq!(listed_b.len(), 1);
        assert_eq!(listed_b[0].org_id, "org_b");
    }

    #[tokio::test]
    async fn tenant_isolation_prevents_cross_tenant_list() {
        let store = test_store().await;
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");

        store
            .record_event(
                &tenant_a,
                sample_event(
                    "org_a",
                    "user_a",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");

        let listed_b = store
            .list_events(MemoryEventQuery::new(tenant_b, 10))
            .await
            .expect("list should succeed");
        assert!(listed_b.is_empty());
    }

    #[tokio::test]
    async fn fact_id_filter_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let fact_a = Uuid::new_v4();
        let fact_b = Uuid::new_v4();

        store
            .record_event(
                &tenant,
                sample_event(
                    "org_a",
                    "user_a",
                    Some(fact_a),
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");
        store
            .record_event(
                &tenant,
                sample_event(
                    "org_a",
                    "user_a",
                    Some(fact_b),
                    MemoryEventOperation::Update,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");

        let mut query = MemoryEventQuery::new(tenant, 10);
        query.fact_id = Some(fact_a);
        let listed = store.list_events(query).await.expect("list should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].fact_id, Some(fact_a));
    }

    #[tokio::test]
    async fn operation_filter_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");

        store
            .record_event(
                &tenant,
                sample_event(
                    "org_a",
                    "user_a",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");
        store
            .record_event(
                &tenant,
                sample_event(
                    "org_a",
                    "user_a",
                    None,
                    MemoryEventOperation::Delete,
                    json!({}),
                ),
            )
            .await
            .expect("record should succeed");

        let mut query = MemoryEventQuery::new(tenant, 10);
        query.operation = Some(MemoryEventOperation::Delete);
        let listed = store.list_events(query).await.expect("list should succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].operation, MemoryEventOperation::Delete);
    }

    #[tokio::test]
    async fn limit_is_respected() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");

        for index in 0..5 {
            store
                .record_event(
                    &tenant,
                    sample_event(
                        "org_a",
                        "user_a",
                        None,
                        MemoryEventOperation::Add,
                        json!({ "index": index }),
                    ),
                )
                .await
                .expect("record should succeed");
        }

        let listed = store
            .list_events(MemoryEventQuery::new(tenant, 2))
            .await
            .expect("list should succeed");
        // Store over-fetches by one for cursor pagination; engine trims to `limit`.
        assert_eq!(listed.len(), 3);
    }

    /// Walks every page the way `MemoryEngine::list_memory_events` does: store over-fetch, then
    /// `build_page`, then the decoded `next_cursor` for the following page.
    async fn walk_pages(
        store: &SqliteMemoryEventStore,
        tenant: &TenantContext,
        limit: usize,
    ) -> (Vec<usize>, Vec<MemoryEvent>) {
        let mut cursor = None;
        let mut sizes = Vec::new();
        let mut seen = Vec::new();
        loop {
            let mut query = MemoryEventQuery::new(tenant.clone(), limit);
            query.cursor = cursor.take();
            let fetched = store.list_events(query).await.expect("list should succeed");
            assert!(
                fetched.len() <= limit + 1,
                "store returns at most limit + 1 rows"
            );
            let page = build_page(fetched, limit, |event: &MemoryEvent| PageCursor {
                last_id: event.id.to_string(),
                last_sort_value: event.created_at,
            })
            .expect("page should build");
            sizes.push(page.items.len());
            seen.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(decode_cursor(&next).expect("cursor should decode")),
                None => break,
            }
            assert!(sizes.len() <= 10, "pagination must terminate");
        }
        (sizes, seen)
    }

    #[tokio::test]
    async fn paginated_listing_has_no_gaps_or_duplicates() {
        let store = test_store().await;
        // 5 events -> pages 2, 2, 1; 4 events (an exact multiple) -> pages 2, 2 with no empty third page.
        for (count, expected_sizes) in [(5usize, vec![2usize, 2, 1]), (4, vec![2, 2])] {
            // A fresh user per case: the shared in-memory database is used by other tests too.
            let user = format!("user_pages_{}", Uuid::new_v4().simple());
            let tenant = tenant("org_pages", &user);
            let mut recorded = HashSet::new();
            for index in 0..count {
                let event = store
                    .record_event(
                        &tenant,
                        sample_event(
                            "org_pages",
                            &user,
                            None,
                            MemoryEventOperation::Add,
                            json!({ "index": index }),
                        ),
                    )
                    .await
                    .expect("record should succeed");
                recorded.insert(event.id);
            }

            let (sizes, seen) = walk_pages(&store, &tenant, 2).await;
            assert_eq!(sizes, expected_sizes);
            let ids: Vec<Uuid> = seen.iter().map(|event| event.id).collect();
            let unique: HashSet<Uuid> = ids.iter().copied().collect();
            assert_eq!(unique.len(), ids.len(), "no duplicates across pages");
            assert_eq!(unique, recorded, "no missing records");
            assert!(
                seen.windows(2)
                    .all(|pair| (pair[0].created_at, pair[0].id.to_string())
                        > (pair[1].created_at, pair[1].id.to_string())),
                "newest first, exact timestamp ties broken by id DESC"
            );
        }
    }

    #[tokio::test]
    async fn metadata_round_trip_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let metadata = json!({ "nested": { "flag": true }, "count": 3 });

        store
            .record_event(
                &tenant,
                sample_event(
                    "org_a",
                    "user_a",
                    None,
                    MemoryEventOperation::Add,
                    metadata.clone(),
                ),
            )
            .await
            .expect("record should succeed");

        let listed = store
            .list_events(MemoryEventQuery::new(tenant, 10))
            .await
            .expect("list should succeed");
        assert_eq!(listed[0].metadata, metadata);
    }

    #[tokio::test]
    async fn created_at_round_trip_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let created_at = Utc.with_ymd_and_hms(2026, 3, 15, 12, 30, 0).unwrap();
        let mut event = sample_event(
            "org_a",
            "user_a",
            None,
            MemoryEventOperation::NoOp,
            json!({}),
        );
        event.created_at = created_at;

        store
            .record_event(&tenant, event)
            .await
            .expect("record should succeed");

        let listed = store
            .list_events(MemoryEventQuery::new(tenant, 10))
            .await
            .expect("list should succeed");
        assert_eq!(listed[0].created_at, created_at);
    }

    #[tokio::test]
    async fn optional_fields_null_works() {
        let store = test_store().await;
        let tenant = tenant("org_a", "user_a");
        let event = MemoryEvent::new(
            "org_a",
            "user_a",
            None,
            MemoryEventOperation::ForgetUser,
            None,
            None,
            None,
            None,
            json!({ "deleted": true }),
        );

        store
            .record_event(&tenant, event)
            .await
            .expect("record should succeed");

        let listed = store
            .list_events(MemoryEventQuery::new(tenant, 10))
            .await
            .expect("list should succeed");
        assert!(listed[0].fact_id.is_none());
        assert!(listed[0].previous_content.is_none());
        assert!(listed[0].new_content.is_none());
        assert!(listed[0].input_text.is_none());
    }

    #[tokio::test]
    async fn mismatched_tenant_is_rejected_on_record() {
        let store = test_store().await;
        let tenant_b = tenant("org_a", "user_b");
        let event = sample_event(
            "org_a",
            "user_a",
            None,
            MemoryEventOperation::Add,
            json!({}),
        );

        let error = store
            .record_event(&tenant_b, event)
            .await
            .expect_err("cross-tenant record should fail");
        assert_eq!(error, MemcoreError::Forbidden);
    }

    #[tokio::test]
    async fn list_events_by_org_filters_and_scopes() {
        use memcore_core::ports::OrgMemoryEventQuery;

        let store = test_store().await;
        let tenant_a = tenant("org_sqlite_audit", "user_a");
        let tenant_b = tenant("org_sqlite_audit", "user_b");
        let other_org = tenant("org_other", "user_x");
        let fact_id = Uuid::new_v4();

        store
            .record_event(
                &tenant_a,
                sample_event(
                    "org_sqlite_audit",
                    "user_a",
                    Some(fact_id),
                    MemoryEventOperation::Update,
                    json!({}),
                ),
            )
            .await
            .expect("record");
        store
            .record_event(
                &tenant_b,
                sample_event(
                    "org_sqlite_audit",
                    "user_b",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record");
        store
            .record_event(
                &other_org,
                sample_event(
                    "org_other",
                    "user_x",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record");

        let all = store
            .list_events_by_org(OrgMemoryEventQuery::new("org_sqlite_audit".to_string(), 10))
            .await
            .expect("list by org");
        assert_eq!(all.len(), 2);

        let user_a = store
            .list_events_by_org(OrgMemoryEventQuery {
                org_id: "org_sqlite_audit".to_string(),
                user_id: Some("user_a".to_string()),
                fact_id: None,
                operation: None,
                created_after: None,
                created_before: None,
                query_text: None,
                limit: 10,
                cursor: None,
            })
            .await
            .expect("list by user");
        assert_eq!(user_a.len(), 1);
        assert_eq!(user_a[0].fact_id, Some(fact_id));
    }

    async fn record_event_at(
        store: &SqliteMemoryEventStore,
        tenant: &TenantContext,
        org_id: &str,
        user_id: &str,
        created_at: chrono::DateTime<Utc>,
    ) {
        let mut event = sample_event(org_id, user_id, None, MemoryEventOperation::Add, json!({}));
        event.created_at = created_at;
        store.record_event(tenant, event).await.expect("record");
    }

    #[tokio::test]
    async fn list_events_filters_by_created_after() {
        let store = test_store().await;
        let tenant = tenant("org_date", "user_a");
        let jan = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let mar = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();

        record_event_at(&store, &tenant, "org_date", "user_a", jan).await;
        record_event_at(&store, &tenant, "org_date", "user_a", mar).await;

        let mut query = MemoryEventQuery::new(tenant, 10);
        query.created_after = Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap());

        let listed = store.list_events(query).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].created_at, mar);
    }

    #[tokio::test]
    async fn list_events_filters_by_created_before() {
        let store = test_store().await;
        let tenant = tenant("org_date", "user_a");
        let jan = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let mar = Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();

        record_event_at(&store, &tenant, "org_date", "user_a", jan).await;
        record_event_at(&store, &tenant, "org_date", "user_a", mar).await;

        let mut query = MemoryEventQuery::new(tenant, 10);
        query.created_before = Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap());

        let listed = store.list_events(query).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].created_at, jan);
    }

    #[tokio::test]
    async fn list_events_by_org_filters_by_date_range() {
        use memcore_core::ports::OrgMemoryEventQuery;

        let store = test_store().await;
        let tenant_a = tenant("org_range", "user_a");
        let tenant_b = tenant("org_range", "user_b");
        let other_org = tenant("org_other", "user_x");
        let early = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
        let mid = Utc.with_ymd_and_hms(2026, 3, 10, 0, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap();

        record_event_at(&store, &tenant_a, "org_range", "user_a", early).await;
        record_event_at(&store, &tenant_a, "org_range", "user_a", mid).await;
        record_event_at(&store, &other_org, "org_other", "user_x", mid).await;
        record_event_at(&store, &tenant_b, "org_range", "user_b", late).await;

        let listed = store
            .list_events_by_org(OrgMemoryEventQuery {
                org_id: "org_range".to_string(),
                user_id: None,
                fact_id: None,
                operation: None,
                created_after: Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()),
                created_before: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
                query_text: None,
                limit: 10,
                cursor: None,
            })
            .await
            .expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].created_at, mid);
        assert_eq!(listed[0].user_id, "user_a");
    }

    #[tokio::test]
    async fn date_filter_does_not_leak_other_user_events() {
        let store = test_store().await;
        let tenant_a = tenant("org_user_date", "user_a");
        let tenant_b = tenant("org_user_date", "user_b");
        let ts = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();

        record_event_at(&store, &tenant_a, "org_user_date", "user_a", ts).await;
        record_event_at(&store, &tenant_b, "org_user_date", "user_b", ts).await;

        let mut query = MemoryEventQuery::new(tenant_a, 10);
        query.created_after = Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());

        let listed = store.list_events(query).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].user_id, "user_a");
    }

    #[tokio::test]
    async fn list_events_keyword_search_matches_new_content() {
        let store = test_store().await;
        let tenant = tenant("org_kw", "user_a");
        let mut rust_event = sample_event(
            "org_kw",
            "user_a",
            None,
            MemoryEventOperation::Add,
            json!({}),
        );
        rust_event.new_content = Some("rust async runtime".to_string());
        store
            .record_event(&tenant, rust_event)
            .await
            .expect("record");

        let mut other = sample_event(
            "org_kw",
            "user_a",
            None,
            MemoryEventOperation::Add,
            json!({}),
        );
        other.new_content = Some("python only".to_string());
        store.record_event(&tenant, other).await.expect("record");

        let mut query = MemoryEventQuery::new(tenant, 10);
        query.query_text = Some("rust".to_string());
        let listed = store.list_events(query).await.expect("list");
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn list_events_by_org_keyword_search_includes_user_id() {
        let store = test_store().await;
        let tenant_a = tenant("org_kw_admin", "user_alpha");
        let tenant_b = tenant("org_kw_admin", "user_beta");
        store
            .record_event(
                &tenant_a,
                sample_event(
                    "org_kw_admin",
                    "user_alpha",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record");
        store
            .record_event(
                &tenant_b,
                sample_event(
                    "org_kw_admin",
                    "user_beta",
                    None,
                    MemoryEventOperation::Add,
                    json!({}),
                ),
            )
            .await
            .expect("record");

        let mut query = OrgMemoryEventQuery::new("org_kw_admin".to_string(), 10);
        query.query_text = Some("alpha".to_string());
        let listed = store.list_events_by_org(query).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].user_id, "user_alpha");
    }
}
