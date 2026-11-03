//! Migration 0015 (`facts.dedup_key` and its partial unique index) applies to an
//! existing schema-14 database that already holds equal facts (as concurrent additions before the
//! fix could leave; written with the schema-14 columns), backfills nothing, keeps every row
//! readable, and is a no-op when re-run.
//! SQLite always; PostgreSQL on a fresh database with `MEMCORE_TEST_POSTGRES_URL` and the
//! `postgres` feature. Invented data only.

use chrono::Utc;
use memcore_core::ports::{DedupInsert, FactStore};
use memcore_core::{
    ExactDedupRule, Fact, MemorySource, MemoryType, TenantContext, exact_dedup_key,
};
use memcore_storage::SqliteFactStore;
use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

const TEXT: &str = "User prefers green tea in the morning.";

fn tenant() -> TenantContext {
    TenantContext::new("org_ar086_mig", "user_1").unwrap()
}

fn fact(t: &TenantContext) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        t.org_id.clone(),
        t.user_id.clone(),
        MemoryType::Preference,
        TEXT,
        None,
        MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        now,
        now,
        json!({}),
    )
    .unwrap()
}

/// The shared assertions once all migrations ran over two pre-existing equal facts.
async fn after_upgrade(store: &dyn FactStore, existing: &[Uuid]) {
    let t = tenant();
    for id in existing {
        let read = store
            .get_fact(&t, *id)
            .await
            .unwrap()
            .expect("legacy row readable");
        assert_eq!(
            (read.content.as_str(), read.memory_type),
            (TEXT, MemoryType::Preference)
        );
    }
    // Existing rows hold no key, so the first keyed addition is stored (the engine's sequential
    // exact-duplicate check, not the index, recognises it against legacy rows) and a second is not.
    let key = exact_dedup_key(ExactDedupRule::Legacy, &fact(&t));
    let DedupInsert::Inserted(first) = store
        .insert_fact_deduplicated(&t, fact(&t), &key)
        .await
        .unwrap()
    else {
        panic!("legacy rows hold no key")
    };
    assert_eq!(
        store
            .insert_fact_deduplicated(&t, fact(&t), &key)
            .await
            .unwrap(),
        DedupInsert::Duplicate {
            existing_fact_id: first.id
        }
    );
}

#[tokio::test]
async fn ar086_migration_0015_applies_to_an_existing_sqlite_database_with_duplicates() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let all = sqlite_migrations();
    assert!(
        all.iter()
            .any(|m| m.version == 15 && m.name == "fact_dedup_key")
    );
    let before: Vec<_> = all.iter().filter(|m| m.version < 15).cloned().collect();
    runner.apply_pending_migrations(&before).await.unwrap();
    // Two equal facts written with the schema-14 columns (the binary of that schema).
    let t = tenant();
    let existing = vec![Uuid::new_v4(), Uuid::new_v4()];
    let now = Utc::now().to_rfc3339();
    for id in &existing {
        sqlx::query(
            "INSERT INTO facts (id, org_id, user_id, memory_type, content, source, confidence, \
             importance, recorded_at, updated_at, metadata) \
             VALUES (?, ?, ?, 'preference', ?, 'user_message', 0.9, 0.8, ?, ?, '{}')",
        )
        .bind(id.to_string())
        .bind(&t.org_id)
        .bind(&t.user_id)
        .bind(TEXT)
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = SqliteFactStore::new(pool.clone());

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(report.clean, "0015 should apply cleanly: {report:?}");
    let keys: Vec<Option<String>> = sqlx::query_scalar("SELECT dedup_key FROM facts")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(keys, vec![None, None], "nothing is backfilled");
    let index: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'uq_facts_tenant_dedup_key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        index.contains("UNIQUE") && index.contains("deleted_at IS NULL"),
        "{index}"
    );
    after_upgrade(&store, &existing).await;
    assert!(runner.apply_pending_migrations(&all).await.unwrap().clean);
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn ar086_migration_0015_applies_to_an_existing_postgres_database_with_duplicates() {
    use memcore_storage::{PostgresFactStore, PostgresMigrationRunner, postgres_migrations};
    use sqlx::postgres::PgPool;

    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("AR086_POSTGRES_SKIPPED: MEMCORE_TEST_POSTGRES_URL not set");
            return;
        }
    };
    eprintln!("AR086_POSTGRES_RAN");
    let admin = PgPool::connect(&url).await.expect("admin");
    let name = format!("ar086mig_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let db_url = format!("{}/{name}", url.rsplit_once('/').unwrap().0);
    let pool = PgPool::connect(&db_url).await.unwrap();
    let runner = PostgresMigrationRunner::new(pool.clone());
    let all = postgres_migrations();
    let before: Vec<_> = all.iter().filter(|m| m.version < 15).cloned().collect();
    runner.apply_pending_migrations(&before).await.unwrap();
    let t = tenant();
    let existing = vec![Uuid::new_v4(), Uuid::new_v4()];
    for id in &existing {
        sqlx::query(
            "INSERT INTO facts (id, org_id, user_id, memory_type, content, source, confidence, \
             importance, recorded_at, updated_at, metadata) \
             VALUES ($1, $2, $3, 'preference', $4, 'user_message', 0.9, 0.8, now(), now(), '{}'::jsonb)",
        )
        .bind(id)
        .bind(&t.org_id)
        .bind(&t.user_id)
        .bind(TEXT)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PostgresFactStore::new(pool.clone());

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(report.clean, "0015 should apply cleanly: {report:?}");
    let keys: Vec<Option<String>> = sqlx::query_scalar("SELECT dedup_key FROM facts")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(keys, vec![None, None], "nothing is backfilled");
    let index: String = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes WHERE indexname = 'uq_facts_tenant_dedup_key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        index.contains("UNIQUE") && index.contains("deleted_at IS NULL"),
        "{index}"
    );
    after_upgrade(&store, &existing).await;
    assert!(runner.apply_pending_migrations(&all).await.unwrap().clean);

    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
}
