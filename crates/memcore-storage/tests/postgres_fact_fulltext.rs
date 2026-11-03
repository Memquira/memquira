//! Full-text search over facts on Postgres (migration 0011, design §2.1).
//!
//! Requires `MEMCORE_TEST_POSTGRES_URL`; skipped otherwise, like the other Postgres tests. Every
//! test works in a database of its own, created and dropped here, so the "existing database" case
//! can apply migrations in two steps.

use chrono::Utc;
use memcore_core::ports::{FactFulltextQuery, FactStore};
use memcore_core::search::FulltextQuery;
use memcore_core::{Fact, FactStructure, MemorySource, MemoryType, TenantContext};
use memcore_storage::PostgresFactStore;
use memcore_storage::migrations::{MigrationRunner, PostgresMigrationRunner, postgres_migrations};
use serde_json::json;
use sqlx::postgres::PgPool;
use uuid::Uuid;

const ORG: &str = "org_fulltext";
const USER: &str = "user_fulltext";

/// A fresh database next to the one `MEMCORE_TEST_POSTGRES_URL` names; dropped by `drop_db`.
async fn fresh_db(test: &str) -> Option<(PgPool, PgPool, String)> {
    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("skipping postgres test `{test}`: MEMCORE_TEST_POSTGRES_URL not set");
            return None;
        }
    };
    let admin = PgPool::connect(&url).await.expect("admin connection");
    let name = format!("memcore_fts_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("create database");
    let base = url
        .rsplit_once('/')
        .map(|(base, _)| base)
        .expect("url has a path");
    let pool = PgPool::connect(&format!("{base}/{name}"))
        .await
        .expect("test database");
    Some((admin, pool, name))
}

async fn drop_db(admin: PgPool, pool: PgPool, name: String) {
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name}"))
        .execute(&admin)
        .await
        .expect("drop database");
}

async fn migrated(pool: &PgPool, below: i64) {
    let runner = PostgresMigrationRunner::new(pool.clone());
    let migrations: Vec<_> = postgres_migrations()
        .into_iter()
        .filter(|m| m.version < below)
        .collect();
    let report = runner.apply_pending_migrations(&migrations).await.unwrap();
    assert!(report.clean, "{report:?}");
}

fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

fn fact(content: &str) -> Fact {
    let now = Utc::now();
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
        now,
        now,
        json!({}),
    )
    .expect("fact")
}

async fn contents(store: &PostgresFactStore, text: &str) -> Vec<String> {
    store
        .fulltext_search_facts(FactFulltextQuery {
            tenant: tenant(),
            memory_types: None,
            query: FulltextQuery::parse(text),
            limit: 10,
        })
        .await
        .expect("search")
        .into_iter()
        .map(|scored| scored.fact.content)
        .collect()
}

#[tokio::test]
async fn postgres_fulltext_ranks_updates_and_deletes() {
    let Some((admin, pool, name)) = fresh_db("postgres_fulltext_ranks_updates_and_deletes").await
    else {
        return;
    };
    migrated(&pool, i64::MAX).await;
    let store = PostgresFactStore::new(pool.clone());

    let best = fact("Priya collects vintage postcards and enamel pins.");
    // Matches "priya" and "postcards" but not "collect", so it ranks below `best` on any backend
    // (with "collected" both facts tie after stemming, and the order would rest on the tie-break).
    let deleted = fact("Priya mailed postcards once.");
    for f in [&best, &deleted, &fact("The weather was mild.")] {
        store.insert_fact(&tenant(), f.clone()).await.unwrap();
    }
    let structured = fact("Went to the lake.").with_structure(FactStructure {
        subject: Some("Zanele".to_string()),
        ..FactStructure::default()
    });
    store.insert_fact(&tenant(), structured).await.unwrap();

    let found = contents(&store, "What does Priya collect? Postcards?").await;
    assert_eq!(found.first(), Some(&best.content));
    assert!(!found.contains(&"The weather was mild.".to_string()));
    assert_eq!(contents(&store, "Zanele").await, vec!["Went to the lake."]);

    store.soft_delete_fact(&tenant(), deleted.id).await.unwrap();
    assert_eq!(
        contents(&store, "postcards").await,
        vec![best.content.clone()]
    );

    let mut updated = best.clone();
    updated.content = "Priya collects seashells.".to_string();
    updated.updated_at = Utc::now();
    store.update_fact(&tenant(), updated).await.unwrap();
    assert!(contents(&store, "postcards").await.is_empty());
    assert_eq!(contents(&store, "seashells").await.len(), 1);

    for hostile in [r#"a" | "b"#, "secret:* & !x", "<->", "'"] {
        assert!(
            store
                .fulltext_search_facts(FactFulltextQuery {
                    tenant: tenant(),
                    memory_types: None,
                    query: FulltextQuery::parse(hostile),
                    limit: 5,
                })
                .await
                .is_ok(),
            "{hostile:?}"
        );
    }

    store.delete_user_data(&tenant()).await.unwrap();
    assert!(contents(&store, "seashells").await.is_empty());
    drop_db(admin, pool, name).await;
}

/// P2-03 "existing database": rows written before 0011 are searchable once it is applied.
#[tokio::test]
async fn postgres_migration_0011_backfills_an_existing_database() {
    let Some((admin, pool, name)) =
        fresh_db("postgres_migration_0011_backfills_an_existing_database").await
    else {
        return;
    };
    // Everything except 0011, so the fact store (which also writes the 0013 supersession columns)
    // can insert the pre-existing fact; 0011 then applies to a database that already holds facts.
    let runner = PostgresMigrationRunner::new(pool.clone());
    let before: Vec<_> = postgres_migrations()
        .into_iter()
        .filter(|m| m.version != 11)
        .collect();
    assert!(
        runner
            .apply_pending_migrations(&before)
            .await
            .unwrap()
            .clean
    );
    let old = fact("Restored a vintage motorbike.");
    PostgresFactStore::new(pool.clone())
        .insert_fact(&tenant(), old.clone())
        .await
        .unwrap();

    migrated(&pool, i64::MAX).await;
    let store = PostgresFactStore::new(pool.clone());
    assert_eq!(
        contents(&store, "motorbike").await,
        vec![old.content.clone()]
    );
    drop_db(admin, pool, name).await;
}

/// Tied scores follow content (byte order), then provenance, however the store was
/// built.
#[tokio::test]
async fn postgres_tied_scores_order_the_same_across_insert_orders() {
    let contents = [
        "Zoe likes kiwi.",
        "Ada likes kiwi.",
        "Max likes kiwi.",
        "Bea likes kiwi.",
        "Kai likes kiwi.",
    ];
    let mut orders = Vec::new();
    for order in [
        vec![0usize, 1, 2, 3, 4],
        vec![4, 3, 2, 1, 0],
        vec![2, 0, 4, 1, 3],
    ] {
        let Some((admin, pool, name)) =
            fresh_db("postgres_tied_scores_order_the_same_across_insert_orders").await
        else {
            return;
        };
        migrated(&pool, i64::MAX).await;
        let store = PostgresFactStore::new(pool.clone());
        for index in order {
            store
                .insert_fact(&tenant(), fact(contents[index]))
                .await
                .unwrap();
        }
        let found = store
            .fulltext_search_facts(FactFulltextQuery {
                tenant: tenant(),
                memory_types: None,
                query: FulltextQuery::parse("kiwi"),
                limit: 3,
            })
            .await
            .unwrap();
        assert!(
            found.windows(2).all(|w| w[0].score == w[1].score),
            "fixture must tie"
        );
        orders.push(
            found
                .into_iter()
                .map(|scored| scored.fact.content)
                .collect::<Vec<_>>(),
        );
        drop_db(admin, pool, name).await;
    }
    assert_eq!(
        orders[0],
        vec!["Ada likes kiwi.", "Bea likes kiwi.", "Kai likes kiwi."]
    );
    assert!(orders.iter().all(|order| *order == orders[0]), "{orders:?}");
}

/// P2-20 on Postgres: an event time without a client observation time round-trips without one.
#[tokio::test]
async fn postgres_event_time_without_observation_time_round_trips() {
    let Some((admin, pool, name)) =
        fresh_db("postgres_event_time_without_observation_time_round_trips").await
    else {
        return;
    };
    migrated(&pool, i64::MAX).await;
    let store = PostgresFactStore::new(pool.clone());
    let start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let original = fact("In that month, Priya bought a car.").with_structure(FactStructure {
        event_time: Some(
            memcore_core::TemporalValue::new(
                start,
                start + chrono::Duration::days(1),
                memcore_core::TemporalGranularity::Day,
                start,
            )
            .unwrap(),
        ),
        event_time_text: Some("that month".to_string()),
        ..FactStructure::default()
    });
    store
        .insert_fact(&tenant(), original.clone())
        .await
        .unwrap();
    let loaded = store
        .get_fact(&tenant(), original.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.structure.observed_at, None);
    assert_eq!(loaded.structure.event_time, original.structure.event_time);
    drop_db(admin, pool, name).await;
}
