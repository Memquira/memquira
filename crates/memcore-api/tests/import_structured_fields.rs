//! Export -> import keeps every structured field the export carries, through the API
//! wire format, on a real fact store.
//!
//! Fields: subject; event time (start, end, granularity, anchor) and its expression; `observed_at`;
//! session and source message IDs; source, summary, `valid_at`, metadata; `invalid_at`,
//! `superseded_at`, `superseded_by`. Replace and append imports are covered (append re-keys every
//! fact and follows `superseded_by`), an older export without the new fields still imports, and the
//! imported fields are used by search, context and consolidation, not merely stored.
//!
//! Runs on SQLite always, and on Postgres when `MEMCORE_TEST_POSTGRES_URL` is set (with the
//! `postgres` feature). Every name and fact here is invented.

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use memcore_api::dto::export::{ExportUserResponse, user_memory_export_from_response};
use memcore_api::dto::import::ImportUserDataRequest;
use memcore_core::ports::FactStore;
use memcore_core::{
    AddMemoryInput, BuildContextInput, CandidateFact, ConsolidationConfig, ContextFormatOptions,
    ExportUserDataInput, Fact, FactStructure, ImportMode, ImportUserDataInput, MemoryEngine,
    MemoryEventStore, MemoryMessage, MemorySource, MemoryType, MessageRole, RetrievalMode,
    SearchMemoryInput, SearchOptions, TemporalGranularity, TemporalValue, TenantContext,
    UserMemoryExport,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockVectorStore, SqliteFactStore, SqliteMemoryEventStore};
use serde_json::json;
use uuid::Uuid;

const ORG: &str = "org_import_structure";

fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}

fn structured_fact(tenant: &TenantContext, content: &str, structure: FactStructure) -> Fact {
    let mut fact = Fact::new(
        Uuid::new_v4(),
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        MemoryType::Event,
        content,
        Some(format!("summary of: {content}")),
        MemorySource::AssistantMessage,
        0.87,
        0.66,
        structure.event_time.as_ref().map(|time| time.start),
        None,
        at(2025, 5, 8, 14),
        at(2025, 5, 9, 10),
        json!({ "client": "import-test", "tags": ["a", "b"] }),
    )
    .unwrap();
    fact = fact.with_structure(structure);
    fact
}

/// Three facts: a day-precision event with full provenance, a superseded month-precision fact
/// with `invalid_at`, and the fact that replaced it.
fn facts(tenant: &TenantContext) -> Vec<Fact> {
    let anchor = at(2025, 5, 8, 13);
    let marathon = structured_fact(
        tenant,
        "Priya ran a half marathon.",
        FactStructure {
            subject: Some("Priya".to_string()),
            event_time: Some(
                TemporalValue::new(
                    at(2025, 5, 3, 0),
                    Utc.with_ymd_and_hms(2025, 5, 3, 23, 59, 59).unwrap(),
                    TemporalGranularity::Day,
                    anchor,
                )
                .unwrap(),
            ),
            event_time_text: Some("last Saturday".to_string()),
            observed_at: Some(anchor),
            session_id: Some("session_7".to_string()),
            source_message_ids: vec!["D7:3".to_string(), "D7:4".to_string()],
        },
    );
    let replacement = structured_fact(
        tenant,
        "Priya works at the harbour museum.",
        FactStructure {
            subject: Some("Priya".to_string()),
            observed_at: Some(anchor),
            session_id: Some("session_7".to_string()),
            source_message_ids: vec!["D7:9".to_string()],
            ..FactStructure::default()
        },
    );
    let mut old = structured_fact(
        tenant,
        "Priya works at the library.",
        FactStructure {
            subject: Some("Priya".to_string()),
            event_time: Some(
                TemporalValue::new(
                    at(2024, 1, 1, 0),
                    Utc.with_ymd_and_hms(2024, 1, 31, 23, 59, 59).unwrap(),
                    TemporalGranularity::Month,
                    at(2024, 2, 2, 9),
                )
                .unwrap(),
            ),
            event_time_text: Some("last January".to_string()),
            observed_at: Some(at(2024, 2, 2, 9)),
            session_id: Some("session_2".to_string()),
            source_message_ids: vec!["D2:1".to_string()],
        },
    );
    old.invalid_at = Some(at(2025, 4, 1, 0));
    old.superseded_at = Some(at(2025, 5, 9, 10));
    old.superseded_by = Some(replacement.id);
    vec![marathon, old, replacement]
}

fn engine(facts: Arc<dyn FactStore>, events: Arc<dyn MemoryEventStore>) -> MemoryEngine {
    engine_with(facts, events, MockLlmProvider::new(), None)
}

fn engine_with(
    facts: Arc<dyn FactStore>,
    events: Arc<dyn MemoryEventStore>,
    llm: MockLlmProvider,
    consolidation: Option<ConsolidationConfig>,
) -> MemoryEngine {
    MemoryEngine::new(
        facts,
        Arc::new(MockVectorStore::new()),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(64)),
    )
    .with_event_store(events)
    .with_min_importance(0.0)
    .with_consolidation(consolidation)
}

/// The export as a client receives it and sends it back (API DTO JSON both ways).
async fn export_over_the_wire(engine: &MemoryEngine, tenant: &TenantContext) -> UserMemoryExport {
    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant: tenant.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    let wire = serde_json::to_string(&ExportUserResponse::from(export)).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&wire).unwrap();
    user_memory_export_from_response(serde_json::from_value(wire["export"].clone()).unwrap())
        .unwrap()
}

async fn import(
    engine: &MemoryEngine,
    tenant: &TenantContext,
    export: UserMemoryExport,
    mode: ImportMode,
) {
    engine
        .import_user_data(ImportUserDataInput {
            tenant: tenant.clone(),
            export,
            mode,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
}

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

async fn stored(store: &dyn FactStore, tenant: &TenantContext) -> Vec<Fact> {
    store
        .search_facts(memcore_core::FactSearchQuery::new(tenant.clone(), 100))
        .await
        .unwrap()
}

/// Every field except the ID equal.
fn same_except_id(left: &Fact, right: &Fact) {
    assert_eq!(
        Fact {
            id: right.id,
            superseded_by: right.superseded_by,
            ..left.clone()
        },
        *right
    );
}

async fn round_trip(facts_store: Arc<dyn FactStore>, events: Arc<dyn MemoryEventStore>) {
    let tenant = TenantContext::new(ORG, format!("user_{}", Uuid::new_v4().simple())).unwrap();
    let originals = facts(&tenant);
    for fact in &originals {
        facts_store
            .insert_fact(&tenant, fact.clone())
            .await
            .unwrap();
    }
    let engine = engine(facts_store.clone(), events);
    let export = export_over_the_wire(&engine, &tenant).await;
    assert_eq!(export.facts.len(), 3);

    // 1. Replace: every fact comes back under its exported ID with every field intact.
    import(&engine, &tenant, export.clone(), ImportMode::Replace).await;
    let after_replace = stored(facts_store.as_ref(), &tenant).await;
    assert_eq!(after_replace.len(), 3);
    for original in &originals {
        let imported = facts_store
            .get_fact(&tenant, original.id)
            .await
            .unwrap()
            .expect("imported under the exported ID");
        assert_eq!(&imported, original);
    }

    // 2 and 3. Append into the same user: every fact is re-keyed, every field intact, and the
    // superseded fact points at the re-keyed replacement. S3I: the unchanged export is refused
    // first (R7, nothing written); the re-key path runs on the same facts under source ids the
    // user does not own, which C+ stores under derived ids.
    let refused = engine
        .import_user_data(ImportUserDataInput {
            tenant: tenant.clone(),
            export: export.clone(),
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .expect_err("R7: already applied");
    assert!(refused.is_import_already_applied(), "{refused:?}");
    assert_eq!(
        stored(facts_store.as_ref(), &tenant).await.len(),
        3,
        "nothing written"
    );
    let renamed = with_new_source_ids(export);
    import(&engine, &tenant, renamed.clone(), ImportMode::Append).await;
    let all = stored(facts_store.as_ref(), &tenant).await;
    assert_eq!(all.len(), 6);
    let copy_of = |original: &Fact| {
        all.iter()
            .find(|fact| fact.content == original.content && fact.id != original.id)
            .unwrap()
            .clone()
    };
    for original in &originals {
        same_except_id(original, &copy_of(original));
    }
    for source in &renamed.facts {
        assert!(
            all.iter()
                .any(|fact| fact.id == memcore_core::derived_import_fact_id(&tenant, source.id)),
            "stored under M(tenant, source id)"
        );
    }
    let (old, replacement) = (&originals[1], &originals[2]);
    assert_eq!(copy_of(old).superseded_by, Some(copy_of(replacement).id));

    // 7. The imported fields are used, not merely stored.
    usable(&engine, facts_store.clone(), &tenant).await;
}

async fn usable(engine: &MemoryEngine, facts_store: Arc<dyn FactStore>, tenant: &TenantContext) {
    // Full-text search reaches the fact through its time expression, which only the imported
    // structure holds (the sentence itself has no "Saturday").
    let hits = engine
        .search_memory_with_options(
            SearchMemoryInput {
                tenant: tenant.clone(),
                query: "Saturday".to_string(),
                limit: 10,
                memory_types: None,
                metadata_filter: None,
            },
            SearchOptions {
                mode: Some(RetrievalMode::Fulltext),
                ..SearchOptions::default()
            },
        )
        .await
        .unwrap()
        .results;
    assert!(!hits.is_empty(), "the time expression is searchable");
    assert!(
        hits.iter()
            .all(|hit| hit.content == "Priya ran a half marathon."
                && hit.structure.subject.as_deref() == Some("Priya")
                && hit.structure.event_time.is_some())
    );

    // Context renders the imported subject and event time.
    let context = engine
        .build_context(BuildContextInput {
            tenant: tenant.clone(),
            query: "Priya marathon".to_string(),
            format_options: ContextFormatOptions {
                include_timestamps: true,
                ..ContextFormatOptions::default()
            },
            ..BuildContextInput::default()
        })
        .await
        .unwrap()
        .context;
    assert!(context.contains("subject=Priya"), "{context}");
    assert!(context.contains("event_time="), "{context}");

    // Consolidation v2 matches on the imported subject and event time: restating the marathon is
    // an exact duplicate (a fast NOOP), which needs both fields to have survived the import.
    let marathon = stored(facts_store.as_ref(), tenant)
        .await
        .into_iter()
        .find(|fact| fact.content == "Priya ran a half marathon.")
        .unwrap();
    let restated = CandidateFact::new(
        "Priya ran a half marathon.",
        MemoryType::Event,
        0.9,
        0.9,
        None,
        json!({}),
    )
    .unwrap()
    .with_structure(FactStructure {
        subject: Some("priya".to_string()),
        event_time: marathon.structure.event_time.clone(),
        ..FactStructure::default()
    });
    let v2 = engine_with(
        facts_store.clone(),
        Arc::new(memcore_storage::MockMemoryEventStore::new()),
        MockLlmProvider::new().with_extraction_candidates(vec![restated]),
        Some(ConsolidationConfig {
            candidates: 10,
            similarity_floor: 0.0,
        }),
    );
    // A fresh vector store holds nothing, so the duplicate is found through full-text search.
    let output = v2
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage::new(MessageRole::User, "a message")],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!((output.added, output.noop), (0, 1), "{output:?}");
}

/// 6. An older export (no structured or supersession keys) imports with them unset.
async fn old_format(facts_store: Arc<dyn FactStore>, events: Arc<dyn MemoryEventStore>) {
    let user = format!("user_{}", Uuid::new_v4().simple());
    let tenant = TenantContext::new(ORG, user.clone()).unwrap();
    let id = Uuid::new_v4();
    let request: ImportUserDataRequest = serde_json::from_value(json!({
        "export": {
            "format_version": "memcore.user_export.v1",
            "org_id": ORG,
            "user_id": user,
            "exported_at": "2026-06-10T10:00:00Z",
            "facts": [{
                "id": id,
                "org_id": ORG,
                "user_id": user,
                "content": "Priya likes green tea.",
                "summary": null,
                "memory_type": "Preference",
                "source": "api_import",
                "confidence": 0.9,
                "importance": 0.8,
                "valid_at": null,
                "invalid_at": null,
                "recorded_at": "2026-06-10T10:00:00Z",
                "updated_at": "2026-06-10T10:00:00Z",
                "metadata": {}
            }],
            "memory_events": []
        },
        "mode": "replace"
    }))
    .unwrap();
    engine(facts_store.clone(), events)
        .import_user_data(request.into_input(tenant.clone()))
        .await
        .unwrap();
    // S3I: the fresh user does not own `id`, so C+ stores the fact under M(tenant, id).
    assert!(facts_store.get_fact(&tenant, id).await.unwrap().is_none());
    let imported = facts_store
        .get_fact(&tenant, memcore_core::derived_import_fact_id(&tenant, id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(imported.content, "Priya likes green tea.");
    assert!(imported.structure.is_empty());
    assert_eq!(
        (
            imported.invalid_at,
            imported.superseded_at,
            imported.superseded_by
        ),
        (None, None, None)
    );
    assert!(imported.is_active());
}

#[tokio::test]
async fn sqlite_export_import_keeps_structured_fields() {
    let facts = SqliteFactStore::connect("sqlite::memory:").await.unwrap();
    let events = SqliteMemoryEventStore::connect("sqlite::memory:")
        .await
        .unwrap();
    let (facts, events): (Arc<dyn FactStore>, Arc<dyn MemoryEventStore>) =
        (Arc::new(facts), Arc::new(events));
    round_trip(facts.clone(), events.clone()).await;
    old_format(facts, events).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_export_import_keeps_structured_fields() {
    use memcore_storage::{PostgresFactStore, PostgresMemoryEventStore};

    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!(
                "skipping postgres_export_import_keeps_structured_fields: MEMCORE_TEST_POSTGRES_URL not set"
            );
            return;
        }
    };
    let facts: Arc<dyn FactStore> = Arc::new(PostgresFactStore::connect(&url).await.unwrap());
    let events: Arc<dyn MemoryEventStore> =
        Arc::new(PostgresMemoryEventStore::connect(&url).await.unwrap());
    round_trip(facts.clone(), events.clone()).await;
    old_format(facts, events).await;
}
