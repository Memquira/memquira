//! Query decomposition under the frozen `decompose-v1` contract.
//!
//! A [`DecompositionProvider`] turns one user question into extra sub-queries. A
//! [`DecompositionService`] runs it at most once per logical question: requests with the same
//! fingerprint (organisation, normalised question, reference time, contract, model, configuration;
//! never the speaker or any retrieved memory) share one result through a bounded cache and
//! single-flight. A failed operation leaves a failure sentinel for the same TTL, so no request with
//! that fingerprint calls the model again while it lives. The original query always stays first;
//! sub-queries only widen retrieval and are never decomposed again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::time::Instant;
use unicode_normalization::UnicodeNormalization;

use crate::ports::{LlmProvider, QueryDecompositionInput};

/// The frozen contract's prompt version.
pub const PROMPT_VERSION: &str = "decompose-v1";

/// The frozen system prompt (contract §2).
pub const SYSTEM_PROMPT: &str = "You split a user's question about their own past conversations into separate search queries, only when the question needs several different facts. Return JSON only: {\"sub_queries\": [...]}. Rules: return an empty list when one search is enough; at most 3 sub-queries; each is a short, standalone question; keep names, dates and time words exactly as written; never answer the question; never add facts that are not in the question.";

/// The frozen user message template (contract §2); `{question}` is the original question.
pub const USER_TEMPLATE: &str = "Question: {question}";

/// Extra queries per candidate request, clauses and model sub-queries together.
pub const MAX_DECOMPOSITION_SUB_QUERIES: usize = 3;
/// Questions longer than this (in characters) are never sent.
pub const MAX_INPUT_CHARS: usize = 1_000;
/// Accepted sub-query length in characters, after trimming.
pub const MIN_SUB_QUERY_CHARS: usize = 3;
pub const MAX_SUB_QUERY_CHARS: usize = 200;
/// Sampling settings.
pub const MAX_OUTPUT_TOKENS: u32 = 150;
pub const TEMPERATURE: f32 = 0.0;
pub const TOP_P: f32 = 1.0;
/// Cache bound and TTL, for results and failure sentinels alike.
pub const CACHE_CAPACITY: usize = 1_024;
pub const CACHE_TTL: Duration = Duration::from_secs(600);
/// How much longer than the leader's timeout a follower waits before falling back.
pub const FOLLOWER_GRACE: Duration = Duration::from_secs(1);

const FINGERPRINT_VERSION: &str = "decompose-fp-v1";

/// SHA-256 of the exact system prompt, a line feed and the user template (contract item 2).
pub fn contract_hash() -> String {
    hex::encode(Sha256::digest(format!("{SYSTEM_PROMPT}\n{USER_TEMPLATE}")))
}

/// SHA-256 of the canonical decomposition configuration (contract §3).
pub fn config_hash(timeout: Duration) -> String {
    let config = serde_json::json!({
        "max_input_chars": MAX_INPUT_CHARS,
        "max_sub_queries": MAX_DECOMPOSITION_SUB_QUERIES,
        "max_tokens": MAX_OUTPUT_TOKENS,
        "temperature": TEMPERATURE,
        "timeout_ms": timeout.as_millis() as u64,
        "top_p": TOP_P,
    });
    hex::encode(Sha256::digest(config.to_string()))
}

/// Why a decomposition operation produced no usable sub-queries. Only this class is kept (in the
/// failure sentinel and the audit); raw provider or model text never is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    ProviderError,
    Timeout,
    /// The provider answered but the output cannot be used (e.g. truncated at the token limit).
    UnusableOutput,
    InvalidJson,
    Validation,
}

/// What produced sub-queries for this question: the configured provider and model, which enter the
/// fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionIdentity {
    pub provider: String,
    pub model: String,
}

#[async_trait]
pub trait DecompositionProvider: Send + Sync {
    fn name(&self) -> &'static str;

    fn identity(&self) -> DecompositionIdentity;

    /// One decomposition operation: validated sub-queries in the provider's order (an empty list
    /// means "do not decompose"), or the class of the failure.
    async fn decompose(&self, query: &str) -> Result<Vec<String>, FailureClass>;
}

/// Plumbing-only provider: never decomposes and never calls a model.
pub struct NoopDecompositionProvider;

#[async_trait]
impl DecompositionProvider for NoopDecompositionProvider {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn identity(&self) -> DecompositionIdentity {
        DecompositionIdentity {
            provider: "noop".to_string(),
            model: "none".to_string(),
        }
    }

    async fn decompose(&self, _query: &str) -> Result<Vec<String>, FailureClass> {
        Ok(Vec::new())
    }
}

/// The contract's model provider: the frozen prompt through the configured LLM provider.
pub struct LlmDecompositionProvider {
    llm: Arc<dyn LlmProvider>,
    identity: DecompositionIdentity,
}

impl LlmDecompositionProvider {
    pub fn new(llm: Arc<dyn LlmProvider>, identity: DecompositionIdentity) -> Self {
        Self { llm, identity }
    }
}

#[async_trait]
impl DecompositionProvider for LlmDecompositionProvider {
    fn name(&self) -> &'static str {
        "llm"
    }

    fn identity(&self) -> DecompositionIdentity {
        self.identity.clone()
    }

    async fn decompose(&self, query: &str) -> Result<Vec<String>, FailureClass> {
        let input = QueryDecompositionInput {
            system: SYSTEM_PROMPT.to_string(),
            user: USER_TEMPLATE.replace("{question}", query),
            max_output_tokens: MAX_OUTPUT_TOKENS,
            temperature: TEMPERATURE,
            top_p: TOP_P,
        };
        match self.llm.decompose_query(input).await {
            Ok(text) => parse_decomposition(&text),
            Err(memcore_common::MemcoreError::ProviderOutputTruncated { .. }) => {
                Err(FailureClass::UnusableOutput)
            }
            Err(_) => Err(FailureClass::ProviderError),
        }
    }
}

/// The contract parser: one surrounding code fence stripped, then exactly one JSON object
/// whose only key is `sub_queries`, an array of 0-3 strings, each 3-200 characters after trimming
/// and without line breaks. Returns the trimmed strings; any violation rejects the whole response.
pub fn parse_decomposition(text: &str) -> Result<Vec<String>, FailureClass> {
    let mut body = text.trim();
    if body.starts_with("```") && body.len() >= 6 && body.ends_with("```") {
        body = body[3..body.len() - 3]
            .strip_prefix("json")
            .unwrap_or(&body[3..body.len() - 3])
            .trim();
    }
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| FailureClass::InvalidJson)?;
    let object = value.as_object().ok_or(FailureClass::Validation)?;
    if object.len() != 1 {
        return Err(FailureClass::Validation);
    }
    let items = object
        .get("sub_queries")
        .and_then(|items| items.as_array())
        .ok_or(FailureClass::Validation)?;
    if items.len() > MAX_DECOMPOSITION_SUB_QUERIES {
        return Err(FailureClass::Validation);
    }
    items
        .iter()
        .map(|item| {
            let text = item.as_str().ok_or(FailureClass::Validation)?.trim();
            let chars = text.chars().count();
            if !(MIN_SUB_QUERY_CHARS..=MAX_SUB_QUERY_CHARS).contains(&chars)
                || text.contains(['\n', '\r'])
            {
                return Err(FailureClass::Validation);
            }
            Ok(text.to_string())
        })
        .collect()
}

/// The question as fingerprinted: Unicode NFC, trimmed, whitespace runs collapsed; case kept.
pub fn normalise_query(query: &str) -> String {
    query
        .nfc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The contract §3 fingerprint. It never includes the speaker / user or any retrieved memory.
pub fn fingerprint(
    org_id: &str,
    query: &str,
    reference_time: Option<DateTime<FixedOffset>>,
    identity: &DecompositionIdentity,
    timeout: Duration,
) -> String {
    let reference_time = reference_time.map_or_else(
        || "none".to_string(),
        |time| {
            time.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        },
    );
    // serde_json's map is ordered by key, so this is the canonical (sorted, compact) form.
    let canonical = serde_json::json!({
        "config": config_hash(timeout),
        "contract": contract_hash(),
        "model": identity.model,
        "org": org_id,
        "provider": identity.provider,
        "query": normalise_query(query),
        "reference_time": reference_time,
        "v": FINGERPRINT_VERSION,
    });
    hex::encode(Sha256::digest(canonical.to_string()))
}

/// How this request obtained its sub-queries (contract §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecompositionState {
    /// This request ran the operation and it succeeded.
    Called,
    /// This request ran the operation and it failed; original query only.
    FailedFallback,
    /// A cached result within its TTL.
    CacheHit,
    /// A failure sentinel within its TTL: no operation, original query only.
    FailureCacheHit,
    /// Joined a concurrent leader's operation and received its result.
    Follower,
    /// Joined a concurrent leader whose operation failed, was cancelled or outlasted the wait.
    FollowerFallback,
    /// The question exceeds the input limit: never sent.
    InputTooLongFallback,
}

/// Audit record of decomposition for one candidate request. No query or model text beyond the
/// accepted sub-queries, and no raw error text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecompositionAudit {
    pub provider: String,
    pub state: DecompositionState,
    /// Logical decomposition operations this request started: 0 or 1. Transport retries below the
    /// operation (provider router, proxy) are not counted here.
    pub operations: u32,
    /// The first 16 hex characters of the fingerprint.
    pub fingerprint: String,
    /// Sub-queries used by this request, in order.
    pub accepted: Vec<String>,
    /// Returned sub-queries dropped here: the original, a clause, a duplicate or over the cap.
    pub dropped: u32,
    pub failure_class: Option<FailureClass>,
    pub latency_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Success(Vec<String>),
    Failure(FailureClass),
}

struct Entry {
    outcome: Outcome,
    inserted_at: Instant,
    last_read: u64,
    inserted_seq: u64,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<String, Entry>,
    in_flight: HashMap<String, watch::Receiver<Option<Outcome>>>,
    clock: u64,
}

enum Role {
    Cached(Outcome),
    Follower(watch::Receiver<Option<Outcome>>),
    Leader(watch::Sender<Option<Outcome>>),
}

/// Runs a [`DecompositionProvider`] at most once per fingerprint within the TTL.
pub struct DecompositionService {
    provider: Arc<dyn DecompositionProvider>,
    capacity: usize,
    ttl: Duration,
    state: Mutex<CacheState>,
}

/// Clears the in-flight slot if the leader's future is dropped before it finishes; followers then
/// see the channel close and fall back without calling.
struct LeaderSlot<'a> {
    service: &'a DecompositionService,
    key: String,
    done: bool,
}

impl Drop for LeaderSlot<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.service.lock().in_flight.remove(&self.key);
        }
    }
}

impl DecompositionService {
    pub fn new(provider: Arc<dyn DecompositionProvider>) -> Self {
        Self::with_limits(provider, CACHE_CAPACITY, CACHE_TTL)
    }

    pub fn with_limits(
        provider: Arc<dyn DecompositionProvider>,
        capacity: usize,
        ttl: Duration,
    ) -> Self {
        Self {
            provider,
            capacity: capacity.max(1),
            ttl,
            state: Mutex::new(CacheState::default()),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn claim(&self, key: &str) -> Role {
        let mut state = self.lock();
        state.clock += 1;
        let now = Instant::now();
        let clock = state.clock;
        if let Some(entry) = state.entries.get_mut(key) {
            if now.duration_since(entry.inserted_at) < self.ttl {
                entry.last_read = clock;
                return Role::Cached(entry.outcome.clone());
            }
            state.entries.remove(key);
        }
        if let Some(receiver) = state.in_flight.get(key) {
            return Role::Follower(receiver.clone());
        }
        let (sender, receiver) = watch::channel(None);
        state.in_flight.insert(key.to_string(), receiver);
        Role::Leader(sender)
    }

    fn store(&self, key: &str, outcome: Outcome) {
        let mut state = self.lock();
        state.in_flight.remove(key);
        let now = Instant::now();
        let ttl = self.ttl;
        state
            .entries
            .retain(|_, entry| now.duration_since(entry.inserted_at) < ttl);
        while state.entries.len() >= self.capacity {
            let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| (entry.last_read, entry.inserted_seq))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            state.entries.remove(&oldest);
        }
        state.clock += 1;
        let clock = state.clock;
        state.entries.insert(
            key.to_string(),
            Entry {
                outcome,
                inserted_at: now,
                last_read: clock,
                inserted_seq: clock,
            },
        );
    }

    /// Sub-queries for one candidate request. `existing` holds the deterministic clauses,
    /// which come first and share the cap; accepted sub-queries never repeat them or the original.
    pub async fn decompose(
        &self,
        org_id: &str,
        query: &str,
        reference_time: Option<DateTime<FixedOffset>>,
        existing: &[String],
        timeout: Duration,
    ) -> DecompositionAudit {
        let started = std::time::Instant::now();
        let identity = self.provider.identity();
        let key = fingerprint(org_id, query, reference_time, &identity, timeout);
        let mut audit = DecompositionAudit {
            provider: self.provider.name().to_string(),
            state: DecompositionState::InputTooLongFallback,
            operations: 0,
            fingerprint: key[..16].to_string(),
            accepted: Vec::new(),
            dropped: 0,
            failure_class: None,
            latency_ms: 0.0,
        };
        if query.chars().count() > MAX_INPUT_CHARS {
            audit.latency_ms = started.elapsed().as_secs_f64() * 1000.0;
            return audit;
        }

        let (state, outcome) = match self.claim(&key) {
            Role::Cached(outcome @ Outcome::Success(_)) => (DecompositionState::CacheHit, outcome),
            Role::Cached(outcome) => (DecompositionState::FailureCacheHit, outcome),
            Role::Follower(mut receiver) => {
                let waited = tokio::time::timeout(
                    timeout + FOLLOWER_GRACE,
                    receiver.wait_for(|outcome| outcome.is_some()),
                )
                .await;
                match waited {
                    Ok(Ok(outcome)) => match outcome.clone() {
                        Some(outcome @ Outcome::Success(_)) => {
                            (DecompositionState::Follower, outcome)
                        }
                        Some(outcome) => (DecompositionState::FollowerFallback, outcome),
                        None => unreachable!("wait_for returns a Some value"),
                    },
                    // The leader was cancelled or outlasted the wait: fall back without calling.
                    _ => (
                        DecompositionState::FollowerFallback,
                        Outcome::Failure(FailureClass::Timeout),
                    ),
                }
            }
            Role::Leader(sender) => {
                let mut slot = LeaderSlot {
                    service: self,
                    key: key.clone(),
                    done: false,
                };
                audit.operations = 1;
                let outcome =
                    match tokio::time::timeout(timeout, self.provider.decompose(query)).await {
                        Ok(Ok(sub_queries)) => Outcome::Success(sub_queries),
                        Ok(Err(class)) => Outcome::Failure(class),
                        Err(_) => Outcome::Failure(FailureClass::Timeout),
                    };
                self.store(&key, outcome.clone());
                slot.done = true;
                // Followers may all have gone; the result is cached either way.
                let _ = sender.send(Some(outcome.clone()));
                let state = match outcome {
                    Outcome::Success(_) => DecompositionState::Called,
                    Outcome::Failure(_) => DecompositionState::FailedFallback,
                };
                if let Outcome::Failure(class) = &outcome {
                    tracing::warn!(
                        event = "query_decomposition_failed",
                        failure_class = ?class,
                        fingerprint = %audit.fingerprint,
                        "query decomposition failed; using the original query"
                    );
                }
                (state, outcome)
            }
        };
        audit.state = state;
        match outcome {
            Outcome::Success(sub_queries) => {
                let cap = MAX_DECOMPOSITION_SUB_QUERIES.saturating_sub(existing.len());
                let (accepted, dropped) = accept_sub_queries(query, existing, sub_queries, cap);
                audit.accepted = accepted;
                audit.dropped = dropped.len() as u32;
            }
            Outcome::Failure(class) => audit.failure_class = Some(class),
        }
        audit.latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        audit
    }
}

fn key(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Accepts sub-queries in order: trimmed, not blank, not the original or anything already present
/// in `existing` (compared case-insensitively), deduplicated, capped at `max`.
pub fn accept_sub_queries(
    original: &str,
    existing: &[String],
    returned: Vec<String>,
    max: usize,
) -> (Vec<String>, Vec<String>) {
    let mut seen: Vec<String> = std::iter::once(key(original))
        .chain(existing.iter().map(|text| key(text)))
        .collect();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for text in returned {
        let trimmed = text.trim().to_string();
        let normalised = key(&trimmed);
        if trimmed.is_empty() || seen.contains(&normalised) || accepted.len() >= max {
            rejected.push(text);
            continue;
        }
        seen.push(normalised);
        accepted.push(trimmed);
    }
    (accepted, rejected)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);

    struct Mock {
        calls: AtomicU32,
        delay: Duration,
        result: Result<Vec<&'static str>, FailureClass>,
    }

    impl Mock {
        fn new(result: Result<Vec<&'static str>, FailureClass>) -> Arc<Self> {
            Self::slow(result, Duration::ZERO)
        }
        fn slow(result: Result<Vec<&'static str>, FailureClass>, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicU32::new(0),
                delay,
                result,
            })
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl DecompositionProvider for Mock {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn identity(&self) -> DecompositionIdentity {
            DecompositionIdentity {
                provider: "mock".to_string(),
                model: "mock-model".to_string(),
            }
        }
        async fn decompose(&self, _query: &str) -> Result<Vec<String>, FailureClass> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.result
                .clone()
                .map(|items| items.iter().map(|item| item.to_string()).collect())
        }
    }

    const QUESTION: &str = "Did Rowena or Dunstan bake more for the harvest festival?";

    fn identity() -> DecompositionIdentity {
        DecompositionIdentity {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
        }
    }

    #[test]
    fn contract_hash_covers_the_exact_prompt() {
        assert_eq!(contract_hash().len(), 64);
        assert_eq!(
            contract_hash(),
            hex::encode(Sha256::digest(format!("{SYSTEM_PROMPT}\n{USER_TEMPLATE}")))
        );
    }

    #[test]
    fn fingerprint_ignores_whitespace_but_not_case_org_time_model_or_config() {
        let base = fingerprint("org-a", QUESTION, None, &identity(), TIMEOUT);
        let spaced = format!("  {}  ", QUESTION.replace(' ', "   "));
        assert_eq!(
            base,
            fingerprint("org-a", &spaced, None, &identity(), TIMEOUT)
        );
        assert_ne!(
            base,
            fingerprint(
                "org-a",
                &QUESTION.to_lowercase(),
                None,
                &identity(),
                TIMEOUT
            )
        );
        assert_ne!(
            base,
            fingerprint("org-b", QUESTION, None, &identity(), TIMEOUT)
        );
        let time = DateTime::parse_from_rfc3339("2023-05-08T13:56:00+02:00").unwrap();
        assert_ne!(
            base,
            fingerprint("org-a", QUESTION, Some(time), &identity(), TIMEOUT)
        );
        // the same instant in another offset is the same reference time
        let same = DateTime::parse_from_rfc3339("2023-05-08T11:56:00+00:00").unwrap();
        assert_eq!(
            fingerprint("org-a", QUESTION, Some(time), &identity(), TIMEOUT),
            fingerprint("org-a", QUESTION, Some(same), &identity(), TIMEOUT)
        );
        let other_model = DecompositionIdentity {
            model: "other".to_string(),
            ..identity()
        };
        assert_ne!(
            base,
            fingerprint("org-a", QUESTION, None, &other_model, TIMEOUT)
        );
        assert_ne!(
            base,
            fingerprint("org-a", QUESTION, None, &identity(), Duration::from_secs(5))
        );
        // NFC: a precomposed and a decomposed accent are the same question
        assert_eq!(
            fingerprint("org-a", "Did Zoë bake?", None, &identity(), TIMEOUT),
            fingerprint("org-a", "Did Zoe\u{0308} bake?", None, &identity(), TIMEOUT)
        );
    }

    #[tokio::test]
    async fn sequential_speaker_requests_share_one_operation() {
        let mock = Mock::new(Ok(vec!["What did Rowena bake?", "What did Dunstan bake?"]));
        let service = DecompositionService::new(mock.clone());
        let first = service.decompose("org", QUESTION, None, &[], TIMEOUT).await;
        let second = service.decompose("org", QUESTION, None, &[], TIMEOUT).await;
        assert_eq!(mock.calls(), 1);
        assert_eq!(
            (first.state, first.operations),
            (DecompositionState::Called, 1)
        );
        assert_eq!(
            (second.state, second.operations),
            (DecompositionState::CacheHit, 0)
        );
        assert_eq!(first.accepted, second.accepted);
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.fingerprint.len(), 16);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_requests_are_single_flight() {
        let mock = Mock::slow(Ok(vec!["What did Rowena bake?"]), Duration::from_secs(2));
        let service = DecompositionService::new(mock.clone());
        let (a, b) = tokio::join!(
            service.decompose("org", QUESTION, None, &[], TIMEOUT),
            service.decompose("org", QUESTION, None, &[], TIMEOUT),
        );
        assert_eq!(mock.calls(), 1);
        let mut states = [a.state, b.state];
        states.sort_by_key(|state| format!("{state:?}"));
        assert_eq!(
            states,
            [DecompositionState::Called, DecompositionState::Follower]
        );
        assert_eq!(a.operations + b.operations, 1);
        assert_eq!(a.accepted, b.accepted);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_is_cached_as_a_sentinel_for_the_full_ttl_never_as_success() {
        let mock = Mock::new(Err(FailureClass::Validation));
        let service = DecompositionService::new(mock.clone());
        let first = service.decompose("org", QUESTION, None, &[], TIMEOUT).await;
        assert_eq!(first.state, DecompositionState::FailedFallback);
        assert_eq!(first.failure_class, Some(FailureClass::Validation));
        assert!(first.accepted.is_empty());
        // well past 60 seconds, still inside the 10-minute TTL: no second operation
        tokio::time::advance(Duration::from_secs(599)).await;
        let second = service.decompose("org", QUESTION, None, &[], TIMEOUT).await;
        assert_eq!(mock.calls(), 1);
        assert_eq!(
            (second.state, second.operations),
            (DecompositionState::FailureCacheHit, 0)
        );
        assert_eq!(second.failure_class, Some(FailureClass::Validation));
        assert!(second.accepted.is_empty());
        // after the TTL the fingerprint may be tried again
        tokio::time::advance(Duration::from_secs(2)).await;
        let third = service.decompose("org", QUESTION, None, &[], TIMEOUT).await;
        assert_eq!(mock.calls(), 2);
        assert_eq!(third.state, DecompositionState::FailedFallback);
    }

    #[tokio::test(start_paused = true)]
    async fn a_concurrent_follower_of_a_failed_leader_falls_back_without_calling() {
        let mock = Mock::slow(Err(FailureClass::ProviderError), Duration::from_secs(1));
        let service = DecompositionService::new(mock.clone());
        let (a, b) = tokio::join!(
            service.decompose("org", QUESTION, None, &[], TIMEOUT),
            service.decompose("org", QUESTION, None, &[], TIMEOUT),
        );
        assert_eq!(mock.calls(), 1);
        let states = [a.state, b.state];
        assert!(states.contains(&DecompositionState::FailedFallback));
        assert!(states.contains(&DecompositionState::FollowerFallback));
        assert!(a.accepted.is_empty() && b.accepted.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_is_a_failure_and_is_cached() {
        let mock = Mock::slow(Ok(vec!["never used"]), Duration::from_secs(3600));
        let service = DecompositionService::new(mock.clone());
        let first = service
            .decompose("org", QUESTION, None, &[], Duration::from_millis(500))
            .await;
        assert_eq!(first.failure_class, Some(FailureClass::Timeout));
        let second = service
            .decompose("org", QUESTION, None, &[], Duration::from_millis(500))
            .await;
        assert_eq!(second.state, DecompositionState::FailureCacheHit);
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_leader_leaves_no_slot_and_its_follower_falls_back() {
        let mock = Mock::slow(Ok(vec!["What did Rowena bake?"]), Duration::from_secs(5));
        let service = DecompositionService::new(mock.clone());
        let follower = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            service.decompose("org", QUESTION, None, &[], TIMEOUT).await
        };
        let leader = tokio::time::timeout(
            Duration::from_secs(1),
            service.decompose("org", QUESTION, None, &[], TIMEOUT),
        );
        let (leader, follower) = tokio::join!(leader, follower);
        assert!(leader.is_err(), "the leader was cancelled");
        assert_eq!(follower.state, DecompositionState::FollowerFallback);
        assert_eq!(follower.operations, 0);
        assert_eq!(mock.calls(), 1);
        // nothing was cached and no slot is left: a later request runs the operation
        let later = service.decompose("org", QUESTION, None, &[], TIMEOUT).await;
        assert_eq!(later.state, DecompositionState::Called);
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test]
    async fn the_cache_is_bounded_with_least_recently_read_eviction() {
        let mock = Mock::new(Ok(vec![]));
        let service = DecompositionService::with_limits(mock.clone(), 2, CACHE_TTL);
        for question in ["q one?", "q two?"] {
            service.decompose("org", question, None, &[], TIMEOUT).await;
        }
        // read "q one?" so "q two?" becomes the least recently read
        service.decompose("org", "q one?", None, &[], TIMEOUT).await;
        service
            .decompose("org", "q three?", None, &[], TIMEOUT)
            .await;
        assert_eq!(mock.calls(), 3);
        let one = service.decompose("org", "q one?", None, &[], TIMEOUT).await;
        assert_eq!(one.state, DecompositionState::CacheHit);
        let two = service.decompose("org", "q two?", None, &[], TIMEOUT).await;
        assert_eq!(two.state, DecompositionState::Called, "evicted");
        assert_eq!(mock.calls(), 4);
    }

    #[tokio::test]
    async fn over_long_input_is_never_sent() {
        let mock = Mock::new(Ok(vec![]));
        let service = DecompositionService::new(mock.clone());
        let long = "a".repeat(MAX_INPUT_CHARS + 1);
        let audit = service.decompose("org", &long, None, &[], TIMEOUT).await;
        assert_eq!(audit.state, DecompositionState::InputTooLongFallback);
        assert_eq!((audit.operations, mock.calls()), (0, 0));
    }

    #[tokio::test]
    async fn d191_clauses_share_the_cap_and_are_never_repeated() {
        let mock = Mock::new(Ok(vec![
            "What did Rowena bake?",
            "what did dunstan bake?",
            "Who judged the festival?",
        ]));
        let service = DecompositionService::new(mock);
        let clauses = vec!["What did Dunstan bake?".to_string()];
        let audit = service
            .decompose("org", QUESTION, None, &clauses, TIMEOUT)
            .await;
        assert_eq!(
            audit.accepted,
            ["What did Rowena bake?", "Who judged the festival?"]
        );
        assert_eq!(audit.dropped, 1);
    }

    #[tokio::test]
    async fn noop_never_decomposes() {
        let service = DecompositionService::new(Arc::new(NoopDecompositionProvider));
        let audit = service
            .decompose("org", "a and b?", None, &[], TIMEOUT)
            .await;
        assert_eq!(audit.provider, "noop");
        assert_eq!(audit.state, DecompositionState::Called);
        assert!(audit.accepted.is_empty() && audit.failure_class.is_none());
    }
}
