//! The SQLite write settings memcore depends on are applied, and in-memory
//! databases still behave as before.
//!
//! SQLite defaults to a rollback journal with `synchronous=FULL`. memcore never
//! overrode them, which capped write throughput at roughly an eighth of what the
//! same disk does under WAL. These tests pin the settings so a future refactor
//! of the connection helpers cannot quietly drop them.

#![cfg(feature = "sqlite")]

use memcore_storage::connect_sqlite_pool;
use sqlx::Row;

async fn pragma(pool: &sqlx::sqlite::SqlitePool, name: &str) -> String {
    sqlx::query(&format!("PRAGMA {name}"))
        .fetch_one(pool)
        .await
        .expect("pragma should be readable")
        .try_get::<String, _>(0)
        .or_else(|_| {
            // some pragmas report as integers
            Ok::<String, sqlx::Error>(String::new())
        })
        .expect("pragma value")
}

#[tokio::test]
async fn file_database_uses_wal_and_normal_synchronous() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("write_settings.db");
    let url = format!(
        "sqlite://{}?mode=rwc",
        path.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );

    let pool = connect_sqlite_pool(&url).await.expect("connect");

    let journal = pragma(&pool, "journal_mode").await;
    assert_eq!(
        journal.to_lowercase(),
        "wal",
        "file databases must run in WAL mode"
    );

    let synchronous: i64 = sqlx::query("PRAGMA synchronous")
        .fetch_one(&pool)
        .await
        .expect("synchronous pragma")
        .try_get(0)
        .expect("integer value");
    assert_eq!(
        synchronous, 1,
        "synchronous should be NORMAL (1), not FULL (2)"
    );
}

#[tokio::test]
async fn in_memory_database_still_connects_and_shares_one_connection() {
    // WAL does not apply to :memory:, and each pooled connection would otherwise
    // get its own empty database — so this path must keep working unchanged.
    let pool = connect_sqlite_pool("sqlite::memory:")
        .await
        .expect("connect");

    sqlx::query("CREATE TABLE probe (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("create table");
    sqlx::query("INSERT INTO probe (id) VALUES (1)")
        .execute(&pool)
        .await
        .expect("insert");

    let count: i64 = sqlx::query("SELECT COUNT(*) FROM probe")
        .fetch_one(&pool)
        .await
        .expect("count")
        .try_get(0)
        .expect("integer value");
    assert_eq!(
        count, 1,
        "the same in-memory database must be visible across pool checkouts"
    );
}
