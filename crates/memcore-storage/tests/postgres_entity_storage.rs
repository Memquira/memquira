//! Entity storage on Postgres (migration 0014).
//!
//! Requires `MEMCORE_TEST_POSTGRES_URL`; skipped otherwise, like the other Postgres tests. Each test
//! gets a database of its own so 0014 can also be applied on top of an existing one.

mod entity_contract;

use memcore_core::ports::EntityStore;
use memcore_storage::PostgresEntityStore;
use memcore_storage::migrations::{MigrationRunner, PostgresMigrationRunner, postgres_migrations};
use sqlx::postgres::PgPool;
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
    let name = format!("memcore_ent_{}", Uuid::new_v4().simple());
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
    for table in ["fact_entities", "entity_relations", "entities"] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn postgres_entity_store_contract() {
    let Some((admin, pool, name)) = fresh_db("postgres_entity_store_contract").await else {
        return;
    };
    migrated(&pool, i64::MAX).await;
    let store = PostgresEntityStore::new(pool.clone());
    entity_contract::round_trip(&store).await;
    clear(&pool).await;
    entity_contract::links_and_relations(&store).await;
    clear(&pool).await;
    entity_contract::lifecycle(&store).await;
    drop_db(admin, pool, name).await;
}

/// 0014 applies on top of 0001–0013 and re-running is a no-op.
#[tokio::test]
async fn postgres_migration_0014_applies_to_an_existing_database() {
    let Some((admin, pool, name)) =
        fresh_db("postgres_migration_0014_applies_to_an_existing_database").await
    else {
        return;
    };
    migrated(&pool, 14).await;
    migrated(&pool, i64::MAX).await;
    migrated(&pool, i64::MAX).await;
    let store = PostgresEntityStore::new(pool.clone());
    assert!(
        store
            .list_all_entities(&entity_contract::tenant())
            .await
            .unwrap()
            .is_empty()
    );
    entity_contract::round_trip(&store).await;
    drop_db(admin, pool, name).await;
}
