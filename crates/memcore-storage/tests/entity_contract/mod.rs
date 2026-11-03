//! The `EntityStore` contract (migration 0014), run against every backend.

use chrono::{DateTime, TimeZone, Utc};
use memcore_common::MemcoreError;
use memcore_core::pagination::PageCursor;
use memcore_core::ports::EntityStore;
use memcore_core::{
    AliasSource, Entity, EntityAlias, EntityRelation, EntityRole, EntityType, FactEntityLink,
    TenantContext,
};
use uuid::Uuid;

pub const ORG: &str = "org_entity_store";
pub const USER: &str = "user_entity_store";

pub fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

pub fn other_user() -> TenantContext {
    TenantContext::new(ORG, "user_entity_other").expect("tenant")
}

pub fn other_org() -> TenantContext {
    TenantContext::new("org_entity_other", USER).expect("tenant")
}

/// Whole-second times, so every backend stores them exactly.
pub fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 27, 10, minute, 0).unwrap()
}

pub fn entity(tenant: &TenantContext, name: &str, kind: EntityType, minute: u32) -> Entity {
    Entity {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        canonical_name: name.to_string(),
        entity_type: kind,
        aliases: vec![EntityAlias {
            name: name.to_string(),
            source: AliasSource::Annotation,
            added_at: at(minute),
            similarity: None,
        }],
        summary: None,
        is_speaker: false,
        name_embedding: Some(vec![0.25, -0.5, 1.0]),
        name_embedding_model: Some("mock/test-embedding".to_string()),
        name_embedding_dimensions: Some(3),
        created_at: at(minute),
        updated_at: at(minute),
    }
}

pub fn link(
    tenant: &TenantContext,
    fact_id: Uuid,
    entity_id: Uuid,
    role: EntityRole,
) -> FactEntityLink {
    FactEntityLink {
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        fact_id,
        entity_id,
        role,
        mention: "Luna".to_string(),
        created_at: at(30),
    }
}

pub fn relation(
    tenant: &TenantContext,
    source: Uuid,
    target: Uuid,
    fact: Option<Uuid>,
) -> EntityRelation {
    EntityRelation {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        source_entity_id: source,
        relation: "owns".to_string(),
        target_entity_id: target,
        valid_at: Some(at(1)),
        invalid_at: None,
        source_fact_id: fact,
        created_at: at(31),
    }
}

/// Every field survives a round trip; upsert replaces; pages are newest first and follow the
/// cursor; nothing crosses users or organizations.
pub async fn round_trip(store: &dyn EntityStore) {
    let mut priya = entity(&tenant(), "Priya", EntityType::Person, 1);
    priya.is_speaker = true;
    priya.summary = Some("The speaker.".to_string());
    priya.aliases.push(EntityAlias {
        name: "Pri".to_string(),
        source: AliasSource::Merge,
        added_at: at(2),
        similarity: Some(0.81),
    });
    let luna = entity(&tenant(), "Luna", EntityType::Animal, 2);
    let paris = entity(&tenant(), "Paris", EntityType::Place, 3);
    for entity in [&priya, &luna, &paris] {
        assert_eq!(
            &store
                .upsert_entity(&tenant(), entity.clone())
                .await
                .unwrap(),
            entity
        );
    }
    assert_eq!(
        store
            .get_entity(&tenant(), priya.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&priya)
    );
    assert_eq!(
        store.list_all_entities(&tenant()).await.unwrap(),
        vec![priya.clone(), luna.clone(), paris.clone()]
    );

    // Upsert replaces the stored entity.
    let mut renamed = luna.clone();
    renamed.canonical_name = "Luna the greyhound".to_string();
    renamed.updated_at = at(9);
    store
        .upsert_entity(&tenant(), renamed.clone())
        .await
        .unwrap();
    assert_eq!(
        store.get_entity(&tenant(), luna.id).await.unwrap(),
        Some(renamed.clone())
    );

    // Newest first, two at a time.
    let first = store.list_entities_page(&tenant(), 2, None).await.unwrap();
    assert_eq!(
        first.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![paris.id, luna.id]
    );
    let cursor = PageCursor {
        last_id: first[1].id.to_string(),
        last_sort_value: first[1].created_at,
    };
    let second = store
        .list_entities_page(&tenant(), 2, Some(&cursor))
        .await
        .unwrap();
    assert_eq!(
        second.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![priya.id]
    );

    // Tenant isolation: another user or organization sees nothing and cannot overwrite.
    for other in [other_user(), other_org()] {
        assert!(store.get_entity(&other, priya.id).await.unwrap().is_none());
        assert!(store.list_all_entities(&other).await.unwrap().is_empty());
        let stolen = Entity {
            org_id: other.org_id.clone(),
            user_id: other.user_id.clone(),
            canonical_name: "Stolen".to_string(),
            ..priya.clone()
        };
        assert!(matches!(
            store.upsert_entity(&other, stolen).await,
            Err(MemcoreError::Forbidden)
        ));
        assert!(matches!(
            store.upsert_entity(&other, priya.clone()).await,
            Err(MemcoreError::Forbidden)
        ));
    }
    assert_eq!(
        store.get_entity(&tenant(), priya.id).await.unwrap(),
        Some(priya)
    );
    assert_eq!(store.count_entities_by_org(ORG).await.unwrap(), 3);
}

/// Links are idempotent per (fact, entity, role) and removable; relations are found from either
/// end; both stay within the tenant.
pub async fn links_and_relations(store: &dyn EntityStore) {
    let priya = entity(&tenant(), "Priya", EntityType::Person, 1);
    let luna = entity(&tenant(), "Luna", EntityType::Animal, 2);
    store.upsert_entity(&tenant(), priya.clone()).await.unwrap();
    store.upsert_entity(&tenant(), luna.clone()).await.unwrap();
    let fact = Uuid::new_v4();
    let subject = link(&tenant(), fact, priya.id, EntityRole::Subject);
    let object = link(&tenant(), fact, luna.id, EntityRole::Object);
    store.link_fact(&tenant(), subject.clone()).await.unwrap();
    store.link_fact(&tenant(), subject.clone()).await.unwrap();
    store.link_fact(&tenant(), object.clone()).await.unwrap();
    assert_eq!(
        store.links_for_entity(&tenant(), priya.id).await.unwrap(),
        vec![subject.clone()]
    );
    assert_eq!(
        store
            .links_for_facts(&tenant(), &[fact])
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(
        store
            .links_for_facts(&tenant(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .links_for_facts(&other_user(), &[fact])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.link_fact(&other_user(), subject.clone()).await,
        Err(MemcoreError::Forbidden)
    ));

    let owns = relation(&tenant(), priya.id, luna.id, Some(fact));
    store
        .insert_relation(&tenant(), owns.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .relations_for_entity(&tenant(), priya.id)
            .await
            .unwrap(),
        vec![owns.clone()]
    );
    assert_eq!(
        store
            .relations_for_entity(&tenant(), luna.id)
            .await
            .unwrap(),
        vec![owns]
    );
    assert!(
        store
            .relations_for_entity(&other_org(), luna.id)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(!store.unlink_fact(&other_user(), &object).await.unwrap());
    assert!(store.unlink_fact(&tenant(), &object).await.unwrap());
    assert!(!store.unlink_fact(&tenant(), &object).await.unwrap());
    assert!(
        store
            .links_for_entity(&tenant(), luna.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Deleting facts removes their links and relations and every entity left without a link; forget
/// user removes everything of that user only.
pub async fn lifecycle(store: &dyn EntityStore) {
    let priya = entity(&tenant(), "Priya", EntityType::Person, 1);
    let luna = entity(&tenant(), "Luna", EntityType::Animal, 2);
    let paris = entity(&tenant(), "Paris", EntityType::Place, 3);
    let neighbour = entity(&other_user(), "Luna", EntityType::Animal, 4);
    for (tenant, entity) in [
        (tenant(), &priya),
        (tenant(), &luna),
        (tenant(), &paris),
        (other_user(), &neighbour),
    ] {
        store.upsert_entity(&tenant, entity.clone()).await.unwrap();
    }
    let dog_fact = Uuid::new_v4();
    let trip_fact = Uuid::new_v4();
    for link in [
        link(&tenant(), dog_fact, priya.id, EntityRole::Subject),
        link(&tenant(), dog_fact, luna.id, EntityRole::Object),
        link(&tenant(), trip_fact, priya.id, EntityRole::Subject),
        link(&tenant(), trip_fact, paris.id, EntityRole::Object),
    ] {
        store.link_fact(&tenant(), link).await.unwrap();
    }
    store
        .link_fact(
            &other_user(),
            link(&other_user(), dog_fact, neighbour.id, EntityRole::Object),
        )
        .await
        .unwrap();
    store
        .insert_relation(
            &tenant(),
            relation(&tenant(), priya.id, luna.id, Some(dog_fact)),
        )
        .await
        .unwrap();
    let visits = relation(&tenant(), priya.id, paris.id, Some(trip_fact));
    store
        .insert_relation(&tenant(), visits.clone())
        .await
        .unwrap();

    // The dog fact goes: Luna loses its only link and is deleted; Priya keeps the trip link.
    let deleted = store
        .delete_for_facts(&tenant(), &[dog_fact])
        .await
        .unwrap();
    assert_eq!(deleted, vec![luna.id]);
    assert!(
        store
            .get_entity(&tenant(), luna.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_entity(&tenant(), priya.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .relations_for_entity(&tenant(), priya.id)
            .await
            .unwrap(),
        vec![visits]
    );
    assert!(
        store
            .delete_for_facts(&tenant(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    // The other user's link to the same fact ID and their entity are untouched.
    assert!(
        store
            .get_entity(&other_user(), neighbour.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .links_for_entity(&other_user(), neighbour.id)
            .await
            .unwrap()
            .len(),
        1
    );

    assert_eq!(store.delete_user_entities(&tenant()).await.unwrap(), 2);
    assert!(store.list_all_entities(&tenant()).await.unwrap().is_empty());
    assert!(
        store
            .links_for_facts(&tenant(), &[trip_fact])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .relations_for_entity(&tenant(), priya.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.list_all_entities(&other_user()).await.unwrap(),
        vec![neighbour]
    );
}
