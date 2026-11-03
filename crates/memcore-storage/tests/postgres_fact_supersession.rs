//! Fact supersession state on Postgres (migration 0013).
//!
//! Requires `MEMCORE_TEST_POSTGRES_URL`; skipped otherwise, like the other Postgres tests. Each test
//! gets a database of its own so 0013 can also be applied on top of an existing one. Postgres
//! backups are managed snapshots or `pg_dump` (memcore has no Postgres backup provider), so backup
//! and restore are covered on SQLite only.

mod supersession_contract;

use memcore_core::ports::FactStore;
use memcore_storage::PostgresFactStore;
use memcore_storage::migrations::{MigrationRunner, PostgresMigrationRunner, postgres_migrations};
use sqlx::postgres::PgPool;
use supersession_contract::{at, tenant};
use uuid::Uuid;

async fn fresh_db(test: &str) -> Option<(PgPool, PgPool, String)> {
    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("skipping postgres test `{test}`: MEMCORE_TEST_POSTGRES_URL not set");
            return None;
        }
    };
    let admin = PgPool::connect(&url).await.expect("admin connection");
    let name = format!("memcore_sup_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("create database");
    let base = url
        .rsplit_once('/')
        .map(|(base, _)| base)
        .expect("url has a path");
    let pool = PgPool::connect(&format!("{base}/{name}"))
        .await
        .expect("test database");
    Some((admin, pool, name))
}

async fn drop_db(admin: PgPool, pool: PgPool, name: String) {
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .expect("drop database");
}

async fn migrated(pool: &PgPool, below: i64) {
    let runner = PostgresMigrationRunner::new(pool.clone());
    let migrations: Vec<_> = postgres_migrations()
        .into_iter()
        .filter(|m| m.version < below)
        .collect();
    let report = runner.apply_pending_migrations(&migrations).await.unwrap();
    assert!(report.clean, "{report:?}");
}

async fn clear(pool: &PgPool) {
    sqlx::query("DELETE FROM facts")
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_supersession_contract() {
    let Some((admin, pool, name)) = fresh_db("postgres_supersession_contract").await else {
        return;
    };
    migrated(&pool, i64::MAX).await;
    let store = PostgresFactStore::new(pool.clone());
    supersession_contract::round_trip(&store).await;
    clear(&pool).await;
    supersession_contract::update(&store).await;
    clear(&pool).await;
    supersession_contract::lifecycle(&store).await;
    drop_db(admin, pool, name).await;
}

/// Existing database: 0013 applies on top of 0001–0012 with rows in place. Existing `invalid_at`
/// values are kept, nothing is backfilled, and re-running is a no-op.
#[tokio::test]
async fn postgres_migration_0013_applies_to_an_existing_database_without_backfill() {
    let Some((admin, pool, name)) =
        fresh_db("postgres_migration_0013_applies_to_an_existing_database_without_backfill").await
    else {
        return;
    };
    migrated(&pool, 13).await;
    let historical = Uuid::new_v4();
    let current = Uuid::new_v4();
    for (id, content, invalid_at) in [
        (historical, "Nadia lived in Leeds.", Some(at(3))),
        (current, "Nadia lives in York.", None),
    ] {
        sqlx::query(
            "INSERT INTO facts (id, org_id, user_id, memory_type, content, summary, source, \
             confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, \
             deleted_at) VALUES ($1, $2, $3, 'preference', $4, NULL, 'user_message', 0.9, 0.8, \
             NULL, $5, $6, $6, '{}'::jsonb, NULL)",
        )
        .bind(id)
        .bind(supersession_contract::ORG)
        .bind(supersession_contract::USER)
        .bind(content)
        .bind(invalid_at)
        .bind(at(1))
        .execute(&pool)
        .await
        .unwrap();
    }

    migrated(&pool, i64::MAX).await;
    let store = PostgresFactStore::new(pool.clone());
    let old = store
        .get_fact(&tenant(), historical)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.invalid_at, Some(at(3)), "existing invalid_at kept");
    assert_eq!((old.superseded_at, old.superseded_by), (None, None));
    assert!(!old.is_active(), "historical invalid_at rows stay inactive");
    let now = store.get_fact(&tenant(), current).await.unwrap().unwrap();
    assert_eq!((now.superseded_at, now.superseded_by), (None, None));
    assert!(now.is_active());
    let index: Option<String> = sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes WHERE indexname = 'idx_facts_tenant_user_superseded_at'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(index.is_some());

    // Re-running is a no-op; the store contract holds on the upgraded database.
    migrated(&pool, i64::MAX).await;
    clear(&pool).await;
    supersession_contract::round_trip(&store).await;
    drop_db(admin, pool, name).await;
}
