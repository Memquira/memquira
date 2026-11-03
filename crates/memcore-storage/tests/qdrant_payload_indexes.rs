//! Qdrant payload index ensure (skipped without MEMCORE_TEST_QDRANT_URL).
//!
//! Every test uses its own disposable collection and deletes it at the end.

#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use memcore_common::MemcoreError;
use memcore_core::{MemoryType, TenantContext};
use memcore_storage::{
    EPISODE_PAYLOAD_INDEXES, FACT_PAYLOAD_INDEXES, QdrantVectorStore, VectorRecord,
    VectorSearchQuery, VectorStore,
};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CollectionConfig, CreateFieldIndexCollectionBuilder, FieldType, PayloadSchemaType,
    RetrievedPoint, ScrollPointsBuilder,
};
use serde_json::json;
use std::collections::BTreeMap;
use uuid::Uuid;

const DIMENSIONS: usize = 4;

fn qdrant_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn raw_client(url: &str) -> Qdrant {
    Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .expect("raw qdrant client should build")
}

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant should be valid")
}

fn record(
    org_id: &str,
    user_id: &str,
    fact_id: Uuid,
    memory_type: MemoryType,
    values: [f32; 4],
) -> VectorRecord {
    VectorRecord {
        id: Uuid::new_v4(),
        fact_id,
        org_id: org_id.to_string(),
        user_id: user_id.to_string(),
        embedding: values.to_vec(),
        content: format!("{org_id}/{user_id}/{fact_id}"),
        memory_type,
        metadata: json!({"timestamp": "2023-05-08T13:56:00"}),
    }
}

fn query(tenant: TenantContext, memory_types: Option<Vec<MemoryType>>) -> VectorSearchQuery {
    VectorSearchQuery {
        tenant,
        embedding: vec![1.0, 0.0, 0.0, 0.0],
        limit: 50,
        memory_types,
        metadata_filter: None,
    }
}

/// Field -> index type, from the collection's payload schema.
async fn schema(client: &Qdrant, collection: &str) -> BTreeMap<String, i32> {
    let info = client
        .collection_info(collection)
        .await
        .expect("collection info should load");
    info.result
        .expect("collection info result")
        .payload_schema
        .into_iter()
        .map(|(field, info)| (field, info.data_type))
        .collect()
}

async fn config(client: &Qdrant, collection: &str) -> CollectionConfig {
    client
        .collection_info(collection)
        .await
        .expect("collection info should load")
        .result
        .and_then(|info| info.config)
        .expect("collection config")
}

/// Every point with its vector and payload, ordered by point ID.
async fn all_points(client: &Qdrant, collection: &str) -> Vec<RetrievedPoint> {
    let mut points = client
        .scroll(
            ScrollPointsBuilder::new(collection)
                .limit(10_000)
                .with_payload(true)
                .with_vectors(true),
        )
        .await
        .expect("scroll should succeed")
        .result;
    points.sort_by_key(|point| format!("{:?}", point.id));
    points
}

fn keyword_schema(fields: &[&str]) -> BTreeMap<String, i32> {
    fields
        .iter()
        .map(|field| (field.to_string(), PayloadSchemaType::Keyword as i32))
        .collect()
}

fn names(fields: &[&str]) -> Vec<String> {
    fields.iter().map(|field| field.to_string()).collect()
}

/// A collection exactly as the earlier code leaves it: created by `connect`, no payload index,
/// two orgs, three users and two memory types.
async fn legacy_collection(url: &str, collection: &str) -> QdrantVectorStore {
    let store = QdrantVectorStore::connect(url, collection, DIMENSIONS)
        .await
        .expect("legacy collection should be created");
    for (index, (org, user, memory_type)) in [
        ("org_a", "user_1", MemoryType::Profile),
        ("org_a", "user_1", MemoryType::Preference),
        ("org_a", "user_2", MemoryType::Profile),
        ("org_b", "user_1", MemoryType::Profile),
        ("org_b", "user_1", MemoryType::Skill),
    ]
    .into_iter()
    .enumerate()
    {
        let value = 1.0 - index as f32 / 10.0;
        store
            .upsert_vector(
                &tenant(org, user),
                record(
                    org,
                    user,
                    Uuid::new_v4(),
                    memory_type,
                    [value, 0.1, 0.0, 0.0],
                ),
            )
            .await
            .expect("legacy upsert should succeed");
    }
    store
}

async fn drop_collection(client: &Qdrant, collection: &str) {
    client
        .delete_collection(collection)
        .await
        .expect("disposable collection should be deleted");
}

fn collection_name(label: &str) -> String {
    format!("memcore_t3b_{label}_{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn ensure_creates_every_fact_and_episode_index() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);

    let facts = collection_name("facts");
    let _owned_facts = qdrant_cleanup::TestCollections::owning(&facts);
    let store = QdrantVectorStore::connect(&url, &facts, DIMENSIONS)
        .await
        .expect("connect");
    assert!(schema(&client, &facts).await.is_empty());
    let report = store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect("ensure");
    assert_eq!(
        report.created,
        names(&["org_id", "user_id", "fact_id", "memory_type"])
    );
    assert!(report.present.is_empty());
    assert_eq!(
        schema(&client, &facts).await,
        keyword_schema(FACT_PAYLOAD_INDEXES)
    );

    let episodes = collection_name("episodes");
    let _owned_episodes = qdrant_cleanup::TestCollections::owning(&episodes);
    let store = QdrantVectorStore::connect(&url, &episodes, DIMENSIONS)
        .await
        .expect("connect");
    let report = store
        .ensure_payload_indexes(EPISODE_PAYLOAD_INDEXES)
        .await
        .expect("ensure");
    assert_eq!(report.created, names(&["org_id", "user_id", "fact_id"]));
    assert_eq!(
        schema(&client, &episodes).await,
        keyword_schema(EPISODE_PAYLOAD_INDEXES)
    );

    drop_collection(&client, &facts).await;
    drop_collection(&client, &episodes).await;
}

#[tokio::test]
async fn repeated_ensure_writes_nothing() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);
    let collection = collection_name("repeat");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let store = legacy_collection(&url, &collection).await;

    let first = store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect("first ensure");
    assert_eq!(first.created.len(), 4);
    let schema_after_first = schema(&client, &collection).await;
    let points_after_first = all_points(&client, &collection).await;

    for _ in 0..3 {
        let again = store
            .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
            .await
            .expect("repeated ensure");
        assert!(
            again.created.is_empty(),
            "a repeated ensure created {again:?}"
        );
        assert_eq!(again.present, names(FACT_PAYLOAD_INDEXES));
    }

    // A fresh process (new client) sees the same schema and also writes nothing.
    let reopened = QdrantVectorStore::connect_existing(&url, &collection, DIMENSIONS)
        .await
        .expect("connect_existing")
        .expect("collection exists");
    let report = reopened
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect("ensure after reopen");
    assert!(report.created.is_empty());

    // Concurrent ensures on an indexed collection: all succeed, nothing changes.
    let (a, b) = tokio::join!(
        store.ensure_payload_indexes(FACT_PAYLOAD_INDEXES),
        reopened.ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
    );
    assert!(a.expect("concurrent ensure a").created.is_empty());
    assert!(b.expect("concurrent ensure b").created.is_empty());

    assert_eq!(schema(&client, &collection).await, schema_after_first);
    assert_eq!(all_points(&client, &collection).await, points_after_first);
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn concurrent_first_ensures_both_succeed() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);
    let collection = collection_name("race");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let first = legacy_collection(&url, &collection).await;
    let second = QdrantVectorStore::connect_existing(&url, &collection, DIMENSIONS)
        .await
        .expect("connect_existing")
        .expect("collection exists");

    // Two openers that both see a missing index (the lazy episode store can race like this).
    let (a, b) = tokio::join!(
        first.ensure_payload_indexes(FACT_PAYLOAD_INDEXES),
        second.ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
    );
    a.expect("ensure a");
    b.expect("ensure b");
    assert_eq!(
        schema(&client, &collection).await,
        keyword_schema(FACT_PAYLOAD_INDEXES)
    );
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn ensure_leaves_vectors_payloads_ids_and_config_unchanged() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);
    let collection = collection_name("unchanged");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let store = legacy_collection(&url, &collection).await;

    let points_before = all_points(&client, &collection).await;
    let config_before = config(&client, &collection).await;
    assert_eq!(points_before.len(), 5);

    store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect("ensure");

    // Same point IDs, vectors and payloads, bit for bit; same vector / HNSW / optimizer config.
    assert_eq!(all_points(&client, &collection).await, points_before);
    assert_eq!(config(&client, &collection).await, config_before);
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn tenant_scoped_search_and_delete_work_on_indexed_collection() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);
    let collection = collection_name("tenant");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let store = legacy_collection(&url, &collection).await;

    // Search results on the unindexed collection, per tenant and with a memory_type filter.
    let tenants = [
        tenant("org_a", "user_1"),
        tenant("org_a", "user_2"),
        tenant("org_b", "user_1"),
    ];
    let mut before = Vec::new();
    for tenant in &tenants {
        before.push(
            store
                .search_vectors(query(tenant.clone(), None))
                .await
                .unwrap(),
        );
        before.push(
            store
                .search_vectors(query(tenant.clone(), Some(vec![MemoryType::Profile])))
                .await
                .unwrap(),
        );
    }

    store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect("ensure");

    // Identical results (IDs, order, scores) after indexing; each tenant sees only its own points.
    let mut after = Vec::new();
    for tenant in &tenants {
        after.push(
            store
                .search_vectors(query(tenant.clone(), None))
                .await
                .unwrap(),
        );
        after.push(
            store
                .search_vectors(query(tenant.clone(), Some(vec![MemoryType::Profile])))
                .await
                .unwrap(),
        );
    }
    assert_eq!(after, before);
    assert_eq!(after[0].len(), 2); // org_a / user_1
    assert_eq!(after[1].len(), 1); // org_a / user_1, profile only
    assert_eq!(after[2].len(), 1); // org_a / user_2
    assert_eq!(after[4].len(), 2); // org_b / user_1
    for result in &after[0] {
        assert!(result.content.starts_with("org_a/user_1/"));
    }

    // A same-fact_id upsert still replaces (pre-delete via the indexed filter).
    let a1 = tenant("org_a", "user_1");
    let fact_id = after[0][0].fact_id;
    store
        .upsert_vector(
            &a1,
            record(
                "org_a",
                "user_1",
                fact_id,
                MemoryType::Profile,
                [0.0, 1.0, 0.0, 0.0],
            ),
        )
        .await
        .unwrap();
    let a1_results = store.search_vectors(query(a1.clone(), None)).await.unwrap();
    assert_eq!(a1_results.len(), 2);
    assert_eq!(
        a1_results.iter().filter(|r| r.fact_id == fact_id).count(),
        1
    );

    // A delete in another tenant finds nothing and never touches org_a / user_1's fact.
    for other in [tenant("org_b", "user_1"), tenant("org_a", "user_2")] {
        let error = store
            .delete_by_fact_id(&other, fact_id)
            .await
            .expect_err("a foreign tenant's fact must not be found");
        assert!(matches!(error, MemcoreError::NotFound(_)), "{error:?}");
    }
    assert_eq!(
        store
            .search_vectors(query(a1.clone(), None))
            .await
            .unwrap()
            .len(),
        2
    );

    // Tenant-scoped deletes remove exactly their own points.
    store.delete_by_fact_id(&a1, fact_id).await.unwrap();
    assert_eq!(
        store
            .search_vectors(query(a1.clone(), None))
            .await
            .unwrap()
            .len(),
        1
    );
    store
        .delete_by_user(&tenant("org_b", "user_1"))
        .await
        .unwrap();
    assert!(
        store
            .search_vectors(query(tenant("org_b", "user_1"), None))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.search_vectors(query(a1, None)).await.unwrap().len(),
        1
    );
    assert_eq!(
        store
            .search_vectors(query(tenant("org_a", "user_2"), None))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(all_points(&client, &collection).await.len(), 2);
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn previous_version_collection_opens_safely() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);

    // 1. A earlier collection (no indexes) opened through connect_existing: indexes are added in
    //    place and the data stays readable.
    let legacy = collection_name("legacy");
    let _owned_legacy = qdrant_cleanup::TestCollections::owning(&legacy);
    drop(legacy_collection(&url, &legacy).await);
    let points_before = all_points(&client, &legacy).await;
    let opened = QdrantVectorStore::connect_existing(&url, &legacy, DIMENSIONS)
        .await
        .expect("connect_existing")
        .expect("collection exists");
    let report = opened
        .ensure_payload_indexes(EPISODE_PAYLOAD_INDEXES)
        .await
        .expect("ensure");
    assert_eq!(report.created.len(), 3);
    assert_eq!(all_points(&client, &legacy).await, points_before);
    assert_eq!(
        opened
            .search_vectors(query(tenant("org_a", "user_1"), None))
            .await
            .unwrap()
            .len(),
        2
    );

    // 2. A collection that already has some keyword indexes (for example the Step 6 measurement's
    //    org_id + user_id): only the missing ones are created.
    let partial = collection_name("partial");
    let _owned_partial = qdrant_cleanup::TestCollections::owning(&partial);
    let store = legacy_collection(&url, &partial).await;
    for field in ["org_id", "user_id"] {
        client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(&partial, field, FieldType::Keyword)
                    .wait(true),
            )
            .await
            .unwrap();
    }
    let report = store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect("ensure");
    assert_eq!(report.present, names(&["org_id", "user_id"]));
    assert_eq!(report.created, names(&["fact_id", "memory_type"]));

    // 3. An indexed collection used by code that never ensures (an older binary) still works:
    //    write, replace, search and delete through the unchanged VectorStore paths.
    let old_binary_view = QdrantVectorStore::connect(&url, &partial, DIMENSIONS)
        .await
        .expect("connect");
    let t = tenant("org_c", "user_9");
    let fact_id = Uuid::new_v4();
    for values in [[1.0, 0.0, 0.0, 0.0], [0.9, 0.1, 0.0, 0.0]] {
        old_binary_view
            .upsert_vector(
                &t,
                record("org_c", "user_9", fact_id, MemoryType::Profile, values),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        old_binary_view
            .search_vectors(query(t.clone(), None))
            .await
            .unwrap()
            .len(),
        1
    );
    old_binary_view.delete_by_user(&t).await.unwrap();
    assert!(
        old_binary_view
            .search_vectors(query(t, None))
            .await
            .unwrap()
            .is_empty()
    );

    drop_collection(&client, &legacy).await;
    drop_collection(&client, &partial).await;
}

#[tokio::test]
async fn initialization_failures_are_surfaced() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw_client(&url);

    // A field indexed with another type is reported, never replaced.
    let conflicting = collection_name("conflict");
    let _owned_conflicting = qdrant_cleanup::TestCollections::owning(&conflicting);
    let store = legacy_collection(&url, &conflicting).await;
    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(&conflicting, "fact_id", FieldType::Uuid)
                .wait(true),
        )
        .await
        .unwrap();
    let schema_before = schema(&client, &conflicting).await;
    let error = store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect_err("a conflicting index type must fail");
    match &error {
        MemcoreError::ValidationError(message) => {
            assert!(message.contains("fact_id"), "{message}");
            assert!(message.contains("Uuid"), "{message}");
            assert!(
                message.contains("MEMCORE_QDRANT_ENSURE_INDEXES"),
                "{message}"
            );
        }
        other => panic!("unexpected error {other:?}"),
    }
    // Fields before the conflict may have been created; the conflicting one is unchanged.
    let schema_after = schema(&client, &conflicting).await;
    assert_eq!(schema_after.get("fact_id"), schema_before.get("fact_id"));
    drop_collection(&client, &conflicting).await;

    // The collection disappears between open and ensure: a storage error, not a silent success.
    let vanished = collection_name("vanished");
    let _owned_vanished = qdrant_cleanup::TestCollections::owning(&vanished);
    let store = QdrantVectorStore::connect(&url, &vanished, DIMENSIONS)
        .await
        .expect("connect");
    drop_collection(&client, &vanished).await;
    let error = store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .expect_err("a missing collection must fail");
    assert!(
        matches!(&error, MemcoreError::StorageError(message) if message.contains("payload schema")),
        "{error:?}"
    );

    // Qdrant unreachable: the open itself fails (nothing to ensure against).
    let error = QdrantVectorStore::connect("http://127.0.0.1:1", &vanished, DIMENSIONS).await;
    assert!(error.is_err());
}
