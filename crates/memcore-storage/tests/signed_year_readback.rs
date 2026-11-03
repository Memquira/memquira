//! Every datetime the SQLite store writes must read back.
//!
//! An internal long-conversation evaluation: the temporal resolver turned "March 1-10" into 1 March of
//! year -10. `to_rfc3339` stored it as "-0010-03-01T00:00:00+00:00", which RFC 3339 parsing rejects,
//! so every later read that listed the row failed with STORAGE_ERROR - among them ingestion's
//! exact-dedup lookup (the 50 most recent facts of the candidate's memory type), which made every
//! later request with an event candidate fail. These tests reproduce that read path.

use chrono::{TimeZone, Utc};
use memcore_core::ports::{FactSearchQuery, FactStore};
use memcore_core::{
    Fact, FactStructure, MemorySource, MemoryType, TemporalGranularity, TemporalValue,
    TenantContext,
};
use memcore_storage::SqliteFactStore;
use serde_json::json;
use uuid::Uuid;

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_signed_year", user).expect("tenant")
}

fn fact(user: &str, memory_type: MemoryType, content: &str, year: i32) -> Fact {
    let start = Utc.with_ymd_and_hms(year, 3, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(year, 3, 1, 23, 59, 59).unwrap();
    let observed = Utc.with_ymd_and_hms(2024, 3, 12, 0, 0, 0).unwrap();
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        "org_signed_year",
        user,
        memory_type,
        content,
        None,
        MemorySource::UserMessage,
        0.9,
        0.9,
        Some(start),
        None,
        now,
        now,
        json!({}),
    )
    .unwrap()
    .with_structure(FactStructure {
        subject: Some("user".to_string()),
        event_time: Some(
            TemporalValue::new(start, end, TemporalGranularity::Day, observed).unwrap(),
        ),
        event_time_text: Some("March 1-10".to_string()),
        observed_at: Some(observed),
        session_id: Some("s3".to_string()),
        source_message_ids: vec!["b504".to_string()],
    })
}

async fn store() -> SqliteFactStore {
    SqliteFactStore::connect("sqlite::memory:")
        .await
        .expect("store")
}

fn event_lookup(user: &str) -> FactSearchQuery {
    // Exactly ingestion's exact-dedup lookup (`find_existing_facts_for_dedup`).
    let mut query = FactSearchQuery::new(tenant(user), 50);
    query.memory_types = Some(vec![MemoryType::Event]);
    query
}

#[tokio::test]
async fn a_stored_signed_year_event_time_does_not_break_later_reads() {
    let store = store().await;
    let earlier = fact(
        "u1",
        MemoryType::Event,
        "The user started a sleep study.",
        2024,
    );
    let poisoned = fact(
        "u1",
        MemoryType::Event,
        "On 1 March -0010, the user compared trackers.",
        -10,
    );
    let later = fact(
        "u1",
        MemoryType::Knowledge,
        "Yoga helps the user relax.",
        2024,
    );
    for f in [&earlier, &poisoned, &later] {
        store
            .insert_fact(&tenant("u1"), (*f).clone())
            .await
            .unwrap();
    }

    // The dedup lookup that failed every later event candidate in c17.
    let events = store
        .search_facts(event_lookup("u1"))
        .await
        .expect("dedup lookup");
    assert_eq!(events.len(), 2);

    // The row round-trips exactly (event time, valid_at, provenance).
    let loaded = store
        .get_fact(&tenant("u1"), poisoned.id)
        .await
        .unwrap()
        .expect("stored fact");
    assert_eq!(loaded.structure, poisoned.structure);
    assert_eq!(loaded.valid_at, poisoned.valid_at);
    assert_eq!(loaded.content, poisoned.content);

    // Every fact is read once: nothing lost, nothing duplicated.
    let all = store
        .search_facts(FactSearchQuery::new(tenant("u1"), 100))
        .await
        .expect("full read");
    let mut ids: Vec<Uuid> = all.iter().map(|f| f.id).collect();
    ids.sort();
    let mut expected = vec![earlier.id, poisoned.id, later.id];
    expected.sort();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn the_read_stays_tenant_scoped() {
    let store = store().await;
    let mine = fact("u1", MemoryType::Event, "On 1 March -0010, mine.", -10);
    let theirs = fact("u2", MemoryType::Event, "On 1 March -0010, theirs.", -10);
    store
        .insert_fact(&tenant("u1"), mine.clone())
        .await
        .unwrap();
    store
        .insert_fact(&tenant("u2"), theirs.clone())
        .await
        .unwrap();

    let found = store.search_facts(event_lookup("u1")).await.unwrap();
    assert_eq!(
        found.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![mine.id]
    );
    assert!(
        store
            .get_fact(&tenant("u1"), theirs.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn an_ordinary_store_is_read_exactly_as_before() {
    let store = store().await;
    let ordinary = fact(
        "u1",
        MemoryType::Event,
        "The user ran a half marathon.",
        2023,
    );
    store
        .insert_fact(&tenant("u1"), ordinary.clone())
        .await
        .unwrap();
    let loaded = store
        .get_fact(&tenant("u1"), ordinary.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.structure, ordinary.structure);
    assert_eq!(loaded.valid_at, ordinary.valid_at);
    assert_eq!(loaded.recorded_at, ordinary.recorded_at);
}
