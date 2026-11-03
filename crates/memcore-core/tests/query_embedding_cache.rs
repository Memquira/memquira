//! With the query-embedding cache on, a candidate request
//! makes exactly one provider embedding operation for its query, both legs consume that embedding,
//! and every output is identical to the cache-off engine on the same stores. Different queries never
//! share; a failure reaches the caller as before and is not cached.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AddMemoryInput, EmbeddingIdentity, EmbeddingProvider, EpisodeComponents, MemoryEngine,
    MemoryMessage, MemorySession, MessageRole, SearchCandidatesInput, SearchEpisodesInput,
    SearchMemoryInput, SearchOptions, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;

const ORG: &str = "org_phase5_h2";

fn tenant(user: &str) -> TenantContext {
    TenantContext::new(ORG, user).expect("tenant")
}

/// The deterministic mock embedder, with a delay (so concurrent legs are both in flight), a known
/// identity, a switchable failure and a direct count of provider operations.
struct Counting {
    inner: MockEmbeddingProvider,
    operations: AtomicU32,
    fail: AtomicBool,
}

impl Counting {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MockEmbeddingProvider::new(8),
            operations: AtomicU32::new(0),
            fail: AtomicBool::new(false),
        })
    }
    fn operations(&self) -> u32 {
        self.operations.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EmbeddingProvider for Counting {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        self.operations.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        if self.fail.load(Ordering::SeqCst) {
            return Err(MemcoreError::ProviderError("titan down".to_string()));
        }
        self.inner.embed_text(text).await
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        self.operations.fetch_add(1, Ordering::SeqCst);
        self.inner.embed_batch(texts).await
    }
    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
    fn embedding_identity(&self) -> Option<EmbeddingIdentity> {
        Some(EmbeddingIdentity::remote("test", "mock-deterministic", 8))
    }
}

struct Stores {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
}

fn engine(stores: &Stores, embedder: Arc<Counting>, cache_capacity: usize) -> MemoryEngine {
    MemoryEngine::new(
        stores.facts.clone(),
        stores.vectors.clone(),
        Arc::new(MockLlmProvider::new()),
        embedder,
    )
    .with_episodes(Some(EpisodeComponents {
        store: stores.episodes.clone(),
        vectors: stores.episode_vectors.clone(),
    }))
    .with_query_embedding_cache(cache_capacity)
}

async fn seeded() -> Stores {
    let stores = Stores {
        facts: Arc::new(MockFactStore::new()),
        vectors: Arc::new(MockVectorStore::new()),
        episodes: Arc::new(MockEpisodeStore::new()),
        episode_vectors: Arc::new(MockVectorStore::new()),
    };
    let loader = engine(&stores, Counting::new(), 0);
    let lines = [
        "Rowena adopted a rescue greyhound called Juniper in the spring.",
        "Juniper enjoys long walks beside the canal most mornings.",
        "Rowena's sister Ixora gave her a copper kettle for her birthday.",
        "Rowena and Dunstan are repainting the kitchen a pale sage green.",
    ];
    let messages = lines
        .iter()
        .enumerate()
        .map(|(index, content)| MemoryMessage {
            role: MessageRole::User,
            content: content.to_string(),
            name: Some("rowena".to_string()),
            timestamp: None,
            message_id: Some(format!("D1:{index}")),
        })
        .collect();
    loader
        .add_memory(AddMemoryInput {
            tenant: tenant("rowena"),
            messages,
            metadata: json!({ "session_id": "S1" }),
            session: MemorySession {
                id: Some("S1".to_string()),
                timestamp: Some(Utc.with_ymd_and_hms(2023, 5, 8, 13, 0, 0).unwrap()),
            },
        })
        .await
        .expect("add");
    stores
}

fn candidates(query: &str) -> SearchCandidatesInput {
    SearchCandidatesInput {
        tenant: tenant("rowena"),
        query: query.to_string(),
        fact_limit: 50,
        episode_limit: 50,
        reference_time: None,
    }
}

#[tokio::test]
async fn one_candidate_request_makes_one_embedding_operation_and_identical_output() {
    let stores = seeded().await;
    let off_embedder = Counting::new();
    let on_embedder = Counting::new();
    let off = engine(&stores, off_embedder.clone(), 0);
    let on = engine(&stores, on_embedder.clone(), 1024);

    for (index, query) in ["greyhound walks", "kitchen paint", "birthday kettle"]
        .iter()
        .enumerate()
    {
        let expected = off.search_candidates(candidates(query)).await.unwrap();
        let actual = on.search_candidates(candidates(query)).await.unwrap();
        assert_eq!(actual, expected, "identical candidate output for {query}");
        assert_eq!(
            off_embedder.operations(),
            2 * (index as u32 + 1),
            "cache off: one per leg"
        );
        assert_eq!(
            on_embedder.operations(),
            index as u32 + 1,
            "cache on: one per request"
        );
    }
    let stats = on.query_embedding_cache_stats().expect("cache on");
    assert_eq!(
        stats.requests, 6,
        "two legs x three requests reached the cache"
    );
    assert_eq!(stats.provider_operations(), 3);
    assert_eq!(stats.leaders, 3);
    // the second leg either waited for the in-flight embedding (concurrent legs) or hit the entry
    // (sequential legs); either way it consumed the same embedding without a provider operation
    assert_eq!(stats.followers + stats.hits, 3);
    assert_eq!(on.query_embedding_cache_stats(), Some(stats));
    assert_eq!(off.query_embedding_cache_stats(), None);
}

#[tokio::test]
async fn search_and_episode_search_outputs_are_identical_with_the_cache_on() {
    let stores = seeded().await;
    let off = engine(&stores, Counting::new(), 0);
    let on = engine(&stores, Counting::new(), 1024);
    for _ in 0..2 {
        let input = SearchMemoryInput {
            tenant: tenant("rowena"),
            query: "greyhound walks".to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        };
        assert_eq!(
            on.search_memory_with_options(input.clone(), SearchOptions::default())
                .await
                .unwrap(),
            off.search_memory_with_options(input, SearchOptions::default())
                .await
                .unwrap()
        );
        let episodes = SearchEpisodesInput {
            tenant: tenant("rowena"),
            query: "greyhound walks".to_string(),
            limit: 10,
        };
        assert_eq!(
            on.search_episodes(episodes.clone()).await.unwrap(),
            off.search_episodes(episodes).await.unwrap()
        );
    }
}

#[tokio::test]
async fn a_provider_failure_reaches_the_caller_and_is_not_cached() {
    let stores = seeded().await;
    let embedder = Counting::new();
    let on = engine(&stores, embedder.clone(), 1024);
    embedder.fail.store(true, Ordering::SeqCst);
    let error = on
        .search_candidates(candidates("greyhound walks"))
        .await
        .unwrap_err();
    assert_eq!(error, MemcoreError::ProviderError("titan down".to_string()));
    let stats = on.query_embedding_cache_stats().unwrap();
    assert_eq!(stats.entries, 0);
    embedder.fail.store(false, Ordering::SeqCst);
    let before = embedder.operations();
    on.search_candidates(candidates("greyhound walks"))
        .await
        .expect("recovers");
    assert_eq!(
        embedder.operations(),
        before + 1,
        "a new flight after the failure"
    );
}
