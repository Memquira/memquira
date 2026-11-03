//! The `FactStore::get_fact_including_deleted` contract on SQLite (in memory)
//! and the mock store. PostgreSQL implements the same query (real-backend tests).
//!
//! Contract: the tenant's row whatever its deletion state, with that state; `None` for an unknown
//! id and for another tenant's id (same organization or same user id elsewhere), deleted or not;
//! `get_fact` stays live-only.

use chrono::Utc;
use memcore_core::ports::FactStore;
use memcore_core::{Fact, MemorySource, MemoryType, TenantContext};
use memcore_storage::{MockFactStore, SqliteFactStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).expect("tenant")
}

fn fact(org: &str, user: &str, content: &str) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        org,
        user,
        MemoryType::Skill,
        content,
        None,
        MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        now,
        now,
        json!({ "k": "v" }),
    )
    .expect("fact")
}

async fn contract(store: &dyn FactStore) {
    let a = tenant("org_a", "u1");
    let same_org = tenant("org_a", "u2");
    let same_user = tenant("org_b", "u1");
    let live = store
        .insert_fact(&a, fact("org_a", "u1", "live fact"))
        .await
        .expect("insert");
    let gone = store
        .insert_fact(&a, fact("org_a", "u1", "deleted fact"))
        .await
        .expect("insert");
    store
        .soft_delete_fact(&a, gone.id)
        .await
        .expect("soft delete");

    let state = store
        .get_fact_including_deleted(&a, live.id)
        .await
        .expect("read")
        .expect("live row");
    assert!(!state.deleted);
    assert_eq!(state.fact.id, live.id);
    assert_eq!(state.fact.content, "live fact");
    assert_eq!(state.fact.metadata, json!({ "k": "v" }));

    let state = store
        .get_fact_including_deleted(&a, gone.id)
        .await
        .expect("read")
        .expect("deleted row");
    assert!(state.deleted);
    assert_eq!(state.fact.content, "deleted fact");
    assert!(
        store.get_fact(&a, gone.id).await.expect("read").is_none(),
        "get_fact live-only"
    );

    for other in [&same_org, &same_user] {
        for id in [live.id, gone.id] {
            assert!(
                store
                    .get_fact_including_deleted(other, id)
                    .await
                    .expect("read")
                    .is_none(),
                "another tenant's row is indistinguishable from an absent one"
            );
        }
    }
    assert!(
        store
            .get_fact_including_deleted(&a, Uuid::new_v4())
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn sqlite_fact_deletion_state_contract() {
    let store = SqliteFactStore::connect("sqlite::memory:")
        .await
        .expect("sqlite fact store");
    contract(&store).await;
}

#[tokio::test]
async fn mock_fact_deletion_state_contract() {
    contract(&MockFactStore::new()).await;
}
