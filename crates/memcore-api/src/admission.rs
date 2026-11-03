//! Admission control and provider concurrency bounds.
//!
//! - Protected API routes are admitted per request class (read, write, admin): at most
//!   `max_in_flight` execute, at most `max_queue` wait up to the queue timeout for a slot, and
//!   everything beyond is refused at once with `503 OVERLOADED` and `Retry-After`. Health,
//!   readiness, metrics and docs are not protected routes, so they never queue behind work.
//! - An admitted request runs under its class deadline (queue wait included). At the deadline the
//!   handler future is dropped, which releases every permit it holds (all permits are RAII guards)
//!   and answers `503 REQUEST_DEADLINE_EXCEEDED` with `Retry-After`.
//! - Model calls are bounded process-wide by two semaphores (chat, embedding). A call holds its
//!   permit for the whole logical call, retries and backoff included, so provider retries cannot
//!   exceed the bound. The wait for a permit is limited by the provider permit timeout and by the
//!   request deadline (task-local), whichever is sooner.
//!
//! Lock order is always class slot, then provider permits, and no provider call holds one provider
//! permit while waiting for another, so permits cannot deadlock. Metric labels are fixed sets
//! (class, reason, role): no tenant, model, URL or request content.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header::RETRY_AFTER};
use axum::middleware::Next;
use axum::response::Response;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_config::{AdmissionClassLimits, AdmissionSettings};
use memcore_core::FactOperationDecision;
use memcore_core::ports::{
    ConsolidationDecision, ConsolidationInput, EmbeddingIdentity, EmbeddingProvider,
    EntityAnnotationInput, EntityAnnotationOutput, EntityMatchInput, FactClassificationInput,
    FactExtractionInput, FactExtractionOutput, LlmProvider, QueryDecompositionInput,
    SummarizationInput,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::observability::{attach_error_code, error_response};
use crate::state::AppState;

pub use memcore_core::deadline::current_deadline;

/// Why a request was refused admission (the `reason` label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shed {
    /// Every slot is busy and the wait queue is full.
    Capacity,
    /// The request waited the whole queue timeout without getting a slot.
    QueueTimeout,
}

impl Shed {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::QueueTimeout => "queue_timeout",
        }
    }
}

/// Admission of one request class.
pub struct ClassGate {
    class: &'static str,
    slots: Arc<Semaphore>,
    max_queue: usize,
    waiting: AtomicUsize,
    deadline: Duration,
}

/// A held class slot; released on drop (success, error, timeout or cancellation).
pub struct ClassPermit {
    class: &'static str,
    _permit: OwnedSemaphorePermit,
}

impl Drop for ClassPermit {
    fn drop(&mut self) {
        metrics::gauge!("memcore_admission_active", "class" => self.class).decrement(1.0);
    }
}

struct Waiting<'a> {
    gate: &'a ClassGate,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.gate.waiting.fetch_sub(1, Ordering::SeqCst);
        metrics::gauge!("memcore_admission_waiting", "class" => self.gate.class).decrement(1.0);
    }
}

impl ClassGate {
    fn new(class: &'static str, limits: AdmissionClassLimits) -> Self {
        Self {
            class,
            slots: Arc::new(Semaphore::new(limits.max_in_flight)),
            max_queue: limits.max_queue,
            waiting: AtomicUsize::new(0),
            deadline: Duration::from_millis(limits.deadline_ms),
        }
    }

    pub fn class(&self) -> &'static str {
        self.class
    }

    pub fn deadline(&self) -> Duration {
        self.deadline
    }

    /// Slots free right now (diagnostics and tests).
    pub fn available(&self) -> usize {
        self.slots.available_permits()
    }

    /// Requests waiting for a slot right now.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    fn granted(&self, permit: OwnedSemaphorePermit) -> ClassPermit {
        metrics::gauge!("memcore_admission_active", "class" => self.class).increment(1.0);
        ClassPermit {
            class: self.class,
            _permit: permit,
        }
    }

    /// A slot now, or after waiting at most `max_wait` in the bounded queue.
    pub async fn admit(&self, max_wait: Duration) -> Result<ClassPermit, Shed> {
        if let Ok(permit) = self.slots.clone().try_acquire_owned() {
            return Ok(self.granted(permit));
        }
        let ahead = self.waiting.fetch_add(1, Ordering::SeqCst);
        metrics::gauge!("memcore_admission_waiting", "class" => self.class).increment(1.0);
        let _waiting = Waiting { gate: self };
        if ahead >= self.max_queue {
            return Err(Shed::Capacity);
        }
        match tokio::time::timeout(max_wait, self.slots.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(self.granted(permit)),
            _ => Err(Shed::QueueTimeout),
        }
    }
}

/// Process-wide admission state (one per `AppState`).
pub struct Admission {
    read: ClassGate,
    write: ClassGate,
    admin: ClassGate,
    queue_timeout: Duration,
    retry_after_seconds: u64,
}

impl Admission {
    /// `None` when `MEMCORE_ADMISSION_CONTROL=off`.
    pub fn from_settings(settings: &memcore_config::Settings) -> Option<Arc<Self>> {
        let a = &settings.admission;
        a.enabled.then(|| Arc::new(Self::new(a)))
    }

    pub fn new(a: &AdmissionSettings) -> Self {
        register_admission_metrics();
        Self {
            read: ClassGate::new("read", a.read),
            write: ClassGate::new("write", a.write),
            admin: ClassGate::new("admin", a.admin),
            queue_timeout: Duration::from_millis(a.queue_timeout_ms),
            retry_after_seconds: a.retry_after_seconds,
        }
    }

    /// The gate of a request class; `None` for classes that are never admitted (health, other).
    pub fn gate(&self, class: &str) -> Option<&ClassGate> {
        match class {
            "read" => Some(&self.read),
            "write" => Some(&self.write),
            "admin" => Some(&self.admin),
            _ => None,
        }
    }

    pub fn retry_after_seconds(&self) -> u64 {
        self.retry_after_seconds
    }
}

/// Registers the admission series at zero so they are scrapeable before the first request.
fn register_admission_metrics() {
    for class in ["read", "write", "admin"] {
        metrics::gauge!("memcore_admission_active", "class" => class).set(0.0);
        metrics::gauge!("memcore_admission_waiting", "class" => class).set(0.0);
        metrics::counter!("memcore_request_deadline_exceeded_total", "class" => class).increment(0);
    }
    for role in ["chat", "embedding"] {
        metrics::gauge!("memcore_provider_permits_in_use", "role" => role).set(0.0);
        metrics::counter!("memcore_provider_permit_timeouts_total", "role" => role).increment(0);
    }
}

fn with_retry_after(mut response: Response, seconds: u64) -> Response {
    if !response.headers().contains_key(RETRY_AFTER)
        && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
    {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

/// Admission middleware for the protected routes (runs after auth, tenant and rate limiting, so a
/// rejected or rate-limited request never takes a slot).
pub async fn enforce_admission(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(admission) = state.admission.clone() else {
        return next.run(request).await;
    };
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let class = crate::metrics::request_class(request.method().as_str(), &path);
    let Some(gate) = admission.gate(class) else {
        return next.run(request).await;
    };

    let deadline = Instant::now() + gate.deadline();
    let permit = match gate.admit(admission.queue_timeout).await {
        Ok(permit) => permit,
        Err(shed) => {
            crate::metrics::record_request_shed(gate.class(), shed.reason());
            let mut response = error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "OVERLOADED",
                "server is at capacity; retry later",
                &request,
            );
            attach_error_code(&mut response, "OVERLOADED");
            return with_retry_after(response, admission.retry_after_seconds());
        }
    };

    let remaining = deadline.saturating_duration_since(Instant::now());
    // Kept for the deadline response after the request is moved into the handler.
    let request_id = request
        .extensions()
        .get::<crate::observability::RequestId>()
        .cloned();
    let outcome = memcore_core::deadline::with_request_deadline(
        deadline,
        tokio::time::timeout(remaining, next.run(request)),
    )
    .await;
    drop(permit);
    match outcome {
        Ok(response) if response.status() == StatusCode::SERVICE_UNAVAILABLE => {
            with_retry_after(response, admission.retry_after_seconds())
        }
        Ok(response) => response,
        Err(_) => {
            metrics::counter!("memcore_request_deadline_exceeded_total", "class" => gate.class())
                .increment(1);
            let mut placeholder = Request::new(axum::body::Body::empty());
            if let Some(id) = request_id {
                placeholder.extensions_mut().insert(id);
            }
            let mut response = error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "REQUEST_DEADLINE_EXCEEDED",
                "request deadline exceeded; retry later",
                &placeholder,
            );
            attach_error_code(&mut response, "REQUEST_DEADLINE_EXCEEDED");
            with_retry_after(response, admission.retry_after_seconds())
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Provider concurrency.

/// One process-wide bound on concurrent provider calls of a role.
pub struct ProviderPermits {
    role: &'static str,
    slots: Arc<Semaphore>,
    max_wait: Duration,
}

/// A held provider slot; released on drop.
pub struct ProviderPermit {
    role: &'static str,
    _permit: OwnedSemaphorePermit,
}

impl Drop for ProviderPermit {
    fn drop(&mut self) {
        metrics::gauge!("memcore_provider_permits_in_use", "role" => self.role).decrement(1.0);
    }
}

impl ProviderPermits {
    pub fn new(role: &'static str, max_concurrency: usize, max_wait: Duration) -> Arc<Self> {
        Arc::new(Self {
            role,
            slots: Arc::new(Semaphore::new(max_concurrency)),
            max_wait,
        })
    }

    /// Slots free right now.
    pub fn available(&self) -> usize {
        self.slots.available_permits()
    }

    /// A slot within the permit timeout and the request deadline, or `Overloaded`.
    pub async fn acquire(&self) -> MemcoreResult<ProviderPermit> {
        let mut wait = self.max_wait;
        if let Some(deadline) = current_deadline() {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        match tokio::time::timeout(wait, self.slots.clone().acquire_owned()).await {
            Ok(Ok(permit)) => {
                metrics::gauge!("memcore_provider_permits_in_use", "role" => self.role)
                    .increment(1.0);
                Ok(ProviderPermit {
                    role: self.role,
                    _permit: permit,
                })
            }
            _ => {
                metrics::counter!("memcore_provider_permit_timeouts_total", "role" => self.role)
                    .increment(1);
                Err(MemcoreError::Overloaded(format!(
                    "no {} provider capacity within the wait limit",
                    self.role
                )))
            }
        }
    }
}

/// Why an in-flight provider call's local future was dropped (the `cause` label).
pub fn abandonment_cause(deadline: Option<Instant>, now: Instant) -> &'static str {
    match deadline {
        Some(deadline) if now >= deadline => "deadline",
        _ => "cancelled",
    }
}

/// One logical provider call in flight (permit held; retries and backoff included).
/// If its future is dropped before the call returns, `memcore_provider_calls_abandoned_total`
/// counts it once, with `cause` = `deadline` when the request deadline seen at the start had passed
/// at the drop, else `cancelled` (client disconnect or any other drop). A call that returns, with a
/// result or an error, counts nothing here (errors are `memcore_provider_requests_total{status=
/// "error"}`). This is the LOCAL abandonment only: the remote provider may still finish, and bill,
/// the request. Labels are fixed sets (role, operation, cause).
struct InFlightCall {
    role: &'static str,
    operation: &'static str,
    deadline: Option<Instant>,
    armed: bool,
}

impl InFlightCall {
    fn start(role: &'static str, operation: &'static str) -> Self {
        Self {
            role,
            operation,
            deadline: current_deadline(),
            armed: true,
        }
    }

    fn finish<T>(mut self, result: T) -> T {
        self.armed = false;
        result
    }
}

impl Drop for InFlightCall {
    fn drop(&mut self) {
        if self.armed {
            metrics::counter!(
                "memcore_provider_calls_abandoned_total",
                "role" => self.role,
                "operation" => self.operation,
                "cause" => abandonment_cause(self.deadline, Instant::now()),
            )
            .increment(1);
        }
    }
}

/// The two provider bounds of one engine.
#[derive(Clone)]
pub struct ProviderBounds {
    pub chat: Arc<ProviderPermits>,
    pub embedding: Arc<ProviderPermits>,
}

impl ProviderBounds {
    pub fn from_settings(settings: &memcore_config::Settings) -> Option<Self> {
        let a = &settings.admission;
        a.enabled.then(|| {
            let wait = Duration::from_millis(a.provider_permit_timeout_ms);
            Self {
                chat: ProviderPermits::new("chat", a.provider_chat_max_concurrency, wait),
                embedding: ProviderPermits::new(
                    "embedding",
                    a.provider_embedding_max_concurrency,
                    wait,
                ),
            }
        })
    }
}

/// An LLM provider whose every call holds a chat permit (retries included).
pub struct BoundedLlmProvider {
    inner: Arc<dyn LlmProvider>,
    permits: Arc<ProviderPermits>,
}

impl BoundedLlmProvider {
    pub fn wrap(
        inner: Arc<dyn LlmProvider>,
        permits: Arc<ProviderPermits>,
    ) -> Arc<dyn LlmProvider> {
        Arc::new(Self { inner, permits })
    }
}

#[async_trait]
impl LlmProvider for BoundedLlmProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_extract_facts")
            .finish(self.inner.extract_facts(input).await)
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_classify_fact_operation")
            .finish(self.inner.classify_fact_operation(input).await)
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_summarize_memory")
            .finish(self.inner.summarize_memory(input).await)
    }

    async fn decide_consolidation(
        &self,
        input: ConsolidationInput,
    ) -> MemcoreResult<ConsolidationDecision> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_decide_consolidation")
            .finish(self.inner.decide_consolidation(input).await)
    }

    async fn annotate_entities(
        &self,
        input: EntityAnnotationInput,
    ) -> MemcoreResult<EntityAnnotationOutput> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_annotate_entities")
            .finish(self.inner.annotate_entities(input).await)
    }

    async fn decompose_query(&self, input: QueryDecompositionInput) -> MemcoreResult<String> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_decompose_query")
            .finish(self.inner.decompose_query(input).await)
    }

    async fn confirm_entity_match(&self, input: EntityMatchInput) -> MemcoreResult<bool> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("llm", "llm_confirm_entity_match")
            .finish(self.inner.confirm_entity_match(input).await)
    }
}

/// An embedding provider whose every call holds an embedding permit (retries included).
pub struct BoundedEmbeddingProvider {
    inner: Arc<dyn EmbeddingProvider>,
    permits: Arc<ProviderPermits>,
}

impl BoundedEmbeddingProvider {
    pub fn wrap(
        inner: Arc<dyn EmbeddingProvider>,
        permits: Arc<ProviderPermits>,
    ) -> Arc<dyn EmbeddingProvider> {
        Arc::new(Self { inner, permits })
    }
}

#[async_trait]
impl EmbeddingProvider for BoundedEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("embedding", "embedding_embed_text")
            .finish(self.inner.embed_text(text).await)
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("embedding", "embedding_embed_batch")
            .finish(self.inner.embed_batch(texts).await)
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    async fn embed_query(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let _permit = self.permits.acquire().await?;
        InFlightCall::start("embedding", "embedding_embed_query")
            .finish(self.inner.embed_query(text).await)
    }

    fn diagnostics(&self) -> Option<serde_json::Value> {
        self.inner.diagnostics()
    }

    fn embedding_identity(&self) -> Option<EmbeddingIdentity> {
        self.inner.embedding_identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cause label is `deadline` only once the request deadline has passed.
    #[test]
    fn abandonment_cause_distinguishes_deadline_from_cancellation() {
        let now = Instant::now();
        assert_eq!(abandonment_cause(None, now), "cancelled");
        assert_eq!(
            abandonment_cause(Some(now + Duration::from_secs(5)), now),
            "cancelled"
        );
        assert_eq!(abandonment_cause(Some(now), now), "deadline");
        assert_eq!(
            abandonment_cause(Some(now), now + Duration::from_millis(1)),
            "deadline"
        );
    }
}
