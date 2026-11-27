//! The two provenance queries behind withholding a deleted memory's source messages:
//! `message_ids_cited_by_deleted_facts` (tenant-scoped, deleted rows only) and
//! `add_fact_source_message_ids` (appends to a live fact only, changes nothing else, keeps the
//! exact-dedup key working). SQLite always; PostgreSQL on a fresh database with
//! `MEMQUIRA_TEST_POSTGRES_URL` and the `postgres` feature. Invented data only.

use std::collections::HashSet;

use chrono::{Duration, Utc};
use memquira_core::ports::{DedupInsert, FactStore};
use memquira_core::{
    ExactDedupRule, Fact, FactStructure, MemorySource, MemoryType, TenantContext, exact_dedup_key,
};
use memquira_storage::SqliteFactStore;
use memquira_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).unwrap()
}

fn fact(t: &TenantContext, content: &str, cites: &[&str]) -> Fact {
    let earlier = Utc::now() - Duration::days(1);
    Fact::new(
        Uuid::new_v4(),
        t.org_id.clone(),
        t.user_id.clone(),
        MemoryType::Preference,
        content,
        None,
        MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        earlier,
        earlier,
        json!({}),
    )
    .unwrap()
    .with_structure(FactStructure {
        session_id: Some("s1".to_string()),
        source_message_ids: cites.iter().map(|id| id.to_string()).collect(),
        ..FactStructure::default()
    })
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn set(values: &[&str]) -> HashSet<String> {
    values.iter().map(|v| v.to_string()).collect()
}

async fn exercise(store: &dyn FactStore) {
    let alice = tenant("org_prov", "alice");
    let bob = tenant("org_prov", "bob");
    let other_org = tenant("org_prov_other", "alice");

    let wheel = fact(&alice, "Alice keeps a pottery wheel.", &["m1", "m2"]);
    let rabbit = fact(&alice, "Alice has a rabbit.", &["m2", "m3"]);
    let bobs = fact(&bob, "Bob keeps a pottery wheel.", &["m1"]);
    let others = fact(&other_org, "Alice elsewhere keeps a wheel.", &["m1"]);
    let uncited = fact(&alice, "Alice likes tea.", &[]);
    for (t, f) in [
        (&alice, &wheel),
        (&alice, &rabbit),
        (&bob, &bobs),
        (&other_org, &others),
        (&alice, &uncited),
    ] {
        store.insert_fact(t, f.clone()).await.unwrap();
    }
    let all = ids(&["m1", "m2", "m3", "m9"]);

    // Nothing is deleted yet: no message is withheld.
    assert!(
        store
            .message_ids_cited_by_deleted_facts(&alice, &all)
            .await
            .unwrap()
            .is_empty()
    );

    // Deleting one fact withholds exactly its sources, for its own tenant only.
    store.soft_delete_fact(&alice, wheel.id).await.unwrap();
    store.soft_delete_fact(&alice, uncited.id).await.unwrap();
    assert_eq!(
        store
            .message_ids_cited_by_deleted_facts(&alice, &all)
            .await
            .unwrap(),
        set(&["m1", "m2"])
    );
    assert_eq!(
        store
            .message_ids_cited_by_deleted_facts(&alice, &ids(&["m2"]))
            .await
            .unwrap(),
        set(&["m2"])
    );
    assert!(
        store
            .message_ids_cited_by_deleted_facts(&alice, &[])
            .await
            .unwrap()
            .is_empty()
    );
    for t in [&bob, &other_org] {
        assert!(
            store
                .message_ids_cited_by_deleted_facts(t, &all)
                .await
                .unwrap()
                .is_empty(),
            "{t:?}"
        );
    }

    // Appending provenance: new ids are added once, in order; content and times are unchanged.
    let before = store.get_fact(&alice, rabbit.id).await.unwrap().unwrap();
    store
        .add_fact_source_message_ids(&alice, rabbit.id, &ids(&["m3", "m7", "m7", "m8"]))
        .await
        .unwrap();
    store
        .add_fact_source_message_ids(&alice, rabbit.id, &ids(&["m8"]))
        .await
        .unwrap();
    let after = store.get_fact(&alice, rabbit.id).await.unwrap().unwrap();
    assert_eq!(
        after.structure.source_message_ids,
        ids(&["m2", "m3", "m7", "m8"])
    );
    assert_eq!(
        (
            after.content.as_str(),
            after.updated_at.timestamp_micros(),
            after.recorded_at.timestamp_micros()
        ),
        (
            before.content.as_str(),
            before.updated_at.timestamp_micros(),
            before.recorded_at.timestamp_micros()
        )
    );
    assert_eq!(after.structure.session_id, before.structure.session_id);
    // A fact with no provenance yet gets some.
    let tea = fact(&alice, "Alice likes coffee.", &[]);
    store.insert_fact(&alice, tea.clone()).await.unwrap();
    store
        .add_fact_source_message_ids(&alice, tea.id, &ids(&["m5"]))
        .await
        .unwrap();
    assert_eq!(
        store
            .get_fact(&alice, tea.id)
            .await
            .unwrap()
            .unwrap()
            .structure
            .source_message_ids,
        ids(&["m5"])
    );

    // Deleted facts, other tenants' facts and unknown ids are left alone (and are not errors).
    store
        .add_fact_source_message_ids(&alice, wheel.id, &ids(&["m4"]))
        .await
        .unwrap();
    store
        .add_fact_source_message_ids(&bob, rabbit.id, &ids(&["m6"]))
        .await
        .unwrap();
    store
        .add_fact_source_message_ids(&alice, Uuid::new_v4(), &ids(&["m6"]))
        .await
        .unwrap();
    let wheel_state = store
        .get_fact_including_deleted(&alice, wheel.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wheel_state.fact.structure.source_message_ids,
        ids(&["m1", "m2"])
    );
    assert!(
        !store
            .get_fact(&alice, rabbit.id)
            .await
            .unwrap()
            .unwrap()
            .structure
            .source_message_ids
            .contains(&"m6".to_string())
    );

    // Once deleted, the appended sources are withheld too.
    store.soft_delete_fact(&alice, rabbit.id).await.unwrap();
    assert_eq!(
        store
            .message_ids_cited_by_deleted_facts(&alice, &ids(&["m1", "m3", "m7", "m8", "m9"]))
            .await
            .unwrap(),
        set(&["m1", "m3", "m7", "m8"])
    );

    // Full erasure removes the deleted rows, and with them anything withheld.
    store.delete_user_data(&alice).await.unwrap();
    assert!(
        store
            .message_ids_cited_by_deleted_facts(&alice, &all)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The exact-dedup key survives a provenance append: an equal keyed addition is still a duplicate.
async fn dedup_key_is_kept(store: &dyn FactStore) {
    let t = tenant("org_prov_key", "carol");
    let first = fact(&t, "Carol prefers green tea.", &["m1"]);
    let key = exact_dedup_key(ExactDedupRule::Legacy, &first);
    let DedupInsert::Inserted(stored) = store
        .insert_fact_deduplicated(&t, first.clone(), &key)
        .await
        .unwrap()
    else {
        panic!("first keyed insert stores")
    };
    store
        .add_fact_source_message_ids(&t, stored.id, &ids(&["m2"]))
        .await
        .unwrap();
    assert_eq!(
        store
            .insert_fact_deduplicated(&t, fact(&t, "Carol prefers green tea.", &["m3"]), &key)
            .await
            .unwrap(),
        DedupInsert::Duplicate {
            existing_fact_id: stored.id
        }
    );
}

#[tokio::test]
async fn sqlite_deleted_fact_provenance() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    SqliteMigrationRunner::new(pool.clone())
        .apply_pending_migrations(&sqlite_migrations())
        .await
        .unwrap();
    let store = SqliteFactStore::new(pool);
    exercise(&store).await;
    dedup_key_is_kept(&store).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_deleted_fact_provenance() {
    use memquira_storage::{PostgresFactStore, PostgresMigrationRunner, postgres_migrations};
    use sqlx::postgres::PgPool;

    let url = match memquira_common::env::var("MEMQUIRA_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("PROVENANCE_POSTGRES_SKIPPED: MEMQUIRA_TEST_POSTGRES_URL not set");
            return;
        }
    };
    eprintln!("PROVENANCE_POSTGRES_RAN");
    let admin = PgPool::connect(&url).await.expect("admin");
    let name = format!("provenance_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let db_url = format!("{}/{name}", url.rsplit_once('/').unwrap().0);
    let pool = PgPool::connect(&db_url).await.unwrap();
    PostgresMigrationRunner::new(pool.clone())
        .apply_pending_migrations(&postgres_migrations())
        .await
        .unwrap();
    let store = PostgresFactStore::new(pool.clone());
    exercise(&store).await;
    dedup_key_is_kept(&store).await;
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
}
