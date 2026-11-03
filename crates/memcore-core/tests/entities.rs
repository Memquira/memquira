//! The entity layer in the engine against mock
//! stores and a scripted mock model. Every name here is invented.
//!
//! Covers: off by default (no requests, no rows, the extraction request unchanged); annotation per
//! extraction window; resolution (normalisation, speaker authority, never merging two speakers,
//! type compatibility, the ambiguous path with at most one confirmation per new entity, reversible
//! merges); relations; tenant isolation (P3-04); forget-user, retention and export / import
//! (P3-03); annotation failure; PII redaction.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    AnnotationAttempt, EntityAnnotationInput, EntityAnnotationOutput, EntityMatchInput,
    EntityMention, EntityStore, FactAnnotation, FactClassificationInput, FactExtractionInput,
    LlmProvider, MemoryEventQuery, RelationMention, SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, AliasSource, ApplyRetentionInput, CandidateFact,
    EntityComponents, EntityConfig, EntityRole, EntityType, ExportUserDataInput,
    FactExtractionOutput, FactOperationDecision, FactStore, FactStructure, ForgetUserInput,
    ImportMode, ImportUserDataInput, ListEntitiesInput, MemoryEngine, MemoryEventOperation,
    MemoryEventStore, MemoryMessage, MemorySession, MemoryType, MessageRole, RetentionPolicy,
    StructuredExtractionConfig, TenantContext, USER_EXPORT_FORMAT_VERSION,
    USER_EXPORT_FORMAT_VERSION_V3,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEntityStore, MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;

struct World {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    entities: Arc<MockEntityStore>,
}

impl World {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
            events: Arc::new(MockMemoryEventStore::new()),
            entities: Arc::new(MockEntityStore::new()),
        }
    }

    fn engine(&self, llm: Arc<dyn LlmProvider>, config: Option<EntityConfig>) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            llm,
            Arc::new(MockEmbeddingProvider::new(64)),
        )
        .with_event_store(self.events.clone())
        .with_min_importance(0.0)
        .with_entities(config.map(|config| EntityComponents {
            store: self.entities.clone(),
            config,
        }))
    }
}

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_entities", user).unwrap()
}

fn fact(content: &str, subject: &str, message: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Relationship, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some(subject.to_string()),
            source_message_ids: vec![message.to_string()],
            ..FactStructure::default()
        })
}

fn said(id: &str, speaker: &str, text: &str) -> MemoryMessage {
    MemoryMessage {
        role: MessageRole::User,
        content: text.to_string(),
        name: Some(speaker.to_string()),
        timestamp: Some(Utc.with_ymd_and_hms(2026, 3, 4, 10, 0, 0).unwrap()),
        message_id: Some(id.to_string()),
    }
}

fn mention(name: &str, kind: EntityType, role: EntityRole) -> EntityMention {
    EntityMention {
        name: name.to_string(),
        entity_type: kind,
        role,
        aliases: Vec::new(),
    }
}

/// Every fact of every request is annotated with the same mentions (for single-fact requests).
fn annotate(
    mentions: Vec<EntityMention>,
    relations: Vec<RelationMention>,
) -> Arc<dyn Fn(&EntityAnnotationInput) -> EntityAnnotationOutput + Send + Sync> {
    Arc::new(move |input| EntityAnnotationOutput {
        facts: input
            .facts
            .iter()
            .map(|fact| FactAnnotation {
                fact_index: fact.index,
                entities: mentions.clone(),
                relations: relations.clone(),
            })
            .collect(),
        repairs: Vec::new(),
    })
}

async fn add(
    engine: &MemoryEngine,
    tenant: &TenantContext,
    messages: Vec<MemoryMessage>,
) -> AddMemoryOutput {
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages,
            metadata: json!({}),
            session: MemorySession::default(),
        })
        .await
        .expect("add_memory")
}

fn exact_only() -> EntityConfig {
    EntityConfig {
        name_similarity_threshold: 1.1,
        llm_confirmation: true,
        ..EntityConfig::default()
    }
}

fn always_candidate(llm_confirmation: bool) -> EntityConfig {
    EntityConfig {
        name_similarity_threshold: -1.0,
        llm_confirmation,
        ..EntityConfig::default()
    }
}

#[tokio::test]
async fn entities_are_off_by_default_and_the_extraction_request_is_unchanged() {
    let messages = vec![said("m1", "Ines", "I adopted a beagle called Biscuit.")];
    let candidates = vec![fact("Ines adopted a beagle called Biscuit.", "Ines", "m1")];

    let off_world = World::new();
    let off_llm = Arc::new(MockLlmProvider::new().with_extraction_candidates(candidates.clone()));
    let off = off_world.engine(off_llm.clone(), None);
    assert!(!off.entities_enabled());
    let off_output = add(&off, &tenant("user_a"), messages.clone()).await;
    assert!(off_output.entities.is_none());
    assert!(off_llm.annotation_inputs().is_empty());
    assert!(
        off_world
            .entities
            .list_all_entities(&tenant("user_a"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        off.list_entities(ListEntitiesInput {
            tenant: tenant("user_a"),
            limit: 10,
            cursor: None,
        })
        .await
        .is_err()
    );

    let on_world = World::new();
    let on_llm = Arc::new(MockLlmProvider::new().with_extraction_candidates(candidates));
    let on = on_world.engine(on_llm.clone(), Some(EntityConfig::default()));
    let on_output = add(&on, &tenant("user_a"), messages).await;
    // The extraction request and the stored facts are identical; annotation is a separate call.
    assert_eq!(
        on_llm.last_extraction_messages(),
        off_llm.last_extraction_messages()
    );
    assert_eq!(
        on_output
            .memories
            .iter()
            .map(|f| &f.content)
            .collect::<Vec<_>>(),
        off_output
            .memories
            .iter()
            .map(|f| &f.content)
            .collect::<Vec<_>>()
    );
    assert_eq!(on_llm.annotation_inputs().len(), 1);
    let summary = on_output.entities.expect("summary when on");
    assert_eq!(
        (summary.annotation_requests, summary.created, summary.links),
        (1, 1, 1)
    );
}

#[tokio::test]
async fn names_normalise_and_speakers_are_authoritative() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Ines keeps bees.", "Ines", "m1")])
            .with_entity_annotator(annotate(
                vec![
                    mention("INES's", EntityType::Other, EntityRole::Subject),
                    mention("ines", EntityType::Person, EntityRole::Mentioned),
                ],
                vec![],
            )),
    );
    let engine = world.engine(llm, Some(exact_only()));
    add(&engine, &user, vec![said("m1", "Ines", "I keep bees.")]).await;

    let entities = world.entities.list_all_entities(&user).await.unwrap();
    assert_eq!(entities.len(), 1, "{entities:?}");
    let ines = &entities[0];
    assert_eq!(ines.canonical_name, "Ines", "the speaker's own name");
    assert!(ines.is_speaker);
    assert_eq!(ines.entity_type, EntityType::Person);
    let links = world
        .entities
        .links_for_entity(&user, ines.id)
        .await
        .unwrap();
    assert_eq!(links.len(), 2, "subject and mentioned roles");
}

#[tokio::test]
async fn two_speakers_are_never_merged_even_when_the_model_would_confirm() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Ana and Anna cook together.", "Ana", "m1")])
            .with_entity_annotator(annotate(
                vec![
                    mention("Ana", EntityType::Person, EntityRole::Subject),
                    mention("Anna", EntityType::Person, EntityRole::Object),
                ],
                vec![],
            ))
            .with_entity_matcher(Arc::new(|_| true)),
    );
    let engine = world.engine(llm.clone(), Some(always_candidate(true)));
    add(
        &engine,
        &user,
        vec![
            said("m1", "Ana", "Anna and I cook."),
            said("m2", "Anna", "Yes!"),
        ],
    )
    .await;
    let entities = world.entities.list_all_entities(&user).await.unwrap();
    assert_eq!(entities.len(), 2, "{entities:?}");
    assert!(entities.iter().all(|entity| entity.is_speaker));
    assert!(
        llm.match_inputs().is_empty(),
        "speakers are never matched by similarity"
    );
}

#[tokio::test]
async fn incompatible_types_stay_apart() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact(
                "Tomasz walked Luna in Luna Park.",
                "Tomasz",
                "m1",
            )])
            .with_entity_annotator(annotate(
                vec![
                    mention("Luna", EntityType::Animal, EntityRole::Object),
                    mention("Luna", EntityType::Place, EntityRole::Mentioned),
                ],
                vec![],
            )),
    );
    add(
        &world.engine(llm, Some(exact_only())),
        &user,
        vec![said("m1", "Tomasz", "x")],
    )
    .await;
    let mut kinds: Vec<_> = world
        .entities
        .list_all_entities(&user)
        .await
        .unwrap()
        .iter()
        .map(|entity| entity.entity_type)
        .collect();
    kinds.sort_by_key(|kind| kind.as_str());
    assert_eq!(kinds, vec![EntityType::Animal, EntityType::Place]);
}

/// Ambiguous path: a candidate above the threshold asks the model once; a confirmed match becomes a
/// `merge` alias with its similarity and an audit event, and can be split back out.
#[tokio::test]
async fn a_confirmed_candidate_merges_reversibly_with_one_confirmation() {
    let world = World::new();
    let user = tenant("user_a");
    let first = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Tomasz Nowak plays cello.", "Tomasz", "m1")])
            .with_entity_annotator(annotate(
                vec![mention(
                    "Tomasz Nowak",
                    EntityType::Person,
                    EntityRole::Subject,
                )],
                vec![],
            )),
    );
    add(
        &world.engine(first, Some(always_candidate(true))),
        &user,
        vec![said("m1", "Ines", "x")],
    )
    .await;

    let second = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Tom plays in a quartet.", "Tom", "m2")])
            .with_entity_annotator(annotate(
                vec![mention("Tom", EntityType::Person, EntityRole::Subject)],
                vec![],
            ))
            .with_entity_matcher(Arc::new(|input: &EntityMatchInput| {
                input.candidate_name == "Tomasz Nowak"
            })),
    );
    let engine = world.engine(second.clone(), Some(always_candidate(true)));
    let output = add(&engine, &user, vec![said("m2", "Ines", "y")]).await;
    let summary = output.entities.unwrap();
    assert_eq!(
        (summary.merged, summary.confirmations, summary.created),
        (1, 1, 0)
    );
    // One confirmation for the one new mention.
    assert_eq!(second.match_inputs().len(), 1);

    let entities = world.entities.list_all_entities(&user).await.unwrap();
    let tomasz = entities
        .iter()
        .find(|entity| entity.canonical_name == "Tomasz Nowak")
        .unwrap();
    let alias = tomasz
        .aliases
        .iter()
        .find(|alias| alias.name == "Tom")
        .unwrap();
    assert_eq!(alias.source, AliasSource::Merge);
    assert!(alias.similarity.is_some());
    let merge_event = world
        .events
        .list_events(MemoryEventQuery::new(user.clone(), 50))
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.operation == MemoryEventOperation::EntityMerge)
        .expect("merge audited");
    assert_eq!(merge_event.metadata["alias"], "Tom");

    // A later "Tom" is an exact alias match: no further confirmation.
    let third = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Tom tunes his cello.", "Tom", "m3")])
            .with_entity_annotator(annotate(
                vec![mention("Tom", EntityType::Person, EntityRole::Subject)],
                vec![],
            ))
            .with_entity_matcher(Arc::new(|_| true)),
    );
    add(
        &world.engine(third.clone(), Some(always_candidate(true))),
        &user,
        vec![said("m3", "Ines", "z")],
    )
    .await;
    assert!(third.match_inputs().is_empty());
    assert_eq!(
        world
            .entities
            .links_for_entity(&user, tomasz.id)
            .await
            .unwrap()
            .len(),
        3
    );

    // Reverse the merge: "Tom" becomes its own entity and takes its two links along.
    let split = engine
        .split_entity_alias(&user, tomasz.id, "tom")
        .await
        .unwrap();
    assert_eq!(split.canonical_name, "Tom");
    assert_eq!(
        world
            .entities
            .links_for_entity(&user, split.id)
            .await
            .unwrap()
            .len(),
        2
    );
    let tomasz_now = world
        .entities
        .get_entity(&user, tomasz.id)
        .await
        .unwrap()
        .unwrap();
    assert!(tomasz_now.aliases.iter().all(|alias| alias.name != "Tom"));
    assert_eq!(
        world
            .entities
            .links_for_entity(&user, tomasz.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        world
            .events
            .list_events(MemoryEventQuery::new(user.clone(), 50))
            .await
            .unwrap()
            .iter()
            .any(|event| event.operation == MemoryEventOperation::EntitySplit)
    );
    // Only merged aliases can be split.
    assert!(
        engine
            .split_entity_alias(&user, tomasz.id, "Tomasz Nowak")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_rejected_or_unconfirmed_candidate_becomes_a_new_entity() {
    for (llm_confirmation, confirms) in [(true, false), (false, true)] {
        let world = World::new();
        let user = tenant("user_a");
        for (content, name, id) in [
            ("Sam paints.", "Sam", "m1"),
            ("Samir surfs.", "Samir", "m2"),
        ] {
            let llm = Arc::new(
                MockLlmProvider::new()
                    .with_extraction_candidates(vec![fact(content, name, id)])
                    .with_entity_annotator(annotate(
                        vec![mention(name, EntityType::Person, EntityRole::Subject)],
                        vec![],
                    ))
                    .with_entity_matcher(Arc::new(move |_| confirms)),
            );
            add(
                &world.engine(llm.clone(), Some(always_candidate(llm_confirmation))),
                &user,
                vec![said(id, "Ines", "x")],
            )
            .await;
            if !llm_confirmation {
                assert!(
                    llm.match_inputs().is_empty(),
                    "confirmation off: never asked"
                );
            }
        }
        let names: Vec<_> = world
            .entities
            .list_all_entities(&user)
            .await
            .unwrap()
            .into_iter()
            .filter(|entity| !entity.is_speaker)
            .map(|entity| entity.canonical_name)
            .collect();
        assert_eq!(
            names,
            vec!["Sam", "Samir"],
            "confirmation {llm_confirmation}"
        );
    }
}

#[tokio::test]
async fn relations_connect_the_facts_own_entities() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Ines adopted Biscuit.", "Ines", "m1")])
            .with_entity_annotator(annotate(
                vec![
                    mention("Ines", EntityType::Person, EntityRole::Subject),
                    mention("Biscuit", EntityType::Animal, EntityRole::Object),
                ],
                vec![
                    RelationMention {
                        subject: "Ines".to_string(),
                        relation: "owns".to_string(),
                        object: "Biscuit".to_string(),
                    },
                    RelationMention {
                        subject: "Ines".to_string(),
                        relation: "owns".to_string(),
                        object: "Somebody Else".to_string(),
                    },
                ],
            )),
    );
    let engine = world.engine(llm, Some(exact_only()));
    let output = add(&engine, &user, vec![said("m1", "Ines", "x")]).await;
    assert_eq!(output.entities.unwrap().relations, 1);
    let biscuit = world
        .entities
        .list_all_entities(&user)
        .await
        .unwrap()
        .into_iter()
        .find(|entity| entity.canonical_name == "Biscuit")
        .unwrap();
    let detail = engine.get_entity(&user, biscuit.id).await.unwrap().unwrap();
    assert_eq!(detail.facts.len(), 1);
    assert_eq!(detail.facts[0].content, "Ines adopted Biscuit.");
    assert!(detail.facts[0].active);
    assert_eq!(detail.relations.len(), 1);
    assert_eq!(detail.relations[0].relation.relation, "owns");
    assert_eq!(detail.relations[0].source_name.as_deref(), Some("Ines"));
    assert!(detail.entity.name_embedding.is_none(), "never exposed");
}

/// P3-04: the same names in two users' stores never resolve to each other's entities, and no read
/// crosses users or organizations.
#[tokio::test]
async fn entities_never_resolve_or_read_across_users_or_organizations() {
    let world = World::new();
    let alice = tenant("user_a");
    let bob = tenant("user_b");
    let other_org = TenantContext::new("org_other", "user_a").unwrap();
    for user in [&alice, &bob, &other_org] {
        let llm = Arc::new(
            MockLlmProvider::new()
                .with_extraction_candidates(vec![fact("Biscuit barks.", "Biscuit", "m1")])
                .with_entity_annotator(annotate(
                    vec![mention("Biscuit", EntityType::Animal, EntityRole::Subject)],
                    vec![],
                ))
                .with_entity_matcher(Arc::new(|_| true)),
        );
        add(
            &world.engine(llm, Some(always_candidate(true))),
            user,
            vec![said("m1", "Ines", "x")],
        )
        .await;
    }
    let engine = world.engine(Arc::new(MockLlmProvider::new()), Some(exact_only()));
    let mut ids = Vec::new();
    for user in [&alice, &bob, &other_org] {
        let page = engine
            .list_entities(ListEntitiesInput {
                tenant: user.clone(),
                limit: 10,
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(page.entities.len(), 1, "each user's own Biscuit");
        assert!(
            page.entities
                .iter()
                .all(|entity| entity.user_id == user.user_id && entity.org_id == user.org_id)
        );
        ids.push(page.entities[0].id);
    }
    assert!(engine.get_entity(&bob, ids[0]).await.unwrap().is_none());
    assert!(
        engine
            .get_entity(&other_org, ids[0])
            .await
            .unwrap()
            .is_none()
    );
    assert!(engine.get_entity(&alice, ids[0]).await.unwrap().is_some());
}

#[tokio::test]
async fn entity_pages_follow_the_cursor() {
    let world = World::new();
    let user = tenant("user_a");
    let names = ["Aoife", "Bram", "Cleo", "Dara", "Eero"];
    for (index, name) in names.iter().enumerate() {
        let id = format!("m{index}");
        let llm = Arc::new(
            MockLlmProvider::new()
                .with_extraction_candidates(vec![fact(&format!("{name} visited."), name, &id)])
                .with_entity_annotator(annotate(
                    vec![mention(name, EntityType::Person, EntityRole::Subject)],
                    vec![],
                )),
        );
        let message = MemoryMessage {
            name: None,
            ..said(&id, "Ines", "x")
        };
        add(&world.engine(llm, Some(exact_only())), &user, vec![message]).await;
    }
    let engine = world.engine(Arc::new(MockLlmProvider::new()), Some(exact_only()));
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = engine
            .list_entities(ListEntitiesInput {
                tenant: user.clone(),
                limit: 2,
                cursor: cursor.clone(),
            })
            .await
            .unwrap();
        seen.extend(
            page.entities
                .iter()
                .map(|entity| entity.canonical_name.clone()),
        );
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let mut expected: Vec<String> = names.iter().rev().map(|name| name.to_string()).collect();
    expected.sort();
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(sorted, expected);
    assert_eq!(seen.len(), 5, "no entity twice: {seen:?}");
    assert!(
        engine
            .list_entities(ListEntitiesInput {
                tenant: user,
                limit: 101,
                cursor: None,
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn forget_user_and_retention_remove_entities() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Biscuit barks.", "Biscuit", "m1")])
            .with_entity_annotator(annotate(
                vec![mention("Biscuit", EntityType::Animal, EntityRole::Subject)],
                vec![],
            )),
    );
    let engine = world.engine(llm, Some(exact_only()));
    add(&engine, &user, vec![said("m1", "Ines", "x")]).await;
    assert_eq!(
        world.entities.list_all_entities(&user).await.unwrap().len(),
        1
    );

    // Retention prunes the fact; Biscuit loses its only link and goes with it.
    let mut stored = world
        .facts
        .search_facts(memcore_core::FactSearchQuery::new(user.clone(), 10))
        .await
        .unwrap()
        .remove(0);
    stored.updated_at = Utc::now() - chrono::Duration::days(400);
    world.facts.update_fact(&user, stored).await.unwrap();
    engine
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
    assert!(
        world
            .entities
            .list_all_entities(&user)
            .await
            .unwrap()
            .is_empty()
    );

    add(&engine, &user, vec![said("m1", "Ines", "x")]).await;
    assert!(
        !world
            .entities
            .list_all_entities(&user)
            .await
            .unwrap()
            .is_empty()
    );
    engine
        .forget_user(ForgetUserInput {
            tenant: user.clone(),
        })
        .await
        .unwrap();
    assert!(
        world
            .entities
            .list_all_entities(&user)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn export_is_v3_with_entities_and_import_remaps_them() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Ines adopted Biscuit.", "Ines", "m1")])
            .with_entity_annotator(annotate(
                vec![
                    mention("Ines", EntityType::Person, EntityRole::Subject),
                    mention("Biscuit", EntityType::Animal, EntityRole::Object),
                ],
                vec![RelationMention {
                    subject: "Ines".to_string(),
                    relation: "owns".to_string(),
                    object: "Biscuit".to_string(),
                }],
            )),
    );
    let engine = world.engine(llm, Some(exact_only()));
    add(&engine, &user, vec![said("m1", "Ines", "x")]).await;
    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant: user.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(export.format_version, USER_EXPORT_FORMAT_VERSION_V3);
    let exported = export.entities.clone().unwrap();
    assert_eq!(
        (
            exported.entities.len(),
            exported.links.len(),
            exported.relations.len()
        ),
        (2, 2, 1)
    );
    // Name embeddings travel with their provenance.
    for entity in &exported.entities {
        let embedding = entity.name_embedding.as_ref().expect("exported embedding");
        assert_eq!(entity.name_embedding_model.as_deref(), Some("unspecified"));
        assert_eq!(entity.name_embedding_dimensions, Some(embedding.len()));
    }

    // An engine with no entity store at all keeps the v1 export.
    let plain = world.engine(Arc::new(MockLlmProvider::new()), None);
    let v1 = plain
        .export_user_data(ExportUserDataInput {
            tenant: user.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(v1.format_version, USER_EXPORT_FORMAT_VERSION);
    assert!(v1.entities.is_none());

    // Re-applying the identical export into the same
    // user is rejected before any write (before R7 its facts were re-keyed into duplicates).
    let err = engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export: export.clone(),
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .expect_err("re-application rejected");
    assert!(err.is_import_already_applied());
    // Append into the same user with edited fact text (a live id with different content keeps
    // the re-key): new fact and entity IDs, links and relations follow.
    let mut edited = export.clone();
    for fact in &mut edited.facts {
        fact.content = format!("{} (edited after export)", fact.content);
    }
    let output = engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export: edited,
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(output.imported_entities, Some(2));
    let entities = world.entities.list_all_entities(&user).await.unwrap();
    assert_eq!(entities.len(), 4);
    let facts = world
        .facts
        .search_facts(memcore_core::FactSearchQuery::new(user.clone(), 10))
        .await
        .unwrap();
    assert_eq!(facts.len(), 2);
    let copy = entities
        .iter()
        .find(|entity| {
            entity.canonical_name == "Biscuit"
                && !exported.entities.iter().any(|e| e.id == entity.id)
        })
        .unwrap();
    let original = exported
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Biscuit")
        .unwrap();
    assert_eq!(
        copy.name_embedding, original.name_embedding,
        "kept on import"
    );
    assert_eq!(copy.name_embedding_model, original.name_embedding_model);
    assert_eq!(
        copy.name_embedding_dimensions,
        original.name_embedding_dimensions
    );
    let links = world
        .entities
        .links_for_entity(&user, copy.id)
        .await
        .unwrap();
    assert_eq!(links.len(), 1);
    assert!(facts.iter().any(|fact| fact.id == links[0].fact_id));
    assert!(
        !exported
            .links
            .iter()
            .any(|link| link.fact_id == links[0].fact_id),
        "re-keyed fact"
    );
    assert_eq!(
        world
            .entities
            .relations_for_entity(&user, copy.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // Replace: the user's entities are replaced by the export's.
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
    assert_eq!(
        world.entities.list_all_entities(&user).await.unwrap().len(),
        2
    );

    // A server without entities skips them.
    let other = tenant("user_z");
    let skipped = plain
        .import_user_data(ImportUserDataInput {
            tenant: other.clone(),
            export: memcore_core::UserMemoryExport {
                org_id: other.org_id.clone(),
                user_id: other.user_id.clone(),
                facts: vec![],
                entities: Some(memcore_core::EntityExport {
                    entities: exported
                        .entities
                        .iter()
                        .map(|entity| memcore_core::Entity {
                            user_id: other.user_id.clone(),
                            ..entity.clone()
                        })
                        .collect(),
                    links: vec![],
                    relations: vec![],
                }),
                ..export
            },
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(skipped.imported_entities, Some(0));
    assert_eq!(skipped.skipped_entities, Some(2));
}

/// A provider whose annotation always fails; everything else is the mock.
struct FailingAnnotation(MockLlmProvider);

#[async_trait]
impl LlmProvider for FailingAnnotation {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        self.0.extract_facts(input).await
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.0.classify_fact_operation(input).await
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        self.0.summarize_memory(input).await
    }

    async fn annotate_entities(
        &self,
        _input: EntityAnnotationInput,
    ) -> MemcoreResult<EntityAnnotationOutput> {
        Err(MemcoreError::ProviderError(
            "annotation unavailable".to_string(),
        ))
    }
}

#[tokio::test]
async fn a_failed_annotation_keeps_the_facts_and_is_reported() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(FailingAnnotation(
        MockLlmProvider::new().with_extraction_candidates(vec![fact(
            "Biscuit barks.",
            "Biscuit",
            "m1",
        )]),
    ));
    let output = add(
        &world.engine(llm, Some(exact_only())),
        &user,
        vec![said("m1", "Ines", "x")],
    )
    .await;
    assert_eq!(output.added, 1);
    let summary = output.entities.unwrap();
    assert_eq!(
        (summary.annotation_requests, summary.failed_annotations),
        (1, 1)
    );
    assert!(
        world
            .entities
            .list_all_entities(&user)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A provider whose first annotation output is unusable; later requests are the mock's.
struct UnusableFirstAnnotation(MockLlmProvider, std::sync::atomic::AtomicUsize);

#[async_trait]
impl LlmProvider for UnusableFirstAnnotation {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        self.0.extract_facts(input).await
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.0.classify_fact_operation(input).await
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        self.0.summarize_memory(input).await
    }

    async fn annotate_entities(
        &self,
        input: EntityAnnotationInput,
    ) -> MemcoreResult<EntityAnnotationOutput> {
        let output = self.0.annotate_entities(input).await;
        if self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Err(memcore_core::ports::unusable_annotation_output(
                "syntax",
                "unusable at line 1 column 7",
            ));
        }
        output
    }
}

#[tokio::test]
async fn an_unusable_annotation_is_retried_once_and_ungrounded_mentions_are_rejected() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(UnusableFirstAnnotation(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Biscuit barks at night.", "Biscuit", "m1")])
            .with_entity_annotator(annotate(
                vec![
                    mention("Biscuit", EntityType::Animal, EntityRole::Subject),
                    // Not in the messages or the fact: speculative, rejected.
                    mention("Quorra Ridge", EntityType::Place, EntityRole::Mentioned),
                ],
                vec![],
            )),
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let output = add(
        &world.engine(llm.clone(), Some(exact_only())),
        &user,
        vec![said("m1", "Ines", "Biscuit barks every night.")],
    )
    .await;
    let summary = output.entities.unwrap();
    assert_eq!(
        (
            summary.annotation_requests,
            summary.retried_annotations,
            summary.failed_annotations,
            summary.rejected_mentions,
            summary.created,
        ),
        (1, 1, 0, 1, 1)
    );
    let inputs = llm.0.annotation_inputs();
    assert_eq!(
        inputs.iter().map(|input| input.attempt).collect::<Vec<_>>(),
        vec![AnnotationAttempt::First, AnnotationAttempt::Retry]
    );
    // The retry carries exactly the same evidence.
    assert_eq!(inputs[0].messages, inputs[1].messages);
    assert_eq!(inputs[0].facts, inputs[1].facts);
    let names: Vec<String> = world
        .entities
        .list_all_entities(&user)
        .await
        .unwrap()
        .into_iter()
        .map(|entity| entity.canonical_name)
        .collect();
    assert_eq!(names, vec!["Biscuit"]);
}

#[tokio::test]
async fn annotation_follows_the_extraction_windows() {
    let world = World::new();
    let user = tenant("user_a");
    let messages: Vec<MemoryMessage> = (0..6)
        .map(|index| {
            said(
                &format!("m{index}"),
                if index % 2 == 0 { "Ines" } else { "Tomasz" },
                "x",
            )
        })
        .collect();
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![
                fact("Ines rows.", "Ines", "m0"),
                fact("Tomasz sails.", "Tomasz", "m5"),
            ])
            .with_entity_annotator(annotate(vec![], vec![])),
    );
    let engine = world
        .engine(llm.clone(), Some(exact_only()))
        .with_structured_extraction(Some(StructuredExtractionConfig {
            window_turns: 3,
            window_overlap: 0,
            concurrency: 2,
        }));
    add(&engine, &user, messages).await;
    let inputs = llm.annotation_inputs();
    assert_eq!(
        inputs.len(),
        2,
        "one request per window holding stored facts"
    );
    assert_eq!(inputs[0].facts.len(), 1);
    assert_eq!(inputs[0].facts[0].content, "Ines rows.");
    assert_eq!(inputs[1].facts[0].content, "Tomasz sails.");
    assert_eq!(inputs[0].speakers, vec!["Ines", "Tomasz"]);
    assert_eq!(inputs[0].messages.len(), 3);
}

#[tokio::test]
async fn entity_names_are_redacted_with_pii_redaction_on() {
    let world = World::new();
    let user = tenant("user_a");
    let llm = Arc::new(
        MockLlmProvider::new()
            .with_extraction_candidates(vec![fact("Ines emailed a friend.", "Ines", "m1")])
            .with_entity_annotator(annotate(
                vec![mention(
                    "ines.dev@example.com",
                    EntityType::Person,
                    EntityRole::Object,
                )],
                vec![],
            )),
    );
    let engine = world
        .engine(llm, Some(exact_only()))
        .with_pii_redaction(true);
    add(&engine, &user, vec![said("m1", "Ines", "x")]).await;
    let names: Vec<String> = world
        .entities
        .list_all_entities(&user)
        .await
        .unwrap()
        .into_iter()
        .map(|entity| entity.canonical_name)
        .collect();
    assert!(
        names.iter().all(|name| !name.contains("example.com")),
        "{names:?}"
    );
}
