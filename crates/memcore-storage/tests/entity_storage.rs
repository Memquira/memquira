//! Entity storage on SQLite (migration 0014) and the mock store.
//!
//! P3-03: 0014 applies to a fresh and to an existing database; the store contract (round trip,
//! links, relations, lifecycle) is shared with the Postgres test through `entity_contract`; a
//! SQLite backup carries the entity tables. P3-04: tenant isolation is part of the contract.

mod entity_contract;

use memcore_core::ports::{EntityStore, FactStore};
use memcore_core::{EntityRole, EntityType, Fact, MemorySource, MemoryType};
use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use memcore_storage::{MockEntityStore, SqliteEntityStore, SqliteFactStore};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

async fn sqlite() -> SqliteEntityStore {
    SqliteEntityStore::connect("sqlite::memory:")
        .await
        .expect("sqlite entity store")
}

#[tokio::test]
async fn sqlite_round_trip() {
    entity_contract::round_trip(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_links_and_relations() {
    entity_contract::links_and_relations(&sqlite().await).await;
}

#[tokio::test]
async fn sqlite_lifecycle() {
    entity_contract::lifecycle(&sqlite().await).await;
}

#[tokio::test]
async fn mock_round_trip() {
    entity_contract::round_trip(&MockEntityStore::new()).await;
}

#[tokio::test]
async fn mock_links_and_relations() {
    entity_contract::links_and_relations(&MockEntityStore::new()).await;
}

#[tokio::test]
async fn mock_lifecycle() {
    entity_contract::lifecycle(&MockEntityStore::new()).await;
}

/// 0014 applies on top of 0001–0013 with facts in place; nothing is backfilled; re-running is a
/// no-op.
#[tokio::test]
async fn migration_0014_applies_to_an_existing_database() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let all = sqlite_migrations();
    assert!(all.iter().any(|m| m.version == 14 && m.name == "entities"));
    let before: Vec<_> = all.iter().filter(|m| m.version < 14).cloned().collect();
    runner.apply_pending_migrations(&before).await.unwrap();
    let now = chrono::Utc::now();
    let fact = Fact::new(
        Uuid::new_v4(),
        entity_contract::ORG,
        entity_contract::USER,
        MemoryType::Preference,
        "Priya walks Luna every morning.",
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
        .insert_fact(&entity_contract::tenant(), fact.clone())
        .await
        .unwrap();

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(report.clean, "0014 should apply cleanly: {report:?}");
    assert_eq!(
        facts
            .get_fact(&entity_contract::tenant(), fact.id)
            .await
            .unwrap(),
        Some(fact)
    );
    let entities = SqliteEntityStore::new(pool);
    assert!(
        entities
            .list_all_entities(&entity_contract::tenant())
            .await
            .unwrap()
            .is_empty(),
        "nothing is backfilled"
    );
    entity_contract::round_trip(&entities).await;
    assert!(runner.apply_pending_migrations(&all).await.unwrap().clean);
}

/// A SQLite backup copies the whole database, so a restored copy (opened as memcore starts)
/// holds the entities, links and relations.
#[tokio::test]
async fn sqlite_backup_and_restore_keep_entities() {
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
    let source = SqliteEntityStore::connect(&source_url).await.unwrap();
    let tenant = entity_contract::tenant();
    let priya = entity_contract::entity(&tenant, "Priya", EntityType::Person, 1);
    let luna = entity_contract::entity(&tenant, "Luna", EntityType::Animal, 2);
    source.upsert_entity(&tenant, priya.clone()).await.unwrap();
    source.upsert_entity(&tenant, luna.clone()).await.unwrap();
    let fact = Uuid::new_v4();
    let link = entity_contract::link(&tenant, fact, luna.id, EntityRole::Object);
    source.link_fact(&tenant, link.clone()).await.unwrap();
    let owns = entity_contract::relation(&tenant, priya.id, luna.id, Some(fact));
    source.insert_relation(&tenant, owns.clone()).await.unwrap();

    let provider = SqliteBackupProvider::new(&source_url, 5).unwrap();
    let backup = provider
        .create_backup(BackupRequest {
            backend: BackupBackend::Sqlite,
            output_dir: dir.path().join("backups"),
            label: Some("entities".to_string()),
        })
        .await
        .unwrap();
    assert!(
        provider
            .validate_backup(&backup.backup_path)
            .await
            .unwrap()
            .valid
    );
    let restored_path = dir.path().join("isolated").join("memcore.db");
    std::fs::create_dir_all(restored_path.parent().unwrap()).unwrap();
    std::fs::copy(&backup.backup_path, &restored_path).unwrap();
    let restored = SqliteEntityStore::connect(&as_url(&restored_path))
        .await
        .unwrap();
    assert_eq!(
        restored.list_all_entities(&tenant).await.unwrap(),
        vec![priya.clone(), luna.clone()]
    );
    assert_eq!(
        restored.links_for_entity(&tenant, luna.id).await.unwrap(),
        vec![link]
    );
    assert_eq!(
        restored
            .relations_for_entity(&tenant, priya.id)
            .await
            .unwrap(),
        vec![owns]
    );
}
