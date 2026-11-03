//! Full-text search over facts on SQLite (migration 0011, design §2.1).
//!
//! Covers P2-03 for SQLite: 0011 applies to a fresh and to an existing database (with backfill), and
//! the triggers keep the index in step with insert, update, soft delete and hard delete. The Postgres
//! equivalents live in `postgres_fact_fulltext.rs`.

use chrono::Utc;
use memcore_core::ports::{FactFulltextQuery, FactStore};
use memcore_core::search::FulltextQuery;
use memcore_core::{Fact, FactStructure, MemorySource, MemoryType, TenantContext};
use memcore_storage::SqliteFactStore;
use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

const ORG: &str = "org_fulltext";
const USER: &str = "user_fulltext";

fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

fn fact_for(user: &str, content: &str, memory_type: MemoryType) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        ORG,
        user,
        memory_type,
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

fn fact(content: &str) -> Fact {
    fact_for(USER, content, MemoryType::Preference)
}

fn query(text: &str, limit: usize) -> FactFulltextQuery {
    FactFulltextQuery {
        tenant: tenant(),
        memory_types: None,
        query: FulltextQuery::parse(text),
        limit,
    }
}

async fn store() -> SqliteFactStore {
    SqliteFactStore::connect("sqlite::memory:")
        .await
        .expect("store")
}

async fn contents(store: &SqliteFactStore, text: &str) -> Vec<String> {
    store
        .fulltext_search_facts(query(text, 10))
        .await
        .expect("search")
        .into_iter()
        .map(|scored| scored.fact.content)
        .collect()
}

#[tokio::test]
async fn fts5_is_compiled_into_the_bundled_sqlite() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE VIRTUAL TABLE probe USING fts5(body)")
        .execute(&pool)
        .await
        .expect("FTS5 must be available");
}

#[tokio::test]
async fn a_natural_question_finds_the_fact_holding_its_key_terms_first() {
    let store = store().await;
    for content in [
        "Priya collects vintage postcards and enamel pins.",
        "Priya went hiking last weekend.",
        "Omar collects nothing but receipts.",
        "The weather was mild.",
    ] {
        store.insert_fact(&tenant(), fact(content)).await.unwrap();
    }
    let found = contents(&store, "What items does Priya collect? Postcards?").await;
    assert_eq!(
        found.first().map(String::as_str),
        Some("Priya collects vintage postcards and enamel pins.")
    );
    assert!(!found.contains(&"The weather was mild.".to_string()));
    // Scores are strictly ordered best first.
    let scores: Vec<f32> = store
        .fulltext_search_facts(query("Priya postcards", 10))
        .await
        .unwrap()
        .iter()
        .map(|scored| scored.score)
        .collect();
    assert!(
        scores.windows(2).all(|pair| pair[0] >= pair[1]),
        "{scores:?}"
    );
}

#[tokio::test]
async fn subject_and_time_expression_are_searchable() {
    let store = store().await;
    let with_structure = fact("Went to the lake.").with_structure(FactStructure {
        subject: Some("Zanele".to_string()),
        event_time_text: Some("last summer".to_string()),
        ..FactStructure::default()
    });
    store.insert_fact(&tenant(), with_structure).await.unwrap();
    assert_eq!(contents(&store, "Zanele").await, vec!["Went to the lake."]);
    assert_eq!(contents(&store, "summer").await, vec!["Went to the lake."]);
}

#[tokio::test]
async fn tenants_soft_deleted_facts_and_memory_types_are_respected() {
    let store = store().await;
    let mine = fact("Kayaking on the river.");
    let other_user = fact_for(
        "someone_else",
        "Kayaking every weekend.",
        MemoryType::Preference,
    );
    let event = fact_for(USER, "Kayaking trip booked.", MemoryType::Event);
    let deleted = fact("Kayaking lessons ended.");
    for f in [&mine, &other_user, &event, &deleted] {
        let owner = TenantContext::new(ORG, &f.user_id).unwrap();
        store.insert_fact(&owner, f.clone()).await.unwrap();
    }
    store.soft_delete_fact(&tenant(), deleted.id).await.unwrap();

    let all = contents(&store, "kayaking").await;
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(!all.contains(&"Kayaking every weekend.".to_string()));
    assert!(!all.contains(&"Kayaking lessons ended.".to_string()));

    let events_only = store
        .fulltext_search_facts(FactFulltextQuery {
            memory_types: Some(vec![MemoryType::Event]),
            ..query("kayaking", 10)
        })
        .await
        .unwrap();
    assert_eq!(events_only.len(), 1);
    assert_eq!(events_only[0].fact.id, event.id);
}

#[tokio::test]
async fn triggers_follow_updates_and_hard_deletes() {
    let store = store().await;
    let original = fact("Plays the cello.");
    store
        .insert_fact(&tenant(), original.clone())
        .await
        .unwrap();
    assert_eq!(contents(&store, "cello").await.len(), 1);

    let mut updated = original.clone();
    updated.content = "Plays the ukulele.".to_string();
    updated.updated_at = Utc::now();
    store.update_fact(&tenant(), updated).await.unwrap();
    assert!(contents(&store, "cello").await.is_empty());
    assert_eq!(
        contents(&store, "ukulele").await,
        vec!["Plays the ukulele."]
    );

    store.delete_user_data(&tenant()).await.unwrap();
    assert!(contents(&store, "ukulele").await.is_empty());
    // The index itself holds nothing for the deleted row either.
    store
        .insert_fact(&tenant(), fact("Plays drums."))
        .await
        .unwrap();
    assert_eq!(
        contents(&store, "ukulele drums").await,
        vec!["Plays drums."]
    );
}

#[tokio::test]
async fn query_syntax_in_the_question_cannot_break_or_widen_the_search() {
    let store = store().await;
    store
        .insert_fact(&tenant(), fact("Kept a secret recipe."))
        .await
        .unwrap();
    for hostile in [
        r#"recipe" OR "*"#,
        "content:secret",
        "NEAR(secret recipe)",
        "secret* -- ; DROP TABLE facts",
        "\"",
        "***",
    ] {
        let result = store.fulltext_search_facts(query(hostile, 10)).await;
        assert!(result.is_ok(), "{hostile:?}: {result:?}");
    }
    assert!(contents(&store, "***").await.is_empty());
    assert_eq!(contents(&store, "content:secret").await.len(), 1);
}

#[tokio::test]
async fn non_english_text_is_indexed_and_found() {
    let store = store().await;
    store
        .insert_fact(&tenant(), fact("Zoë a déménagé à München."))
        .await
        .unwrap();
    // remove_diacritics folds accents on both sides.
    assert_eq!(contents(&store, "Munchen").await.len(), 1);
    assert_eq!(contents(&store, "zoe demenage").await.len(), 1);
    assert!(contents(&store, "東京").await.is_empty());
}

#[tokio::test]
async fn an_empty_or_stop_word_only_question_returns_nothing() {
    let store = store().await;
    store
        .insert_fact(&tenant(), fact("Anything."))
        .await
        .unwrap();
    assert!(contents(&store, "what is it?").await.is_empty());
    assert!(contents(&store, "   ").await.is_empty());
}

/// P2-03 "existing database": rows written before 0011 are backfilled into the index.
#[tokio::test]
async fn migration_0011_backfills_an_existing_database() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let all = sqlite_migrations();
    // Everything except 0011, so the fact store (which also writes the 0013 supersession columns)
    // can insert the pre-existing fact; 0011 then applies to a database that already holds facts.
    let before: Vec<_> = all.iter().filter(|m| m.version != 11).cloned().collect();
    runner.apply_pending_migrations(&before).await.unwrap();

    let old = fact("Restored a vintage motorbike.");
    SqliteFactStore::new(pool.clone())
        .insert_fact(&tenant(), old.clone())
        .await
        .unwrap();

    let report = runner.apply_pending_migrations(&all).await.unwrap();
    assert!(report.clean, "0011 should apply cleanly: {report:?}");

    let store = SqliteFactStore::new(pool);
    let found = store
        .fulltext_search_facts(query("motorbike", 10))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].fact.id, old.id);
    store
        .insert_fact(&tenant(), fact("Sold the motorbike."))
        .await
        .unwrap();
    assert_eq!(contents(&store, "motorbike").await.len(), 2);
}

/// Equal BM25 scores are ordered by stable fields, so two stores holding the same facts return
/// the same order even when the facts were inserted in a different order under new random ids.
#[tokio::test]
async fn tied_scores_order_the_same_across_rebuilds_and_insert_orders() {
    // Same length and one shared matched word: these score identically.
    let contents = [
        "Zoe likes kiwi.",
        "Ada likes kiwi.",
        "Max likes kiwi.",
        "Bea likes kiwi.",
        "Kai likes kiwi.",
        "Eli likes kiwi.",
    ];
    let build = |order: Vec<usize>| async move {
        let store = store().await;
        for index in order {
            store
                .insert_fact(&tenant(), fact(contents[index]))
                .await
                .unwrap();
        }
        store
    };
    let forward = build((0..contents.len()).collect()).await;
    let backward = build((0..contents.len()).rev().collect()).await;
    let shuffled = build(vec![3, 0, 5, 1, 4, 2]).await;

    let scores: Vec<f32> = forward
        .fulltext_search_facts(query("kiwi", 10))
        .await
        .unwrap()
        .iter()
        .map(|scored| scored.score)
        .collect();
    assert!(
        scores.windows(2).all(|pair| pair[0] == pair[1]),
        "the fixture must tie: {scores:?}"
    );

    let first = contents_of(&forward, "kiwi", 3).await;
    assert_eq!(
        first,
        vec!["Ada likes kiwi.", "Bea likes kiwi.", "Eli likes kiwi."],
        "ties are ordered by content, and the limit cuts the same facts"
    );
    assert_eq!(contents_of(&backward, "kiwi", 3).await, first);
    assert_eq!(contents_of(&shuffled, "kiwi", 3).await, first);
}

/// Facts with identical content are ordered by their source messages next.
#[tokio::test]
async fn identical_content_ties_are_ordered_by_provenance() {
    let with_sources = |sources: &[&str]| {
        fact("Likes kiwi.").with_structure(FactStructure {
            source_message_ids: sources.iter().map(|s| s.to_string()).collect(),
            ..FactStructure::default()
        })
    };
    for order in [[0usize, 1], [1, 0]] {
        let store = store().await;
        let facts = [with_sources(&["D2:1"]), with_sources(&["D1:9"])];
        for index in order {
            store
                .insert_fact(&tenant(), facts[index].clone())
                .await
                .unwrap();
        }
        let found = store
            .fulltext_search_facts(query("kiwi", 10))
            .await
            .unwrap();
        let sources: Vec<Vec<String>> = found
            .iter()
            .map(|scored| scored.fact.structure.source_message_ids.clone())
            .collect();
        assert_eq!(
            sources,
            vec![vec!["D1:9".to_string()], vec!["D2:1".to_string()]]
        );
    }
}

/// In a scope that belongs to one user, that user's own name stops matching and ranking every
/// fact that mentions it; the question's other terms decide.
#[tokio::test]
async fn a_scoped_query_does_not_rank_facts_by_the_scoped_users_own_name() {
    let store = store().await;
    for content in [
        "Priya likes tea.",
        "Priya went out.",
        "Priya read a book.",
        "Bought a red kayak last spring.",
    ] {
        store.insert_fact(&tenant(), fact(content)).await.unwrap();
    }
    let question = "What kayak did Priya buy?";
    // Unscoped, every fact naming Priya matches and takes a result slot.
    let unscoped = contents(&store, question).await;
    assert_eq!(unscoped.len(), 4, "{unscoped:?}");

    let scoped: Vec<String> = store
        .fulltext_search_facts(FactFulltextQuery {
            query: FulltextQuery::parse_scoped(question, &["Priya".to_string()]),
            ..query(question, 10)
        })
        .await
        .unwrap()
        .into_iter()
        .map(|scored| scored.fact.content)
        .collect();
    assert_eq!(scoped, vec!["Bought a red kayak last spring."]);
}

async fn contents_of(store: &SqliteFactStore, text: &str, limit: usize) -> Vec<String> {
    store
        .fulltext_search_facts(query(text, limit))
        .await
        .unwrap()
        .into_iter()
        .map(|scored| scored.fact.content)
        .collect()
}
