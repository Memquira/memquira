use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use tokio::sync::Mutex;
use tokio::time;

/// Background (stale-while-revalidate) refreshes allowed at once per process. Beyond it
/// a stale hit is served without starting a refresh; a later request can start one.
pub const MAX_BACKGROUND_REFRESHES: usize = 4;
/// Deadline of one background refresh; it fails as a timeout after it.
pub const BACKGROUND_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

use super::cache::{
    CachedContextEntry, ContextCache, ContextCacheConfig, ContextCacheKey, ContextCacheUsage,
};
use super::cache_metrics::ContextCacheMetricsRecorder;

/// Process-local stampede protection configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextCacheStampedeConfig {
    pub enabled: bool,
    pub lock_timeout: Duration,
}

impl ContextCacheStampedeConfig {
    pub fn from_cache_config(config: &ContextCacheConfig) -> Self {
        Self {
            enabled: config.stampede_protection_active(),
            lock_timeout: Duration::from_secs(config.stampede_lock_timeout_seconds),
        }
    }

    pub fn validate(&self) -> MemcoreResult<()> {
        if self.enabled && self.lock_timeout.is_zero() {
            return Err(MemcoreError::ValidationError(
                "context cache stampede lock timeout must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }
}

/// Coordinates cache lookups with optional in-process request coalescing.
#[derive(Clone)]
pub struct ContextCacheCoordinator {
    cache: Arc<dyn ContextCache>,
    cache_config: ContextCacheConfig,
    stampede_config: ContextCacheStampedeConfig,
    metrics: Arc<dyn ContextCacheMetricsRecorder>,
    inflight: Arc<Mutex<HashMap<ContextCacheKey, Arc<Mutex<()>>>>>,
    /// Keys with a background refresh started and not finished. Its size is the number of
    /// outstanding refresh tasks, bounded by `max_background_refreshes`.
    refresh_inflight: Arc<std::sync::Mutex<HashSet<ContextCacheKey>>>,
    max_background_refreshes: usize,
    background_refresh_timeout: Duration,
    /// A per-user invalidation generation. [`Self::invalidate_user`] bumps
    /// it (holding the user's lock across the cache invalidation), and a computed entry is stored
    /// only if the generation it observed BEFORE reading the stores is still current, so a context
    /// computed from rows read before a deletion is never cached after that deletion invalidated
    /// the user. Process-local: it orders the computations and invalidations of this coordinator
    /// (one per engine), not those of other processes sharing an external cache.
    generations: UserGenerations,
}

/// Per (org, user): the invalidation generation, behind the lock that orders a store against an
/// invalidation of that user.
type UserGenerations = Arc<Mutex<HashMap<(String, String), Arc<Mutex<u64>>>>>;

/// Releases a refresh key on every exit: success, error, timeout, panic or cancellation.
struct RefreshKeyGuard {
    set: Arc<std::sync::Mutex<HashSet<ContextCacheKey>>>,
    key: ContextCacheKey,
}

impl Drop for RefreshKeyGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.set.lock() {
            set.remove(&self.key);
        }
    }
}

impl ContextCacheCoordinator {
    pub fn new(
        cache: Arc<dyn ContextCache>,
        cache_config: ContextCacheConfig,
        metrics: Arc<dyn ContextCacheMetricsRecorder>,
    ) -> Self {
        let stampede_config = ContextCacheStampedeConfig::from_cache_config(&cache_config);
        Self {
            cache,
            cache_config,
            stampede_config,
            metrics,
            inflight: Arc::new(Mutex::new(HashMap::new())),
            refresh_inflight: Arc::new(std::sync::Mutex::new(HashSet::new())),
            max_background_refreshes: MAX_BACKGROUND_REFRESHES,
            background_refresh_timeout: BACKGROUND_REFRESH_TIMEOUT,
            generations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn user_generation(&self, org_id: &str, user_id: &str) -> Arc<Mutex<u64>> {
        self.generations
            .lock()
            .await
            .entry((org_id.to_string(), user_id.to_string()))
            .or_insert_with(|| Arc::new(Mutex::new(0)))
            .clone()
    }

    async fn current_generation(&self, key: &ContextCacheKey) -> u64 {
        *self
            .user_generation(&key.org_id, &key.user_id)
            .await
            .lock()
            .await
    }

    /// Invalidates every cached context of the user and bumps the user's
    /// generation, so a computation that started before this call never stores its entry.
    pub async fn invalidate_user(&self, org_id: &str, user_id: &str) -> MemcoreResult<usize> {
        let generation = self.user_generation(org_id, user_id).await;
        let mut current = generation.lock().await;
        *current = current.wrapping_add(1);
        self.cache.invalidate_user(org_id, user_id).await
    }

    /// Stores `entry` only if the user's generation is still `observed`; `false` = skipped
    /// because the user was invalidated while the entry was computed.
    async fn set_if_current(
        &self,
        key: &ContextCacheKey,
        entry: &CachedContextEntry,
        observed: u64,
    ) -> MemcoreResult<bool> {
        let generation = self.user_generation(&key.org_id, &key.user_id).await;
        let current = generation.lock().await;
        if *current != observed {
            return Ok(false);
        }
        self.cache.set(key.clone(), entry.clone()).await?;
        Ok(true)
    }

    fn log_store_outcome(&self, stored: bool, key: &ContextCacheKey, waited_for_inflight: bool) {
        if stored {
            self.metrics.record_set();
            self.log_cache_event("context_cache_set", key, false, false, waited_for_inflight);
        } else {
            tracing::debug!(
                event = "context_cache_set_skipped_invalidated",
                org_id = %key.org_id,
                user_id = %key.user_id,
                "context computed across a user invalidation is not cached"
            );
        }
    }

    /// Overrides the background refresh budget and deadline (tests).
    pub fn with_refresh_bounds(mut self, max_concurrent: usize, timeout: Duration) -> Self {
        self.max_background_refreshes = max_concurrent.max(1);
        self.background_refresh_timeout = timeout;
        self
    }

    pub fn metrics(&self) -> Arc<dyn ContextCacheMetricsRecorder> {
        self.metrics.clone()
    }

    /// Background refreshes started and not finished (diagnostics and tests).
    pub fn refresh_inflight_count(&self) -> usize {
        self.refresh_inflight
            .lock()
            .map(|set| set.len())
            .unwrap_or(0)
    }

    /// Marks a background refresh as in-flight for the given key. Returns `true` when started:
    /// never for a key already refreshing (coalesced), and never beyond the per-process refresh
    /// budget (skipped).
    pub async fn try_start_refresh(&self, key: &ContextCacheKey) -> bool {
        let Ok(mut set) = self.refresh_inflight.lock() else {
            return false;
        };
        if set.contains(key) {
            return false;
        }
        if set.len() >= self.max_background_refreshes {
            self.metrics.record_refresh_skipped();
            return false;
        }
        set.insert(key.clone())
    }

    /// Releases a key marked by `try_start_refresh` whose refresh will not run (for example at
    /// shutdown), so the budget and later refreshes of the key are not blocked.
    pub async fn finish_refresh(&self, key: &ContextCacheKey) {
        if let Ok(mut set) = self.refresh_inflight.lock() {
            set.remove(key);
        }
    }

    pub fn cache_config(&self) -> &ContextCacheConfig {
        &self.cache_config
    }

    #[cfg(test)]
    pub(crate) fn inflight_lock_count(&self) -> usize {
        self.inflight.try_lock().map(|map| map.len()).unwrap_or(0)
    }

    pub async fn get_or_compute<F, Fut>(
        &self,
        key: ContextCacheKey,
        compute: F,
    ) -> MemcoreResult<(CachedContextEntry, ContextCacheUsage)>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = MemcoreResult<CachedContextEntry>>,
    {
        self.cache_config.validate()?;
        self.stampede_config.validate()?;

        if let Some(entry) = self.cache.get(&key).await? {
            self.metrics.record_hit();
            self.log_cache_event("context_cache_hit", &key, false, false, false);
            return Ok((entry, ContextCacheUsage::hit(&self.cache_config)));
        }

        if self.cache_config.stale_while_revalidate_active() {
            let now = Utc::now();
            if let Some(entry) = self.cache.get_any(&key).await?
                && entry.is_stale_servable(now)
            {
                let refresh_started = self.try_start_refresh(&key).await;
                self.metrics.record_stale_served();
                if refresh_started {
                    self.metrics.record_refresh_started();
                }
                self.log_cache_event(
                    "context_cache_stale_served",
                    &key,
                    true,
                    refresh_started,
                    false,
                );
                return Ok((
                    entry,
                    ContextCacheUsage::stale_hit(&self.cache_config, refresh_started),
                ));
            }
        }

        self.compute_with_stampede(key, compute, false).await
    }

    /// Recomputes and stores a cache entry, coalescing with stampede protection.
    pub async fn refresh_entry<F, Fut>(&self, key: ContextCacheKey, compute: F) -> MemcoreResult<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = MemcoreResult<CachedContextEntry>>,
    {
        let _release = RefreshKeyGuard {
            set: self.refresh_inflight.clone(),
            key: key.clone(),
        };
        let result = match time::timeout(
            self.background_refresh_timeout,
            self.compute_with_stampede(key.clone(), compute, true),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(MemcoreError::Timeout(
                "background context refresh exceeded its deadline".to_string(),
            )),
        };
        match result {
            Ok(_) => {
                self.metrics.record_refresh_succeeded();
                self.log_cache_event("context_cache_refresh_succeeded", &key, false, false, false);
                Ok(())
            }
            Err(error) => {
                self.metrics.record_refresh_failed();
                tracing::warn!(
                    event = "context_cache_refresh_failed",
                    org_id = %key.org_id,
                    user_id = %key.user_id,
                    error_code = %error.code(),
                    "context cache background refresh failed"
                );
                Err(error)
            }
        }
    }

    async fn compute_with_stampede<F, Fut>(
        &self,
        key: ContextCacheKey,
        compute: F,
        is_refresh: bool,
    ) -> MemcoreResult<(CachedContextEntry, ContextCacheUsage)>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = MemcoreResult<CachedContextEntry>>,
    {
        if !self.stampede_config.enabled {
            if !is_refresh {
                self.metrics.record_miss();
            }
            let observed = self.current_generation(&key).await;
            let compute_result = compute().await;
            return match compute_result {
                Ok(entry) => {
                    let stored = self.set_if_current(&key, &entry, observed).await?;
                    self.log_store_outcome(stored, &key, false);
                    Ok((
                        entry,
                        if is_refresh {
                            ContextCacheUsage::hit(&self.cache_config)
                        } else {
                            ContextCacheUsage::miss(&self.cache_config)
                        },
                    ))
                }
                Err(error) => {
                    self.metrics.record_compute_error();
                    tracing::warn!(
                        event = "context_cache_compute_failed",
                        org_id = %key.org_id,
                        user_id = %key.user_id,
                        error_code = %error.code(),
                        is_refresh,
                        "context cache computation failed"
                    );
                    Err(error)
                }
            };
        }

        let lock_arc = self.lock_for_key(&key).await;
        let waited_for_inflight = lock_arc.try_lock().is_err();
        if waited_for_inflight {
            self.metrics.record_stampede_wait();
        }
        let _guard = if waited_for_inflight {
            match time::timeout(self.stampede_config.lock_timeout, lock_arc.lock()).await {
                Ok(guard) => guard,
                Err(_) => {
                    self.metrics.record_stampede_timeout();
                    tracing::warn!(
                        event = "context_cache_stampede_timeout",
                        org_id = %key.org_id,
                        user_id = %key.user_id,
                        "timed out waiting for context cache computation"
                    );
                    return Err(MemcoreError::Timeout(
                        "timed out waiting for context cache computation".to_string(),
                    ));
                }
            }
        } else {
            lock_arc.lock().await
        };

        if let Some(entry) = self.cache.get(&key).await? {
            self.cleanup_inflight_lock(&key, &lock_arc).await;
            self.metrics.record_hit();
            self.log_cache_event("context_cache_hit", &key, false, false, waited_for_inflight);
            return Ok((
                entry,
                ContextCacheUsage::hit_with_wait(&self.cache_config, waited_for_inflight),
            ));
        }

        if !is_refresh {
            self.metrics.record_miss();
        }

        let observed = self.current_generation(&key).await;
        let compute_result = compute().await;
        match compute_result {
            Ok(entry) => {
                let stored = self.set_if_current(&key, &entry, observed).await?;
                self.cleanup_inflight_lock(&key, &lock_arc).await;
                self.log_store_outcome(stored, &key, waited_for_inflight);
                Ok((
                    entry,
                    if is_refresh {
                        ContextCacheUsage::hit(&self.cache_config)
                    } else {
                        ContextCacheUsage::miss(&self.cache_config)
                    },
                ))
            }
            Err(error) => {
                self.cleanup_inflight_lock(&key, &lock_arc).await;
                self.metrics.record_compute_error();
                tracing::warn!(
                    event = "context_cache_compute_failed",
                    org_id = %key.org_id,
                    user_id = %key.user_id,
                    error_code = %error.code(),
                    is_refresh,
                    waited_for_inflight,
                    "context cache computation failed"
                );
                Err(error)
            }
        }
    }

    fn log_cache_event(
        &self,
        event: &str,
        key: &ContextCacheKey,
        served_stale: bool,
        refresh_started: bool,
        waited_for_inflight: bool,
    ) {
        tracing::debug!(
            event,
            cache_enabled = self.cache_config.enabled,
            cache_hit = event.contains("hit"),
            served_stale,
            refresh_started,
            waited_for_inflight,
            org_id = %key.org_id,
            user_id = %key.user_id,
            stampede_protection_enabled = self.cache_config.stampede_protection_active(),
            stale_while_revalidate_enabled = self.cache_config.stale_while_revalidate_active(),
            "context cache event"
        );
    }

    async fn lock_for_key(&self, key: &ContextCacheKey) -> Arc<Mutex<()>> {
        let mut map = self.inflight.lock().await;
        map.entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn cleanup_inflight_lock(&self, key: &ContextCacheKey, lock_arc: &Arc<Mutex<()>>) {
        let mut map = self.inflight.lock().await;
        if let Some(existing) = map.get(key)
            && Arc::ptr_eq(existing, lock_arc)
        {
            map.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::{Duration as ChronoDuration, Utc};
    use tokio::sync::Barrier;

    use super::*;
    use crate::TenantContext;
    use crate::context::budget::ContextBudgetUsage;
    use crate::context::compression_options::ContextCompressionUsage;
    use crate::context::format_options::ContextFormat;
    use crate::context::types::BuildContextInput;
    use crate::context::{
        InMemoryContextCache, InMemoryContextCacheMetrics, build_context_cache_key,
    };

    fn cache_config(stampede_enabled: bool) -> ContextCacheConfig {
        ContextCacheConfig {
            enabled: true,
            ttl_seconds: 300,
            max_entries: 100,
            stampede_protection_enabled: stampede_enabled,
            stampede_lock_timeout_seconds: 30,
            stale_while_revalidate_enabled: false,
            stale_ttl_seconds: 120,
            metrics_enabled: true,
        }
    }

    fn swr_cache_config() -> ContextCacheConfig {
        ContextCacheConfig {
            stale_while_revalidate_enabled: true,
            stale_ttl_seconds: 120,
            ..cache_config(true)
        }
    }

    fn stale_entry(context: &str) -> CachedContextEntry {
        let now = Utc::now();
        let expires_at = now - ChronoDuration::seconds(10);
        CachedContextEntry {
            context: context.to_string(),
            memories: Vec::new(),
            created_at: now - ChronoDuration::seconds(310),
            expires_at,
            stale_until: Some(expires_at + ChronoDuration::seconds(120)),
            budget: ContextBudgetUsage {
                max_tokens: 2000,
                reserved_tokens: 300,
                available_tokens: 1700,
                used_tokens: 10,
                included_memories: 0,
                skipped_memories: 0,
            },
            compression: ContextCompressionUsage::disabled(),
        }
    }

    fn sample_input(org_id: &str, user_id: &str, query: &str) -> BuildContextInput {
        BuildContextInput {
            tenant: TenantContext::new(org_id, user_id).expect("tenant"),
            query: query.to_string(),
            ..Default::default()
        }
    }

    fn sample_entry(context: &str) -> CachedContextEntry {
        let now = Utc::now();
        CachedContextEntry {
            context: context.to_string(),
            memories: Vec::new(),
            created_at: now,
            expires_at: now + ChronoDuration::seconds(300),
            stale_until: None,
            budget: ContextBudgetUsage {
                max_tokens: 2000,
                reserved_tokens: 300,
                available_tokens: 1700,
                used_tokens: 10,
                included_memories: 0,
                skipped_memories: 0,
            },
            compression: ContextCompressionUsage::disabled(),
        }
    }

    fn coordinator(stampede_enabled: bool) -> ContextCacheCoordinator {
        let config = cache_config(stampede_enabled);
        ContextCacheCoordinator::new(
            Arc::new(InMemoryContextCache::new(100)),
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        )
    }

    fn coordinator_with_metrics(
        config: ContextCacheConfig,
        metrics: Arc<InMemoryContextCacheMetrics>,
    ) -> ContextCacheCoordinator {
        ContextCacheCoordinator::new(Arc::new(InMemoryContextCache::new(100)), config, metrics)
    }

    #[tokio::test]
    async fn cache_hit_returns_immediately_without_compute() {
        let cache = Arc::new(InMemoryContextCache::new(100));
        let config = cache_config(true);
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "hit"));
        cache
            .set(key.clone(), sample_entry("cached"))
            .await
            .expect("seed cache");
        let coordinator = ContextCacheCoordinator::new(
            cache,
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        );

        let compute_count = Arc::new(AtomicUsize::new(0));
        let count = compute_count.clone();
        let (entry, usage) = coordinator
            .get_or_compute(key, || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("computed"))
            })
            .await
            .expect("cache hit");

        assert_eq!(entry.context, "cached");
        assert!(usage.hit);
        assert!(!usage.waited_for_inflight);
        assert_eq!(compute_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cache_miss_computes_once_and_stores_entry() {
        let coordinator = coordinator(true);
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "miss"));
        let compute_count = Arc::new(AtomicUsize::new(0));
        let count = compute_count.clone();

        let (entry, usage) = coordinator
            .get_or_compute(key.clone(), || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("computed once"))
            })
            .await
            .expect("compute");

        assert_eq!(entry.context, "computed once");
        assert!(!usage.hit);
        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
        let (entry, usage) = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("ignored")) })
            .await
            .expect("second hit");
        assert_eq!(entry.context, "computed once");
        assert!(usage.hit);
    }

    #[tokio::test]
    async fn concurrent_identical_requests_compute_only_once() {
        let coordinator = Arc::new(coordinator(true));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "concurrent"));
        let compute_count = Arc::new(AtomicUsize::new(0));
        let compute_started = Arc::new(tokio::sync::Notify::new());

        let coordinator_first = coordinator.clone();
        let key_first = key.clone();
        let compute_count_first = compute_count.clone();
        let compute_started_first = compute_started.clone();
        let first = tokio::spawn(async move {
            coordinator_first
                .get_or_compute(key_first, || async move {
                    compute_count_first.fetch_add(1, Ordering::SeqCst);
                    compute_started_first.notify_waiters();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(sample_entry("shared"))
                })
                .await
        });

        compute_started.notified().await;
        let second = coordinator
            .get_or_compute(key, || async {
                compute_count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("should not run"))
            })
            .await
            .expect("second request");

        let first = first.await.expect("join first").expect("first request");

        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
        assert_eq!(first.0.context, "shared");
        assert_eq!(second.0.context, "shared");
        assert!(second.1.hit);
        assert!(second.1.waited_for_inflight);
    }

    #[tokio::test]
    async fn waiting_request_reports_waited_for_inflight_on_hit() {
        let coordinator = Arc::new(coordinator(true));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "wait hit"));
        let compute_started = Arc::new(tokio::sync::Notify::new());

        let coordinator_first = coordinator.clone();
        let key_first = key.clone();
        let compute_started_first = compute_started.clone();
        let first = tokio::spawn(async move {
            coordinator_first
                .get_or_compute(key_first, || async move {
                    compute_started_first.notify_waiters();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(sample_entry("from first"))
                })
                .await
        });

        compute_started.notified().await;
        let (_, usage) = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("should not run")) })
            .await
            .expect("waiter");
        let _ = first.await.expect("first join");

        assert!(usage.hit);
        assert!(usage.waited_for_inflight);
    }

    #[tokio::test]
    async fn different_query_keys_compute_independently() {
        let coordinator = coordinator(true);
        let key_a = build_context_cache_key(&sample_input("org_a", "user_a", "alpha"));
        let key_b = build_context_cache_key(&sample_input("org_a", "user_a", "beta"));
        let compute_count = Arc::new(AtomicUsize::new(0));

        for key in [key_a, key_b] {
            let count = compute_count.clone();
            coordinator
                .get_or_compute(key, || async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(sample_entry("x"))
                })
                .await
                .expect("compute");
        }

        assert_eq!(compute_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn different_users_and_orgs_compute_independently() {
        let coordinator = coordinator(true);
        let keys = [
            build_context_cache_key(&sample_input("org_a", "user_a", "same")),
            build_context_cache_key(&sample_input("org_a", "user_b", "same")),
            build_context_cache_key(&sample_input("org_b", "user_a", "same")),
        ];
        let compute_count = Arc::new(AtomicUsize::new(0));

        for key in keys {
            let count = compute_count.clone();
            coordinator
                .get_or_compute(key, || async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(sample_entry("x"))
                })
                .await
                .expect("compute");
        }

        assert_eq!(compute_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn different_options_compute_independently() {
        let coordinator = coordinator(true);
        let mut markdown = sample_input("org_a", "user_a", "same");
        markdown.format_options.format = ContextFormat::Markdown;
        let keys = [
            build_context_cache_key(&sample_input("org_a", "user_a", "same")),
            build_context_cache_key(&markdown),
        ];
        let compute_count = Arc::new(AtomicUsize::new(0));

        for key in keys {
            let count = compute_count.clone();
            coordinator
                .get_or_compute(key, || async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(sample_entry("x"))
                })
                .await
                .expect("compute");
        }

        assert_eq!(compute_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn compute_error_is_not_cached_and_later_request_can_recompute() {
        let coordinator = coordinator(true);
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "error"));
        let compute_count = Arc::new(AtomicUsize::new(0));

        let count = compute_count.clone();
        let error = coordinator
            .get_or_compute(key.clone(), || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(MemcoreError::Internal("compute failed".to_string()))
            })
            .await
            .expect_err("compute error");
        assert_eq!(error.code(), "internal");

        let count = compute_count.clone();
        let (entry, _) = coordinator
            .get_or_compute(key, || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("recovered"))
            })
            .await
            .expect("recompute");

        assert_eq!(entry.context, "recovered");
        assert_eq!(compute_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn lock_timeout_returns_timeout_error() {
        let config = ContextCacheConfig {
            enabled: true,
            ttl_seconds: 300,
            max_entries: 100,
            stampede_protection_enabled: true,
            stampede_lock_timeout_seconds: 1,
            stale_while_revalidate_enabled: false,
            stale_ttl_seconds: 120,
            metrics_enabled: true,
        };
        let coordinator = Arc::new(ContextCacheCoordinator::new(
            Arc::new(InMemoryContextCache::new(100)),
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        ));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "timeout"));
        let hold = Arc::new(Barrier::new(2));

        let coordinator_bg = coordinator.clone();
        let key_bg = key.clone();
        let hold_bg = hold.clone();
        let _background = tokio::spawn(async move {
            let _ = coordinator_bg
                .get_or_compute(key_bg, || async {
                    hold_bg.wait().await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(sample_entry("slow"))
                })
                .await;
        });

        hold.wait().await;
        let error = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("never")) })
            .await
            .expect_err("timeout");

        assert_eq!(error.code(), "timeout");
        assert!(
            error
                .to_string()
                .contains("timed out waiting for context cache computation")
        );
    }

    #[tokio::test]
    async fn lock_map_cleaned_up_after_success() {
        let coordinator = coordinator(true);
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "cleanup"));
        assert_eq!(coordinator.inflight_lock_count(), 0);

        coordinator
            .get_or_compute(key, || async { Ok(sample_entry("done")) })
            .await
            .expect("compute");

        assert_eq!(coordinator.inflight_lock_count(), 0);
    }

    #[tokio::test]
    async fn stampede_protection_disabled_allows_independent_computes() {
        let coordinator = Arc::new(coordinator(false));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "no stampede"));
        let compute_count = Arc::new(AtomicUsize::new(0));
        let compute_started = Arc::new(tokio::sync::Notify::new());

        let coordinator_first = coordinator.clone();
        let key_first = key.clone();
        let compute_count_first = compute_count.clone();
        let compute_started_first = compute_started.clone();
        let first = tokio::spawn(async move {
            coordinator_first
                .get_or_compute(key_first, || async move {
                    compute_count_first.fetch_add(1, Ordering::SeqCst);
                    compute_started_first.notify_waiters();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(sample_entry("parallel"))
                })
                .await
        });

        compute_started.notified().await;
        let _ = coordinator
            .get_or_compute(key, || async {
                compute_count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("parallel two"))
            })
            .await
            .expect("second compute");

        let _ = first.await.expect("join first");
        assert_eq!(compute_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn many_concurrent_identical_requests_compute_only_once() {
        let coordinator = Arc::new(coordinator(true));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "many"));
        let compute_count = Arc::new(AtomicUsize::new(0));
        let compute_started = Arc::new(tokio::sync::Notify::new());
        let sleeping = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let coordinator = coordinator.clone();
            let key = key.clone();
            let compute_count = compute_count.clone();
            let compute_started = compute_started.clone();
            let sleeping = sleeping.clone();
            handles.push(tokio::spawn(async move {
                coordinator
                    .get_or_compute(key, || async move {
                        compute_count.fetch_add(1, Ordering::SeqCst);
                        if !sleeping.swap(true, Ordering::SeqCst) {
                            compute_started.notify_waiters();
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Ok(sample_entry("shared many"))
                    })
                    .await
            }));
        }

        compute_started.notified().await;
        for handle in handles {
            handle.await.expect("join").expect("compute");
        }

        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stale_entry_returned_when_swr_enabled() {
        let config = swr_cache_config();
        let cache = Arc::new(InMemoryContextCache::new(100));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "stale serve"));
        cache
            .set(key.clone(), stale_entry("stale context"))
            .await
            .expect("seed stale");
        let coordinator = ContextCacheCoordinator::new(
            cache,
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        );
        let compute_count = Arc::new(AtomicUsize::new(0));
        let count = compute_count.clone();

        let (entry, usage) = coordinator
            .get_or_compute(key, || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("should not run"))
            })
            .await
            .expect("stale hit");

        assert_eq!(entry.context, "stale context");
        assert!(usage.hit);
        assert!(usage.served_stale);
        assert!(usage.refresh_started);
        assert_eq!(compute_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stale_entry_not_returned_when_swr_disabled() {
        let cache = Arc::new(InMemoryContextCache::new(100));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "no swr"));
        cache
            .set(key.clone(), stale_entry("stale context"))
            .await
            .expect("seed stale");
        let coordinator = ContextCacheCoordinator::new(
            cache,
            cache_config(true),
            Arc::new(InMemoryContextCacheMetrics::new()),
        );
        let compute_count = Arc::new(AtomicUsize::new(0));
        let count = compute_count.clone();

        let (entry, usage) = coordinator
            .get_or_compute(key, || async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(sample_entry("fresh compute"))
            })
            .await
            .expect("compute");

        assert_eq!(entry.context, "fresh compute");
        assert!(!usage.hit);
        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn duplicate_stale_requests_start_only_one_refresh() {
        let config = swr_cache_config();
        let cache = Arc::new(InMemoryContextCache::new(100));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "dup refresh"));
        cache
            .set(key.clone(), stale_entry("stale"))
            .await
            .expect("seed");

        let coordinator = Arc::new(ContextCacheCoordinator::new(
            cache,
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        ));
        let first = coordinator
            .get_or_compute(key.clone(), || async { Ok(sample_entry("nope")) })
            .await
            .expect("first stale");
        let second = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("nope")) })
            .await
            .expect("second stale");

        assert!(first.1.refresh_started);
        assert!(!second.1.refresh_started);
        assert_eq!(coordinator.refresh_inflight_count(), 1);
    }

    #[tokio::test]
    async fn refresh_success_updates_cache() {
        let config = swr_cache_config();
        let cache = Arc::new(InMemoryContextCache::new(100));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "refresh ok"));
        cache
            .set(key.clone(), stale_entry("old"))
            .await
            .expect("seed");

        let coordinator = ContextCacheCoordinator::new(
            cache.clone(),
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        );
        coordinator
            .refresh_entry(key.clone(), || async { Ok(sample_entry("refreshed")) })
            .await
            .expect("refresh");

        let fresh = cache.get(&key).await.expect("get").expect("hit");
        assert_eq!(fresh.context, "refreshed");
        assert!(fresh.is_fresh(Utc::now()));
    }

    #[tokio::test]
    async fn refresh_failure_keeps_stale_entry() {
        let config = swr_cache_config();
        let cache = Arc::new(InMemoryContextCache::new(100));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "refresh fail"));
        cache
            .set(key.clone(), stale_entry("keep me"))
            .await
            .expect("seed");

        let coordinator = ContextCacheCoordinator::new(
            cache.clone(),
            config,
            Arc::new(InMemoryContextCacheMetrics::new()),
        );
        let error = coordinator
            .refresh_entry(key.clone(), || async {
                Err(MemcoreError::Internal("refresh failed".to_string()))
            })
            .await
            .expect_err("refresh error");
        assert_eq!(error.code(), "internal");

        let stale = cache.get_any(&key).await.expect("get").expect("stale");
        assert_eq!(stale.context, "keep me");
    }

    #[tokio::test]
    async fn cache_hit_records_hit_metric() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let cache = Arc::new(InMemoryContextCache::new(100));
        let config = cache_config(true);
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics hit"));
        cache
            .set(key.clone(), sample_entry("cached"))
            .await
            .expect("seed");
        let coordinator = ContextCacheCoordinator::new(cache, config, metrics.clone());

        let _ = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("nope")) })
            .await
            .expect("hit");

        assert_eq!(metrics.snapshot().hits, 1);
    }

    #[tokio::test]
    async fn cache_miss_and_set_record_metrics() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = coordinator_with_metrics(cache_config(true), metrics.clone());
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics miss"));

        let _ = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("computed")) })
            .await
            .expect("compute");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.misses, 1);
        assert_eq!(snapshot.sets, 1);
    }

    #[tokio::test]
    async fn compute_error_records_compute_error_metric() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = coordinator_with_metrics(cache_config(true), metrics.clone());
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics error"));

        let _ = coordinator
            .get_or_compute(key, || async {
                Err(MemcoreError::Internal("compute failed".to_string()))
            })
            .await
            .expect_err("error");

        assert_eq!(metrics.snapshot().compute_errors, 1);
    }

    #[tokio::test]
    async fn stale_served_records_stale_and_refresh_started_metrics() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let cache = Arc::new(InMemoryContextCache::new(100));
        let config = swr_cache_config();
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics stale"));
        cache
            .set(key.clone(), stale_entry("stale"))
            .await
            .expect("seed");
        let coordinator = ContextCacheCoordinator::new(cache, config, metrics.clone());

        let _ = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("nope")) })
            .await
            .expect("stale");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.stale_served, 1);
        assert_eq!(snapshot.refresh_started, 1);
    }

    #[tokio::test]
    async fn refresh_success_records_refresh_succeeded_metric() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let cache = Arc::new(InMemoryContextCache::new(100));
        let config = swr_cache_config();
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics refresh"));
        cache
            .set(key.clone(), stale_entry("old"))
            .await
            .expect("seed");
        let coordinator = ContextCacheCoordinator::new(cache, config, metrics.clone());

        coordinator
            .refresh_entry(key, || async { Ok(sample_entry("new")) })
            .await
            .expect("refresh");

        assert_eq!(metrics.snapshot().refresh_succeeded, 1);
    }

    #[tokio::test]
    async fn refresh_failure_records_refresh_failed_metric() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = coordinator_with_metrics(swr_cache_config(), metrics.clone());
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics refresh fail"));

        let _ = coordinator
            .refresh_entry(key, || async {
                Err(MemcoreError::Internal("refresh failed".to_string()))
            })
            .await
            .expect_err("error");

        assert_eq!(metrics.snapshot().refresh_failed, 1);
    }

    #[tokio::test]
    async fn stampede_wait_records_stampede_wait_metric() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = Arc::new(coordinator_with_metrics(
            cache_config(true),
            metrics.clone(),
        ));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics wait"));
        let compute_started = Arc::new(tokio::sync::Notify::new());

        let coordinator_first = coordinator.clone();
        let key_first = key.clone();
        let compute_started_first = compute_started.clone();
        let _first = tokio::spawn(async move {
            coordinator_first
                .get_or_compute(key_first, || async move {
                    compute_started_first.notify_waiters();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(sample_entry("shared"))
                })
                .await
        });

        compute_started.notified().await;
        let _ = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("nope")) })
            .await
            .expect("waiter");

        assert!(metrics.snapshot().stampede_waits >= 1);
    }

    #[tokio::test]
    async fn stampede_timeout_records_stampede_timeout_metric() {
        let config = ContextCacheConfig {
            enabled: true,
            ttl_seconds: 300,
            max_entries: 100,
            stampede_protection_enabled: true,
            stampede_lock_timeout_seconds: 1,
            stale_while_revalidate_enabled: false,
            stale_ttl_seconds: 120,
            metrics_enabled: true,
        };
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = Arc::new(ContextCacheCoordinator::new(
            Arc::new(InMemoryContextCache::new(100)),
            config,
            metrics.clone(),
        ));
        let key = build_context_cache_key(&sample_input("org_a", "user_a", "metrics timeout"));
        let hold = Arc::new(Barrier::new(2));

        let coordinator_bg = coordinator.clone();
        let key_bg = key.clone();
        let hold_bg = hold.clone();
        let _background = tokio::spawn(async move {
            let _ = coordinator_bg
                .get_or_compute(key_bg, || async {
                    hold_bg.wait().await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(sample_entry("slow"))
                })
                .await;
        });

        hold.wait().await;
        let _ = coordinator
            .get_or_compute(key, || async { Ok(sample_entry("never")) })
            .await
            .expect_err("timeout");

        assert_eq!(metrics.snapshot().stampede_timeouts, 1);
    }

    // ---- Bounded background refreshes ------------------------------------------------

    async fn seeded_stale(keys: &[&str]) -> (Arc<InMemoryContextCache>, Vec<ContextCacheKey>) {
        let cache = Arc::new(InMemoryContextCache::new(100));
        let mut out = Vec::new();
        for k in keys {
            let key = build_context_cache_key(&sample_input("org_a", "user_a", k));
            cache
                .set(key.clone(), stale_entry("stale"))
                .await
                .expect("seed");
            out.push(key);
        }
        (cache, out)
    }

    #[tokio::test]
    async fn background_refreshes_are_capped_and_excess_stale_hits_are_skipped() {
        let names: Vec<String> = (0..10).map(|i| format!("key {i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (cache, keys) = seeded_stale(&refs).await;
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = ContextCacheCoordinator::new(cache, swr_cache_config(), metrics.clone())
            .with_refresh_bounds(3, Duration::from_secs(5));
        let mut started = 0;
        for key in &keys {
            let (entry, usage) = coordinator
                .get_or_compute(key.clone(), || async { Ok(sample_entry("never")) })
                .await
                .expect("stale hit");
            // The foreground always gets the stale entry at once, refresh or not.
            assert_eq!(entry.context, "stale");
            assert!(usage.served_stale);
            started += usize::from(usage.refresh_started);
        }
        assert_eq!(started, 3, "only the budget's worth of refreshes start");
        assert_eq!(coordinator.refresh_inflight_count(), 3);
        assert_eq!(metrics.snapshot().refresh_skipped, 7);
        assert_eq!(metrics.snapshot().refresh_started, 3);
    }

    #[tokio::test]
    async fn refresh_slots_are_released_on_success_error_timeout_cancel_and_release() {
        let (cache, keys) = seeded_stale(&["ok", "err", "slow", "cancel", "never run"]).await;
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = Arc::new(
            ContextCacheCoordinator::new(cache, swr_cache_config(), metrics.clone())
                .with_refresh_bounds(5, Duration::from_millis(100)),
        );
        for key in &keys {
            assert!(coordinator.try_start_refresh(key).await);
        }
        assert_eq!(coordinator.refresh_inflight_count(), 5);

        coordinator
            .refresh_entry(keys[0].clone(), || async { Ok(sample_entry("fresh")) })
            .await
            .expect("success");
        coordinator
            .refresh_entry(keys[1].clone(), || async {
                Err(MemcoreError::ProviderError("injected".to_string()))
            })
            .await
            .expect_err("provider error");
        let slow = coordinator
            .refresh_entry(keys[2].clone(), || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(sample_entry("too late"))
            })
            .await
            .expect_err("deadline");
        assert!(matches!(slow, MemcoreError::Timeout(_)), "{slow:?}");
        // Cancellation: the refresh future is dropped part-way.
        let c = coordinator.clone();
        let k = keys[3].clone();
        let handle = tokio::spawn(async move {
            c.refresh_entry(k, || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(sample_entry("cancelled"))
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.abort();
        let _ = handle.await;
        // A reserved slot whose refresh never runs (e.g. shutdown).
        coordinator.finish_refresh(&keys[4]).await;

        assert_eq!(
            coordinator.refresh_inflight_count(),
            0,
            "every slot released"
        );
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.refresh_succeeded, 1);
        assert_eq!(snapshot.refresh_failed, 2, "error + deadline");
        // Released keys can refresh again.
        assert!(coordinator.try_start_refresh(&keys[3]).await);
    }

    #[tokio::test]
    async fn concurrent_stale_hits_never_exceed_the_refresh_budget() {
        let names: Vec<String> = (0..40).map(|i| format!("burst {i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (cache, keys) = seeded_stale(&refs).await;
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let coordinator = Arc::new(
            ContextCacheCoordinator::new(cache, swr_cache_config(), metrics.clone())
                .with_refresh_bounds(4, Duration::from_secs(5)),
        );
        let mut handles = Vec::new();
        for key in keys.iter().cycle().take(200).cloned() {
            let c = coordinator.clone();
            handles.push(tokio::spawn(async move {
                c.get_or_compute(key, || async { Ok(sample_entry("never")) })
                    .await
                    .map(|(_, usage)| usage.refresh_started)
            }));
        }
        let mut started = 0;
        for h in handles {
            started += usize::from(h.await.unwrap().unwrap());
        }
        assert_eq!(started, 4);
        assert!(coordinator.refresh_inflight_count() <= 4);
        // 200 hits over 40 keys: 4 start; the other 16 hits on those 4 keys are coalesced (not
        // skipped); the 180 hits on the other 36 keys are skipped at the budget.
        assert_eq!(metrics.snapshot().refresh_skipped, 180);
    }
}
