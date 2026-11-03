//! An export marked `truncated` never drives a `replace` import. The refusal happens at
//! validation, before any SQL, vector or episode / entity write; `append` keeps its semantics;
//! malformed truncation metadata is rejected in every mode. Invented data only.

use std::sync::Arc;

use chrono::Utc;
use memcore_common::MemcoreError;
use memcore_core::ports::EntityStore;
use memcore_core::{
    AddMemoryInput, EntityComponents, EntityConfig, EntityType, Episode, EpisodeComponents,
    EpisodeStore, ExportSectionTruncation, ExportUserDataInput, ImportMode, ImportUserDataInput,
    MemoryEngine, MemoryEventQuery, MemoryEventStore, MemoryMessage, MessageRole, TenantContext,
    UserMemoryExport, VectorSearchQuery, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{
    MockEntityStore, MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore,
};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 8;

struct World {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    episodes: Arc<MockEpisodeStore>,
    entities: Arc<MockEntityStore>,
    engine: MemoryEngine,
}

fn tenant() -> TenantContext {
    TenantContext::new("org_t1c", "user_t1c").expect("tenant")
}

fn world() -> World {
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let events = Arc::new(MockMemoryEventStore::new());
    let episodes = Arc::new(MockEpisodeStore::new());
    let entities = Arc::new(MockEntityStore::new());
    let engine = MemoryEngine::new(
        facts.clone(),
        vectors.clone(),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(DIMS)),
    )
    .with_event_store(events.clone())
    .with_min_importance(0.0)
    .with_episodes(Some(EpisodeComponents {
        store: episodes.clone(),
        vectors: Arc::new(MockVectorStore::new()),
    }))
    .with_entities(Some(EntityComponents {
        store: entities.clone(),
        config: EntityConfig {
            name_similarity_threshold: 0.9,
            llm_confirmation: false,
            embedding_model: "mock/mock-embedding".to_string(),
        },
    }));
    World {
        facts,
        vectors,
        events,
        episodes,
        entities,
        engine,
    }
}

/// Existing data for the tenant: facts + vectors + events (via add_memory), an episode, an entity.
async fn seed(world: &World) {
    let t = tenant();
    for text in [
        "Avery keeps a pottery wheel in the garage.",
        "Avery adopted a grey rabbit named Pebble.",
    ] {
        world
            .engine
            .add_memory(AddMemoryInput {
                tenant: t.clone(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: text.to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .expect("add memory");
    }
    world
        .episodes
        .insert_episodes(
            &t,
            vec![Episode {
                id: Uuid::new_v4(),
                org_id: t.org_id.clone(),
                user_id: t.user_id.clone(),
                session_id: Some("S1".to_string()),
                message_id: Some("M1".to_string()),
                speaker: None,
                role: MessageRole::User,
                content: "Invented chat message.".to_string(),
                observed_at: None,
                recorded_at: Utc::now(),
                embedded: false,
                metadata: json!({}),
            }],
        )
        .await
        .expect("insert episode");
    world
        .entities
        .upsert_entity(
            &t,
            memcore_core::Entity {
                id: Uuid::new_v4(),
                org_id: t.org_id.clone(),
                user_id: t.user_id.clone(),
                canonical_name: "Avery".to_string(),
                entity_type: EntityType::Person,
                aliases: vec![],
                summary: None,
                is_speaker: false,
                name_embedding: None,
                name_embedding_model: None,
                name_embedding_dimensions: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        )
        .await
        .expect("insert entity");
}

/// Everything a replace import would delete, as comparable counts and ids.
async fn snapshot(world: &World) -> (Vec<Uuid>, usize, usize, Vec<Uuid>, Vec<Uuid>) {
    let t = tenant();
    let export = world
        .engine
        .export_user_data(ExportUserDataInput {
            tenant: t.clone(),
            include_events: true,
            include_deleted: false,
        })
        .await
        .expect("export");
    let mut facts: Vec<Uuid> = export.facts.iter().map(|f| f.id).collect();
    facts.sort();
    let vectors = world
        .vectors
        .search_vectors(VectorSearchQuery {
            tenant: t.clone(),
            embedding: vec![0.1; DIMS],
            limit: 200,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("vectors")
        .len();
    let events = world
        .events
        .list_events(MemoryEventQuery::new(t.clone(), 200))
        .await
        .expect("events")
        .len();
    let mut episodes: Vec<Uuid> = world
        .episodes
        .list_episodes(&t, 200)
        .await
        .expect("episodes")
        .iter()
        .map(|e| e.id)
        .collect();
    episodes.sort();
    let mut entities: Vec<Uuid> = world
        .entities
        .list_all_entities(&t)
        .await
        .expect("entities")
        .iter()
        .map(|e| e.id)
        .collect();
    entities.sort();
    let _ = &world.facts;
    (facts, vectors, events, episodes, entities)
}

/// A small invented export of the tenant (complete unless `truncated` is set).
async fn invented_export(truncated: Option<Vec<(&str, usize)>>) -> UserMemoryExport {
    let source = world();
    let t = tenant();
    source
        .engine
        .add_memory(AddMemoryInput {
            tenant: t.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Jordan is training for a ten kilometre trail race.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add");
    let mut export = source
        .engine
        .export_user_data(ExportUserDataInput {
            tenant: t,
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export");
    export.truncated = truncated.map(|sections| {
        sections
            .into_iter()
            .map(|(section, limit)| ExportSectionTruncation {
                section: section.to_string(),
                limit,
            })
            .collect()
    });
    export
}

async fn import(
    world: &World,
    export: UserMemoryExport,
    mode: ImportMode,
    dry_run: bool,
) -> Result<memcore_core::ImportUserDataOutput, MemcoreError> {
    world
        .engine
        .import_user_data(ImportUserDataInput {
            tenant: tenant(),
            export,
            mode,
            restore_events: false,
            dry_run,
        })
        .await
}

#[tokio::test]
async fn complete_export_replace_import_succeeds() {
    let w = world();
    seed(&w).await;
    let before = snapshot(&w).await;
    let export = invented_export(None).await;
    let exported_episodes = export.episodes.as_ref().map_or(0, Vec::len);
    let output = import(&w, export, ImportMode::Replace, false)
        .await
        .expect("replace");
    assert!(output.replaced_existing);
    assert_eq!(output.imported_facts, 1);
    let (facts, _, _, episodes, entities) = snapshot(&w).await;
    assert_eq!(facts.len(), 1, "replace keeps only the imported facts");
    assert!(
        before.0.iter().all(|id| !facts.contains(id)),
        "old facts replaced"
    );
    assert_eq!(
        episodes.len(),
        exported_episodes,
        "only the imported episodes remain"
    );
    assert!(
        before.3.iter().all(|id| !episodes.contains(id)),
        "old episode replaced"
    );
    assert!(
        entities.is_empty(),
        "old entity replaced (export carries none)"
    );
}

async fn assert_rejected_and_untouched(sections: Vec<(&str, usize)>) {
    let w = world();
    seed(&w).await;
    let before = snapshot(&w).await;
    assert!(
        !before.0.is_empty()
            && before.1 > 0
            && before.2 > 0
            && !before.3.is_empty()
            && !before.4.is_empty()
    );

    let export = invented_export(Some(sections)).await;
    let error = import(&w, export, ImportMode::Replace, false)
        .await
        .expect_err("a truncated export must not drive a replace import");
    let message = error.to_string();
    assert!(
        matches!(error, MemcoreError::ValidationError(_)),
        "{message}"
    );
    assert!(message.contains("marked truncated"), "{message}");
    assert!(message.contains("mode=append"), "{message}");

    assert_eq!(
        snapshot(&w).await,
        before,
        "a rejected import must not change any stored data"
    );
}

#[tokio::test]
async fn truncated_export_replace_import_is_rejected_before_any_write() {
    assert_rejected_and_untouched(vec![("facts", 100)]).await;
}

#[tokio::test]
async fn multiple_truncated_sections_are_rejected() {
    assert_rejected_and_untouched(vec![
        ("facts", 100),
        ("memory_events", 100),
        ("episodes", 10_000),
    ])
    .await;
}

#[tokio::test]
async fn dry_run_reports_the_same_rejection() {
    let w = world();
    seed(&w).await;
    let before = snapshot(&w).await;
    let export = invented_export(Some(vec![("facts", 100)])).await;
    let output = import(&w, export, ImportMode::Replace, true)
        .await
        .expect("dry run returns a report");
    assert!(!output.validation.valid);
    assert!(
        output
            .validation
            .errors
            .iter()
            .any(|e| e.code == "TRUNCATED_EXPORT_REPLACE_REJECTED")
    );
    assert_eq!(snapshot(&w).await, before);
}

#[tokio::test]
async fn truncated_export_append_keeps_merge_semantics_with_a_warning() {
    let w = world();
    seed(&w).await;
    let before = snapshot(&w).await;
    let export = invented_export(Some(vec![("facts", 100)])).await;
    let exported_episodes = export.episodes.as_ref().map_or(0, Vec::len);
    let output = import(&w, export, ImportMode::Append, false)
        .await
        .expect("append");
    assert!(!output.replaced_existing);
    assert_eq!(output.imported_facts, 1);
    assert!(
        output
            .validation
            .warnings
            .iter()
            .any(|w| w.code == "TRUNCATED_EXPORT_PARTIAL")
    );
    let after = snapshot(&w).await;
    assert_eq!(
        after.0.len(),
        before.0.len() + 1,
        "append adds the exported fact"
    );
    assert!(
        before.0.iter().all(|id| after.0.contains(id)),
        "append keeps existing facts"
    );
    assert_eq!(
        after.3.len(),
        before.3.len() + exported_episodes,
        "append adds the exported episodes"
    );
    assert!(
        before.3.iter().all(|id| after.3.contains(id)),
        "append keeps existing episodes"
    );
    assert_eq!(
        after.4, before.4,
        "entities untouched (export carries none)"
    );
}

#[tokio::test]
async fn malformed_truncation_metadata_is_rejected_in_every_mode() {
    for (sections, why) in [
        (vec![], "empty"),
        (vec![("facts_and_more", 100)], "unknown section"),
        (vec![("facts", 0)], "zero limit"),
        (vec![("facts", 100), ("facts", 100)], "duplicate"),
    ] {
        for mode in [ImportMode::Append, ImportMode::Replace] {
            let w = world();
            seed(&w).await;
            let before = snapshot(&w).await;
            let export = invented_export(Some(sections.clone())).await;
            let error = import(&w, export, mode, false).await.expect_err(&format!(
                "malformed metadata ({why}) must be rejected in {mode:?}"
            ));
            assert!(
                error.to_string().contains("truncated") || error.to_string().contains("section"),
                "{why}: {error}"
            );
            assert_eq!(
                snapshot(&w).await,
                before,
                "{why} / {mode:?}: nothing written"
            );
        }
    }
}
