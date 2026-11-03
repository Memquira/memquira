//! Qdrant VectorStore integration tests (skipped without MEMCORE_TEST_QDRANT_URL).

#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use memcore_core::{MemoryType, TenantContext};
use memcore_storage::{QdrantVectorStore, VectorRecord, VectorSearchQuery, VectorStore};
use serde_json::json;
use uuid::Uuid;

const DIMENSIONS: usize = 4;

fn qdrant_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant should be valid")
}

fn embedding(values: [f32; 4]) -> Vec<f32> {
    values.to_vec()
}

fn sample_record(
    org_id: &str,
    user_id: &str,
    fact_id: Uuid,
    content: &str,
    memory_type: MemoryType,
    metadata: serde_json::Value,
    values: [f32; 4],
) -> VectorRecord {
    VectorRecord {
        id: Uuid::new_v4(),
        fact_id,
        org_id: org_id.to_string(),
        user_id: user_id.to_string(),
        embedding: embedding(values),
        content: content.to_string(),
        memory_type,
        metadata,
    }
}

async fn with_qdrant_store<F, Fut>(test_name: &str, test: F)
where
    F: FnOnce(QdrantVectorStore) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant test `{test_name}`: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };

    let collection = format!("memcore_vectors_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let store = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
        .await
        .expect("qdrant store should connect");
    test(store).await;
}

#[tokio::test]
async fn qdrant_connect_creates_collection() {
    with_qdrant_store("qdrant_connect_creates_collection", |_store| async move {
        // connect() in with_qdrant_store already verifies collection creation.
    })
    .await;
}

#[tokio::test]
async fn qdrant_upsert_and_search_vector() {
    with_qdrant_store("qdrant_upsert_and_search_vector", |store| async move {
        let tenant = tenant("org_qdrant_a", "user_a");
        let fact_id = Uuid::new_v4();

        store
            .upsert_vector(
                &tenant,
                sample_record(
                    "org_qdrant_a",
                    "user_a",
                    fact_id,
                    "learning rust",
                    MemoryType::Skill,
                    json!({}),
                    [0.1, 0.2, 0.3, 0.4],
                ),
            )
            .await
            .expect("upsert should succeed");

        let results = store
            .search_vectors(VectorSearchQuery {
                tenant: tenant.clone(),
                embedding: embedding([0.1, 0.2, 0.3, 0.4]),
                limit: 5,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact_id, fact_id);
        assert_eq!(results[0].content, "learning rust");
        assert!(results[0].score > 0.0);
    })
    .await;
}

#[tokio::test]
async fn qdrant_search_respects_org_isolation() {
    with_qdrant_store("qdrant_search_respects_org_isolation", |store| async move {
        let tenant_a = tenant("org_qdrant_a", "user_a");
        let tenant_b = tenant("org_qdrant_b", "user_a");

        store
            .upsert_vector(
                &tenant_a,
                sample_record(
                    "org_qdrant_a",
                    "user_a",
                    Uuid::new_v4(),
                    "org a only",
                    MemoryType::Skill,
                    json!({}),
                    [1.0, 0.0, 0.0, 0.0],
                ),
            )
            .await
            .expect("upsert should succeed");

        let results = store
            .search_vectors(VectorSearchQuery {
                tenant: tenant_b,
                embedding: embedding([1.0, 0.0, 0.0, 0.0]),
                limit: 5,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("search should succeed");

        assert!(results.is_empty());
    })
    .await;
}

#[tokio::test]
async fn qdrant_search_respects_user_isolation() {
    with_qdrant_store(
        "qdrant_search_respects_user_isolation",
        |store| async move {
            let tenant_a = tenant("org_qdrant_a", "user_a");
            let tenant_b = tenant("org_qdrant_a", "user_b");

            store
                .upsert_vector(
                    &tenant_a,
                    sample_record(
                        "org_qdrant_a",
                        "user_a",
                        Uuid::new_v4(),
                        "user a only",
                        MemoryType::Skill,
                        json!({}),
                        [0.0, 1.0, 0.0, 0.0],
                    ),
                )
                .await
                .expect("upsert should succeed");

            let results = store
                .search_vectors(VectorSearchQuery {
                    tenant: tenant_b,
                    embedding: embedding([0.0, 1.0, 0.0, 0.0]),
                    limit: 5,
                    memory_types: None,
                    metadata_filter: None,
                })
                .await
                .expect("search should succeed");

            assert!(results.is_empty());
        },
    )
    .await;
}

#[tokio::test]
async fn qdrant_upsert_same_fact_id_replaces_record() {
    with_qdrant_store(
        "qdrant_upsert_same_fact_id_replaces_record",
        |store| async move {
            let tenant = tenant("org_qdrant_a", "user_a");
            let fact_id = Uuid::new_v4();

            store
                .upsert_vector(
                    &tenant,
                    sample_record(
                        "org_qdrant_a",
                        "user_a",
                        fact_id,
                        "first version",
                        MemoryType::Skill,
                        json!({}),
                        [0.1, 0.1, 0.1, 0.1],
                    ),
                )
                .await
                .expect("first upsert should succeed");

            store
                .upsert_vector(
                    &tenant,
                    sample_record(
                        "org_qdrant_a",
                        "user_a",
                        fact_id,
                        "second version",
                        MemoryType::Project,
                        json!({"v": 2}),
                        [0.9, 0.9, 0.9, 0.9],
                    ),
                )
                .await
                .expect("second upsert should succeed");

            let results = store
                .search_vectors(VectorSearchQuery {
                    tenant,
                    embedding: embedding([0.9, 0.9, 0.9, 0.9]),
                    limit: 10,
                    memory_types: None,
                    metadata_filter: None,
                })
                .await
                .expect("search should succeed");

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].fact_id, fact_id);
            assert_eq!(results[0].content, "second version");
            assert_eq!(results[0].memory_type, MemoryType::Project);
        },
    )
    .await;
}

#[tokio::test]
async fn qdrant_delete_by_fact_id_removes_tenant_scoped_record() {
    with_qdrant_store(
        "qdrant_delete_by_fact_id_removes_tenant_scoped_record",
        |store| async move {
            let tenant = tenant("org_qdrant_a", "user_a");
            let fact_id = Uuid::new_v4();

            store
                .upsert_vector(
                    &tenant,
                    sample_record(
                        "org_qdrant_a",
                        "user_a",
                        fact_id,
                        "to delete",
                        MemoryType::Skill,
                        json!({}),
                        [0.2, 0.2, 0.2, 0.2],
                    ),
                )
                .await
                .expect("upsert should succeed");

            store
                .delete_by_fact_id(&tenant, fact_id)
                .await
                .expect("delete should succeed");

            let results = store
                .search_vectors(VectorSearchQuery {
                    tenant,
                    embedding: embedding([0.2, 0.2, 0.2, 0.2]),
                    limit: 5,
                    memory_types: None,
                    metadata_filter: None,
                })
                .await
                .expect("search should succeed");

            assert!(results.is_empty());
        },
    )
    .await;
}

#[tokio::test]
async fn qdrant_delete_by_user_removes_only_that_user_vectors() {
    with_qdrant_store(
        "qdrant_delete_by_user_removes_only_that_user_vectors",
        |store| async move {
            let tenant_a = tenant("org_qdrant_a", "user_a");
            let tenant_b = tenant("org_qdrant_a", "user_b");

            store
                .upsert_vector(
                    &tenant_a,
                    sample_record(
                        "org_qdrant_a",
                        "user_a",
                        Uuid::new_v4(),
                        "user a",
                        MemoryType::Skill,
                        json!({}),
                        [0.3, 0.3, 0.3, 0.3],
                    ),
                )
                .await
                .expect("upsert a should succeed");

            store
                .upsert_vector(
                    &tenant_b,
                    sample_record(
                        "org_qdrant_a",
                        "user_b",
                        Uuid::new_v4(),
                        "user b",
                        MemoryType::Skill,
                        json!({}),
                        [0.4, 0.4, 0.4, 0.4],
                    ),
                )
                .await
                .expect("upsert b should succeed");

            store
                .delete_by_user(&tenant_a)
                .await
                .expect("delete user should succeed");

            let results_a = store
                .search_vectors(VectorSearchQuery {
                    tenant: tenant_a,
                    embedding: embedding([0.3, 0.3, 0.3, 0.3]),
                    limit: 5,
                    memory_types: None,
                    metadata_filter: None,
                })
                .await
                .expect("search a should succeed");
            assert!(results_a.is_empty());

            let results_b = store
                .search_vectors(VectorSearchQuery {
                    tenant: tenant_b,
                    embedding: embedding([0.4, 0.4, 0.4, 0.4]),
                    limit: 5,
                    memory_types: None,
                    metadata_filter: None,
                })
                .await
                .expect("search b should succeed");
            assert_eq!(results_b.len(), 1);
        },
    )
    .await;
}

#[tokio::test]
async fn qdrant_memory_type_round_trip() {
    with_qdrant_store("qdrant_memory_type_round_trip", |store| async move {
        let tenant = tenant("org_qdrant_a", "user_a");
        let fact_id = Uuid::new_v4();

        store
            .upsert_vector(
                &tenant,
                sample_record(
                    "org_qdrant_a",
                    "user_a",
                    fact_id,
                    "preference memory",
                    MemoryType::Preference,
                    json!({}),
                    [0.5, 0.5, 0.5, 0.5],
                ),
            )
            .await
            .expect("upsert should succeed");

        let results = store
            .search_vectors(VectorSearchQuery {
                tenant,
                embedding: embedding([0.5, 0.5, 0.5, 0.5]),
                limit: 1,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results[0].memory_type, MemoryType::Preference);
    })
    .await;
}

#[tokio::test]
async fn qdrant_metadata_round_trip() {
    with_qdrant_store("qdrant_metadata_round_trip", |store| async move {
        let tenant = tenant("org_qdrant_a", "user_a");
        let fact_id = Uuid::new_v4();
        let metadata = json!({ "topic": "rust", "level": 3 });

        store
            .upsert_vector(
                &tenant,
                sample_record(
                    "org_qdrant_a",
                    "user_a",
                    fact_id,
                    "metadata test",
                    MemoryType::Skill,
                    metadata.clone(),
                    [0.6, 0.6, 0.6, 0.6],
                ),
            )
            .await
            .expect("upsert should succeed");

        let results = store
            .search_vectors(VectorSearchQuery {
                tenant,
                embedding: embedding([0.6, 0.6, 0.6, 0.6]),
                limit: 1,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results[0].metadata, metadata);
    })
    .await;
}

#[tokio::test]
async fn qdrant_search_respects_limit() {
    with_qdrant_store("qdrant_search_respects_limit", |store| async move {
        let tenant = tenant("org_qdrant_a", "user_a");

        for i in 0..5 {
            let v = (i as f32) * 0.1;
            store
                .upsert_vector(
                    &tenant,
                    sample_record(
                        "org_qdrant_a",
                        "user_a",
                        Uuid::new_v4(),
                        &format!("memory {i}"),
                        MemoryType::Skill,
                        json!({}),
                        [v, v, v, v],
                    ),
                )
                .await
                .expect("upsert should succeed");
        }

        let results = store
            .search_vectors(VectorSearchQuery {
                tenant,
                embedding: embedding([0.2, 0.2, 0.2, 0.2]),
                limit: 2,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("search should succeed");

        assert_eq!(results.len(), 2);
    })
    .await;
}

// ---- Store isolation by embedding identity and dimension ----

fn identity(provider: &str, model: &str) -> memcore_core::EmbeddingIdentity {
    memcore_core::EmbeddingIdentity::remote(provider, model, DIMENSIONS)
}

async fn put_one(store: &QdrantVectorStore) {
    store
        .upsert_vector(
            &tenant("org_identity", "user"),
            sample_record(
                "org_identity",
                "user",
                Uuid::new_v4(),
                "x",
                MemoryType::Skill,
                json!({}),
                [0.1, 0.2, 0.3, 0.4],
            ),
        )
        .await
        .expect("upsert");
}

#[tokio::test]
async fn embedding_identity_guard_fails_closed() {
    let Some(url) = qdrant_url() else {
        eprintln!(
            "skipping qdrant test `embedding_identity_guard_fails_closed`: MEMCORE_TEST_QDRANT_URL not set"
        );
        return;
    };
    let local = identity("local_onnx", "model-a");
    let other_local = identity("local_onnx", "model-b");
    let remote = identity("openai_compatible", "titan");

    // A local provider records its identity on a new collection and reconnects to it.
    let fresh = format!("memcore_identity_test_{}", Uuid::new_v4().simple());
    let _owned_fresh = qdrant_cleanup::TestCollections::owning(&fresh);
    let store = QdrantVectorStore::connect_with_identity(&url, &fresh, DIMENSIONS, Some(&local))
        .await
        .expect("local provider records a new store");
    put_one(&store).await;
    QdrantVectorStore::connect_with_identity(&url, &fresh, DIMENSIONS, Some(&local))
        .await
        .expect("same identity reconnects");
    // Another local model, a remote provider, or a provider without identity is refused.
    for (who, id) in [
        ("other local", Some(&other_local)),
        ("remote", Some(&remote)),
        ("unknown", None),
    ] {
        let error = QdrantVectorStore::connect_with_identity(&url, &fresh, DIMENSIONS, id)
            .await
            .err()
            .unwrap_or_else(|| panic!("{who} must be refused"));
        assert!(
            error.to_string().contains("refusing to mix vectors"),
            "{who}: {error}"
        );
    }
    // A collection of another dimension is refused.
    let error =
        QdrantVectorStore::connect_with_identity(&url, &fresh, DIMENSIONS + 1, Some(&local))
            .await
            .err()
            .expect("dimension mismatch refused");
    assert!(error.to_string().contains("4-dimensional"), "{error}");

    // A store from an earlier version (no record, non-empty): remote keeps working and writes nothing; a local
    // provider refuses it.
    let legacy = format!("memcore_identity_legacy_{}", Uuid::new_v4().simple());
    let _owned_legacy = qdrant_cleanup::TestCollections::owning(&legacy);
    let store = QdrantVectorStore::connect(&url, &legacy, DIMENSIONS)
        .await
        .expect("legacy");
    put_one(&store).await;
    QdrantVectorStore::connect_with_identity(&url, &legacy, DIMENSIONS, Some(&remote))
        .await
        .expect("remote provider on a legacy store");
    let client = qdrant_client::Qdrant::from_url(&url)
        .skip_compatibility_check()
        .build()
        .unwrap();
    assert!(
        !client
            .collection_exists(memcore_storage::identity_collection_name(&legacy))
            .await
            .unwrap(),
        "nothing written next to a legacy store"
    );
    let error = QdrantVectorStore::connect_with_identity(&url, &legacy, DIMENSIONS, Some(&local))
        .await
        .err()
        .expect("local provider refuses an unrecorded non-empty store");
    assert!(error.to_string().contains("no recorded"), "{error}");

    for name in [fresh.clone(), legacy.clone()] {
        let _ = client.delete_collection(&name).await;
        let _ = client
            .delete_collection(memcore_storage::identity_collection_name(&name))
            .await;
    }
}

/// A store searching with query-time hnsw_ef = 128 writes, deletes and filters exactly
/// like one without it; on this small collection (exact search) both return identical results.
#[tokio::test]
async fn search_hnsw_ef_changes_no_write_delete_or_tenant_filter() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collection = format!("memcore_vectors_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let plain = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
        .await
        .expect("connect");
    let ef = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
        .await
        .expect("connect")
        .with_search_hnsw_ef(Some(128));
    assert_eq!(ef.search_hnsw_ef(), Some(128));
    assert_eq!(plain.search_hnsw_ef(), None);
    let (a, b) = (tenant("org_ef", "user_a"), tenant("org_ef", "user_b"));
    let mut ids = Vec::new();
    for i in 0..6u8 {
        let f = i as f32;
        let fact = Uuid::new_v4();
        ids.push(fact);
        // Writes go through the hnsw_ef store: they must behave exactly as before.
        ef.upsert_vector(
            &a,
            sample_record(
                "org_ef",
                "user_a",
                fact,
                &format!("a{i}"),
                MemoryType::Preference,
                json!({}),
                [1.0, f, 0.5, 0.25],
            ),
        )
        .await
        .expect("upsert");
    }
    let b_fact = Uuid::new_v4();
    ef.upsert_vector(
        &b,
        sample_record(
            "org_ef",
            "user_b",
            b_fact,
            "b0",
            MemoryType::Preference,
            json!({}),
            [1.0, 0.0, 0.5, 0.25],
        ),
    )
    .await
    .expect("upsert other tenant");
    ef.delete_by_fact_id(&a, ids[0]).await.expect("delete");
    let query = |t: &TenantContext| VectorSearchQuery {
        tenant: t.clone(),
        embedding: vec![1.0, 0.0, 0.5, 0.25],
        limit: 10,
        memory_types: None,
        metadata_filter: None,
    };
    let with_ef = ef.search_vectors(query(&a)).await.expect("search ef");
    let without = plain.search_vectors(query(&a)).await.expect("search plain");
    let key = |r: &Vec<memcore_storage::VectorSearchResult>| {
        r.iter()
            .map(|x| (x.fact_id, x.content.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        key(&with_ef),
        key(&without),
        "same results with and without hnsw_ef"
    );
    assert_eq!(
        with_ef.len(),
        5,
        "5 of user_a's 6 points remain after the delete"
    );
    assert!(
        with_ef
            .iter()
            .all(|r| r.content.starts_with('a') && r.fact_id != b_fact),
        "tenant filter unchanged"
    );
    assert!(
        !with_ef.iter().any(|r| r.fact_id == ids[0]),
        "deleted fact not returned"
    );
    let other = ef.search_vectors(query(&b)).await.expect("search b");
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].fact_id, b_fact);
}

/// `MEMCORE_QDRANT_SEARCH_HNSW_EF=256`: the beam only reaches searches that
/// Qdrant runs on the HNSW graph. Searches it plans as plain or small-cardinality scans (unindexed
/// segments, and tenants below the full-scan threshold) must return the same
/// ids, scores and order with and without it, and each tenant must stay inside its own org + user.
#[tokio::test]
async fn search_hnsw_ef_256_is_inert_on_scanned_tenants_and_keeps_isolation() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collection = format!("memcore_vectors_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let plain = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
        .await
        .expect("connect");
    let ef = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
        .await
        .expect("connect")
        .with_search_hnsw_ef(Some(256));
    assert_eq!(ef.search_hnsw_ef(), Some(256));
    // Eight users of one org, plus the same user id in another org (cross-org isolation).
    let tenants: Vec<(&str, String)> = (0..8u8)
        .map(|u| ("org_ef256", format!("user_{u}")))
        .chain(std::iter::once(("org_other", "user_0".to_string())))
        .collect();
    let mut owner = std::collections::HashMap::new();
    for (t, (org, user)) in tenants.iter().enumerate() {
        for i in 0..5u8 {
            let fact = Uuid::new_v4();
            owner.insert(fact, (org.to_string(), user.clone()));
            let (a, b) = (t as f32 * 0.37, i as f32 * 0.11);
            ef.upsert_vector(
                &tenant(org, user),
                sample_record(
                    org,
                    user,
                    fact,
                    &format!("{org}/{user}/{i}"),
                    MemoryType::Knowledge,
                    json!({}),
                    [1.0, a.sin() + b, a.cos() - b, 0.5 + b],
                ),
            )
            .await
            .expect("upsert");
        }
    }
    for (org, user) in &tenants {
        let query = VectorSearchQuery {
            tenant: tenant(org, user),
            embedding: vec![1.0, 0.2, 0.7, 0.4],
            limit: 50,
            memory_types: None,
            metadata_filter: None,
        };
        let with_ef = ef.search_vectors(query.clone()).await.expect("search ef");
        let without = plain.search_vectors(query).await.expect("search plain");
        let key = |r: &Vec<memcore_storage::VectorSearchResult>| {
            r.iter()
                .map(|x| (x.fact_id, x.score.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            key(&with_ef),
            key(&without),
            "{org}/{user}: ids, scores and order unchanged by hnsw_ef 256"
        );
        assert_eq!(with_ef.len(), 5, "{org}/{user}: all of its own points");
        assert!(
            with_ef
                .iter()
                .all(|r| owner[&r.fact_id] == (org.to_string(), user.clone())),
            "{org}/{user}: tenant filter unchanged"
        );
    }
}

/// Bounded exact search (live Qdrant, small bound 3 so the boundary is reachable): the
/// count is the tenant's points in THIS collection under the mandatory org + user filter; other users
/// and other orgs never move a tenant across the bound; exact and approximate searches return the same
/// tenant-filtered results (memory-type filter honoured); searching and counting mutate nothing; a
/// concurrent writer of another tenant never leaks into exact results.
#[tokio::test]
async fn bounded_exact_search_counts_the_tenant_and_keeps_isolation() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collection = format!("memcore_vectors_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let plain = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
        .await
        .expect("connect");
    let bounded = std::sync::Arc::new(
        QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
            .await
            .expect("connect")
            .with_exact_search_max_tenant_points(Some(3)),
    );
    let a = tenant("org_x", "user_a");
    let write = |org: &'static str, user: &'static str, i: u32, ty: MemoryType| {
        sample_record(
            org,
            user,
            Uuid::new_v4(),
            &format!("{org}/{user}/{i}"),
            ty,
            json!({}),
            [1.0, i as f32 * 0.1, 0.5, 0.25],
        )
    };
    // user_a: 3 points (one Project); the same user id in another org and another user of the same org
    // hold many more points, which must not count.
    for i in 0..3 {
        let ty = if i == 2 {
            MemoryType::Project
        } else {
            MemoryType::Preference
        };
        plain
            .upsert_vector(&a, write("org_x", "user_a", i, ty))
            .await
            .expect("upsert a");
    }
    for i in 0..10 {
        plain
            .upsert_vector(
                &tenant("org_x", "user_b"),
                write("org_x", "user_b", i, MemoryType::Preference),
            )
            .await
            .expect("upsert b");
        plain
            .upsert_vector(
                &tenant("org_y", "user_a"),
                write("org_y", "user_a", i, MemoryType::Preference),
            )
            .await
            .expect("upsert other org");
    }
    assert_eq!(bounded.tenant_point_count_for_search(&a).await.unwrap(), 3);
    assert_eq!(
        bounded
            .tenant_point_count_for_search(&tenant("org_x", "user_b"))
            .await
            .unwrap(),
        10
    );
    assert_eq!(
        bounded.search_path(&a).await.unwrap(),
        memcore_storage::SearchPath::Exact
    );
    assert_eq!(
        bounded
            .search_path(&tenant("org_x", "user_b"))
            .await
            .unwrap(),
        memcore_storage::SearchPath::Approximate
    );
    let query = |t: &TenantContext, types: Option<Vec<MemoryType>>| VectorSearchQuery {
        tenant: t.clone(),
        embedding: vec![1.0, 0.0, 0.5, 0.25],
        limit: 10,
        memory_types: types,
        metadata_filter: None,
    };
    let key = |r: &Vec<memcore_storage::VectorSearchResult>| {
        r.iter()
            .map(|x| (x.fact_id, x.content.clone()))
            .collect::<Vec<_>>()
    };
    let writes_before = bounded.write_operation_count();
    let points_before = plain.count_tenant_points(&a).await.unwrap();
    let exact = bounded
        .search_vectors(query(&a, None))
        .await
        .expect("exact");
    let approximate = plain.search_vectors(query(&a, None)).await.expect("plain");
    assert_eq!(key(&exact), key(&approximate), "same tenant results");
    assert_eq!(exact.len(), 3);
    assert!(exact.iter().all(|r| r.content.starts_with("org_x/user_a/")));
    let typed = bounded
        .search_vectors(query(&a, Some(vec![MemoryType::Project])))
        .await
        .expect("typed");
    assert_eq!(
        typed.len(),
        1,
        "memory-type filter honoured on the exact path"
    );
    // user_b (10 > 3) goes approximate and still sees only its own points.
    let b = bounded
        .search_vectors(query(&tenant("org_x", "user_b"), None))
        .await
        .expect("b");
    assert_eq!(b.len(), 10);
    assert!(b.iter().all(|r| r.content.starts_with("org_x/user_b/")));
    let stats = bounded.search_policy_stats();
    assert_eq!((stats.exact_searches, stats.approximate_searches), (2, 1));
    assert_eq!(stats.tenant_count_failures, 0);
    // Counting and searching wrote nothing.
    assert_eq!(bounded.write_operation_count(), writes_before);
    assert_eq!(plain.count_tenant_points(&a).await.unwrap(), points_before);
    // One more point moves user_a across the bound: the next search is approximate.
    plain
        .upsert_vector(&a, write("org_x", "user_a", 3, MemoryType::Preference))
        .await
        .expect("4th point");
    assert_eq!(
        bounded.search_path(&a).await.unwrap(),
        memcore_storage::SearchPath::Approximate
    );
    // A concurrent writer of another tenant never leaks into user_a's results.
    let writer = {
        let url = url.clone();
        let collection = collection.clone();
        tokio::spawn(async move {
            let w = QdrantVectorStore::connect(&url, &collection, DIMENSIONS)
                .await
                .expect("connect writer");
            for i in 10..60 {
                w.upsert_vector(
                    &tenant("org_x", "user_c"),
                    sample_record(
                        "org_x",
                        "user_c",
                        Uuid::new_v4(),
                        &format!("org_x/user_c/{i}"),
                        MemoryType::Preference,
                        json!({}),
                        [1.0, 0.0, 0.5, 0.25],
                    ),
                )
                .await
                .expect("concurrent upsert");
            }
        })
    };
    let small = tenant("org_x", "user_d");
    plain
        .upsert_vector(&small, write("org_x", "user_d", 0, MemoryType::Preference))
        .await
        .expect("user_d");
    for _ in 0..30 {
        let r = bounded
            .search_vectors(query(&small, None))
            .await
            .expect("search during writes");
        assert!(r.iter().all(|x| x.content.starts_with("org_x/user_d/")));
        assert_eq!(r.len(), 1);
    }
    writer.await.expect("writer");
}
