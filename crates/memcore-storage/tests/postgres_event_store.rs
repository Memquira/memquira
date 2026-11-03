//! Postgres MemoryEventStore integration tests.
//!
//! Requires `MEMCORE_TEST_POSTGRES_URL` (e.g. `postgres://postgres:postgres@localhost:5432/memcore_test`).
//! Tests are skipped when the variable is unset so normal `cargo test` does not require Postgres.

use std::collections::HashSet;

use chrono::{TimeZone, Utc};
use memcore_common::MemcoreError;
use memcore_core::{
    MemoryEvent, MemoryEventOperation, PageCursor, TenantContext, build_page, decode_cursor,
};
use memcore_storage::{MemoryEventQuery, MemoryEventStore, PostgresMemoryEventStore};
use serde_json::json;
use uuid::Uuid;

fn postgres_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

async fn test_store() -> Option<PostgresMemoryEventStore> {
    let url = postgres_url()?;
    Some(
        PostgresMemoryEventStore::connect(&url)
            .await
            .expect("postgres event store should connect"),
    )
}

async fn with_postgres_store<F, Fut>(test_name: &str, test: F)
where
    F: FnOnce(PostgresMemoryEventStore) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Some(store) = test_store().await else {
        eprintln!("skipping postgres test `{test_name}`: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    test(store).await;
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
    with_postgres_store("record_event_stores_event", |store| async move {
        let tenant = tenant("org_pg_evt_a", "user_pg_evt_a");
        let fact_id = Uuid::new_v4();
        let event = sample_event(
            "org_pg_evt_a",
            "user_pg_evt_a",
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
    })
    .await;
}

#[tokio::test]
async fn list_events_returns_tenant_events() {
    with_postgres_store("list_events_returns_tenant_events", |store| async move {
        let tenant_a = tenant("org_pg_evt_b", "user_pg_evt_b");
        let tenant_b = tenant("org_pg_evt_c", "user_pg_evt_c");

        store
            .record_event(
                &tenant_a,
                sample_event(
                    "org_pg_evt_b",
                    "user_pg_evt_b",
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
                    "org_pg_evt_c",
                    "user_pg_evt_c",
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
        assert_eq!(listed_a[0].org_id, "org_pg_evt_b");

        let listed_b = store
            .list_events(MemoryEventQuery::new(tenant_b, 10))
            .await
            .expect("list should succeed");
        assert_eq!(listed_b.len(), 1);
        assert_eq!(listed_b[0].org_id, "org_pg_evt_c");
    })
    .await;
}

#[tokio::test]
async fn tenant_isolation_prevents_cross_tenant_list() {
    with_postgres_store(
        "tenant_isolation_prevents_cross_tenant_list",
        |store| async move {
            let tenant_a = tenant("org_pg_evt_d", "user_pg_evt_d");
            let tenant_b = tenant("org_pg_evt_d", "user_pg_evt_e");

            store
                .record_event(
                    &tenant_a,
                    sample_event(
                        "org_pg_evt_d",
                        "user_pg_evt_d",
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
        },
    )
    .await;
}

#[tokio::test]
async fn fact_id_filter_works() {
    with_postgres_store("fact_id_filter_works", |store| async move {
        let tenant = tenant("org_pg_evt_f", "user_pg_evt_f");
        let fact_a = Uuid::new_v4();
        let fact_b = Uuid::new_v4();

        store
            .record_event(
                &tenant,
                sample_event(
                    "org_pg_evt_f",
                    "user_pg_evt_f",
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
                    "org_pg_evt_f",
                    "user_pg_evt_f",
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
    })
    .await;
}

#[tokio::test]
async fn operation_filter_works() {
    with_postgres_store("operation_filter_works", |store| async move {
        let tenant = tenant("org_pg_evt_g", "user_pg_evt_g");

        store
            .record_event(
                &tenant,
                sample_event(
                    "org_pg_evt_g",
                    "user_pg_evt_g",
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
                    "org_pg_evt_g",
                    "user_pg_evt_g",
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
    })
    .await;
}

#[tokio::test]
async fn limit_is_respected() {
    with_postgres_store("limit_is_respected", |store| async move {
        let tenant = tenant("org_pg_evt_h", "user_pg_evt_h");

        for index in 0..5 {
            store
                .record_event(
                    &tenant,
                    sample_event(
                        "org_pg_evt_h",
                        "user_pg_evt_h",
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
        // Store over-fetches by one for cursor pagination; engine trims to `limit` (same contract
        // as the SQLite store; see `MemoryEventStore::list_events`).
        assert_eq!(listed.len(), 3);
    })
    .await;
}

/// Walks every page the way `MemoryEngine::list_memory_events` does: store over-fetch, then
/// `build_page`, then the decoded `next_cursor` for the following page.
async fn walk_pages(
    store: &PostgresMemoryEventStore,
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
    with_postgres_store(
        "paginated_listing_has_no_gaps_or_duplicates",
        |store| async move {
            // 5 events -> pages 2, 2, 1; 4 events (an exact multiple) -> pages 2, 2 with no empty third page.
            for (count, expected_sizes) in [(5usize, vec![2usize, 2, 1]), (4, vec![2, 2])] {
                // A fresh user per run keeps the test rerunnable on the same database.
                let user = format!("user_pg_evt_pages_{}", Uuid::new_v4().simple());
                let tenant = tenant("org_pg_evt_pages", &user);
                let mut recorded = HashSet::new();
                for index in 0..count {
                    let event = store
                        .record_event(
                            &tenant,
                            sample_event(
                                "org_pg_evt_pages",
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
        },
    )
    .await;
}

#[tokio::test]
async fn metadata_round_trip_works() {
    with_postgres_store("metadata_round_trip_works", |store| async move {
        let tenant = tenant("org_pg_evt_i", "user_pg_evt_i");
        let metadata = json!({ "nested": { "flag": true }, "count": 3 });

        store
            .record_event(
                &tenant,
                sample_event(
                    "org_pg_evt_i",
                    "user_pg_evt_i",
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
    })
    .await;
}

#[tokio::test]
async fn created_at_round_trip_works() {
    with_postgres_store("created_at_round_trip_works", |store| async move {
        let tenant = tenant("org_pg_evt_j", "user_pg_evt_j");
        let created_at = Utc.with_ymd_and_hms(2026, 3, 15, 12, 30, 0).unwrap();
        let mut event = sample_event(
            "org_pg_evt_j",
            "user_pg_evt_j",
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
    })
    .await;
}

#[tokio::test]
async fn optional_fields_null_works() {
    with_postgres_store("optional_fields_null_works", |store| async move {
        let tenant = tenant("org_pg_evt_k", "user_pg_evt_k");
        let event = MemoryEvent::new(
            "org_pg_evt_k",
            "user_pg_evt_k",
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
    })
    .await;
}

#[tokio::test]
async fn mismatched_tenant_is_rejected_on_record() {
    with_postgres_store(
        "mismatched_tenant_is_rejected_on_record",
        |store| async move {
            let tenant_b = tenant("org_pg_evt_l", "user_pg_evt_m");
            let event = sample_event(
                "org_pg_evt_l",
                "user_pg_evt_l",
                None,
                MemoryEventOperation::Add,
                json!({}),
            );

            let error = store
                .record_event(&tenant_b, event)
                .await
                .expect_err("cross-tenant record should fail");
            assert_eq!(error, MemcoreError::Forbidden);
        },
    )
    .await;
}
