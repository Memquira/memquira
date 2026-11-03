//! Episode storage on SQLite (migration 0012) and the mock store.
//!
//! P2-03 for 0012: applies to a fresh and to an existing database. The store contract (round trip,
//! idempotency per organization / user / session / message ID, tenant isolation, `embedded`,
//! listing, hard delete, retention) is shared with the Postgres test through `episode_contract`.

mod episode_contract;

use memcore_core::ports::{EpisodeStore, FactStore};
use memcore_core::{Fact, MemorySource, MemoryType};
use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use memcore_storage::{MockEpisodeStore, SqliteEpisodeStore, SqliteFactStore};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

async fn sqlite() -> SqliteEpisodeStore {
    SqliteEpisodeStore::connect("sqlite::memory:")
        .await
        .expect("sqlite episode store")
}

#[tokio::test]
async fn sqlite_round_trip() {
    episode_contract::round_trip(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_idempotency_and_scopes() {
    episode_contract::idempotency_and_scopes(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_lifecycle() {
    episode_contract::lifecycle(&sqlite().await).await;
}

/// A file-backed database (WAL, several pooled connections), so the concurrency case below runs
/// against real SQLite write locking rather than one shared in-memory connection.
async fn sqlite_file() -> (SqliteEpisodeStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("episodes.db").display()
    );
    let store = SqliteEpisodeStore::connect(&url)
        .await
        .expect("sqlite file episode store");
    (store, dir)
}

#[tokio::test]
async fn sqlite_checked_conflicts() {
    episode_contract::checked_conflicts(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_checked_batch_is_atomic() {
    episode_contract::checked_batch_is_atomic(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_checked_scopes() {
    episode_contract::checked_scopes(&sqlite().await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_file_checked_concurrent_reuse() {
    let (store, _dir) = sqlite_file().await;
    episode_contract::checked_concurrent_reuse(&store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_file_checked_conflicts_and_atomicity() {
    let (store, _dir) = sqlite_file().await;
    episode_contract::checked_conflicts(&store).await;
    episode_contract::checked_batch_is_atomic(&store).await;
}

#[tokio::test]
async fn mock_checked_contract() {
    let store = MockEpisodeStore::new();
    episode_contract::checked_conflicts(&store).await;
    episode_contract::checked_batch_is_atomic(&store).await;
    episode_contract::checked_scopes(&store).await;
    episode_contract::checked_concurrent_reuse(&store).await;
}
#[tokio::test]
async fn mock_round_trip() {
    episode_contract::round_trip(&MockEpisodeStore::new()).await;
}

#[tokio::test]
async fn mock_idempotency_and_scopes() {
    episode_contract::idempotency_and_scopes(&MockEpisodeStore::new()).await;
}

#[tokio::test]
async fn mock_lifecycle() {
    episode_contract::lifecycle(&MockEpisodeStore::new()).await;
}

/// P2-03 "existing database": 0012 applies on top of 0001–0011 with data in place, and neither
/// facts nor their full-text index are disturbed.
#[tokio::test]
async fn migration_0012_applies_to_an_existing_database() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let all = sqlite_migrations();
    assert!(all.iter().any(|m| m.version == 12));
    // Everything except 0012, so the fact store (which also writes the 0013 supersession columns)
    // can insert the pre-existing fact; 0012 then applies to a database that already holds facts.
    let before: Vec<_> = all.iter().filter(|m| m.version != 12).cloned().collect();
    runner.apply_pending_migrations(&before).await.unwrap();

    let now = chrono::Utc::now();
    let fact = Fact::new(
        Uuid::new_v4(),
        episode_contract::ORG,
        episode_contract::USER,
        MemoryType::Preference,
        "Restored a vintage motorbike.",
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
    .unwrap();
    let facts = SqliteFactStore::new(pool.clone());
    facts
        .insert_fact(&episode_contract::tenant(), fact.clone())
        .await
        .unwrap();

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(report.clean, "0012 should apply cleanly: {report:?}");
    assert_eq!(
        facts
            .get_fact(&episode_contract::tenant(), fact.id)
            .await
            .unwrap(),
        Some(fact)
    );

    let episodes = SqliteEpisodeStore::new(pool);
    episode_contract::round_trip(&episodes).await;
    // Re-running the migrations is a no-op.
    assert!(runner.apply_pending_migrations(&all).await.unwrap().clean);
    assert_eq!(
        episodes
            .list_episodes(&episode_contract::tenant(), 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// P2-04 backup: a SQLite backup is a copy of the whole database, so episodes are in it.
#[tokio::test]
async fn sqlite_backup_contains_the_episodes() {
    use memcore_core::{BackupBackend, BackupRequest};
    use memcore_storage::{DatabaseBackupProvider, SqliteBackupProvider};

    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("source.db")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let source = SqliteEpisodeStore::connect(&url).await.unwrap();
    episode_contract::round_trip(&source).await;

    let backup = SqliteBackupProvider::new(&url, 5)
        .unwrap()
        .create_backup(BackupRequest {
            backend: BackupBackend::Sqlite,
            output_dir: dir.path().join("backups"),
            label: None,
        })
        .await
        .unwrap();
    let restored = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            backup.backup_path.display().to_string().replace('\\', "/")
        ))
        .await
        .unwrap();
    let episodes = SqliteEpisodeStore::new(restored)
        .list_episodes(&episode_contract::tenant(), 10)
        .await
        .unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].content, "I adopted a greyhound.");
}

/// Restore drill: validate the backup, restore it
/// into an isolated database, open that database the way memcore starts (migrations run), and check
/// the episodes and their idempotency key survived. Episode vectors, like fact vectors, live in the
/// vector store and are not part of a SQLite backup.
#[tokio::test]
async fn sqlite_restore_drill_brings_back_the_episodes() {
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
    let source = SqliteEpisodeStore::connect(&source_url).await.unwrap();
    episode_contract::round_trip(&source).await;

    let provider = SqliteBackupProvider::new(&source_url, 5).unwrap();
    let backup = provider
        .create_backup(BackupRequest {
            backend: BackupBackend::Sqlite,
            output_dir: dir.path().join("backups"),
            label: Some("restore-drill".to_string()),
        })
        .await
        .unwrap();
    let validation = provider.validate_backup(&backup.backup_path).await.unwrap();
    assert!(validation.valid, "{:?}", validation.issues);
    assert_eq!(validation.checksum_sha256, backup.checksum_sha256);

    // Restore into an isolated location and start on it as memcore would.
    let restored_path = dir.path().join("isolated").join("memcore.db");
    std::fs::create_dir_all(restored_path.parent().unwrap()).unwrap();
    std::fs::copy(&backup.backup_path, &restored_path).unwrap();
    let restored = SqliteEpisodeStore::connect(&as_url(&restored_path))
        .await
        .unwrap();

    let tenant = episode_contract::tenant();
    let episodes = restored.list_episodes(&tenant, 10).await.unwrap();
    assert_eq!(episodes.len(), 1);
    let original = episodes[0].clone();
    assert_eq!(original.content, "I adopted a greyhound.");
    assert_eq!(
        restored
            .get_episodes(&tenant, &[original.id])
            .await
            .unwrap(),
        vec![original.clone()]
    );
    // The idempotency key came back with the data: the same client message is not stored twice.
    let again = episode_contract::episode(&tenant, Some("S1"), Some("D1:1"), "A retried copy.");
    let stored = restored
        .insert_episodes(&tenant, vec![again])
        .await
        .unwrap();
    assert_eq!(stored[0].id, original.id);
    assert_eq!(restored.list_episodes(&tenant, 10).await.unwrap().len(), 1);
}
