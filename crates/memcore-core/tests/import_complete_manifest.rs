//! Launch pre-freeze check (proof gaps): every complete-export manifest check rejects a bad
//! archive with `COMPLETE_EXPORT_MANIFEST_INVALID` in append and replace mode, real and dry run,
//! before any write. A valid manifest only proves the archive matches ITSELF: an archive edited
//! together with its manifest still validates (`manifest_consistent` is not source completeness).
//! Invented data only.

use std::sync::Arc;

use chrono::Utc;
use memcore_common::MemcoreError;
use memcore_core::ports::EntityStore;
use memcore_core::{
    AddMemoryInput, CompleteExportInput, EntityComponents, EntityConfig, EntityType, Episode,
    EpisodeComponents, EpisodeStore, ExportSectionTruncation, ExportUserDataInput, ImportMode,
    ImportUserDataInput, MemoryEngine, MemoryEventQuery, MemoryEventStore, MemoryMessage,
    MessageRole, TenantContext, UserMemoryExport, VectorSearchQuery, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{
    MockEntityStore, MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore,
};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 8;
const CODE: &str = "COMPLETE_EXPORT_MANIFEST_INVALID";

struct World {
    vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    episodes: Arc<MockEpisodeStore>,
    entities: Arc<MockEntityStore>,
    engine: MemoryEngine,
}

fn tenant() -> TenantContext {
    TenantContext::new("org_manifest", "user_manifest").expect("tenant")
}

fn world() -> World {
    let vectors = Arc::new(MockVectorStore::new());
    let events = Arc::new(MockMemoryEventStore::new());
    let episodes = Arc::new(MockEpisodeStore::new());
    let entities = Arc::new(MockEntityStore::new());
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
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
        vectors,
        events,
        episodes,
        entities,
        engine,
    }
}

/// Facts + vectors + events (via add_memory), one episode, one entity.
async fn seed(world: &World, texts: &[&str]) {
    let t = tenant();
    for text in texts {
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

type Snapshot = (Vec<Uuid>, usize, usize, Vec<Uuid>, Vec<Uuid>);

/// Everything an import could change, as comparable ids and counts.
async fn snapshot(world: &World) -> Snapshot {
    let t = tenant();
    let export = world
        .engine
        .export_user_data(ExportUserDataInput {
            tenant: t.clone(),
            include_events: true,
            include_deleted: true,
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
    (facts, vectors, events, episodes, entities)
}

/// A valid complete export of an invented source user (facts, events, episodes, 1 entity).
async fn complete_export() -> UserMemoryExport {
    let source = world();
    seed(
        &source,
        &[
            "Jordan is training for a ten kilometre trail race.",
            "Jordan grows chilli peppers on the balcony.",
        ],
    )
    .await;
    let export = source
        .engine
        .export_user_data_complete(CompleteExportInput {
            tenant: tenant(),
            include_deleted: false,
            page_size: 1,
            max_records: 1_000,
        })
        .await
        .expect("complete export");
    let counts = &export.completeness.as_ref().expect("manifest").counts;
    assert!(counts.facts >= 1 && counts.memory_events >= 1);
    // add_memory also stores each message as an episode (2) + the seeded one.
    assert!(counts.episodes >= 1 && counts.entities == 1);
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
            restore_events: true,
            dry_run,
        })
        .await
}

type Tamper = fn(&mut UserMemoryExport);

fn manifest(e: &mut UserMemoryExport) -> &mut memcore_core::ExportCompleteness {
    e.completeness.as_mut().expect("manifest")
}

/// Each case breaks exactly one manifest rule (the counts are kept consistent otherwise), so the
/// specific check is the one that must fire.
fn cases() -> Vec<(&'static str, &'static str, Tamper)> {
    vec![
        ("missing episodes section", "carries every section", |e| {
            e.episodes = None;
            manifest(e).counts.episodes = 0;
        }),
        ("missing entities section", "carries every section", |e| {
            e.entities = None;
            let c = &mut manifest(e).counts;
            (c.entities, c.entity_links, c.entity_relations) = (0, 0, 0);
        }),
        ("unknown mode", "unknown completeness mode", |e| {
            manifest(e).mode = "partial".to_string()
        }),
        ("verified_reads below 2", "verified_reads", |e| {
            manifest(e).verified_reads = 1
        }),
        ("page_size 0", "page_size", |e| manifest(e).page_size = 0),
        ("page_size above the maximum", "page_size", |e| {
            manifest(e).page_size = 1_001
        }),
        (
            "section count inflated",
            "section counts do not match",
            |e| manifest(e).counts.facts += 1,
        ),
        (
            "missing page (record dropped, count kept)",
            "section counts do not match",
            |e| {
                e.facts.pop();
            },
        ),
        ("duplicate fact id", "duplicate record ids", |e| {
            let dup = e.facts[0].clone();
            e.facts.push(dup);
            manifest(e).counts.facts += 1;
        }),
        ("duplicate event id", "duplicate record ids", |e| {
            let dup = e.memory_events[0].clone();
            e.memory_events.push(dup);
            manifest(e).counts.memory_events += 1;
        }),
        ("duplicate entity id", "duplicate record ids", |e| {
            let entities = e.entities.as_mut().expect("entities");
            let dup = entities.entities[0].clone();
            entities.entities.push(dup);
            manifest(e).counts.entities += 1;
        }),
        (
            "complete and truncated",
            "both complete and truncated",
            |e| {
                e.truncated = Some(vec![ExportSectionTruncation {
                    section: "facts".to_string(),
                    limit: 100,
                }])
            },
        ),
    ]
}

#[tokio::test]
async fn every_manifest_check_rejects_before_any_write_in_every_mode() {
    let source = complete_export().await;
    for (name, fragment, tamper) in cases() {
        let mut archive = source.clone();
        tamper(&mut archive);
        for mode in [ImportMode::Append, ImportMode::Replace] {
            for dry_run in [false, true] {
                let target = world();
                seed(&target, &["Avery keeps a pottery wheel in the garage."]).await;
                let before = snapshot(&target).await;
                let result = import(&target, archive.clone(), mode, dry_run).await;
                if dry_run {
                    let output = result.expect("a dry run returns a report");
                    assert!(!output.validation.valid, "{name} {mode:?}: dry run valid");
                    assert!(!output.manifest_consistent, "{name}");
                    assert!(
                        output
                            .validation
                            .errors
                            .iter()
                            .any(|e| e.code == CODE && e.message.contains(fragment)),
                        "{name} {mode:?} dry run: {:?}",
                        output.validation.errors
                    );
                } else {
                    let error = result.expect_err(name);
                    assert!(
                        matches!(error, MemcoreError::ValidationError(_)),
                        "{name} {mode:?}: {error}"
                    );
                    assert!(
                        error.to_string().contains(fragment),
                        "{name} {mode:?}: {error}"
                    );
                }
                assert_eq!(
                    snapshot(&target).await,
                    before,
                    "{name} / {mode:?} / dry_run={dry_run}: nothing written"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_valid_manifest_imports_and_is_reported_as_manifest_consistent() {
    let archive = complete_export().await;
    let target = world();
    let output = import(&target, archive.clone(), ImportMode::Replace, false)
        .await
        .expect("valid complete archive imports");
    assert!(output.manifest_consistent);
    assert_eq!(output.imported_facts, archive.facts.len());
}

/// Owner ruling: the manifest verifies the archive's declared contents, not the source's
/// historical completeness. A record removed together with its count still validates; this test
/// pins that limitation so it is never mistaken for proof of completeness.
#[tokio::test]
async fn a_self_consistent_edit_still_validates_so_the_flag_is_not_source_completeness() {
    let mut archive = complete_export().await;
    let removed = archive.facts.pop().expect("a fact");
    manifest(&mut archive).counts.facts -= 1;
    let target = world();
    let output = import(&target, archive, ImportMode::Replace, true)
        .await
        .expect("dry run");
    assert!(output.validation.valid, "{:?}", output.validation.errors);
    assert!(output.manifest_consistent);
    let _ = removed;
}
