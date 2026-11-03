//! The read-only inventory on real SQLite files built with the project's own migrations (current
//! and older schemas) and, with `MEMCORE_TEST_POSTGRES_URL`, on a fresh PostgreSQL database.
//! RG4, RG5, RG7, RG19 at the storage level. Invented data only.

use memcore_storage::rollout_inventory::{Assessment, InventoryReport, assess, read_sqlite};
use memcore_storage::sqlite_migrations;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::{Path, PathBuf};

const V4: &str = "3f2b8c1e-9d4a-4b7c-8e21-0a1b2c3d4e5f";
const V4B: &str = "7a000000-0000-4000-8000-000000000002";
const V8: &str = "7a000000-0000-8000-8000-000000000003";

fn candidate() -> Vec<i64> {
    sqlite_migrations().iter().map(|m| m.version).collect()
}

/// A SQLite file with the first `k` project migrations applied and recorded, journal `delete`.
async fn fixture(dir: &Path, k: usize, wal: bool) -> (PathBuf, SqlitePool) {
    let path = dir.join("memcore.db");
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(if wal {
            SqliteJournalMode::Wal
        } else {
            SqliteJournalMode::Delete
        });
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("fixture pool");
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL, \
         checksum TEXT NOT NULL, applied_at TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for m in sqlite_migrations().into_iter().take(k) {
        sqlx::raw_sql(m.sql)
            .execute(&pool)
            .await
            .expect("migration");
        sqlx::query("INSERT INTO schema_migrations VALUES (?, ?, ?, '2026-10-01T00:00:00Z')")
            .bind(m.version)
            .bind(m.name)
            .bind(&m.checksum)
            .execute(&pool)
            .await
            .unwrap();
    }
    (path, pool)
}

async fn insert(pool: &SqlitePool, id: &str, deleted: bool, superseded: bool) {
    sqlx::query(
        "INSERT INTO facts (id, org_id, user_id, memory_type, content, source, confidence, importance, \
         recorded_at, updated_at, metadata, deleted_at) VALUES (?, 'org_rg', 'u1', 'preference', \
         'invented text', 'api_import', 0.9, 0.5, '2026-10-01T00:00:00Z', '2026-10-01T00:00:00Z', '{}', ?)",
    )
    .bind(id)
    .bind(deleted.then_some("2026-10-02T00:00:00Z"))
    .execute(pool)
    .await
    .expect("insert fact");
    if superseded {
        sqlx::query("UPDATE facts SET superseded_at = '2026-10-03T00:00:00Z' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

fn file_state(path: &Path) -> Vec<(String, Option<String>)> {
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let p = PathBuf::from(format!("{}{suffix}", path.display()));
            let digest = std::fs::read(&p)
                .ok()
                .map(|b| hex::encode(Sha256::digest(b)));
            (suffix.to_string(), digest)
        })
        .collect()
}

async fn inventory(path: &Path, assessment: Assessment) -> InventoryReport {
    let raw = read_sqlite(path).await.expect("read");
    assess(raw, &candidate(), assessment)
}

#[tokio::test]
async fn rg19_rg4_current_schema_clean_and_legacy_v8_in_every_state_without_touching_the_file() {
    let dir = tempfile::tempdir().unwrap();
    // The current schema: every candidate migration (15 at the time of writing).
    let (path, pool) = fixture(dir.path(), candidate().len(), false).await;
    insert(&pool, V4, false, false).await;
    insert(&pool, V4B, true, false).await;
    pool.close().await;
    let before = file_state(&path);
    let clean = inventory(&path, Assessment::FirstCPlusRollout).await;
    assert_eq!(clean.outcome, "PASS", "{:?}", clean.reasons);
    assert!(clean.complete && clean.database.read_only_verified && !clean.schema.behind_candidate);
    assert_eq!(
        (
            clean.facts.total,
            clean.facts.live,
            clean.facts.soft_deleted
        ),
        (2, 1, 1)
    );
    assert_eq!(clean.facts.ids.v4, 2);
    // RG19: identical bytes, no -wal / -shm / -journal created.
    assert_eq!(file_state(&path), before);
    assert!(before[1..].iter().all(|(_, d)| d.is_none()));

    for (deleted, superseded) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir().unwrap();
        let (path, pool) = fixture(dir.path(), 14, false).await;
        insert(&pool, V4, false, false).await;
        insert(&pool, V8, deleted, superseded).await;
        pool.close().await;
        let first = inventory(&path, Assessment::FirstCPlusRollout).await;
        assert_eq!(
            first.outcome, "BLOCKED",
            "deleted={deleted} superseded={superseded}"
        );
        assert_eq!(first.facts.ids.v8, 1);
        assert_eq!(first.facts.ids_soft_deleted.v8, deleted as u64);
        assert_eq!(
            first.facts.ids_superseded.as_ref().unwrap().v8,
            superseded as u64
        );
        // RG16 at the inventory level: a later upgrade accepts C+ v8 ids.
        assert_eq!(
            inventory(&path, Assessment::LaterCPlusUpgrade)
                .await
                .outcome,
            "PASS"
        );
    }
}

#[tokio::test]
async fn rg5_invalid_noncanonical_and_non_text_ids_block() {
    for id in [
        "fact-1",
        "3F2B8C1E-9D4A-4B7C-8E21-0A1B2C3D4E5F",
        "{3f2b8c1e-9d4a-4b7c-8e21-0a1b2c3d4e5f}",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (path, pool) = fixture(dir.path(), 14, false).await;
        insert(&pool, id, false, false).await;
        pool.close().await;
        let report = inventory(&path, Assessment::LaterCPlusUpgrade).await;
        assert_eq!(report.outcome, "BLOCKED", "{id}");
        assert_eq!(report.facts.ids.unclassifiable(), 1);
    }
    // an integer id in SQLite's dynamic typing
    let dir = tempfile::tempdir().unwrap();
    let (path, pool) = fixture(dir.path(), 14, false).await;
    sqlx::query(
        "INSERT INTO facts (id, org_id, user_id, memory_type, content, source, confidence, importance, \
         recorded_at, updated_at, metadata) VALUES (42, 'o', 'u', 'p', 'x', 's', 1, 1, 't', 't', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    assert_eq!(
        inventory(&path, Assessment::LaterCPlusUpgrade)
            .await
            .facts
            .ids
            .invalid,
        1
    );
}

#[tokio::test]
async fn rg7_old_schema_is_inventoried_and_unknown_schemas_block() {
    // Migration 9 of 14 (the S5 staging example's level): no superseded_at column, no episodes.
    let dir = tempfile::tempdir().unwrap();
    let (path, pool) = fixture(dir.path(), 9, false).await;
    sqlx::query(
        "INSERT INTO facts (id, org_id, user_id, memory_type, content, source, confidence, importance, \
         recorded_at, updated_at, metadata, deleted_at) VALUES (?, 'o', 'u', 'p', 'x', 's', 1, 1, 't', 't', '{}', 't')",
    )
    .bind(V4)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let old = inventory(&path, Assessment::FirstCPlusRollout).await;
    assert_eq!(old.outcome, "PASS", "{:?}", old.reasons);
    assert!(old.schema.supported && old.schema.behind_candidate);
    assert_eq!(old.schema.versions, (1..=9).collect::<Vec<i64>>());
    assert!(old.facts.superseded.is_none());
    assert_eq!(old.facts.soft_deleted, 1);

    // Empty current schema: PASS with zero facts.
    let dir = tempfile::tempdir().unwrap();
    let (path, pool) = fixture(dir.path(), 14, false).await;
    pool.close().await;
    let empty = inventory(&path, Assessment::FirstCPlusRollout).await;
    assert_eq!((empty.outcome, empty.facts.total), ("PASS", 0));

    // A version unknown to the candidate, and a store with no migration table.
    let dir = tempfile::tempdir().unwrap();
    let (path, pool) = fixture(dir.path(), 14, false).await;
    sqlx::query("INSERT INTO schema_migrations VALUES (99, 'future', 'x', 't')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(
        inventory(&path, Assessment::FirstCPlusRollout)
            .await
            .outcome,
        "BLOCKED"
    );
    let dir = tempfile::tempdir().unwrap();
    let (path, pool) = fixture(dir.path(), 14, false).await;
    sqlx::query("DROP TABLE schema_migrations")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let unknown = inventory(&path, Assessment::FirstCPlusRollout).await;
    assert_eq!(unknown.outcome, "BLOCKED");
    assert!(!unknown.schema.supported);
}

#[tokio::test]
async fn rg19_a_live_wal_database_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (path, pool) = fixture(dir.path(), 14, true).await;
    insert(&pool, V4, false, false).await; // stays in the -wal while the writer is open
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    assert!(wal.metadata().unwrap().len() > 0);
    let before = file_state(&path);
    let report = inventory(&path, Assessment::FirstCPlusRollout).await;
    assert_eq!(report.outcome, "BLOCKED");
    assert!(
        report
            .reasons
            .iter()
            .any(|r| r.contains("checkpointed snapshot"))
    );
    assert_eq!(file_state(&path), before);
    pool.close().await;
}

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use memcore_storage::postgres_migrations;
    use memcore_storage::rollout_inventory::read_postgres;
    use sqlx::postgres::PgPool;

    async fn fresh(k: usize) -> Option<(PgPool, String, String, PgPool)> {
        let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ => {
                eprintln!("skipping postgres rollout inventory: MEMCORE_TEST_POSTGRES_URL not set");
                return None;
            }
        };
        let admin = PgPool::connect(&url).await.expect("admin");
        let name = format!("ar023rg_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .unwrap();
        let db_url = format!("{}/{name}", url.rsplit_once('/').unwrap().0);
        let pool = PgPool::connect(&db_url).await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE schema_migrations (version BIGINT PRIMARY KEY, name TEXT NOT NULL, \
             checksum TEXT NOT NULL, applied_at TIMESTAMPTZ NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for m in postgres_migrations().into_iter().take(k) {
            sqlx::raw_sql(m.sql)
                .execute(&pool)
                .await
                .expect("pg migration");
            sqlx::query("INSERT INTO schema_migrations VALUES ($1, $2, $3, now())")
                .bind(m.version)
                .bind(m.name)
                .bind(&m.checksum)
                .execute(&pool)
                .await
                .unwrap();
        }
        Some((admin, name, db_url, pool))
    }

    async fn insert_pg(pool: &PgPool, id: &str, deleted: bool) {
        sqlx::query(&format!(
            "INSERT INTO facts (id, org_id, user_id, memory_type, content, source, confidence, importance, \
             recorded_at, updated_at, deleted_at) VALUES ('{id}'::uuid, 'org_rg', 'u1', 'preference', \
             'invented text', 'api_import', 0.9, 0.5, now(), now(), {})",
            if deleted { "now()" } else { "NULL" }
        ))
        .execute(pool)
        .await
        .expect("insert pg fact");
    }

    async fn logical(pool: &PgPool) -> (i64, String) {
        sqlx::query_as(
            "SELECT count(*), md5(coalesce(string_agg(id::text || deleted_at::text, ',' ORDER BY id), '')) FROM facts",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn rg19_rg4_rg7_postgres_read_only_inventory_on_current_and_old_schemas() {
        for (k, ids) in [
            (14usize, vec![(V4, false), (V8, true)]),
            (9, vec![(V4, false)]),
        ] {
            let Some((admin, name, url, pool)) = fresh(k).await else {
                return;
            };
            for (id, deleted) in &ids {
                insert_pg(&pool, id, *deleted).await;
            }
            let before = logical(&pool).await;
            let report = assess(
                read_postgres(&url).await.expect("read"),
                &(1..=14).collect::<Vec<_>>(),
                Assessment::FirstCPlusRollout,
            );
            assert!(report.database.read_only_verified);
            assert!(
                report.database.incarnation.is_some(),
                "{:?}",
                report.reasons
            );
            assert!(
                report.database.other_sessions.unwrap_or(0) >= 1,
                "the fixture pool is connected"
            );
            assert_eq!(report.schema.versions.len(), k);
            assert_eq!(report.schema.behind_candidate, k < 14);
            assert_eq!(report.facts.total, ids.len() as u64);
            if k == 14 {
                assert_eq!(
                    (report.facts.ids.v8, report.facts.ids_soft_deleted.v8),
                    (1, 1)
                );
                assert_eq!(report.outcome, "BLOCKED");
            } else {
                assert!(report.facts.superseded.is_none());
                assert_eq!(report.outcome, "PASS", "{:?}", report.reasons);
            }
            assert_eq!(
                logical(&pool).await,
                before,
                "inventory must not change the database"
            );
            // The session option really refuses writes.
            let ro = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect_with(
                    url.parse::<sqlx::postgres::PgConnectOptions>()
                        .unwrap()
                        .options([("default_transaction_read_only", "on")]),
                )
                .await
                .unwrap();
            let write = sqlx::query("UPDATE facts SET content = 'x'")
                .execute(&ro)
                .await;
            assert!(write.unwrap_err().to_string().contains("read-only"));
            ro.close().await;
            pool.close().await;
            // A faithful copy in another database (P4 restore-copy provenance): same state,
            // different incarnation-bound digest.
            let copy = format!("{name}_copy");
            sqlx::query(&format!("CREATE DATABASE {copy} TEMPLATE {name}"))
                .execute(&admin)
                .await
                .unwrap();
            let copy_url = format!("{}/{copy}", url.rsplit_once('/').unwrap().0);
            let copied = assess(
                read_postgres(&copy_url).await.expect("read copy"),
                &(1..=14).collect::<Vec<_>>(),
                Assessment::FirstCPlusRollout,
            );
            assert_eq!(
                copied.fingerprint.state_sha256,
                report.fingerprint.state_sha256
            );
            assert_ne!(copied.fingerprint.digest, report.fingerprint.digest);
            assert_ne!(copied.database.incarnation, report.database.incarnation);
            sqlx::query(&format!("DROP DATABASE IF EXISTS {copy}"))
                .execute(&admin)
                .await
                .unwrap();
            sqlx::query(&format!("DROP DATABASE IF EXISTS {name}"))
                .execute(&admin)
                .await
                .unwrap();
        }
    }
}
