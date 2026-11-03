//! closure: PostgreSQL migration runs are serialised by a session advisory lock
//! (`MIGRATION_LOCK_KEY`). Real PostgreSQL (`MEMCORE_TEST_POSTGRES_URL`, a database of its own per
//! test); skipped otherwise. Concurrent runners each use their own pool, as separate processes do.
//! Startup from independent processes is exercised outside the unit tests.

#![cfg(feature = "postgres")]

use std::time::{Duration, Instant};

use memcore_storage::migrations::{
    MIGRATION_LOCK_KEY, Migration, MigrationRunner, PostgresMigrationRunner, postgres_migrations,
};
use sqlx::Connection;
use sqlx::postgres::{PgConnection, PgPool};
use uuid::Uuid;

struct Db {
    admin: PgPool,
    name: String,
    url: String,
}

async fn fresh_db(test: &str) -> Option<Db> {
    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("skipping postgres test `{test}`: MEMCORE_TEST_POSTGRES_URL not set");
            return None;
        }
    };
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!("memcore_v21c_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let base = url.rsplit_once('/').unwrap().0;
    Some(Db {
        admin,
        url: format!("{base}/{name}"),
        name,
    })
}

impl Db {
    async fn pool(&self) -> PgPool {
        PgPool::connect(&self.url).await.unwrap()
    }

    async fn drop(self) {
        let _ = sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        ))
        .execute(&self.admin)
        .await;
    }
}

/// (rows, distinct versions, max version) of `schema_migrations`, and whether every checksum
/// equals the code's.
async fn schema_state(pool: &PgPool, migrations: &[Migration]) -> (i64, i64, i64, bool) {
    let (rows, distinct, max): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT version), COALESCE(MAX(version), 0) FROM schema_migrations",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let stored: Vec<(i64, String)> =
        sqlx::query_as("SELECT version, checksum FROM schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .unwrap();
    let exact = stored.iter().all(|(v, c)| {
        migrations
            .iter()
            .any(|m| m.version == *v && &m.checksum == c)
    });
    (rows, distinct, max, exact)
}

/// The expected `schema_state` once every migration of `all` is applied.
fn full(all: &[Migration]) -> (i64, i64, i64, bool) {
    let n = all.len() as i64;
    let max = all.iter().map(|m| m.version).max().unwrap();
    (n, n, max, true)
}

async fn concurrent_apply(
    db: &Db,
    runners: usize,
    migrations: Vec<Migration>,
) -> Vec<Result<(), String>> {
    let mut tasks = Vec::new();
    for _ in 0..runners {
        let pool = db.pool().await;
        let migrations = migrations.clone();
        tasks.push(tokio::spawn(async move {
            PostgresMigrationRunner::new(pool)
                .apply_pending_migrations(&migrations)
                .await
                .map(|report| assert!(report.clean, "{report:?}"))
                .map_err(|e| e.to_string())
        }));
    }
    let mut out = Vec::new();
    for task in tasks {
        out.push(task.await.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_runners_on_empty_fully_and_partially_migrated_databases() {
    let all = postgres_migrations();
    for (label, runners, pre) in [
        ("empty x2", 2, 0),
        ("empty x4", 4, 0),
        ("partial x4", 4, 10),
        ("full x4", 4, 16),
    ] {
        for round in 0..5 {
            let Some(db) = fresh_db("concurrent_runners").await else {
                return;
            };
            if pre > 0 {
                let first: Vec<_> = all.iter().filter(|m| m.version <= pre).cloned().collect();
                PostgresMigrationRunner::new(db.pool().await)
                    .apply_pending_migrations(&first)
                    .await
                    .unwrap();
            }
            let results = concurrent_apply(&db, runners, all.clone()).await;
            assert!(
                results.iter().all(Result::is_ok),
                "{label} round {round}: {results:?}"
            );
            let pool = db.pool().await;
            assert_eq!(
                schema_state(&pool, &all).await,
                full(&all),
                "{label} round {round}"
            );
            pool.close().await;
            db.drop().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runners_wait_for_the_holder_then_apply_each_migration_once() {
    let Some(db) = fresh_db("runners_wait_for_the_holder").await else {
        return;
    };
    // A controlled barrier: hold the lock, start four runners, release.
    let mut holder = PgConnection::connect(&db.url).await.unwrap();
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .fetch_one(&mut holder)
        .await
        .unwrap();
    assert!(locked);
    let all = postgres_migrations();
    let db_url = db.url.clone();
    let waiting = tokio::spawn({
        let all = all.clone();
        async move {
            let mut tasks = Vec::new();
            for _ in 0..4 {
                let pool = PgPool::connect(&db_url).await.unwrap();
                let all = all.clone();
                tasks.push(tokio::spawn(async move {
                    PostgresMigrationRunner::new(pool)
                        .apply_pending_migrations(&all)
                        .await
                }));
            }
            let mut ok = 0;
            for t in tasks {
                ok += usize::from(t.await.unwrap().is_ok());
            }
            ok
        }
    });
    tokio::time::sleep(Duration::from_millis(800)).await;
    let pool = db.pool().await;
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'schema_migrations'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        exists, 0,
        "nothing runs while another process holds the lock"
    );
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut holder)
        .await
        .unwrap();
    assert_eq!(
        waiting.await.unwrap(),
        4,
        "every waiting runner succeeds after the release"
    );
    assert_eq!(schema_state(&pool, &all).await, full(&all));
    pool.close().await;
    holder.close().await.unwrap();
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_holder_that_dies_releases_the_lock_and_a_wait_is_bounded() {
    let Some(db) = fresh_db("a_holder_that_dies").await else {
        return;
    };
    let mut holder = PgConnection::connect(&db.url).await.unwrap();
    let _: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .fetch_one(&mut holder)
        .await
        .unwrap();
    // Bounded wait: a clear error instead of a permanent startup hang.
    let started = Instant::now();
    let err = PostgresMigrationRunner::new(db.pool().await)
        .with_lock_wait(Duration::from_secs(2))
        .apply_pending_migrations(&postgres_migrations())
        .await
        .expect_err("times out while the lock is held");
    assert!(
        err.to_string()
            .contains("waiting for the postgres migration lock"),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(6));
    // The holder "crashes": its session ends without an unlock (connection dropped).
    drop(holder);
    let report = PostgresMigrationRunner::new(db.pool().await)
        .with_lock_wait(Duration::from_secs(20))
        .apply_pending_migrations(&postgres_migrations())
        .await
        .expect("the lock died with its session");
    assert!(report.clean);
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_migration_rolls_back_alone_and_the_next_start_resumes() {
    let Some(db) = fresh_db("a_failed_migration").await else {
        return;
    };
    let mut broken = postgres_migrations();
    let next = broken.iter().map(|m| m.version).max().unwrap() + 1;
    broken.push(Migration::new(
        next,
        "broken",
        "CREATE TABLE v21c_broken (id INT);\nSELECT * FROM v21c_no_such_table",
    ));
    let err = PostgresMigrationRunner::new(db.pool().await)
        .apply_pending_migrations(&broken)
        .await
        .expect_err("the broken migration fails");
    assert!(
        err.to_string()
            .contains("apply postgres migration statement"),
        "{err}"
    );
    let pool = db.pool().await;
    let all = postgres_migrations();
    assert_eq!(
        schema_state(&pool, &all).await,
        full(&all),
        "every valid migration applied and recorded"
    );
    let partial: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'v21c_broken'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        partial, 0,
        "the failed migration's transaction left nothing"
    );
    // The lock is not left behind by the failed run: a new start (with valid migrations) proceeds
    // at once, and repeated starts change nothing.
    for _ in 0..3 {
        let report = PostgresMigrationRunner::new(db.pool().await)
            .with_lock_wait(Duration::from_secs(5))
            .apply_pending_migrations(&all)
            .await
            .unwrap();
        assert!(report.clean);
    }
    assert_eq!(schema_state(&pool, &all).await, full(&all));
    let held: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_locks WHERE locktype = 'advisory' AND granted \n         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held, 0, "no advisory lock remains after the runs");
    pool.close().await;
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_run_does_not_leave_the_lock_on_a_pooled_connection() {
    let Some(db) = fresh_db("a_cancelled_run").await else {
        return;
    };
    let pool = db.pool().await;
    // Hold the lock elsewhere so the run is parked inside `lock()`, then cancel it.
    let mut holder = PgConnection::connect(&db.url).await.unwrap();
    let _: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .fetch_one(&mut holder)
        .await
        .unwrap();
    let run = tokio::spawn({
        let pool = pool.clone();
        async move {
            PostgresMigrationRunner::new(pool)
                .apply_pending_migrations(&postgres_migrations())
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    run.abort();
    let _ = run.await;
    holder.close().await.unwrap();
    // Cancel a run while it holds the lock (mid-migration).
    let run = tokio::spawn({
        let pool = pool.clone();
        async move {
            PostgresMigrationRunner::new(pool)
                .apply_pending_migrations(&postgres_migrations())
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    run.abort();
    let _ = run.await;
    // Whatever point it reached, a new start on the same pool completes: the cancelled run's
    // connection was closed, not pooled with the lock.
    let report = PostgresMigrationRunner::new(pool.clone())
        .with_lock_wait(Duration::from_secs(10))
        .apply_pending_migrations(&postgres_migrations())
        .await
        .expect("no lock left behind");
    assert!(report.clean);
    assert_eq!(
        schema_state(&pool, &postgres_migrations()).await,
        full(&postgres_migrations())
    );
    pool.close().await;
    db.drop().await;
}
