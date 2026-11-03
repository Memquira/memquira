//! An in-process, bounded, single-flight cache of search-query embeddings (off
//! by default, `MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY`).
//!
//! Key: the provider's embedding semantic identity (which covers model, dimensions and query
//! encoding / instruction semantics), the query-encoding operation, the organisation and the exact
//! query text. No normalisation, and no reuse across providers, models or configurations. A provider
//! without a known identity is never cached.
//!
//! Single flight: concurrent requests for one key make exactly one provider operation; the leader
//! makes it, followers wait for its outcome. A failed, cancelled or panicked flight is shared as a
//! failure with the followers that were waiting (no follower launches its own call; no retry loop
//! here), is never cached, and leaves no state behind, so a later request starts a new flight. Only a
//! validated vector (non-empty, every value finite, the provider's dimensions) is cached; an invalid
//! one is returned exactly as the provider produced it, uncached. Eviction is least recently read.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use memcore_common::{MemcoreError, MemcoreResult};
use tokio::sync::watch;

use crate::ports::EmbeddingProvider;

/// The query-encoding operation the cached vectors come from (`EmbeddingProvider::embed_query`).
/// Part of the key, so vectors of a different encoding are never served for this one.
const ENCODING: &str = "embed_query";

pub const QUERY_EMBEDDING_FLIGHT_ABANDONED: &str =
    "query embedding in flight was cancelled before completing";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryEmbeddingCacheStats {
    /// Every query-embedding request that reached the cache.
    pub requests: u64,
    /// Served from a stored entry: no provider operation.
    pub hits: u64,
    /// Started a flight: one provider operation each.
    pub leaders: u64,
    /// Waited for a concurrent leader's flight: no provider operation.
    pub followers: u64,
    /// Provider without a known identity: one uncached provider operation each.
    pub bypassed: u64,
    /// Flights that ended in a provider error (shared with their followers).
    pub failed_flights: u64,
    /// Flights cancelled or panicked before completing (shared with their followers as a failure).
    pub abandoned_flights: u64,
    /// Followers that received a failed or abandoned flight's failure.
    pub follower_failures: u64,
    /// Provider results returned but not cached because they failed validation.
    pub invalid_not_cached: u64,
    pub entries: usize,
}

impl QueryEmbeddingCacheStats {
    /// Actual provider embedding operations started through the cache.
    pub fn provider_operations(&self) -> u64 {
        self.leaders + self.bypassed
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    identity: String,
    encoding: &'static str,
    org_id: String,
    query: String,
}

struct Entry {
    embedding: Arc<Vec<f32>>,
    last_read: u64,
}

type Outcome = Result<Arc<Vec<f32>>, MemcoreError>;
type Flight = watch::Receiver<Option<Outcome>>;

#[derive(Default)]
struct State {
    entries: HashMap<Key, Entry>,
    in_flight: HashMap<Key, Flight>,
    clock: u64,
}

enum Role {
    Hit(Arc<Vec<f32>>),
    Follower(Flight),
    Leader(watch::Sender<Option<Outcome>>),
}

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    hits: AtomicU64,
    leaders: AtomicU64,
    followers: AtomicU64,
    bypassed: AtomicU64,
    failed_flights: AtomicU64,
    abandoned_flights: AtomicU64,
    follower_failures: AtomicU64,
    invalid_not_cached: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

pub struct QueryEmbeddingCache {
    capacity: usize,
    state: Mutex<State>,
    counters: Counters,
}

/// Removes the flight if the leader's future ends without completing it (cancellation or panic);
/// dropping the sender with it tells the waiting followers the flight was abandoned.
struct LeaderSlot<'a> {
    cache: &'a QueryEmbeddingCache,
    key: Key,
    done: bool,
}

impl Drop for LeaderSlot<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.cache.lock().in_flight.remove(&self.key);
            bump(&self.cache.counters.abandoned_flights);
        }
    }
}

fn valid(embedding: &[f32], dimensions: usize) -> bool {
    !embedding.is_empty()
        && embedding.len() == dimensions
        && embedding.iter().all(|value| value.is_finite())
}

impl QueryEmbeddingCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(State::default()),
            counters: Counters::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn claim(&self, key: &Key) -> Role {
        let mut state = self.lock();
        state.clock += 1;
        let clock = state.clock;
        if let Some(entry) = state.entries.get_mut(key) {
            entry.last_read = clock;
            return Role::Hit(entry.embedding.clone());
        }
        if let Some(flight) = state.in_flight.get(key) {
            return Role::Follower(flight.clone());
        }
        let (sender, receiver) = watch::channel(None);
        state.in_flight.insert(key.clone(), receiver);
        Role::Leader(sender)
    }

    /// The search query's embedding: from the cache, from a concurrent request's flight, or from one
    /// provider operation.
    pub async fn embed(
        &self,
        provider: &dyn EmbeddingProvider,
        org_id: &str,
        query: &str,
    ) -> MemcoreResult<Vec<f32>> {
        bump(&self.counters.requests);
        let Some(identity) = provider.embedding_identity() else {
            bump(&self.counters.bypassed);
            return provider.embed_query(query).await;
        };
        let key = Key {
            identity: identity.semantic_hash(),
            encoding: ENCODING,
            org_id: org_id.to_string(),
            query: query.to_string(),
        };
        match self.claim(&key) {
            Role::Hit(embedding) => {
                bump(&self.counters.hits);
                Ok(embedding.as_ref().clone())
            }
            Role::Follower(mut flight) => {
                bump(&self.counters.followers);
                let outcome = match flight.wait_for(|outcome| outcome.is_some()).await {
                    Ok(outcome) => outcome
                        .clone()
                        .expect("wait_for returns a completed outcome"),
                    // The leader was cancelled or panicked: its sender was dropped uncompleted.
                    Err(_) => Err(MemcoreError::ProviderError(
                        QUERY_EMBEDDING_FLIGHT_ABANDONED.to_string(),
                    )),
                };
                if outcome.is_err() {
                    bump(&self.counters.follower_failures);
                }
                outcome.map(|embedding| embedding.as_ref().clone())
            }
            Role::Leader(sender) => {
                bump(&self.counters.leaders);
                let mut slot = LeaderSlot {
                    cache: self,
                    key: key.clone(),
                    done: false,
                };
                let outcome: Outcome = provider.embed_query(query).await.map(Arc::new);
                {
                    let mut state = self.lock();
                    state.in_flight.remove(&key);
                    match &outcome {
                        Ok(embedding) if valid(embedding, provider.dimensions()) => {
                            self.insert(&mut state, key, embedding.clone());
                        }
                        Ok(_) => bump(&self.counters.invalid_not_cached),
                        Err(_) => bump(&self.counters.failed_flights),
                    }
                }
                slot.done = true;
                // Followers may all have gone; nothing depends on them receiving it.
                let _ = sender.send(Some(outcome.clone()));
                outcome.map(|embedding| embedding.as_ref().clone())
            }
        }
    }

    fn insert(&self, state: &mut State, key: Key, embedding: Arc<Vec<f32>>) {
        if !state.entries.contains_key(&key) {
            while state.entries.len() >= self.capacity {
                let Some(oldest) = state
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_read)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                state.entries.remove(&oldest);
            }
        }
        state.clock += 1;
        let clock = state.clock;
        state.entries.insert(
            key,
            Entry {
                embedding,
                last_read: clock,
            },
        );
    }

    pub fn stats(&self) -> QueryEmbeddingCacheStats {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let counters = &self.counters;
        QueryEmbeddingCacheStats {
            requests: load(&counters.requests),
            hits: load(&counters.hits),
            leaders: load(&counters.leaders),
            followers: load(&counters.followers),
            bypassed: load(&counters.bypassed),
            failed_flights: load(&counters.failed_flights),
            abandoned_flights: load(&counters.abandoned_flights),
            follower_failures: load(&counters.follower_failures),
            invalid_not_cached: load(&counters.invalid_not_cached),
            entries: self.lock().entries.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::future::join_all;

    use super::*;
    use crate::ports::EmbeddingIdentity;

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Ok,
        Fail,
        NonFinite,
        WrongDimensions,
        Panic,
    }

    /// Counts provider operations directly.
    struct Counting {
        identity: Option<EmbeddingIdentity>,
        operations: AtomicU32,
        mode: Mode,
        delay: Duration,
    }

    impl Counting {
        fn new(model: Option<&str>, mode: Mode, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                identity: model.map(|model| EmbeddingIdentity::remote("test", model, DIMS)),
                operations: AtomicU32::new(0),
                mode,
                delay,
            })
        }
        fn ok() -> Arc<Self> {
            Self::new(Some("model-a"), Mode::Ok, Duration::ZERO)
        }
        fn slow(mode: Mode) -> Arc<Self> {
            Self::new(Some("model-a"), mode, Duration::from_millis(40))
        }
        fn operations(&self) -> u32 {
            self.operations.load(Ordering::SeqCst)
        }
    }

    const DIMS: usize = 4;

    fn vector(text: &str) -> Vec<f32> {
        let mut out = vec![0.0; DIMS];
        for (index, byte) in text.bytes().enumerate() {
            out[index % DIMS] += f32::from(byte);
        }
        out
    }

    #[async_trait]
    impl EmbeddingProvider for Counting {
        async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            match self.mode {
                Mode::Ok => Ok(vector(text)),
                Mode::Fail => Err(MemcoreError::ProviderError("titan down".to_string())),
                Mode::NonFinite => Ok(vec![f32::NAN; DIMS]),
                Mode::WrongDimensions => Ok(vec![1.0; DIMS + 1]),
                Mode::Panic => panic!("provider panicked"),
            }
        }
        async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| vector(text)).collect())
        }
        fn dimensions(&self) -> usize {
            DIMS
        }
        fn embedding_identity(&self) -> Option<EmbeddingIdentity> {
            self.identity.clone()
        }
    }

    // 1
    #[tokio::test]
    async fn two_concurrent_identical_requests_make_one_provider_operation() {
        let provider = Counting::slow(Mode::Ok);
        let cache = QueryEmbeddingCache::new(16);
        let (a, b) = tokio::join!(
            cache.embed(provider.as_ref(), "org", "q"),
            cache.embed(provider.as_ref(), "org", "q")
        );
        assert_eq!(provider.operations(), 1);
        // 5: leader success -> the follower receives the identical vector
        assert_eq!(a.unwrap(), vector("q"));
        assert_eq!(b.unwrap(), vector("q"));
        let stats = cache.stats();
        assert_eq!((stats.leaders, stats.followers, stats.hits), (1, 1, 0));
        assert_eq!(stats.provider_operations(), 1);
    }

    // 2
    #[tokio::test]
    async fn many_concurrent_identical_requests_make_one_provider_operation() {
        let provider = Counting::slow(Mode::Ok);
        let cache = QueryEmbeddingCache::new(16);
        let results = join_all((0..32).map(|_| cache.embed(provider.as_ref(), "org", "q"))).await;
        assert_eq!(provider.operations(), 1);
        assert!(results.into_iter().all(|r| r.unwrap() == vector("q")));
        let stats = cache.stats();
        assert_eq!(
            (stats.requests, stats.leaders, stats.followers),
            (32, 1, 31)
        );
    }

    // 3
    #[tokio::test]
    async fn different_queries_make_separate_operations_and_never_share() {
        let provider = Counting::slow(Mode::Ok);
        let cache = QueryEmbeddingCache::new(16);
        let (a, b) = tokio::join!(
            cache.embed(provider.as_ref(), "org", "q1"),
            cache.embed(provider.as_ref(), "org", "q2")
        );
        assert_eq!(a.unwrap(), vector("q1"));
        assert_eq!(b.unwrap(), vector("q2"));
        assert_eq!(provider.operations(), 2);
        // exact text and organisation scope: no normalisation, no sharing across orgs
        for (org, query) in [("other", "q1"), ("org", "q1 "), ("org", "Q1")] {
            assert_eq!(
                cache.embed(provider.as_ref(), org, query).await.unwrap(),
                vector(query)
            );
        }
        assert_eq!(provider.operations(), 5);
    }

    // 4
    #[tokio::test]
    async fn the_same_query_under_another_embedding_identity_is_never_reused() {
        let model_a = Counting::new(Some("model-a"), Mode::Ok, Duration::ZERO);
        let model_b = Counting::new(Some("model-b"), Mode::Ok, Duration::ZERO);
        let cache = QueryEmbeddingCache::new(16);
        cache.embed(model_a.as_ref(), "org", "q").await.unwrap();
        cache.embed(model_b.as_ref(), "org", "q").await.unwrap();
        cache.embed(model_a.as_ref(), "org", "q").await.unwrap();
        assert_eq!((model_a.operations(), model_b.operations()), (1, 1));
        // a provider with no known identity bypasses the cache entirely
        let unknown = Counting::new(None, Mode::Ok, Duration::ZERO);
        cache.embed(unknown.as_ref(), "org", "q").await.unwrap();
        cache.embed(unknown.as_ref(), "org", "q").await.unwrap();
        assert_eq!(unknown.operations(), 2);
        assert_eq!(cache.stats().bypassed, 2);
    }

    // 6
    #[tokio::test]
    async fn a_failed_flight_is_shared_with_its_followers_and_never_cached() {
        let provider = Counting::slow(Mode::Fail);
        let cache = QueryEmbeddingCache::new(16);
        let results = join_all((0..8).map(|_| cache.embed(provider.as_ref(), "org", "q"))).await;
        assert_eq!(
            provider.operations(),
            1,
            "followers do not launch their own calls"
        );
        for result in results {
            assert_eq!(
                result.unwrap_err(),
                MemcoreError::ProviderError("titan down".to_string())
            );
        }
        let stats = cache.stats();
        assert_eq!((stats.failed_flights, stats.follower_failures), (1, 7));
        assert_eq!(stats.entries, 0);
        assert!(cache.lock().in_flight.is_empty(), "no poisoned flight");
        // a later independent request starts a new flight
        assert!(cache.embed(provider.as_ref(), "org", "q").await.is_err());
        assert_eq!(provider.operations(), 2);
    }

    // 7
    #[tokio::test]
    async fn a_cancelled_flight_fails_its_followers_cleans_up_and_a_later_request_retries() {
        let provider = Counting::slow(Mode::Ok);
        let cache = QueryEmbeddingCache::new(16);
        let (leader, follower) = tokio::join!(
            tokio::time::timeout(
                Duration::from_millis(10),
                cache.embed(provider.as_ref(), "org", "q")
            ),
            async {
                tokio::time::sleep(Duration::from_millis(2)).await;
                cache.embed(provider.as_ref(), "org", "q").await
            }
        );
        assert!(leader.is_err(), "leader cancelled by the timeout");
        assert_eq!(
            follower.unwrap_err(),
            MemcoreError::ProviderError(QUERY_EMBEDDING_FLIGHT_ABANDONED.to_string())
        );
        assert_eq!(provider.operations(), 1);
        let stats = cache.stats();
        assert_eq!((stats.abandoned_flights, stats.entries), (1, 0));
        assert!(cache.lock().in_flight.is_empty());
        assert_eq!(
            cache.embed(provider.as_ref(), "org", "q").await.unwrap(),
            vector("q")
        );
        assert_eq!(provider.operations(), 2);
    }

    #[tokio::test]
    async fn a_panicking_provider_leaves_no_poisoned_flight() {
        let cache = Arc::new(QueryEmbeddingCache::new(16));
        let panicking = Counting::new(Some("model-a"), Mode::Panic, Duration::ZERO);
        let (c, p) = (cache.clone(), panicking.clone());
        let joined = tokio::spawn(async move { c.embed(p.as_ref(), "org", "q").await }).await;
        assert!(joined.is_err(), "the panic surfaced to the task");
        assert!(cache.lock().in_flight.is_empty());
        let healthy = Counting::ok();
        assert_eq!(
            cache.embed(healthy.as_ref(), "org", "q").await.unwrap(),
            vector("q")
        );
    }

    // 8
    #[tokio::test]
    async fn invalid_vectors_are_returned_as_produced_but_never_cached() {
        for mode in [Mode::NonFinite, Mode::WrongDimensions] {
            let provider = Counting::new(Some("model-a"), mode, Duration::ZERO);
            let cache = QueryEmbeddingCache::new(16);
            let first = cache.embed(provider.as_ref(), "org", "q").await.unwrap();
            let second = cache.embed(provider.as_ref(), "org", "q").await.unwrap();
            assert_eq!(first.len(), second.len());
            assert_eq!(provider.operations(), 2, "not served from the cache");
            let stats = cache.stats();
            assert_eq!((stats.invalid_not_cached, stats.entries), (2, 0));
        }
    }

    // 9
    #[tokio::test]
    async fn a_cache_hit_makes_zero_provider_operations() {
        let provider = Counting::ok();
        let cache = QueryEmbeddingCache::new(16);
        let first = cache.embed(provider.as_ref(), "org", "q").await.unwrap();
        let before = provider.operations();
        let second = cache.embed(provider.as_ref(), "org", "q").await.unwrap();
        assert_eq!(provider.operations(), before);
        assert_eq!(first, second);
        assert_eq!(cache.stats().hits, 1);
    }

    // 10
    #[tokio::test]
    async fn eviction_is_least_recently_read_and_recomputes_correctly() {
        let provider = Counting::ok();
        let cache = QueryEmbeddingCache::new(2);
        for query in ["a", "b", "a", "c"] {
            cache.embed(provider.as_ref(), "org", query).await.unwrap(); // c evicts b
        }
        assert_eq!(cache.stats().entries, 2);
        let before = provider.operations();
        assert_eq!(
            cache.embed(provider.as_ref(), "org", "a").await.unwrap(),
            vector("a")
        );
        assert_eq!(provider.operations(), before, "a kept");
        assert_eq!(
            cache.embed(provider.as_ref(), "org", "b").await.unwrap(),
            vector("b")
        );
        assert_eq!(
            provider.operations(),
            before + 1,
            "b evicted and recomputed"
        );
    }

    #[test]
    fn identities_differing_in_any_semantic_field_hash_differently() {
        let base = EmbeddingIdentity::remote("local_onnx", "example/local-model", 1024);
        let mut instructed = base.clone();
        instructed.query_instruction_sha256 = Some("abc".to_string());
        let mut narrower = base.clone();
        narrower.dimensions = 384;
        let mut pooled = base.clone();
        pooled.pooling = Some("last_token".to_string());
        let hashes = [&base, &instructed, &narrower, &pooled].map(|i| i.semantic_hash());
        for (i, left) in hashes.iter().enumerate() {
            for right in &hashes[i + 1..] {
                assert_ne!(left, right);
            }
        }
        assert_eq!(base.semantic_hash(), base.clone().semantic_hash());
    }
}
