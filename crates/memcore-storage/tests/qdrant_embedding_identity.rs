//! Embedding identity, probe fingerprint and legacy compatibility proof (skipped
//! without MEMCORE_TEST_QDRANT_URL; disposable Qdrant only). Invented data.

#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use async_trait::async_trait;
use memcore_common::MemcoreResult;
use memcore_core::ports::{EmbeddingIdentity, EmbeddingProvider};
use memcore_core::{MemoryType, TenantContext};
use memcore_storage::{QdrantVectorStore, VectorRecord, VectorStore, identity_collection_name};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{CountPointsBuilder, GetPointsBuilder};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

const DIMS: usize = 8;

/// Deterministic fake embedder: `seed` stands for "the model behind the endpoint".
struct Fake {
    seed: u64,
    dims: usize,
    calls: AtomicUsize,
}

impl Fake {
    fn new(seed: u64) -> Self {
        Self {
            seed,
            dims: DIMS,
            calls: AtomicUsize::new(0),
        }
    }
    fn vector(&self, text: &str) -> Vec<f32> {
        let mut h: u64 = 1469598103934665603 ^ self.seed;
        (0..self.dims)
            .map(|i| {
                for b in text.bytes().chain([i as u8]) {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(1099511628211);
                }
                ((h % 2000) as f32 / 1000.0) - 1.0
            })
            .collect()
    }
}

#[async_trait]
impl EmbeddingProvider for Fake {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.vector(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        self.dims
    }
}

fn qdrant_url() -> Option<String> {
    std::env::var("MEMCORE_TEST_QDRANT_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
}

fn identity(model: &str) -> EmbeddingIdentity {
    let mut id = EmbeddingIdentity::remote("openai_compatible", model, DIMS);
    id.dedup_policy = Some("exact_text_only_uncalibrated".into());
    id
}

fn raw(url: &str) -> Qdrant {
    Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap()
}

async fn points(client: &Qdrant, collection: &str) -> u64 {
    client
        .count(CountPointsBuilder::new(collection).exact(true))
        .await
        .unwrap()
        .result
        .unwrap()
        .count
}

async fn has_fingerprint(client: &Qdrant, collection: &str) -> bool {
    let marker = identity_collection_name(collection);
    let got = client
        .get_points(GetPointsBuilder::new(&marker, vec![1u64.into()]).with_payload(true))
        .await
        .unwrap();
    got.result
        .first()
        .is_some_and(|p| p.payload.contains_key("probe_vector"))
}

async fn write_point(store: &QdrantVectorStore, embedder: &Fake, content: &str) {
    let t = TenantContext::new("org_id_t19", "user_id_t19").unwrap();
    store
        .upsert_vector(
            &t,
            VectorRecord {
                id: Uuid::new_v4(),
                fact_id: Uuid::new_v4(),
                org_id: t.org_id.clone(),
                user_id: t.user_id.clone(),
                embedding: embedder.vector(content),
                content: content.to_string(),
                memory_type: MemoryType::Preference,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
}

async fn cleanup(client: &Qdrant, names: &[&str]) {
    for name in names {
        let _ = client.delete_collection(*name).await;
        let _ = client
            .delete_collection(identity_collection_name(name))
            .await;
    }
}

#[tokio::test]
async fn new_store_records_identity_and_fingerprint_and_a_swapped_model_is_refused() {
    let Some(url) = qdrant_url() else { return };
    let client = raw(&url);
    let name = format!("memcore_t19_id_{}", Uuid::new_v4().simple());
    let _owned_name = qdrant_cleanup::TestCollections::owning(&name);
    let model_a = Fake::new(1);
    let swapped = Fake::new(2);

    let store = QdrantVectorStore::connect_verified(
        &url,
        &name,
        DIMS,
        Some(&identity("m")),
        Some(&model_a),
    )
    .await
    .expect("new store records its identity");
    assert!(
        has_fingerprint(&client, &name).await,
        "fingerprint recorded"
    );
    write_point(&store, &model_a, "an invented memory").await;
    QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&identity("m")), Some(&model_a))
        .await
        .expect("same model reconnects");

    // Same configured identity, different vectors behind it: refused, nothing written.
    let before = points(&client, &name).await;
    let error = QdrantVectorStore::connect_verified(
        &url,
        &name,
        DIMS,
        Some(&identity("m")),
        Some(&swapped),
    )
    .await
    .err()
    .expect("swapped model refused");
    assert!(error.to_string().contains("fingerprint"), "{error}");
    assert_eq!(points(&client, &name).await, before, "0 points written");

    // A different identity (model id, dedup policy) is refused before any probe.
    let mut other_policy = identity("m");
    other_policy.dedup_policy = Some("cosine_0.92_titan_v2_validated".into());
    for (who, id) in [
        ("model", identity("other-model")),
        ("dedup policy", other_policy),
    ] {
        let error =
            QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&id), Some(&model_a))
                .await
                .err()
                .unwrap_or_else(|| panic!("{who} change must be refused"));
        assert!(
            error.to_string().contains("refusing to mix vectors"),
            "{who}: {error}"
        );
    }
    assert_eq!(points(&client, &name).await, before);
    cleanup(&client, &[&name]).await;
}

#[tokio::test]
async fn a_record_without_fingerprint_gets_one_when_the_identity_matches() {
    let Some(url) = qdrant_url() else { return };
    let client = raw(&url);
    let name = format!("memcore_t19_fp_{}", Uuid::new_v4().simple());
    let _owned_name = qdrant_cleanup::TestCollections::owning(&name);
    let model_a = Fake::new(1);
    // Probe off at creation: identity only.
    QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&identity("m")), None)
        .await
        .unwrap();
    assert!(!has_fingerprint(&client, &name).await);
    QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&identity("m")), Some(&model_a))
        .await
        .unwrap();
    assert!(
        has_fingerprint(&client, &name).await,
        "fingerprint added at first verified start"
    );
    assert!(
        QdrantVectorStore::connect_verified(
            &url,
            &name,
            DIMS,
            Some(&identity("m")),
            Some(&Fake::new(9))
        )
        .await
        .is_err()
    );
    cleanup(&client, &[&name]).await;
}

#[tokio::test]
async fn legacy_store_needs_proven_compatibility_and_is_never_rebound() {
    let Some(url) = qdrant_url() else { return };
    let client = raw(&url);
    let name = format!("memcore_t19_legacy_{}", Uuid::new_v4().simple());
    let _owned_name = qdrant_cleanup::TestCollections::owning(&name);
    let built_with = Fake::new(1);
    // A earlier store: vectors, no identity record.
    let legacy = QdrantVectorStore::connect(&url, &name, DIMS).await.unwrap();
    write_point(&legacy, &built_with, "a stored memory text").await;
    write_point(&legacy, &built_with, "another stored text").await;
    let before = points(&client, &name).await;

    // The provider that built it reproduces a stored vector: used, nothing written next to it.
    QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&identity("m")), Some(&built_with))
        .await
        .expect("compatible provider proven");
    assert!(
        !client
            .collection_exists(identity_collection_name(&name))
            .await
            .unwrap(),
        "a legacy store is never silently rebound (no record written)"
    );
    // Another provider (same name and dimension) cannot reproduce it: refused, 0 points written.
    let error = QdrantVectorStore::connect_verified(
        &url,
        &name,
        DIMS,
        Some(&identity("m")),
        Some(&Fake::new(2)),
    )
    .await
    .err()
    .expect("incompatible provider refused");
    assert!(error.to_string().contains("does not reproduce"), "{error}");
    assert_eq!(points(&client, &name).await, before);
    // The same through the lazy (existing-store) path.
    let existing = QdrantVectorStore::connect_existing(&url, &name, DIMS)
        .await
        .unwrap()
        .unwrap();
    assert!(
        existing
            .verify_identity(Some(&identity("m")), Some(&Fake::new(2)))
            .await
            .is_err()
    );
    existing
        .verify_identity(Some(&identity("m")), Some(&built_with))
        .await
        .unwrap();
    // Probe off: used unverified (operator choice), still nothing written.
    QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&identity("m")), None)
        .await
        .unwrap();
    assert!(
        !client
            .collection_exists(identity_collection_name(&name))
            .await
            .unwrap()
    );
    cleanup(&client, &[&name]).await;
}

#[tokio::test]
async fn a_probe_of_the_wrong_dimension_is_refused() {
    let Some(url) = qdrant_url() else { return };
    let client = raw(&url);
    let name = format!("memcore_t19_dim_{}", Uuid::new_v4().simple());
    let _owned_name = qdrant_cleanup::TestCollections::owning(&name);
    let mut wrong = Fake::new(1);
    wrong.dims = DIMS + 2;
    let error =
        QdrantVectorStore::connect_verified(&url, &name, DIMS, Some(&identity("m")), Some(&wrong))
            .await
            .err()
            .expect("refused");
    assert!(error.to_string().contains("probe"), "{error}");
    assert_eq!(points(&client, &name).await, 0);
    cleanup(&client, &[&name]).await;
}

// ---- Deletion-only access to an identity-mismatched store ------------------

async fn scroll_all(client: &Qdrant, collection: &str) -> Vec<String> {
    let mut out: Vec<String> = client
        .scroll(
            qdrant_client::qdrant::ScrollPointsBuilder::new(collection)
                .limit(1000)
                .with_payload(true)
                .with_vectors(true),
        )
        .await
        .unwrap()
        .result
        .into_iter()
        .map(|p| format!("{:?}", (p.id, p.payload.get("user_id").cloned(), p.vectors)))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn deletion_only_opener_deletes_tenant_points_while_reads_and_writes_still_refuse() {
    let Some(url) = qdrant_url() else { return };
    let client = raw(&url);
    let name = format!("memcore_t20_del_{}", Uuid::new_v4().simple());
    let _owned_name = qdrant_cleanup::TestCollections::owning(&name);
    let protected = format!("memcore_t20_protected_{}", Uuid::new_v4().simple());
    let _owned_protected = qdrant_cleanup::TestCollections::owning(&protected);
    let built_with = Fake::new(1);
    // A store built with identity "old-model", holding two users of one org.
    let store = QdrantVectorStore::connect_verified(
        &url,
        &name,
        DIMS,
        Some(&identity("old-model")),
        Some(&built_with),
    )
    .await
    .unwrap();
    let mut user1_fact = Uuid::nil();
    for (user, n) in [("user_1", 3), ("user_2", 2)] {
        let t = TenantContext::new("org_del", user).unwrap();
        for i in 0..n {
            let fact_id = Uuid::new_v4();
            if user == "user_1" && i == 0 {
                user1_fact = fact_id;
            }
            store
                .upsert_vector(
                    &t,
                    VectorRecord {
                        id: fact_id,
                        fact_id,
                        org_id: t.org_id.clone(),
                        user_id: t.user_id.clone(),
                        embedding: built_with.vector(&format!("{user} {i}")),
                        content: format!("{user} memory {i}"),
                        memory_type: MemoryType::Conversation,
                        metadata: json!({}),
                    },
                )
                .await
                .unwrap();
        }
    }
    // An unrelated collection standing in for a protected reference store.
    let other = QdrantVectorStore::connect(&url, &protected, DIMS)
        .await
        .unwrap();
    write_point(&other, &built_with, "reference data").await;
    let protected_before = scroll_all(&client, &protected).await;

    // The configured provider is now "new-model": the verified opener refuses.
    let (u1, u2) = (url.clone(), url.clone());
    let (n1, n2) = (name.clone(), name.clone());
    let lazy = memcore_storage::LazyVectorStore::new(
        move || {
            let (url, name) = (u1.clone(), n1.clone());
            Box::pin(async move {
                let Some(store) = QdrantVectorStore::connect_existing(&url, &name, DIMS).await?
                else {
                    return Ok(None);
                };
                store
                    .verify_identity(Some(&identity("new-model")), None)
                    .await?;
                Ok(Some(
                    std::sync::Arc::new(store) as std::sync::Arc<dyn VectorStore>
                ))
            })
        },
        move || {
            Box::pin(async move {
                Err(memcore_common::MemcoreError::Internal(
                    "create not expected".into(),
                ))
            })
        },
    )
    .with_deletion_opener(move || {
        let (url, name) = (u2.clone(), n2.clone());
        Box::pin(async move {
            Ok(QdrantVectorStore::connect_existing(&url, &name, DIMS)
                .await?
                .map(|s| std::sync::Arc::new(s) as std::sync::Arc<dyn VectorStore>))
        })
    });
    let user1 = TenantContext::new("org_del", "user_1").unwrap();
    let user2 = TenantContext::new("org_del", "user_2").unwrap();
    let user2_before: Vec<String> = scroll_all(&client, &name)
        .await
        .into_iter()
        .filter(|p| p.contains("user_2"))
        .collect();

    // Search and write still refuse the mismatch.
    let search = lazy
        .search_vectors(memcore_storage::VectorSearchQuery {
            tenant: user1.clone(),
            embedding: vec![0.1; DIMS],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await;
    assert!(search.is_err(), "search must refuse an identity mismatch");
    let write = lazy
        .upsert_vector(
            &user1,
            VectorRecord {
                id: Uuid::new_v4(),
                fact_id: Uuid::new_v4(),
                org_id: "org_del".into(),
                user_id: "user_1".into(),
                embedding: vec![0.1; DIMS],
                content: "must not land".into(),
                memory_type: MemoryType::Conversation,
                metadata: json!({}),
            },
        )
        .await;
    assert!(write.is_err(), "write must refuse an identity mismatch");

    // Retention-style delete of one point, then forget-user: only user_1 is affected.
    lazy.delete_by_fact_id(&user1, user1_fact)
        .await
        .expect("retention delete");
    assert_eq!(points(&client, &name).await, 4);
    lazy.delete_by_user(&user1)
        .await
        .expect("forget-user delete");
    let after = scroll_all(&client, &name).await;
    assert!(
        after.iter().all(|p| !p.contains("user_1")),
        "user_1 removed"
    );
    assert_eq!(after, user2_before, "user_2 points bit-identical");
    // A foreign tenant's delete through the same path touches nothing.
    lazy.delete_by_user(&TenantContext::new("org_other", "user_2").unwrap())
        .await
        .unwrap();
    assert_eq!(scroll_all(&client, &name).await, user2_before);
    assert_eq!(
        scroll_all(&client, &protected).await,
        protected_before,
        "other collections untouched"
    );
    let _ = user2;
    cleanup(&client, &[&name, &protected]).await;
}

/// The deletion-only path surfaces Qdrant failures; an unreachable store is an error,
/// never a silent no-op (forget-user must not report success when nothing was deleted).
#[tokio::test]
async fn deletion_only_opener_surfaces_qdrant_failures() {
    let unreachable = "http://127.0.0.1:1".to_string();
    let lazy = memcore_storage::LazyVectorStore::new(
        || Box::pin(async { Ok(None) }),
        || {
            Box::pin(async {
                Err(memcore_common::MemcoreError::Internal(
                    "create not expected".into(),
                ))
            })
        },
    )
    .with_deletion_opener(move || {
        let url = unreachable.clone();
        Box::pin(async move {
            Ok(
                QdrantVectorStore::connect_existing(&url, "memcore_t20_unreachable", DIMS)
                    .await?
                    .map(|s| std::sync::Arc::new(s) as std::sync::Arc<dyn VectorStore>),
            )
        })
    });
    let tenant = TenantContext::new("org_del", "user_1").unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        lazy.delete_by_user(&tenant),
    )
    .await
    .expect("delete must not hang");
    assert!(
        result.is_err(),
        "unreachable Qdrant must surface as an error"
    );
    let result = lazy.delete_by_fact_id(&tenant, Uuid::new_v4()).await;
    assert!(
        result.is_err(),
        "unreachable Qdrant must surface as an error"
    );
}
