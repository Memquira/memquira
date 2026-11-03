//! Subject, event time and provenance on facts (migration 0010).
//!
//! Covers acceptance criteria P1-03 (migration applies on a fresh and on an existing database) and
//! P1-04 (round trip, deletion) for SQLite. The Postgres equivalents live in `postgres_fact_store.rs`
//! and run when `MEMCORE_TEST_POSTGRES_URL` is set.

use chrono::{TimeZone, Utc};
use memcore_core::ports::{FactSearchQuery, FactStore};
use memcore_core::{
    Fact, FactStructure, MemorySource, MemoryType, TemporalGranularity, TemporalValue,
    TenantContext,
};
use memcore_storage::SqliteFactStore;
use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

fn tenant() -> TenantContext {
    TenantContext::new("org_structure", "user_structure").expect("tenant")
}

fn structured() -> FactStructure {
    let anchor = Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap();
    FactStructure {
        subject: Some("Priya".to_string()),
        event_time: Some(
            TemporalValue::new(
                Utc.with_ymd_and_hms(2023, 5, 6, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2023, 5, 6, 23, 59, 59).unwrap(),
                TemporalGranularity::Day,
                anchor,
            )
            .unwrap(),
        ),
        event_time_text: Some("last Saturday".to_string()),
        observed_at: Some(anchor),
        session_id: Some("session_1".to_string()),
        source_message_ids: vec!["D1:3".to_string(), "D1:4".to_string()],
    }
}

fn fact(content: &str, structure: FactStructure) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        "org_structure",
        "user_structure",
        MemoryType::Event,
        content,
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
    .with_structure(structure)
}

async fn fresh_store() -> SqliteFactStore {
    SqliteFactStore::connect("sqlite::memory:")
        .await
        .expect("store")
}

#[tokio::test]
async fn structured_fields_round_trip() {
    let store = fresh_store().await;
    let original = fact("Priya ran a half marathon.", structured());
    store
        .insert_fact(&tenant(), original.clone())
        .await
        .unwrap();

    let loaded = store
        .get_fact(&tenant(), original.id)
        .await
        .unwrap()
        .expect("fact should exist");
    assert_eq!(loaded.structure, original.structure);
}

#[tokio::test]
async fn structured_fields_survive_search_and_update() {
    let store = fresh_store().await;
    let mut original = fact("Priya ran a half marathon.", structured());
    store
        .insert_fact(&tenant(), original.clone())
        .await
        .unwrap();

    let found = store
        .search_facts(FactSearchQuery::new(tenant(), 10))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].structure, original.structure);

    original.structure.subject = Some("Tomas".to_string());
    original
        .structure
        .source_message_ids
        .push("D2:1".to_string());
    store
        .update_fact(&tenant(), original.clone())
        .await
        .unwrap();
    let loaded = store
        .get_fact(&tenant(), original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.structure.subject.as_deref(), Some("Tomas"));
    assert_eq!(
        loaded.structure.source_message_ids,
        vec!["D1:3", "D1:4", "D2:1"]
    );
}

#[tokio::test]
async fn a_fact_without_structure_reads_back_empty() {
    let store = fresh_store().await;
    let legacy = fact("User likes green tea.", FactStructure::default());
    store.insert_fact(&tenant(), legacy.clone()).await.unwrap();
    let loaded = store.get_fact(&tenant(), legacy.id).await.unwrap().unwrap();
    assert!(loaded.structure.is_empty());
    assert_eq!(loaded, legacy);
}

#[tokio::test]
async fn delete_user_data_removes_structured_facts() {
    let store = fresh_store().await;
    let original = fact("Priya ran a half marathon.", structured());
    store
        .insert_fact(&tenant(), original.clone())
        .await
        .unwrap();
    store.delete_user_data(&tenant()).await.unwrap();

    let remaining = store
        .search_facts(FactSearchQuery {
            include_deleted: true,
            ..FactSearchQuery::new(tenant(), 10)
        })
        .await
        .unwrap();
    assert!(
        remaining.iter().all(|fact| fact.id != original.id),
        "delete_user_data must remove structured facts along with their provenance"
    );
}

/// P1-03 "existing database": a database migrated before Part B, holding real rows, must take
/// migration 0010 and read those rows back unchanged.
#[tokio::test]
async fn migration_0010_applies_to_an_existing_database_and_keeps_old_rows() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let all = sqlite_migrations();
    let before_part_b: Vec<_> = all.iter().filter(|m| m.version < 10).cloned().collect();
    runner
        .apply_pending_migrations(&before_part_b)
        .await
        .unwrap();

    let old_id = Uuid::new_v4();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO facts (id, org_id, user_id, memory_type, content, summary, source, confidence, \
         importance, valid_at, invalid_at, recorded_at, updated_at, metadata, deleted_at) \
         VALUES (?, 'org_structure', 'user_structure', 'profile', 'Written before Part B', NULL, \
         'user_message', 0.9, 0.8, NULL, NULL, ?, ?, '{}', NULL)",
    )
    .bind(old_id.to_string())
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .unwrap();

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(
        report.clean,
        "migration 0010 should apply cleanly: {report:?}"
    );

    let store = SqliteFactStore::new(pool);
    let old = store.get_fact(&tenant(), old_id).await.unwrap().unwrap();
    assert_eq!(old.content, "Written before Part B");
    assert!(old.structure.is_empty());

    let new = fact("Written after Part B", structured());
    store.insert_fact(&tenant(), new.clone()).await.unwrap();
    assert_eq!(
        store
            .get_fact(&tenant(), new.id)
            .await
            .unwrap()
            .unwrap()
            .structure,
        new.structure
    );
}

/// The migration runner splits SQL on `;`. A semicolon inside a comment therefore breaks a
/// migration — it did, while 0010 was being written — so no migration may contain one.
#[test]
fn no_migration_has_a_semicolon_inside_a_comment() {
    for migration in sqlite_migrations() {
        for line in migration.sql.lines() {
            if let Some(comment) = line.trim_start().strip_prefix("--") {
                assert!(
                    !comment.contains(';'),
                    "sqlite migration {} has a ';' in a comment: {line}",
                    migration.version
                );
            }
        }
    }
}

/// P1-04 retention: pruning old facts takes their structured fields with them.
#[tokio::test]
async fn retention_prunes_structured_facts() {
    let store = fresh_store().await;
    let mut old = fact("An old structured fact.", structured());
    old.updated_at = Utc::now() - chrono::Duration::days(400);
    old.recorded_at = old.updated_at;
    store.insert_fact(&tenant(), old.clone()).await.unwrap();

    let result = store
        .delete_facts_older_than(&tenant(), Utc::now() - chrono::Duration::days(365), false)
        .await
        .unwrap();
    assert_eq!(result.count, 1);
    assert_eq!(result.fact_ids, vec![old.id]);
    assert!(store.get_fact(&tenant(), old.id).await.unwrap().is_none());
}

/// P2-20: an absolute event time settled without any client time has no observation time. Storage
/// must not invent one from the event time's anchor, and the value must read back unchanged.
#[tokio::test]
async fn an_event_time_without_a_client_observation_time_round_trips_without_one() {
    let store = fresh_store().await;
    let start = Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
    let structure = FactStructure {
        subject: Some("Priya".to_string()),
        event_time: Some(
            TemporalValue::new(
                start,
                Utc.with_ymd_and_hms(2024, 3, 31, 23, 59, 59).unwrap(),
                TemporalGranularity::Month,
                start,
            )
            .unwrap(),
        ),
        event_time_text: Some("March 2024".to_string()),
        observed_at: None,
        session_id: None,
        source_message_ids: vec!["m1".to_string()],
    };
    let original = fact("In March 2024, Priya bought a car.", structure);
    store
        .insert_fact(&tenant(), original.clone())
        .await
        .unwrap();
    let loaded = store
        .get_fact(&tenant(), original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.structure, original.structure);
    assert_eq!(loaded.structure.observed_at, None);
}
