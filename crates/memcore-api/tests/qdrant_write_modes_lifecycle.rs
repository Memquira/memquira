//! Lifecycle paths on a real Qdrant in both write modes (skipped without
//! MEMCORE_TEST_QDRANT_URL; use a disposable Qdrant, never a reference collection).
//!
//! Invented, deterministic data. After every step the invariant is checked: each active fact of a
//! tenant has exactly one point whose payload content equals the fact's, the tenant has no other
//! fact points, and each embedded episode has exactly one point whose id is the episode id.

#[path = "../../memcore-storage/tests/support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::lifecycle::{
    LifecycleApplyResult, LifecycleContext, apply_fact_operation, apply_update_with_content,
};
use memcore_core::ports::VectorWriteIntent;
use memcore_core::{
    AddMemoryInput, CandidateFact, DeleteMemoryInput, EmbeddingProvider, EpisodeComponents,
    ExportUserDataInput, FactOperation, FactOperationDecision, FactStore, ForgetUserInput,
    ImportMode, ImportUserDataInput, MemoryEngine, MemoryMessage, MemoryType, MessageRole,
    SearchMemoryInput, TenantContext, VectorRecord, VectorSearchQuery, VectorSearchResult,
    VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{
    EPISODE_PAYLOAD_INDEXES, FACT_PAYLOAD_INDEXES, MockEpisodeStore, MockFactStore,
    QdrantVectorStore, QdrantWriteMode,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

const DIMENSIONS: usize = 8;

fn qdrant_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).expect("tenant")
}

fn candidate(content: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.8, None, json!({}))
        .expect("candidate")
}

fn decision(operation: FactOperation, target: Option<Uuid>) -> FactOperationDecision {
    FactOperationDecision {
        operation,
        target_fact_id: target,
        reason: Some("scripted".to_string()),
        confidence: 0.9,
    }
}

/// Every point of a tenant in a collection: fact_id -> (point ids, payload contents).
async fn tenant_points(
    store: &QdrantVectorStore,
    url: &str,
    collection: &str,
    t: &TenantContext,
) -> BTreeMap<Uuid, Vec<(String, String)>> {
    let _ = store;
    let client = qdrant_client::Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap();
    use qdrant_client::qdrant::{Condition, Filter, ScrollPointsBuilder};
    let points = client
        .scroll(
            ScrollPointsBuilder::new(collection)
                .filter(Filter::must([
                    Condition::matches("org_id", t.org_id.clone()),
                    Condition::matches("user_id", t.user_id.clone()),
                ]))
                .limit(10_000)
                .with_payload(true),
        )
        .await
        .unwrap()
        .result;
    let mut out: BTreeMap<Uuid, Vec<(String, String)>> = BTreeMap::new();
    for p in points {
        let get = |k: &str| {
            p.payload
                .get(k)
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default()
        };
        let id = match p.id.and_then(|id| id.point_id_options) {
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
            other => format!("{other:?}"),
        };
        out.entry(Uuid::parse_str(&get("fact_id")).unwrap())
            .or_default()
            .push((id, get("content")));
    }
    out
}

struct World {
    url: String,
    facts_collection: String,
    episodes_collection: String,
    fact_store: Arc<MockFactStore>,
    fact_vectors: Arc<QdrantVectorStore>,
    episode_vectors: Arc<QdrantVectorStore>,
    engine: MemoryEngine,
    embedding: Arc<MockEmbeddingProvider>,
    /// The test-owned collections, deleted when the world drops (also on panic).
    _owned: qdrant_cleanup::TestCollections,
}

impl World {
    async fn new(url: &str, mode: QdrantWriteMode) -> Self {
        let suffix = Uuid::new_v4().simple().to_string();
        let facts_collection = format!("memcore_t3c_life_{suffix}");
        let owned = qdrant_cleanup::TestCollections::owning(&facts_collection);
        let episodes_collection = format!("{facts_collection}_episodes");
        let fact_vectors = Arc::new(
            QdrantVectorStore::connect(url, &facts_collection, DIMENSIONS)
                .await
                .unwrap()
                .with_write_mode(mode),
        );
        fact_vectors
            .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
            .await
            .unwrap();
        let episode_vectors = Arc::new(
            QdrantVectorStore::connect(url, &episodes_collection, DIMENSIONS)
                .await
                .unwrap()
                .with_write_mode(mode),
        );
        episode_vectors
            .ensure_payload_indexes(EPISODE_PAYLOAD_INDEXES)
            .await
            .unwrap();
        let fact_store = Arc::new(MockFactStore::new());
        let embedding = Arc::new(MockEmbeddingProvider::new(DIMENSIONS));
        let engine = MemoryEngine::new(
            fact_store.clone(),
            fact_vectors.clone(),
            Arc::new(MockLlmProvider::new().with_extraction_candidates(vec![])),
            embedding.clone(),
        )
        .with_min_importance(0.0)
        .with_episodes(Some(EpisodeComponents {
            store: Arc::new(MockEpisodeStore::new()),
            vectors: episode_vectors.clone(),
        }));
        Self {
            url: url.to_string(),
            facts_collection,
            episodes_collection,
            fact_store,
            fact_vectors,
            episode_vectors,
            engine,
            embedding,
            _owned: owned,
        }
    }

    fn ctx(&self) -> LifecycleContext<'_> {
        LifecycleContext {
            fact_store: self.fact_store.as_ref(),
            vector_store: self.fact_vectors.as_ref(),
            embedding_provider: self.embedding.as_ref(),
        }
    }

    async fn add(&self, t: &TenantContext, content: &str) -> Uuid {
        match apply_fact_operation(
            &self.ctx(),
            t,
            &candidate(content),
            &decision(FactOperation::Add, None),
            &json!({}),
            None,
        )
        .await
        .unwrap()
        {
            LifecycleApplyResult::Added(fact) => fact.id,
            other => panic!("expected Added, got {other:?}"),
        }
    }

    async fn update(&self, t: &TenantContext, target: Uuid, content: &str) {
        let result = apply_fact_operation(
            &self.ctx(),
            t,
            &candidate(content),
            &decision(FactOperation::Update, Some(target)),
            &json!({}),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(result, LifecycleApplyResult::Updated { .. }));
    }

    /// The invariant, for one tenant. Returns (active facts, embedded episodes).
    async fn check(&self, t: &TenantContext, step: &str) -> (usize, usize) {
        let export = self
            .engine
            .export_user_data(ExportUserDataInput {
                tenant: t.clone(),
                include_events: false,
                include_deleted: false,
            })
            .await
            .unwrap();
        let active: BTreeMap<Uuid, String> = export
            .facts
            .iter()
            .map(|f| (f.id, f.content.clone()))
            .collect();
        let points = tenant_points(&self.fact_vectors, &self.url, &self.facts_collection, t).await;
        assert_eq!(
            points.keys().collect::<Vec<_>>(),
            active.keys().collect::<Vec<_>>(),
            "{step}: fact points == active facts"
        );
        for (fact_id, content) in &active {
            let pts = &points[fact_id];
            assert_eq!(pts.len(), 1, "{step}: one point for fact {fact_id}");
            assert_eq!(
                &pts[0].1, content,
                "{step}: point payload is the current content"
            );
        }
        let embedded: Vec<Uuid> = export
            .episodes
            .unwrap_or_default()
            .iter()
            .filter(|e| e.embedded)
            .map(|e| e.id)
            .collect();
        let episode_points = tenant_points(
            &self.episode_vectors,
            &self.url,
            &self.episodes_collection,
            t,
        )
        .await;
        assert_eq!(
            episode_points.len(),
            embedded.len(),
            "{step}: episode points"
        );
        for id in &embedded {
            let pts = &episode_points[id];
            assert_eq!(pts.len(), 1, "{step}: one point for episode {id}");
            assert_eq!(
                pts[0].0,
                id.to_string(),
                "{step}: episode point id = episode id"
            );
        }
        (active.len(), embedded.len())
    }

    async fn visible(&self, t: &TenantContext, query: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .engine
            .search_memory(SearchMemoryInput {
                tenant: t.clone(),
                query: query.to_string(),
                limit: 20,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .unwrap()
            .results
            .into_iter()
            .map(|r| format!("{:.5} {:?} {}", r.score, r.vector_score, r.content))
            .collect();
        out.sort();
        out
    }

    async fn drop_collections(&self) {
        let client = qdrant_client::Qdrant::from_url(&self.url)
            .skip_compatibility_check()
            .build()
            .unwrap();
        let _ = client.delete_collection(&self.facts_collection).await;
        let _ = client.delete_collection(&self.episodes_collection).await;
    }
}

fn messages(prefix: &str) -> Vec<MemoryMessage> {
    (0..3)
        .map(|i| MemoryMessage {
            role: MessageRole::User,
            content: format!("{prefix} message {i}: an invented sentence long enough to embed."),
            message_id: Some(format!("{prefix}-m{i}")),
            ..Default::default()
        })
        .collect()
}

/// The full scenario; returns what a client can see at each stage (scores and contents).
async fn scenario(url: &str, mode: QdrantWriteMode) -> Vec<(String, Vec<String>)> {
    let w = World::new(url, mode).await;
    let a = tenant("org_a", "user_1");
    let b = tenant("org_b", "user_1"); // same user id, other org
    let c = tenant("org_a", "user_2"); // same org, other user
    let mut seen = Vec::new();

    // Fresh legacy ADD (apply_add is also the consolidation-v2 ADD path).
    let mut ids = Vec::new();
    for content in [
        "likes tea",
        "plays chess",
        "lives in Lisbon",
        "works nights",
    ] {
        ids.push(w.add(&a, content).await);
    }
    for t in [&b, &c] {
        for content in ["likes tea", "plays chess"] {
            w.add(t, content).await;
        }
    }
    let b_before = tenant_points(&w.fact_vectors, url, &w.facts_collection, &b).await;
    let c_before = tenant_points(&w.fact_vectors, url, &w.facts_collection, &c).await;
    assert_eq!(w.check(&a, "fresh add").await.0, 4);
    seen.push(("after add".to_string(), w.visible(&a, "tea").await));

    // Legacy UPDATE, consolidation-v2 UPDATE, repeated re-embedding of the same content.
    w.update(&a, ids[0], "likes green tea").await;
    let v2 = apply_update_with_content(
        &w.ctx(),
        &a,
        &candidate("plays chess online"),
        ids[1],
        "plays chess online every Sunday",
    )
    .await
    .unwrap();
    assert!(matches!(v2, LifecycleApplyResult::Updated { .. }));
    for _ in 0..3 {
        w.update(&a, ids[2], "lives in Lisbon").await;
    }
    w.check(&a, "updates").await;
    seen.push(("after updates".to_string(), w.visible(&a, "tea").await));

    // Episodes through the engine, then a retry of the same messages (same message ids).
    let add = AddMemoryInput {
        tenant: a.clone(),
        messages: messages("s1"),
        metadata: json!({}),
        session: Default::default(),
    };
    w.engine.add_memory(add.clone()).await.unwrap();
    let (_, episodes_first) = w.check(&a, "episodes").await;
    w.engine.add_memory(add).await.unwrap();
    let (_, episodes_retry) = w.check(&a, "episode retry").await;
    assert_eq!(episodes_first, 3);
    assert_eq!(episodes_retry, 3);

    // Import append of the user's own export. Since R7 the unchanged export is
    // refused (IMPORT_ALREADY_APPLIED, nothing written); the append path runs on the same facts
    // under source ids the user does not own (stored under derived ids by C+).
    // This file runs only with a real Qdrant.
    let export = w
        .engine
        .export_user_data(ExportUserDataInput {
            tenant: a.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    let refused = w
        .engine
        .import_user_data(ImportUserDataInput {
            tenant: a.clone(),
            export: export.clone(),
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .expect_err("R7: already applied");
    assert!(refused.is_import_already_applied(), "{refused:?}");
    assert_eq!(w.check(&a, "refused append").await.0, 4);
    let mut renamed = export.clone();
    for fact in &mut renamed.facts {
        fact.id = Uuid::new_v4();
    }
    let appended = w
        .engine
        .import_user_data(ImportUserDataInput {
            tenant: a.clone(),
            export: renamed,
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(appended.imported_facts, 4);
    assert_eq!(w.check(&a, "import append (re-key)").await.0, 8);

    // Import replace: back to exactly the export's facts.
    w.engine
        .import_user_data(ImportUserDataInput {
            tenant: a.clone(),
            export,
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(w.check(&a, "import replace").await.0, 4);
    seen.push(("after import".to_string(), w.visible(&a, "tea").await));

    // Delete one memory, then forget the user.
    let survivor = w
        .engine
        .export_user_data(ExportUserDataInput {
            tenant: a.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap()
        .facts[0]
        .id;
    w.engine
        .delete_memory(DeleteMemoryInput {
            tenant: a.clone(),
            memory_id: survivor,
        })
        .await
        .unwrap();
    assert_eq!(w.check(&a, "delete memory").await.0, 3);
    w.engine
        .forget_user(ForgetUserInput { tenant: a.clone() })
        .await
        .unwrap();
    assert_eq!(w.check(&a, "forget user").await, (0, 0));

    // The other tenants were never touched (same point ids and contents).
    assert_eq!(
        tenant_points(&w.fact_vectors, url, &w.facts_collection, &b).await,
        b_before
    );
    assert_eq!(
        tenant_points(&w.fact_vectors, url, &w.facts_collection, &c).await,
        c_before
    );
    seen.push(("other tenant".to_string(), w.visible(&b, "tea").await));
    w.drop_collections().await;
    seen
}

#[tokio::test]
async fn lifecycle_paths_keep_one_point_per_fact_and_modes_agree() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let legacy = scenario(&url, QdrantWriteMode::Legacy).await;
    let optimized = scenario(&url, QdrantWriteMode::Optimized).await;
    assert!(
        legacy.iter().all(|(_, visible)| !visible.is_empty()),
        "every checkpoint shows results: {legacy:?}"
    );
    assert_eq!(
        legacy, optimized,
        "visible results are identical in both write modes"
    );
    eprintln!("visible results agree at {} checkpoints", legacy.len());
}

// ------------------------------------------------------------------------------------------------
// Failure injection.

/// An embedding provider that fails every call once armed.
struct ArmedFailingEmbedding {
    inner: MockEmbeddingProvider,
    fail: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl EmbeddingProvider for ArmedFailingEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(MemcoreError::ProviderError(
                "injected embedding failure".into(),
            ));
        }
        self.inner.embed_text(text).await
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(MemcoreError::ProviderError(
                "injected embedding failure".into(),
            ));
        }
        self.inner.embed_batch(texts).await
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

#[tokio::test]
async fn embedding_failure_before_replacement_keeps_the_old_vector() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    for mode in [QdrantWriteMode::Legacy, QdrantWriteMode::Optimized] {
        let w = World::new(&url, mode).await;
        let t = tenant("org_f", "user_f");
        let legacy_target = w.add(&t, "old legacy content").await;
        let v2_target = w.add(&t, "old v2 content").await;
        let embedding = ArmedFailingEmbedding {
            inner: MockEmbeddingProvider::new(DIMENSIONS),
            fail: std::sync::atomic::AtomicBool::new(true),
        };
        let ctx = LifecycleContext {
            fact_store: w.fact_store.as_ref(),
            vector_store: w.fact_vectors.as_ref(),
            embedding_provider: &embedding,
        };
        let ops_before = w.fact_vectors.write_operation_count();

        let legacy = apply_fact_operation(
            &ctx,
            &t,
            &candidate("new legacy content"),
            &decision(FactOperation::Update, Some(legacy_target)),
            &json!({}),
            None,
        )
        .await;
        assert!(
            matches!(legacy, Err(MemcoreError::ProviderError(_))),
            "{legacy:?}"
        );
        let v2 = apply_update_with_content(
            &ctx,
            &t,
            &candidate("new v2"),
            v2_target,
            "new v2 merged content",
        )
        .await;
        assert!(matches!(v2, Err(MemcoreError::ProviderError(_))), "{v2:?}");

        // Nothing changed: no Qdrant request, SQL rows keep the old text, the old vectors are
        // still there (one each) and still searchable.
        assert_eq!(
            w.fact_vectors.write_operation_count(),
            ops_before,
            "{mode:?}"
        );
        for (id, old) in [
            (legacy_target, "old legacy content"),
            (v2_target, "old v2 content"),
        ] {
            let row = w.fact_store.get_fact(&t, id).await.unwrap().unwrap();
            assert_eq!(row.content, old, "{mode:?}");
        }
        w.check(&t, "after failed updates").await;
        let query = w.embedding.embed_text("old legacy content").await.unwrap();
        let hits: Vec<VectorSearchResult> = w
            .fact_vectors
            .search_vectors(VectorSearchQuery {
                tenant: t.clone(),
                embedding: query,
                limit: 5,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .unwrap();
        assert_eq!(
            hits[0].fact_id, legacy_target,
            "{mode:?}: the old vector is still searchable"
        );

        // Once the provider recovers, the same update succeeds and leaves one current point.
        embedding
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        apply_fact_operation(
            &ctx,
            &t,
            &candidate("new legacy content"),
            &decision(FactOperation::Update, Some(legacy_target)),
            &json!({}),
            None,
        )
        .await
        .unwrap();
        w.check(&t, "after recovered update").await;
        w.drop_collections().await;
    }
}

/// A vector store that fails writes in a chosen way.
struct FailingWrites {
    inner: Arc<QdrantVectorStore>,
    /// 0 = fail before touching Qdrant; 1 = delete the fact's vectors, then fail.
    how: std::sync::atomic::AtomicU8,
    armed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl VectorStore for FailingWrites {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        self.write_vector(tenant, record, VectorWriteIntent::Replace)
            .await
    }

    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            if self.how.load(std::sync::atomic::Ordering::SeqCst) == 1 {
                let _ = self.inner.delete_by_fact_id(tenant, record.fact_id).await;
            }
            return Err(MemcoreError::StorageError(
                "injected vector write failure".into(),
            ));
        }
        self.inner.write_vector(tenant, record, intent).await
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(query).await
    }

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_by_user(tenant).await
    }
}

/// The vector write is not transactional with the SQL row. Characterised, not fixed.
#[tokio::test]
async fn vector_write_failures_are_characterised() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let w = World::new(&url, QdrantWriteMode::Optimized).await;
    let t = tenant("org_v", "user_v");
    let failing = FailingWrites {
        inner: w.fact_vectors.clone(),
        how: std::sync::atomic::AtomicU8::new(0),
        armed: std::sync::atomic::AtomicBool::new(false),
    };
    let ctx = LifecycleContext {
        fact_store: w.fact_store.as_ref(),
        vector_store: &failing,
        embedding_provider: w.embedding.as_ref(),
    };
    let target = match apply_fact_operation(
        &ctx,
        &t,
        &candidate("seed content"),
        &decision(FactOperation::Add, None),
        &json!({}),
        None,
    )
    .await
    .unwrap()
    {
        LifecycleApplyResult::Added(f) => f.id,
        other => panic!("{other:?}"),
    };

    // (a) A fresh ADD whose vector write fails: the error is returned, the SQL row exists without
    //     a vector (known sequence "add without vector").
    failing
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let add = apply_fact_operation(
        &ctx,
        &t,
        &candidate("added without vector"),
        &decision(FactOperation::Add, None),
        &json!({}),
        None,
    )
    .await;
    assert!(add.is_err());
    let points = tenant_points(&w.fact_vectors, &url, &w.facts_collection, &t).await;
    assert_eq!(points.len(), 1, "only the seed has a point");

    // (b) An UPDATE whose vector write fails before touching Qdrant: the row has the new text,
    //     the old vector (old text) is still the only point: stale but searchable.
    let update = apply_fact_operation(
        &ctx,
        &t,
        &candidate("updated text"),
        &decision(FactOperation::Update, Some(target)),
        &json!({}),
        None,
    )
    .await;
    assert!(update.is_err());
    let points = tenant_points(&w.fact_vectors, &url, &w.facts_collection, &t).await;
    assert_eq!(points[&target].len(), 1);
    assert_eq!(points[&target][0].1, "seed content", "stale vector kept");

    // (c) An UPDATE whose delete succeeds and whose upsert then fails: the fact has no vector
    //     (known sequence). The caller sees the error.
    failing.how.store(1, std::sync::atomic::Ordering::SeqCst);
    let update = apply_fact_operation(
        &ctx,
        &t,
        &candidate("updated again"),
        &decision(FactOperation::Update, Some(target)),
        &json!({}),
        None,
    )
    .await;
    assert!(update.is_err());
    let points = tenant_points(&w.fact_vectors, &url, &w.facts_collection, &t).await;
    assert!(
        !points.contains_key(&target),
        "vector lost after delete-then-failed-upsert"
    );

    // A later successful update of the same fact restores exactly one current point.
    failing
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    apply_fact_operation(
        &ctx,
        &t,
        &candidate("final text"),
        &decision(FactOperation::Update, Some(target)),
        &json!({}),
        None,
    )
    .await
    .unwrap();
    let points = tenant_points(&w.fact_vectors, &url, &w.facts_collection, &t).await;
    assert_eq!(points[&target].len(), 1);
    assert_eq!(points[&target][0].1, "final text");
    w.drop_collections().await;
}
