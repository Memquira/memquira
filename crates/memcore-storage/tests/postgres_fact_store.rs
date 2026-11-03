//! Postgres FactStore integration tests.
//!
//! Requires `MEMCORE_TEST_POSTGRES_URL` (e.g. `postgres://postgres:postgres@localhost:5432/memcore_test`).
//! Tests are skipped when the variable is unset so normal `cargo test` does not require Postgres.

use chrono::{Duration, Utc};
use memcore_core::{MemorySource, MemoryType, TenantContext};
use memcore_storage::{FactSearchQuery, FactStore, PostgresFactStore};
use serde_json::json;
use uuid::Uuid;

fn postgres_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

async fn test_store() -> Option<PostgresFactStore> {
    let url = postgres_url()?;
    Some(
        PostgresFactStore::connect(&url)
            .await
            .expect("postgres store should connect"),
    )
}

async fn with_postgres_store<F, Fut>(test_name: &str, test: F)
where
    F: FnOnce(PostgresFactStore) -> Fut,
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
    with_postgres_store("insert_and_get_fact", |store| async move {
        let tenant = tenant("org_pg_a", "user_pg_a");
        let fact = sample_fact(
            "org_pg_a",
            "user_pg_a",
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
    })
    .await;
}

#[tokio::test]
async fn update_fact() {
    with_postgres_store("update_fact", |store| async move {
        let tenant = tenant("org_pg_a", "user_pg_b");
        let mut fact = sample_fact(
            "org_pg_a",
            "user_pg_b",
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
    })
    .await;
}

#[tokio::test]
async fn search_facts_by_tenant() {
    with_postgres_store("search_facts_by_tenant", |store| async move {
        let tenant_a = tenant("org_pg_b", "user_pg_a");
        let tenant_b = tenant("org_pg_c", "user_pg_b");

        store
            .insert_fact(
                &tenant_a,
                sample_fact(
                    "org_pg_b",
                    "user_pg_a",
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
                    "org_pg_c",
                    "user_pg_b",
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
        assert_eq!(results[0].org_id, "org_pg_b");
    })
    .await;
}

#[tokio::test]
async fn search_facts_by_memory_type() {
    with_postgres_store("search_facts_by_memory_type", |store| async move {
        let tenant = tenant("org_pg_d", "user_pg_a");

        store
            .insert_fact(
                &tenant,
                sample_fact(
                    "org_pg_d",
                    "user_pg_a",
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
                    "org_pg_d",
                    "user_pg_a",
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
    })
    .await;
}

#[tokio::test]
async fn tenant_isolation_prevents_cross_tenant_reads() {
    with_postgres_store(
        "tenant_isolation_prevents_cross_tenant_reads",
        |store| async move {
            let tenant_a = tenant("org_pg_e", "user_pg_a");
            let tenant_b = tenant("org_pg_e", "user_pg_b");
            let fact = sample_fact(
                "org_pg_e",
                "user_pg_a",
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
        },
    )
    .await;
}

#[tokio::test]
async fn soft_delete_hides_fact_from_normal_search() {
    with_postgres_store(
        "soft_delete_hides_fact_from_normal_search",
        |store| async move {
            let tenant = tenant("org_pg_f", "user_pg_a");
            let fact = sample_fact(
                "org_pg_f",
                "user_pg_a",
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
        },
    )
    .await;
}

#[tokio::test]
async fn include_deleted_returns_soft_deleted_fact() {
    with_postgres_store(
        "include_deleted_returns_soft_deleted_fact",
        |store| async move {
            let tenant = tenant("org_pg_g", "user_pg_a");
            let fact = sample_fact(
                "org_pg_g",
                "user_pg_a",
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
        },
    )
    .await;
}

#[tokio::test]
async fn delete_user_data_removes_only_target_user() {
    with_postgres_store(
        "delete_user_data_removes_only_target_user",
        |store| async move {
            let tenant_a = tenant("org_pg_h", "user_pg_a");
            let tenant_b = tenant("org_pg_h", "user_pg_b");

            store
                .insert_fact(
                    &tenant_a,
                    sample_fact(
                        "org_pg_h",
                        "user_pg_a",
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
                        "org_pg_h",
                        "user_pg_b",
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
        },
    )
    .await;
}

#[tokio::test]
async fn metadata_round_trip_works() {
    with_postgres_store("metadata_round_trip_works", |store| async move {
        let tenant = tenant("org_pg_i", "user_pg_a");
        let fact = sample_fact(
            "org_pg_i",
            "user_pg_a",
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
    })
    .await;
}

#[tokio::test]
async fn valid_and_invalid_at_round_trip_works() {
    with_postgres_store(
        "valid_and_invalid_at_round_trip_works",
        |store| async move {
            let tenant = tenant("org_pg_j", "user_pg_a");
            let valid_at = Utc::now() - Duration::days(2);
            let invalid_at = Utc::now() - Duration::days(1);
            let fact = sample_fact(
                "org_pg_j",
                "user_pg_a",
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

            assert_eq!(
                fetched.valid_at.map(|t| t.timestamp()),
                Some(valid_at.timestamp())
            );
            assert_eq!(
                fetched.invalid_at.map(|t| t.timestamp()),
                Some(invalid_at.timestamp())
            );
        },
    )
    .await;
}

// ---------------------------------------------------------------------------------------------
// Subject, event time and provenance (migration 0010) — P1-03 / P1-04 for Postgres.

fn structured() -> memcore_core::FactStructure {
    use chrono::TimeZone;
    let anchor = Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap();
    memcore_core::FactStructure {
        subject: Some("Priya".to_string()),
        event_time: Some(
            memcore_core::TemporalValue::new(
                Utc.with_ymd_and_hms(2023, 5, 6, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2023, 5, 6, 23, 59, 59).unwrap(),
                memcore_core::TemporalGranularity::Day,
                anchor,
            )
            .unwrap(),
        ),
        event_time_text: Some("last Saturday".to_string()),
        observed_at: Some(anchor),
        session_id: Some("session_1".to_string()),
        source_message_ids: vec!["D1:3".to_string(), "D1:4".to_string()],
    }
}

#[tokio::test]
async fn postgres_structured_fields_round_trip_update_and_delete() {
    with_postgres_store(
        "postgres_structured_fields_round_trip_update_and_delete",
        |store| async move {
            let org = format!("org_pb_{}", Uuid::new_v4().simple());
            let tenant = tenant(&org, "user_pb");
            let mut fact = sample_fact(
                &org,
                "user_pb",
                "Priya ran a half marathon.",
                MemoryType::Event,
                json!({}),
                None,
                None,
            )
            .with_structure(structured());
            store
                .insert_fact(&tenant, fact.clone())
                .await
                .expect("insert");

            let loaded = store
                .get_fact(&tenant, fact.id)
                .await
                .expect("get")
                .expect("exists");
            assert_eq!(loaded.structure, fact.structure);

            let found = store
                .search_facts(FactSearchQuery::new(tenant.clone(), 10))
                .await
                .expect("search");
            assert_eq!(found[0].structure, fact.structure);

            fact.structure.source_message_ids.push("D2:1".to_string());
            store
                .update_fact(&tenant, fact.clone())
                .await
                .expect("update");
            let updated = store
                .get_fact(&tenant, fact.id)
                .await
                .expect("get")
                .expect("exists");
            assert_eq!(
                updated.structure.source_message_ids,
                vec!["D1:3", "D1:4", "D2:1"]
            );

            store.delete_user_data(&tenant).await.expect("delete");
            assert!(
                store
                    .get_fact(&tenant, fact.id)
                    .await
                    .expect("get")
                    .is_none()
            );
        },
    )
    .await;
}

/// P1-03 "existing database" on Postgres: migrate a brand-new database to 0009, write a row the
/// pre-Part-B way, then apply 0010 and read the old row back unchanged.
#[tokio::test]
async fn postgres_migration_0010_applies_to_an_existing_database() {
    use memcore_storage::migrations::{
        MigrationRunner, PostgresMigrationRunner, postgres_migrations,
    };

    let Some(base_url) = postgres_url() else {
        eprintln!(
            "skipping postgres_migration_0010_applies_to_an_existing_database: MEMCORE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let admin = sqlx::postgres::PgPool::connect(&base_url)
        .await
        .expect("admin connect");
    let db_name = format!("memcore_p1_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {db_name}"))
        .execute(&admin)
        .await
        .expect("create db");
    let (prefix, _) = base_url.rsplit_once('/').expect("url has a database name");
    let url = format!("{prefix}/{db_name}");

    let result = async {
        let pool = sqlx::postgres::PgPool::connect(&url).await.expect("connect new db");
        let runner = PostgresMigrationRunner::new(pool.clone());
        let all = postgres_migrations();
        let before_part_b: Vec<_> = all.iter().filter(|m| m.version < 10).cloned().collect();
        runner.apply_pending_migrations(&before_part_b).await.expect("apply 1-9");

        let old_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO facts (id, org_id, user_id, memory_type, content, summary, source, confidence, \
             importance, valid_at, invalid_at, recorded_at, updated_at, metadata, deleted_at) \
             VALUES ($1, 'org_pb', 'user_pb', 'profile', 'Written before Part B', NULL, 'user_message', \
             0.9, 0.8, NULL, NULL, now(), now(), '{}'::jsonb, NULL)",
        )
        .bind(old_id)
        .execute(&pool)
        .await
        .expect("insert legacy row");

        let report = runner.apply_pending_migrations(&all).await.expect("apply 0010");
        assert!(report.clean, "0010 should apply cleanly: {report:?}");

        let store = PostgresFactStore::new(pool.clone());
        let tenant = tenant("org_pb", "user_pb");
        let old = store.get_fact(&tenant, old_id).await.expect("get").expect("exists");
        assert_eq!(old.content, "Written before Part B");
        assert!(old.structure.is_empty());
        pool.close().await;
    }
    .await;

    sqlx::query(&format!("DROP DATABASE IF EXISTS {db_name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .expect("drop db");
    result
}
