//! A consolidation v2
//! INVALIDATE with no source-supported change time supersedes the old fact (`superseded_at` set,
//! `invalid_at` unset, `superseded_by` linked), and search, context, `include_history`, the
//! API export / import round trip (including an append import that re-keys the facts), delete,
//! retention, forget-user and counting all treat it consistently.
//!
//! Runs on SQLite always, and on Postgres when `MEMCORE_TEST_POSTGRES_URL` is set (with the
//! `postgres` feature). Every name and fact here is invented.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use memcore_api::dto::export::{ExportUserResponse, user_memory_export_from_response};
use memcore_core::ports::{ConsolidationDecision, ConsolidationOperation, FactStore};
use memcore_core::{
    AddMemoryInput, ApplyRetentionInput, BuildContextInput, CandidateFact, ConsolidationConfig,
    DeleteMemoryInput, ExportUserDataInput, Fact, FactStructure, ForgetUserInput, ImportMode,
    ImportUserDataInput, MemoryEngine, MemoryEventStore, MemoryMessage, MemoryType, MessageRole,
    RetentionPolicy, SearchMemoryInput, SearchOptions, TenantContext, UserMemoryExport,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockVectorStore, SqliteFactStore, SqliteMemoryEventStore};
use serde_json::json;
use uuid::Uuid;

/// The export with every fact under a new source id (`superseded_by`
/// following), as the same user's export from another deployment would be. Since R7 (O8 S2) an
/// unchanged export appended into the user that holds it is refused with IMPORT_ALREADY_APPLIED,
/// so the append re-key path is driven with ids the user does not own; under C+ they are stored
/// under `derived_import_fact_id`.
fn with_new_source_ids(mut export: UserMemoryExport) -> UserMemoryExport {
    let renamed: std::collections::HashMap<Uuid, Uuid> = export
        .facts
        .iter()
        .map(|fact| (fact.id, Uuid::new_v4()))
        .collect();
    for fact in &mut export.facts {
        fact.id = renamed[&fact.id];
        fact.superseded_by = fact
            .superseded_by
            .map(|id| renamed.get(&id).copied().unwrap_or(id));
    }
    export
}

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_supersession_api", user).unwrap()
}

fn candidate(content: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some("Ravi".to_string()),
            observed_at: Some(Utc.with_ymd_and_hms(2025, 5, 6, 9, 0, 0).unwrap()),
            ..FactStructure::default()
        })
}

fn engine(
    facts: Arc<dyn FactStore>,
    vectors: Arc<MockVectorStore>,
    events: Arc<dyn MemoryEventStore>,
    llm: MockLlmProvider,
) -> MemoryEngine {
    MemoryEngine::new(
        facts,
        vectors,
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(64)),
    )
    .with_event_store(events)
    .with_min_importance(0.0)
    .with_consolidation(Some(ConsolidationConfig {
        candidates: 10,
        similarity_floor: 0.0,
    }))
}

async fn add(engine: &MemoryEngine, tenant: &TenantContext) {
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage::new(MessageRole::User, "a message")],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .unwrap();
}

async fn search(engine: &MemoryEngine, tenant: &TenantContext, history: bool) -> Vec<String> {
    engine
        .search_memory_with_options(
            SearchMemoryInput {
                tenant: tenant.clone(),
                query: "Ravi lives".to_string(),
                limit: 20,
                memory_types: None,
                metadata_filter: None,
            },
            SearchOptions {
                include_history: history,
                ..SearchOptions::default()
            },
        )
        .await
        .unwrap()
        .results
        .into_iter()
        .map(|result| result.content)
        .collect()
}

async fn context(engine: &MemoryEngine, tenant: &TenantContext, history: bool) -> String {
    engine
        .build_context(BuildContextInput {
            tenant: tenant.clone(),
            query: "Ravi lives".to_string(),
            include_history: history,
            ..BuildContextInput::default()
        })
        .await
        .unwrap()
        .context
}

async fn facts_of(store: &dyn FactStore, tenant: &TenantContext) -> Vec<Fact> {
    store
        .search_facts(memcore_core::FactSearchQuery::new(tenant.clone(), 100))
        .await
        .unwrap()
}

/// Stores "Ravi lives in Pune.", then supersedes it with "Ravi lives in Mysore now." through a
/// model INVALIDATE that gives no change time. Returns (old, replacement) as stored.
async fn supersede(
    facts: Arc<dyn FactStore>,
    events: Arc<dyn MemoryEventStore>,
    tenant: &TenantContext,
) -> (Fact, Fact, MemoryEngine) {
    let vectors = Arc::new(MockVectorStore::new());
    let first =
        MockLlmProvider::new().with_extraction_candidates(vec![candidate("Ravi lives in Pune.")]);
    add(
        &engine(facts.clone(), vectors.clone(), events.clone(), first),
        tenant,
    )
    .await;
    let second = MockLlmProvider::new()
        .with_extraction_candidates(vec![candidate("Ravi lives in Mysore now.")])
        .with_consolidation_decider(Arc::new(|input| ConsolidationDecision {
            operation: ConsolidationOperation::Invalidate,
            target_fact_id: Some(input.existing_facts[0].id),
            merged_content: None,
            invalid_at: None,
            invalid_at_unparsed: None,
            reason: Some("moved".to_string()),
        }));
    let engine = engine(facts.clone(), vectors, events, second);
    add(&engine, tenant).await;

    let stored = facts_of(facts.as_ref(), tenant).await;
    assert_eq!(stored.len(), 2, "never deleted");
    let old = stored
        .iter()
        .find(|fact| fact.content.contains("Pune"))
        .unwrap()
        .clone();
    let replacement = stored
        .iter()
        .find(|fact| fact.content.contains("Mysore"))
        .unwrap()
        .clone();
    (old, replacement, engine)
}

async fn lifecycle(facts: Arc<dyn FactStore>, events: Arc<dyn MemoryEventStore>) {
    let user = tenant(&format!("user_supersession_{}", Uuid::new_v4().simple()));
    let (old, replacement, engine) = supersede(facts.clone(), events.clone(), &user).await;

    // INVALIDATE with an unknown change time.
    assert_eq!(old.invalid_at, None);
    assert!(old.superseded_at.is_some());
    assert_eq!(old.superseded_by, Some(replacement.id));
    assert!(!old.is_active() && replacement.is_active());

    // Search and context leave the superseded fact out unless history is asked for.
    assert!(!search(&engine, &user, false).await.contains(&old.content));
    assert!(search(&engine, &user, true).await.contains(&old.content));
    assert!(!context(&engine, &user, false).await.contains("Pune"));
    assert!(context(&engine, &user, true).await.contains("Pune"));

    // Counting reports stored rows (superseded included), as before.
    assert_eq!(facts.count_facts_by_user(&user).await.unwrap(), 2);

    // Export through the API DTOs and import back: all three fields survive.
    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant: user.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    let wire = serde_json::to_value(ExportUserResponse::from(export)).unwrap();
    let exported = &wire["export"]["facts"];
    let old_json = exported
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["id"] == json!(old.id))
        .unwrap();
    assert_eq!(old_json["superseded_by"], json!(replacement.id));
    assert!(old_json.get("invalid_at").is_some());
    let active_json = exported
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["id"] == json!(replacement.id))
        .unwrap();
    assert!(
        active_json.get("superseded_at").is_none(),
        "omitted when unset"
    );
    let export =
        user_memory_export_from_response(serde_json::from_value(wire["export"].clone()).unwrap())
            .unwrap();

    // A replace import (the user's data is removed, then restored under the exported IDs).
    engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export: export.clone(),
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    let imported_old = facts.get_fact(&user, old.id).await.unwrap().unwrap();
    assert_eq!(imported_old.invalid_at, old.invalid_at);
    assert_eq!(imported_old.superseded_at, old.superseded_at);
    assert_eq!(imported_old.superseded_by, Some(replacement.id));
    assert!(!imported_old.is_active());
    assert!(!search(&engine, &user, false).await.contains(&old.content));

    // Append into the same user re-keys every fact; superseded_by follows the re-keyed replacement.
    // S3I: the unchanged export is refused first (R7, nothing written); the re-key path runs on the
    // same facts under source ids the user does not own (stored under derived ids by C+).
    let refused = engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export: export.clone(),
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .expect_err("R7: already applied");
    assert!(refused.is_import_already_applied(), "{refused:?}");
    assert_eq!(
        facts_of(facts.as_ref(), &user).await.len(),
        2,
        "nothing written"
    );
    engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export: with_new_source_ids(export),
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    let all = facts_of(facts.as_ref(), &user).await;
    assert_eq!(all.len(), 4);
    let copy_old = all
        .iter()
        .find(|fact| fact.content.contains("Pune") && fact.id != old.id)
        .unwrap();
    let copy_new = all
        .iter()
        .find(|fact| fact.content.contains("Mysore") && fact.id != replacement.id)
        .unwrap();
    assert_eq!(copy_old.superseded_by, Some(copy_new.id));
    assert_eq!(copy_old.superseded_at, old.superseded_at);
    assert!(
        !search(&engine, &user, false)
            .await
            .iter()
            .any(|c| c.contains("Pune"))
    );

    // Delete a superseded fact.
    engine
        .delete_memory(DeleteMemoryInput {
            tenant: user.clone(),
            memory_id: copy_old.id,
        })
        .await
        .unwrap();
    assert!(facts.get_fact(&user, copy_old.id).await.unwrap().is_none());

    // Retention prunes a superseded fact past the cutoff like any other.
    let mut aged = facts.get_fact(&user, old.id).await.unwrap().unwrap();
    aged.updated_at = Utc::now() - Duration::days(400);
    facts.update_fact(&user, aged).await.unwrap();
    let retention = engine
        .apply_retention(ApplyRetentionInput {
            tenant: user.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(365),
                event_retention_days: None,
            },
            dry_run: false,
        })
        .await
        .unwrap();
    assert!(retention.facts_deleted >= 1, "{retention:?}");
    assert!(facts.get_fact(&user, old.id).await.unwrap().is_none());

    // Forget-user removes everything, superseded or not.
    engine
        .forget_user(ForgetUserInput {
            tenant: user.clone(),
        })
        .await
        .unwrap();
    assert!(facts_of(facts.as_ref(), &user).await.is_empty());
    assert_eq!(facts.count_facts_by_user(&user).await.unwrap(), 0);
}

#[tokio::test]
async fn sqlite_supersession_lifecycle() {
    let facts = SqliteFactStore::connect("sqlite::memory:").await.unwrap();
    let events = SqliteMemoryEventStore::connect("sqlite::memory:")
        .await
        .unwrap();
    lifecycle(Arc::new(facts), Arc::new(events)).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_supersession_lifecycle() {
    use memcore_storage::{PostgresFactStore, PostgresMemoryEventStore};

    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!(
                "skipping postgres_supersession_lifecycle: MEMCORE_TEST_POSTGRES_URL not set"
            );
            return;
        }
    };
    let facts = PostgresFactStore::connect(&url).await.unwrap();
    let events = PostgresMemoryEventStore::connect(&url).await.unwrap();
    lifecycle(Arc::new(facts), Arc::new(events)).await;
}
