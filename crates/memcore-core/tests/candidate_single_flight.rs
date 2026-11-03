//! Composition (H1 + H2): with the candidate legs running concurrently (H1) and the
//! query-embedding cache on (H2), the two legs of one candidate request share ONE provider embedding
//! operation through the single flight: the first leg leads, the second follows the in-flight
//! embedding (not a later cache hit), and the output equals the cache-off engine's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AddMemoryInput, EmbeddingIdentity, EmbeddingProvider, EpisodeComponents, MemoryEngine,
    MemoryMessage, MemorySession, MessageRole, SearchCandidatesInput, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;

const ORG: &str = "org_phase5_composition";

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
async fn concurrent_legs_share_one_in_flight_embedding() {
    let stores = seeded().await;
    let off = engine(&stores, Counting::new(), 0);
    let embedder = Counting::new();
    let on = engine(&stores, embedder.clone(), 1024);
    for (index, query) in ["greyhound walks", "kitchen paint", "birthday kettle"]
        .iter()
        .enumerate()
    {
        let expected = off.search_candidates(candidates(query)).await.unwrap();
        assert_eq!(
            on.search_candidates(candidates(query)).await.unwrap(),
            expected
        );
        assert_eq!(
            embedder.operations(),
            index as u32 + 1,
            "one provider operation per request"
        );
    }
    let stats = on.query_embedding_cache_stats().expect("cache on");
    // the provider delay keeps the first leg in flight while the second leg claims the key
    assert_eq!(
        (stats.requests, stats.leaders, stats.followers, stats.hits),
        (6, 3, 3, 0),
        "each request: one leader, one follower of its in-flight embedding"
    );
    assert_eq!(stats.provider_operations(), 3);
    assert_eq!((stats.failed_flights, stats.abandoned_flights), (0, 0));
}
