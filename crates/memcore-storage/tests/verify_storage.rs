//! The read contracts `verify-vectors` relies on, on SQLite (in memory) and
//! the mock stores. PostgreSQL implements the same queries (real-backend tests).
//!
//! - `FactStore::list_fact_states`: the tenant's rows only, live and soft-deleted with their state,
//!   ascending id, keyset pagination that visits every row exactly once.
//! - `FactStore::fact_id_exists_in_any_tenant` / `EpisodeStore::episode_id_exists_in_any_tenant`:
//!   existence in any tenant, live or deleted, nothing else.

use chrono::Utc;
use memcore_core::ports::{EpisodeStore, FactStore};
use memcore_core::{Episode, Fact, MemorySource, MemoryType, MessageRole, TenantContext};
use memcore_storage::{MockEpisodeStore, MockFactStore, SqliteEpisodeStore, SqliteFactStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).expect("tenant")
}

fn fact(org: &str, user: &str, n: usize) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        org,
        user,
        MemoryType::Skill,
        format!("fact number {n}"),
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
    .expect("fact")
}

async fn fact_contract(store: &dyn FactStore) {
    let a = tenant("org_a", "u1");
    let mut ids = Vec::new();
    for n in 0..7 {
        ids.push(
            store
                .insert_fact(&a, fact("org_a", "u1", n))
                .await
                .expect("insert")
                .id,
        );
    }
    for other in [tenant("org_a", "u2"), tenant("org_b", "u1")] {
        store
            .insert_fact(&other, fact(&other.org_id, &other.user_id, 99))
            .await
            .expect("insert other");
    }
    store
        .soft_delete_fact(&a, ids[2])
        .await
        .expect("soft delete");
    store
        .soft_delete_fact(&a, ids[5])
        .await
        .expect("soft delete");
    ids.sort();

    // Pages of 3: every row of the tenant exactly once, ascending, deletion state carried.
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = store.list_fact_states(&a, after, 3).await.expect("page");
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 3);
        after = page.last().map(|state| state.fact.id);
        seen.extend(page);
    }
    let seen_ids: Vec<Uuid> = seen.iter().map(|state| state.fact.id).collect();
    assert_eq!(seen_ids, ids, "complete, ascending, no other tenant");
    assert_eq!(seen.iter().filter(|state| state.deleted).count(), 2);

    // Existence in any tenant: live, deleted, another tenant's; unknown is false.
    for id in &ids {
        assert!(
            store
                .fact_id_exists_in_any_tenant(*id)
                .await
                .expect("exists")
        );
    }
    let other_rows = store
        .list_fact_states(&tenant("org_b", "u1"), None, 10)
        .await
        .expect("other");
    assert_eq!(other_rows.len(), 1);
    assert!(
        store
            .fact_id_exists_in_any_tenant(other_rows[0].fact.id)
            .await
            .expect("exists")
    );
    assert!(
        !store
            .fact_id_exists_in_any_tenant(Uuid::new_v4())
            .await
            .expect("exists")
    );
}

fn episode(tenant: &TenantContext, n: usize) -> Episode {
    Episode {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        session_id: None,
        message_id: Some(format!("m{n}")),
        speaker: None,
        role: MessageRole::User,
        content: format!("episode number {n} with enough characters"),
        observed_at: None,
        recorded_at: Utc::now(),
        embedded: false,
        metadata: json!({}),
    }
}

async fn episode_contract(store: &dyn EpisodeStore) {
    let a = tenant("org_a", "u1");
    let b = tenant("org_b", "u1");
    let stored_a = store
        .insert_episodes(&a, vec![episode(&a, 1)])
        .await
        .expect("insert");
    let stored_b = store
        .insert_episodes(&b, vec![episode(&b, 2)])
        .await
        .expect("insert");
    for id in [stored_a[0].id, stored_b[0].id] {
        assert!(
            store
                .episode_id_exists_in_any_tenant(id)
                .await
                .expect("exists")
        );
    }
    assert!(
        !store
            .episode_id_exists_in_any_tenant(Uuid::new_v4())
            .await
            .expect("exists")
    );
    store.delete_user_episodes(&a).await.expect("delete");
    assert!(
        !store
            .episode_id_exists_in_any_tenant(stored_a[0].id)
            .await
            .expect("exists"),
        "a hard-deleted episode no longer exists anywhere"
    );
}

#[tokio::test]
async fn sqlite_fact_verify_contract() {
    let store = SqliteFactStore::connect("sqlite::memory:")
        .await
        .expect("sqlite fact store");
    fact_contract(&store).await;
}

#[tokio::test]
async fn mock_fact_verify_contract() {
    fact_contract(&MockFactStore::new()).await;
}

#[tokio::test]
async fn sqlite_episode_verify_contract() {
    let store = SqliteEpisodeStore::connect("sqlite::memory:")
        .await
        .expect("sqlite episode store");
    episode_contract(&store).await;
}

#[tokio::test]
async fn mock_episode_verify_contract() {
    episode_contract(&MockEpisodeStore::new()).await;
}
