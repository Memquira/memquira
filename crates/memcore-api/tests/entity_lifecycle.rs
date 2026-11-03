//! The entity layer across the lifecycle on real stores: add with
//! annotation, read, export (v3) and replace import, retention and forget-user.
//!
//! Runs on SQLite always, and on Postgres when `MEMCORE_TEST_POSTGRES_URL` is set (with the
//! `postgres` feature). Every name here is invented.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use memcore_core::ports::{
    EntityAnnotationOutput, EntityMention, EntityStore, FactAnnotation, FactStore, RelationMention,
};
use memcore_core::{
    AddMemoryInput, ApplyRetentionInput, CandidateFact, EntityComponents, EntityConfig, EntityRole,
    EntityType, ExportUserDataInput, FactStructure, ForgetUserInput, ImportMode,
    ImportUserDataInput, ListEntitiesInput, MemoryEngine, MemoryMessage, MemoryType, MessageRole,
    RetentionPolicy, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockVectorStore, SqliteEntityStore, SqliteFactStore};
use serde_json::json;
use uuid::Uuid;

fn engine(facts: Arc<dyn FactStore>, entities: Arc<dyn EntityStore>) -> MemoryEngine {
    let candidate = CandidateFact::new(
        "Ines adopted a beagle called Biscuit.",
        MemoryType::Relationship,
        0.9,
        0.9,
        None,
        json!({}),
    )
    .unwrap()
    .with_structure(FactStructure {
        subject: Some("Ines".to_string()),
        source_message_ids: vec!["m1".to_string()],
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
                            aliases: vec!["the beagle".to_string()],
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
    MemoryEngine::new(
        facts,
        Arc::new(MockVectorStore::new()),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(64)),
    )
    .with_min_importance(0.0)
    .with_entities(Some(EntityComponents {
        store: entities,
        config: EntityConfig::default(),
    }))
}

async fn lifecycle(facts: Arc<dyn FactStore>, entities: Arc<dyn EntityStore>) {
    let user = TenantContext::new(
        "org_entity_lifecycle",
        format!("user_{}", Uuid::new_v4().simple()),
    )
    .unwrap();
    let engine = engine(facts.clone(), entities.clone());
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: user.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "I adopted a beagle called Biscuit.".to_string(),
                name: Some("Ines".to_string()),
                timestamp: Some(Utc.with_ymd_and_hms(2026, 3, 4, 10, 0, 0).unwrap()),
                message_id: Some("m1".to_string()),
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .unwrap();
    let summary = output.entities.unwrap();
    assert_eq!(
        (summary.created, summary.links, summary.relations),
        (2, 2, 1)
    );

    let page = engine
        .list_entities(ListEntitiesInput {
            tenant: user.clone(),
            limit: 10,
            cursor: None,
        })
        .await
        .unwrap();
    assert_eq!(page.entities.len(), 2);
    let biscuit = page
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Biscuit")
        .unwrap();
    assert_eq!(biscuit.aliases.len(), 1);
    let detail = engine.get_entity(&user, biscuit.id).await.unwrap().unwrap();
    assert_eq!((detail.facts.len(), detail.relations.len()), (1, 1));

    // Export and replace import: the entity layer comes back under new IDs, linked to the facts.
    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant: user.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(export.entities.as_ref().unwrap().entities.len(), 2);
    let imported = engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export,
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(imported.imported_entities, Some(2));
    let restored = entities.list_all_entities(&user).await.unwrap();
    assert_eq!(restored.len(), 2);
    for entity in &restored {
        assert_eq!(
            entities
                .links_for_entity(&user, entity.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    // Retention prunes the fact and with it every entity left without a link.
    let mut fact = facts
        .search_facts(memcore_core::FactSearchQuery::new(user.clone(), 10))
        .await
        .unwrap()
        .remove(0);
    fact.updated_at = Utc::now() - Duration::days(400);
    facts.update_fact(&user, fact).await.unwrap();
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
    assert!(entities.list_all_entities(&user).await.unwrap().is_empty());

    // Forget-user removes whatever a new add stores.
    engine
        .add_memory(AddMemoryInput {
            tenant: user.clone(),
            messages: vec![MemoryMessage::new(MessageRole::User, "again")],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .unwrap();
    assert!(!entities.list_all_entities(&user).await.unwrap().is_empty());
    engine
        .forget_user(ForgetUserInput {
            tenant: user.clone(),
        })
        .await
        .unwrap();
    assert!(entities.list_all_entities(&user).await.unwrap().is_empty());
}

#[tokio::test]
async fn sqlite_entity_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("memcore.db")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let facts = SqliteFactStore::connect(&url).await.unwrap();
    let entities = SqliteEntityStore::connect(&url).await.unwrap();
    lifecycle(Arc::new(facts), Arc::new(entities)).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_entity_lifecycle() {
    use memcore_storage::{PostgresEntityStore, PostgresFactStore};

    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("skipping postgres_entity_lifecycle: MEMCORE_TEST_POSTGRES_URL not set");
            return;
        }
    };
    let facts = PostgresFactStore::connect(&url).await.unwrap();
    let entities = PostgresEntityStore::connect(&url).await.unwrap();
    lifecycle(Arc::new(facts), Arc::new(entities)).await;
}
