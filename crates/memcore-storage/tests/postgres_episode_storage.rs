//! Episode storage on Postgres (migration 0012).
//!
//! Requires `MEMCORE_TEST_POSTGRES_URL`; skipped otherwise, like the other Postgres tests. Each test
//! gets a database of its own so 0012 can also be applied on top of an existing one.

mod episode_contract;

use memcore_storage::PostgresEpisodeStore;
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
    let name = format!("memcore_ep_{}", Uuid::new_v4().simple());
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
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name}"))
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

#[tokio::test]
async fn postgres_episode_store_contract() {
    let Some((admin, pool, name)) = fresh_db("postgres_episode_store_contract").await else {
        return;
    };
    migrated(&pool, i64::MAX).await;
    let store = PostgresEpisodeStore::new(pool.clone());
    episode_contract::round_trip(&store).await;
    sqlx::query("DELETE FROM episodes")
        .execute(&pool)
        .await
        .unwrap();
    episode_contract::idempotency_and_scopes(&store).await;
    sqlx::query("DELETE FROM episodes")
        .execute(&pool)
        .await
        .unwrap();
    episode_contract::lifecycle(&store).await;
    // The checked (ingestion) insert, including real concurrent transactions.
    sqlx::query("DELETE FROM episodes")
        .execute(&pool)
        .await
        .unwrap();
    episode_contract::checked_conflicts(&store).await;
    episode_contract::checked_batch_is_atomic(&store).await;
    sqlx::query("DELETE FROM episodes")
        .execute(&pool)
        .await
        .unwrap();
    episode_contract::checked_scopes(&store).await;
    episode_contract::checked_concurrent_reuse(&store).await;
    drop_db(admin, pool, name).await;
}

#[tokio::test]
async fn postgres_migration_0012_applies_to_an_existing_database() {
    let Some((admin, pool, name)) =
        fresh_db("postgres_migration_0012_applies_to_an_existing_database").await
    else {
        return;
    };
    migrated(&pool, 12).await;
    migrated(&pool, i64::MAX).await;
    episode_contract::round_trip(&PostgresEpisodeStore::new(pool.clone())).await;
    drop_db(admin, pool, name).await;
}
