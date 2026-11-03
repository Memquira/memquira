//! Fact supersession state on SQLite (migration 0013) and the mock store.
//!
//! Migration 0013 applies to a fresh and to an existing database, keeps existing `invalid_at` data
//! and backfills nothing. The store contract (round trip, update, soft delete, forget-user,
//! retention, counting) is shared with the Postgres test through `supersession_contract`, and a
//! SQLite backup / restore carries both columns.

mod supersession_contract;

use memcore_core::ports::FactStore;
use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use memcore_storage::{MockFactStore, SqliteFactStore};
use sqlx::Row;
use sqlx::sqlite::SqlitePoolOptions;
use supersession_contract::{at, fact, superseded, tenant};
use uuid::Uuid;

async fn sqlite() -> SqliteFactStore {
    SqliteFactStore::connect("sqlite::memory:")
        .await
        .expect("sqlite fact store")
}

#[tokio::test]
async fn sqlite_round_trip() {
    supersession_contract::round_trip(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_update() {
    supersession_contract::update(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_lifecycle() {
    supersession_contract::lifecycle(&sqlite().await).await;
}

#[tokio::test]
async fn mock_round_trip() {
    supersession_contract::round_trip(&MockFactStore::new()).await;
}

#[tokio::test]
async fn mock_update() {
    supersession_contract::update(&MockFactStore::new()).await;
}

#[tokio::test]
async fn mock_lifecycle() {
    supersession_contract::lifecycle(&MockFactStore::new()).await;
}

/// Fresh database: 0013 is registered and both columns and the index exist.
#[tokio::test]
async fn migration_0013_applies_to_a_fresh_database() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let all = sqlite_migrations();
    assert!(
        all.iter()
            .any(|m| m.version == 13 && m.name == "fact_supersession")
    );
    let runner = SqliteMigrationRunner::new(pool.clone());
    assert!(runner.apply_pending_migrations(&all).await.unwrap().clean);

    let columns: Vec<String> = sqlx::query("SELECT name FROM pragma_table_info('facts')")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect();
    assert!(columns.contains(&"superseded_at".to_string()));
    assert!(columns.contains(&"superseded_by".to_string()));
    let index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_facts_tenant_user_superseded_at'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(index.is_some());
}

/// Existing database: 0013 applies on top of 0001–0012 with rows in place. Existing `invalid_at`
/// values are kept, nothing is backfilled, and re-running is a no-op.
#[tokio::test]
async fn migration_0013_applies_to_an_existing_database_without_backfill() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let all = sqlite_migrations();
    let before: Vec<_> = all.iter().filter(|m| m.version < 13).cloned().collect();
    runner.apply_pending_migrations(&before).await.unwrap();

    let historical = Uuid::new_v4();
    let current = Uuid::new_v4();
    for (id, content, invalid_at) in [
        (
            historical,
            "Nadia lived in Leeds.",
            Some(at(3).to_rfc3339()),
        ),
        (current, "Nadia lives in York.", None),
    ] {
        sqlx::query(
            "INSERT INTO facts (id, org_id, user_id, memory_type, content, summary, source, \
             confidence, importance, valid_at, invalid_at, recorded_at, updated_at, metadata, \
             deleted_at) VALUES (?, ?, ?, 'preference', ?, NULL, 'user_message', 0.9, 0.8, NULL, \
             ?, ?, ?, '{}', NULL)",
        )
        .bind(id.to_string())
        .bind(supersession_contract::ORG)
        .bind(supersession_contract::USER)
        .bind(content)
        .bind(invalid_at)
        .bind(at(1).to_rfc3339())
        .bind(at(1).to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(report.clean, "0013 should apply cleanly: {report:?}");
    let store = SqliteFactStore::new(pool.clone());
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

    assert!(runner.apply_pending_migrations(&all).await.unwrap().clean);
    supersession_contract::update(&store).await;
}

/// A SQLite backup is a copy of the database: a restored copy, opened the way memcore starts
/// (migrations run), reads both fields and `invalid_at` back unchanged.
#[tokio::test]
async fn sqlite_backup_and_restore_keep_supersession() {
    use memcore_core::{BackupBackend, BackupRequest};
    use memcore_storage::{DatabaseBackupProvider, SqliteBackupProvider};

    let dir = tempfile::tempdir().unwrap();
    let as_url = |path: &std::path::Path| {
        format!(
            "sqlite://{}?mode=rwc",
            path.display().to_string().replace('\\', "/")
        )
    };
    let source_url = as_url(&dir.path().join("source.db"));
    let source = SqliteFactStore::connect(&source_url).await.unwrap();
    let known = superseded("Nadia rides a red bicycle.", Some(at(5)));
    let unknown = superseded("Nadia rides a blue scooter.", None);
    let active = fact("Nadia rides a yellow bus.");
    for fact in [&known, &unknown, &active] {
        source.insert_fact(&tenant(), fact.clone()).await.unwrap();
    }

    let provider = SqliteBackupProvider::new(&source_url, 5).unwrap();
    let backup = provider
        .create_backup(BackupRequest {
            backend: BackupBackend::Sqlite,
            output_dir: dir.path().join("backups"),
            label: Some("supersession".to_string()),
        })
        .await
        .unwrap();
    let validation = provider.validate_backup(&backup.backup_path).await.unwrap();
    assert!(validation.valid, "{:?}", validation.issues);

    let restored_path = dir.path().join("isolated").join("memcore.db");
    std::fs::create_dir_all(restored_path.parent().unwrap()).unwrap();
    std::fs::copy(&backup.backup_path, &restored_path).unwrap();
    let restored = SqliteFactStore::connect(&as_url(&restored_path))
        .await
        .unwrap();
    for original in [&known, &unknown, &active] {
        assert_eq!(
            &restored
                .get_fact(&tenant(), original.id)
                .await
                .unwrap()
                .unwrap(),
            original
        );
    }
}
