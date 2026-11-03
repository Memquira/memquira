//! A feature flag controls creating and using optional data, never whether persisted user
//! data can be deleted, retained, exported or imported.
//!
//! Each test writes entity or episode data through an engine with the feature on, then runs the
//! lifecycle operation through an engine on the same stores with the feature off. entity
//! name embeddings are compared only within one embedding model and dimension. Every name here is
//! invented.

use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_core::ports::{
    EntityAnnotationOutput, EntityMention, EntityStore, EpisodeStore, FactAnnotation,
    RelationMention, VectorSearchQuery,
};
use memcore_core::{
    AddMemoryInput, ApplyRetentionInput, CandidateFact, DeleteMemoryInput, EntityComponents,
    EntityConfig, EntityRole, EntityType, EpisodeComponents, EpisodeLifecycle, ExportUserDataInput,
    FactSearchQuery, FactStore, FactStructure, ForgetUserInput, ImportMode, ImportUserDataInput,
    ListEntitiesInput, MemoryEngine, MemoryMessage, MemoryType, MessageRole, RetentionPolicy,
    TenantContext, USER_EXPORT_FORMAT_VERSION, USER_EXPORT_FORMAT_VERSION_V3, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEntityStore, MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_lifecycle_flags", user).unwrap()
}

struct Stores {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    entities: Arc<MockEntityStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
}

impl Stores {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
            entities: Arc::new(MockEntityStore::new()),
            episodes: Arc::new(MockEpisodeStore::new()),
            episode_vectors: Arc::new(MockVectorStore::new()),
        }
    }

    fn base(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            Arc::new(llm),
            Arc::new(MockEmbeddingProvider::new(64)),
        )
        .with_min_importance(0.0)
    }

    /// Entities on: every fact names Ines (person) and Biscuit (animal), Ines owns Biscuit.
    fn entities_on(&self) -> MemoryEngine {
        let candidate = CandidateFact::new(
            "Ines adopted Biscuit.",
            MemoryType::Relationship,
            0.9,
            0.9,
            None,
            json!({}),
        )
        .unwrap()
        .with_structure(FactStructure {
            subject: Some("Ines".to_string()),
            ..FactStructure::default()
        });
        let llm = MockLlmProvider::new()
            .with_extraction_candidates(vec![candidate])
            .with_entity_annotator(Arc::new(|input| EntityAnnotationOutput {
                facts: input
                    .facts
                    .iter()
                    .map(|fact| FactAnnotation {
                        fact_index: fact.index,
                        entities: vec![
                            EntityMention {
                                name: "Ines".to_string(),
                                entity_type: EntityType::Person,
                                role: EntityRole::Subject,
                                aliases: vec![],
                            },
                            EntityMention {
                                name: "Biscuit".to_string(),
                                entity_type: EntityType::Animal,
                                role: EntityRole::Object,
                                aliases: vec![],
                            },
                        ],
                        relations: vec![RelationMention {
                            subject: "Ines".to_string(),
                            relation: "owns".to_string(),
                            object: "Biscuit".to_string(),
                        }],
                    })
                    .collect(),
                repairs: Vec::new(),
            }));
        self.base(llm).with_entities(Some(EntityComponents {
            store: self.entities.clone(),
            config: EntityConfig::default(),
        }))
    }

    /// Entities off, with the entity store for lifecycle operations only (as the API wires it).
    fn entities_off(&self, llm: MockLlmProvider) -> MemoryEngine {
        self.base(llm)
            .with_entity_store(Some(self.entities.clone() as Arc<dyn EntityStore>))
    }

    fn episodes_on(&self) -> MemoryEngine {
        self.base(MockLlmProvider::new())
            .with_episodes(Some(EpisodeComponents {
                store: self.episodes.clone(),
                vectors: self.episode_vectors.clone(),
            }))
    }

    fn episodes_off(&self) -> MemoryEngine {
        self.base(MockLlmProvider::new())
            .with_episode_lifecycle(Some(EpisodeLifecycle {
                store: self.episodes.clone(),
                vectors: self.episode_vectors.clone() as Arc<dyn VectorStore>,
            }))
    }
}

async fn add(engine: &MemoryEngine, user: &TenantContext, text: &str) {
    engine
        .add_memory(AddMemoryInput {
            tenant: user.clone(),
            messages: vec![MemoryMessage {
                name: Some("Ines".to_string()),
                ..MemoryMessage::new(MessageRole::User, text)
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .unwrap();
}

async fn entity_rows(stores: &Stores, user: &TenantContext) -> (usize, usize, usize) {
    let entities = stores.entities.list_all_entities(user).await.unwrap();
    let mut links = 0;
    let mut relations = std::collections::HashSet::new();
    for entity in &entities {
        links += stores
            .entities
            .links_for_entity(user, entity.id)
            .await
            .unwrap()
            .len();
        for relation in stores
            .entities
            .relations_for_entity(user, entity.id)
            .await
            .unwrap()
        {
            relations.insert(relation.id);
        }
    }
    (entities.len(), links, relations.len())
}

async fn vectors_of(store: &MockVectorStore, user: &TenantContext) -> usize {
    store
        .search_vectors(VectorSearchQuery {
            tenant: user.clone(),
            embedding: vec![1.0 / 8.0; 64],
            limit: 100,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .unwrap()
        .len()
}

/// Forget-user with entities off removes the user's entity rows, links and
/// relations, and nobody else's.
#[tokio::test]
async fn forget_user_with_entities_off_deletes_persisted_entity_data() {
    let stores = Stores::new();
    let (ines, other) = (tenant("user_ines"), tenant("user_other"));
    let on = stores.entities_on();
    add(&on, &ines, "I adopted Biscuit.").await;
    add(&on, &other, "I adopted Biscuit.").await;
    assert_eq!(entity_rows(&stores, &ines).await, (2, 2, 1));

    let off = stores.entities_off(MockLlmProvider::new());
    assert!(!off.entities_enabled());
    off.forget_user(ForgetUserInput {
        tenant: ines.clone(),
    })
    .await
    .unwrap();
    assert_eq!(entity_rows(&stores, &ines).await, (0, 0, 0));
    assert_eq!(
        entity_rows(&stores, &other).await,
        (2, 2, 1),
        "other user untouched"
    );
}

/// With entities off, deleting a fact and pruning by retention remove its links,
/// relations and the entities left without a link.
#[tokio::test]
async fn fact_deletion_and_retention_with_entities_off_clean_entity_data() {
    let stores = Stores::new();
    let (ines, other) = (tenant("user_ines"), tenant("user_other"));
    let on = stores.entities_on();
    add(&on, &ines, "I adopted Biscuit.").await;
    add(&on, &other, "I adopted Biscuit.").await;
    let off = stores.entities_off(MockLlmProvider::new());

    let fact = stores
        .facts
        .search_facts(FactSearchQuery::new(ines.clone(), 10))
        .await
        .unwrap()
        .remove(0);
    off.delete_memory(DeleteMemoryInput {
        tenant: ines.clone(),
        memory_id: fact.id,
    })
    .await
    .unwrap();
    assert_eq!(entity_rows(&stores, &ines).await, (0, 0, 0));

    // Retention: the other user's fact is aged and pruned with entities off.
    let mut aged = stores
        .facts
        .search_facts(FactSearchQuery::new(other.clone(), 10))
        .await
        .unwrap()
        .remove(0);
    aged.updated_at = Utc::now() - Duration::days(400);
    stores.facts.update_fact(&other, aged).await.unwrap();
    // A fresh fact of a third user keeps its entities.
    let third = tenant("user_third");
    add(&on, &third, "I adopted Biscuit.").await;
    off.apply_retention(ApplyRetentionInput {
        tenant: other.clone(),
        policy: RetentionPolicy {
            enabled: true,
            fact_retention_days: Some(365),
            event_retention_days: None,
        },
        dry_run: false,
    })
    .await
    .unwrap();
    assert_eq!(entity_rows(&stores, &other).await, (0, 0, 0));
    assert_eq!(entity_rows(&stores, &third).await, (2, 2, 1));
}

/// With entities off, persisted entity data is still exported (v3) and restored by
/// import, while entity behaviour stays off (no annotation, read endpoints disabled). A user
/// without entity data keeps the v1 export.
#[tokio::test]
async fn export_and_import_with_entities_off_keep_entity_data() {
    let stores = Stores::new();
    let ines = tenant("user_ines");
    add(&stores.entities_on(), &ines, "I adopted Biscuit.").await;
    let off_llm = MockLlmProvider::new();
    let off = stores.entities_off(off_llm);

    let export = off
        .export_user_data(ExportUserDataInput {
            tenant: ines.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(export.format_version, USER_EXPORT_FORMAT_VERSION_V3);
    assert_eq!(export.entities.as_ref().unwrap().entities.len(), 2);

    let nobody = tenant("user_nobody");
    let plain = off
        .export_user_data(ExportUserDataInput {
            tenant: nobody,
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(plain.format_version, USER_EXPORT_FORMAT_VERSION);
    assert!(plain.entities.is_none());

    let output = off
        .import_user_data(ImportUserDataInput {
            tenant: ines.clone(),
            export,
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(output.imported_entities, Some(2));
    assert_eq!(entity_rows(&stores, &ines).await, (2, 2, 1));

    // Entity behaviour is still off.
    assert!(!off.entities_enabled());
    assert!(
        off.list_entities(ListEntitiesInput {
            tenant: ines.clone(),
            limit: 10,
            cursor: None,
        })
        .await
        .is_err()
    );
    let annotating = MockLlmProvider::new().with_extraction_candidates(vec![]);
    let off_again = stores.entities_off(annotating);
    add(&off_again, &ines, "Hello.").await;
    assert_eq!(
        entity_rows(&stores, &ines).await,
        (2, 2, 1),
        "nothing new annotated"
    );
}

/// Forget-user and retention reach persisted episodes with episodes off; other users
/// are untouched.
#[tokio::test]
async fn forget_user_and_retention_with_episodes_off_delete_persisted_episodes() {
    let stores = Stores::new();
    let (ines, other) = (tenant("user_ines"), tenant("user_other"));
    let on = stores.episodes_on();
    add(&on, &ines, "I adopted a beagle called Biscuit last spring.").await;
    add(
        &on,
        &other,
        "I adopted a beagle called Biscuit last spring.",
    )
    .await;
    assert_eq!(
        stores
            .episodes
            .list_episodes(&ines, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(vectors_of(&stores.episode_vectors, &ines).await, 1);

    let off = stores.episodes_off();
    assert!(!off.episodes_enabled());
    off.forget_user(ForgetUserInput {
        tenant: ines.clone(),
    })
    .await
    .unwrap();
    assert!(
        stores
            .episodes
            .list_episodes(&ines, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(vectors_of(&stores.episode_vectors, &ines).await, 0);
    assert_eq!(
        stores
            .episodes
            .list_episodes(&other, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(vectors_of(&stores.episode_vectors, &other).await, 1);

    // Retention with episodes off prunes the other user's episode by its storage time.
    let output = off
        .apply_retention(ApplyRetentionInput {
            tenant: other.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(1),
                event_retention_days: None,
            },
            dry_run: true,
        })
        .await
        .unwrap();
    assert_eq!(
        output.episodes_matched, None,
        "nothing old enough: response unchanged"
    );

    // An episode stored 400 days ago is pruned with episodes off, and reported.
    let old = memcore_core::Episode {
        id: uuid::Uuid::new_v4(),
        org_id: other.org_id.clone(),
        user_id: other.user_id.clone(),
        session_id: None,
        message_id: Some("old-1".to_string()),
        speaker: Some("Ines".to_string()),
        role: MessageRole::User,
        content: "An old message about kayaking.".to_string(),
        observed_at: None,
        recorded_at: Utc::now() - Duration::days(400),
        embedded: false,
        metadata: json!({}),
    };
    stores
        .episodes
        .insert_episodes(&other, vec![old])
        .await
        .unwrap();
    let output = off
        .apply_retention(ApplyRetentionInput {
            tenant: other.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(365),
                event_retention_days: None,
            },
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(
        (output.episodes_matched, output.episodes_deleted),
        (Some(1), Some(1))
    );
    assert_eq!(
        stores
            .episodes
            .list_episodes(&other, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    // Without any episode vector collection, forget still deletes the rows.
    let rows_only = stores
        .base(MockLlmProvider::new())
        .with_episode_lifecycle(Some(EpisodeLifecycle {
            store: stores.episodes.clone(),
            vectors: Arc::new(never_created()),
        }));
    rows_only
        .forget_user(ForgetUserInput {
            tenant: other.clone(),
        })
        .await
        .unwrap();
    assert!(
        stores
            .episodes
            .list_episodes(&other, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A stored name embedding from another model or dimension is never compared, so it cannot
/// produce a candidate match (no confirmation request); the same model and dimension can.
#[tokio::test]
async fn name_embeddings_are_compared_only_within_one_model_and_dimension() {
    for (stored_model, stored_dimensions, compared) in [
        (Some("mock/embedding-a"), Some(64), true),
        (Some("mock/embedding-b"), Some(64), false),
        (Some("mock/embedding-a"), Some(32), false),
        (None, None, false),
    ] {
        let stores = Stores::new();
        let user = tenant("user_ines");
        let now = Utc::now();
        let embedding =
            memcore_providers::deterministic_embedding("Tomasz", stored_dimensions.unwrap_or(64))
                .unwrap();
        stores
            .entities
            .upsert_entity(
                &user,
                memcore_core::Entity {
                    id: uuid::Uuid::new_v4(),
                    org_id: user.org_id.clone(),
                    user_id: user.user_id.clone(),
                    canonical_name: "Tomasz".to_string(),
                    entity_type: EntityType::Person,
                    aliases: vec![],
                    summary: None,
                    is_speaker: false,
                    name_embedding: Some(embedding),
                    name_embedding_model: stored_model.map(str::to_string),
                    name_embedding_dimensions: stored_dimensions,
                    created_at: now,
                    updated_at: now,
                },
            )
            .await
            .unwrap();
        let candidate = CandidateFact::new(
            "Tom plays cello.",
            MemoryType::Skill,
            0.9,
            0.9,
            None,
            json!({}),
        )
        .unwrap();
        let llm = Arc::new(
            MockLlmProvider::new()
                .with_extraction_candidates(vec![candidate])
                .with_entity_annotator(Arc::new(|input| EntityAnnotationOutput {
                    facts: input
                        .facts
                        .iter()
                        .map(|fact| FactAnnotation {
                            fact_index: fact.index,
                            entities: vec![EntityMention {
                                name: "Tom".to_string(),
                                entity_type: EntityType::Person,
                                role: EntityRole::Subject,
                                aliases: vec![],
                            }],
                            relations: vec![],
                        })
                        .collect(),
                    repairs: Vec::new(),
                }))
                .with_entity_matcher(Arc::new(|_| true)),
        );
        let engine = MemoryEngine::new(
            stores.facts.clone(),
            stores.vectors.clone(),
            llm.clone(),
            Arc::new(MockEmbeddingProvider::new(64)),
        )
        .with_min_importance(0.0)
        .with_entities(Some(EntityComponents {
            store: stores.entities.clone(),
            config: EntityConfig {
                name_similarity_threshold: -1.0,
                llm_confirmation: true,
                embedding_model: "mock/embedding-a".to_string(),
            },
        }));
        add(&engine, &user, "x").await;
        assert_eq!(
            llm.match_inputs().len(),
            usize::from(compared),
            "{stored_model:?} / {stored_dimensions:?}"
        );
        let entities = stores.entities.list_all_entities(&user).await.unwrap();
        assert_eq!(entities.len(), if compared { 1 } else { 2 });
        // A new entity records the configured model and its dimension.
        for entity in entities
            .iter()
            .filter(|entity| entity.canonical_name == "Tom")
        {
            assert_eq!(
                entity.name_embedding_model.as_deref(),
                Some("mock/embedding-a")
            );
            assert_eq!(entity.name_embedding_dimensions, Some(64));
        }
    }
}

#[tokio::test]
async fn episode_store_contract_is_untouched_by_the_lifecycle_store() {
    // With episodes off and no lifecycle store (a bare engine), forget-user still works.
    let stores = Stores::new();
    let engine = stores.base(MockLlmProvider::new());
    engine
        .forget_user(ForgetUserInput {
            tenant: tenant("user_ines"),
        })
        .await
        .unwrap();
    let _ = stores
        .episodes
        .list_episodes(&tenant("user_ines"), 1)
        .await
        .unwrap();
}

/// An episode vector store whose collection does not exist and must never be created.
fn never_created() -> memcore_storage::LazyVectorStore {
    memcore_storage::LazyVectorStore::new(
        || Box::pin(async { Ok(None) }),
        || {
            Box::pin(async {
                Err(memcore_common::MemcoreError::Internal(
                    "the episode collection must not be created".to_string(),
                ))
            })
        },
    )
}
