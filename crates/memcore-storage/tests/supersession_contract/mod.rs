//! The fact supersession contract (migration 0013), run against every
//! fact store backend.
//!
//! `superseded_at` is memcore's bookkeeping time and `superseded_by` the replacement fact
//! (provenance only). Storage keeps both, and an existing `invalid_at`, exactly as written; deciding
//! what is active is the engine's job (`Fact::is_active`).

use chrono::{Duration, TimeZone, Timelike, Utc};
use memcore_core::ports::{FactFulltextQuery, FactSearchQuery, FactStore};
use memcore_core::search::FulltextQuery;
use memcore_core::{Fact, MemorySource, MemoryType, TenantContext};
use serde_json::json;
use uuid::Uuid;

pub const ORG: &str = "org_supersession";
pub const USER: &str = "user_supersession";

pub fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

/// Whole-second times, so every backend stores them exactly.
pub fn at(day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 5, day, 9, 30, 0).unwrap()
}

pub fn fact(content: &str) -> Fact {
    Fact::new(
        Uuid::new_v4(),
        ORG,
        USER,
        MemoryType::Preference,
        content,
        None,
        MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        at(1),
        at(1),
        json!({}),
    )
    .unwrap()
}

/// A superseded fact: `superseded_at` and `superseded_by` set, `invalid_at` as given.
pub fn superseded(content: &str, invalid_at: Option<chrono::DateTime<Utc>>) -> Fact {
    let mut fact = fact(content);
    fact.invalid_at = invalid_at;
    fact.superseded_at = Some(at(10));
    fact.superseded_by = Some(Uuid::new_v4());
    fact
}

async fn get(store: &dyn FactStore, id: Uuid) -> Fact {
    store
        .get_fact(&tenant(), id)
        .await
        .unwrap()
        .expect("fact exists")
}

/// Both fields, with and without `invalid_at`, survive insert, get, listing search and full-text
/// search; a fact written without them reads back without them.
pub async fn round_trip(store: &dyn FactStore) {
    let known = superseded("Nadia rides a red bicycle.", Some(at(5)));
    let unknown = superseded("Nadia rides a blue scooter.", None);
    let mut unlinked = superseded("Nadia rides a green tram.", None);
    unlinked.superseded_by = None;
    let active = fact("Nadia rides a yellow bus.");
    for fact in [&known, &unknown, &unlinked, &active] {
        store.insert_fact(&tenant(), fact.clone()).await.unwrap();
    }

    for original in [&known, &unknown, &unlinked, &active] {
        assert_eq!(&get(store, original.id).await, original);
    }
    assert!(get(store, active.id).await.is_active());
    assert!(!get(store, unknown.id).await.is_active());
    assert!(!get(store, unlinked.id).await.is_active());
    assert_eq!(get(store, unknown.id).await.invalid_at, None);

    let listed = store
        .search_facts(FactSearchQuery::new(tenant(), 10))
        .await
        .unwrap();
    assert_eq!(listed.len(), 4, "storage lists stored rows, active or not");
    // Stored (capacity) count versus active count.
    assert_eq!(store.count_facts_by_org(ORG).await.unwrap(), 4);
    assert_eq!(store.count_active_facts_by_org(ORG).await.unwrap(), 1);
    for fact in &listed {
        let original = [&known, &unknown, &unlinked, &active]
            .into_iter()
            .find(|original| original.id == fact.id)
            .unwrap();
        assert_eq!(fact, original);
    }

    let hits = store
        .fulltext_search_facts(FactFulltextQuery {
            tenant: tenant(),
            memory_types: None,
            query: FulltextQuery::parse("Nadia rides"),
            limit: 10,
        })
        .await
        .unwrap();
    let scooter = hits
        .iter()
        .find(|hit| hit.fact.id == unknown.id)
        .expect("full-text search reads the row");
    assert_eq!(scooter.fact.superseded_at, unknown.superseded_at);
    assert_eq!(scooter.fact.superseded_by, unknown.superseded_by);
}

/// `update_fact` writes the fields (the INVALIDATE and link paths) and keeps an existing
/// `invalid_at` untouched when only supersession changes.
pub async fn update(store: &dyn FactStore) {
    let mut historical = fact("Nadia lived in Leeds.");
    historical.invalid_at = Some(at(3));
    store
        .insert_fact(&tenant(), historical.clone())
        .await
        .unwrap();

    historical.superseded_at = Some(at(12));
    store
        .update_fact(&tenant(), historical.clone())
        .await
        .unwrap();
    let stored = get(store, historical.id).await;
    assert_eq!(stored.invalid_at, Some(at(3)), "invalid_at preserved");
    assert_eq!(stored.superseded_at, Some(at(12)));
    assert_eq!(stored.superseded_by, None);

    let replacement = Uuid::new_v4();
    historical.superseded_by = Some(replacement);
    store
        .update_fact(&tenant(), historical.clone())
        .await
        .unwrap();
    assert_eq!(
        get(store, historical.id).await.superseded_by,
        Some(replacement)
    );

    // A plain content update of a superseded fact keeps it superseded.
    historical.content = "Nadia lived in Leeds city centre.".to_string();
    store
        .update_fact(&tenant(), historical.clone())
        .await
        .unwrap();
    assert_eq!(get(store, historical.id).await, historical);

    // No foreign key: a replacement ID need not exist in the store.
    assert!(
        store
            .get_fact(&tenant(), replacement)
            .await
            .unwrap()
            .is_none()
    );
}

/// Soft delete, forget-user and retention treat superseded facts like any other stored fact, and
/// the stored-row count includes them.
pub async fn lifecycle(store: &dyn FactStore) {
    // Recent (whole-second) update times, so only `old` is past the retention cutoff.
    let recent = Utc::now().with_nanosecond(0).unwrap();
    let mut deleted = superseded("Nadia owned a kayak.", Some(at(4)));
    let mut kept = superseded("Nadia owned a canoe.", None);
    for fact in [&mut deleted, &mut kept] {
        fact.recorded_at = recent;
        fact.updated_at = recent;
    }
    let mut old = superseded("Nadia owned a rowing boat.", None);
    old.updated_at = recent - Duration::days(400);
    old.recorded_at = old.updated_at;
    for fact in [&deleted, &kept, &old] {
        store.insert_fact(&tenant(), fact.clone()).await.unwrap();
    }
    assert_eq!(store.count_facts_by_user(&tenant()).await.unwrap(), 3);

    store.soft_delete_fact(&tenant(), deleted.id).await.unwrap();
    assert!(
        store
            .get_fact(&tenant(), deleted.id)
            .await
            .unwrap()
            .is_none()
    );
    let with_deleted = store
        .search_facts(FactSearchQuery {
            include_deleted: true,
            ..FactSearchQuery::new(tenant(), 10)
        })
        .await
        .unwrap();
    let tombstone = with_deleted
        .iter()
        .find(|fact| fact.id == deleted.id)
        .expect("soft-deleted row kept");
    assert_eq!(tombstone.superseded_at, deleted.superseded_at);
    assert_eq!(tombstone.invalid_at, deleted.invalid_at);

    let pruned = store
        .delete_facts_older_than(&tenant(), Utc::now() - Duration::days(365), false)
        .await
        .unwrap();
    assert_eq!(pruned.fact_ids, vec![old.id]);
    assert_eq!(get(store, kept.id).await, kept);
    assert_eq!(store.count_facts_by_user(&tenant()).await.unwrap(), 1);

    store.delete_user_data(&tenant()).await.unwrap();
    let remaining = store
        .search_facts(FactSearchQuery {
            include_deleted: true,
            ..FactSearchQuery::new(tenant(), 10)
        })
        .await
        .unwrap();
    assert!(remaining.is_empty(), "{remaining:?}");
}
