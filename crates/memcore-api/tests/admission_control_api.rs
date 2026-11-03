//! Admission control, overload responses, deadlines, cancellation and provider bounds.
//!
//! A gated fake embedding provider holds requests in flight deterministically (no real provider,
//! invented data). The Prometheus recorder is process-wide, so the tests in this file run one at a
//! time (SERIAL).

mod common;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::admission::{BoundedEmbeddingProvider, ProviderPermits};
use memcore_api::{AppState, create_app};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_config::{AdmissionClassLimits, Settings};
use memcore_core::{EmbeddingProvider, MemoryEngine};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tower::ServiceExt;

use common::authorization_header;

/// Serialises this file's tests: they read process-wide gauges and counters.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Embedding calls block until the gate opens; records concurrency and live calls.
struct GatedEmbedding {
    inner: MockEmbeddingProvider,
    gate: Semaphore,
    active: AtomicUsize,
    max_active: AtomicUsize,
    calls: AtomicUsize,
}

struct ActiveGuard<'a>(&'a AtomicUsize);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl GatedEmbedding {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MockEmbeddingProvider::new(4),
            gate: Semaphore::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        })
    }

    fn open(&self) {
        self.gate.add_permits(1_000_000);
    }

    async fn wait_gate(&self) -> ActiveGuard<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        let guard = ActiveGuard(&self.active);
        self.gate.acquire().await.expect("gate").forget();
        self.gate.add_permits(1);
        guard
    }
}

#[async_trait]
impl EmbeddingProvider for GatedEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let _active = self.wait_gate().await;
        self.inner.embed_text(text).await
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let _active = self.wait_gate().await;
        self.inner.embed_batch(texts).await
    }
    fn dimensions(&self) -> usize {
        4
    }
}

fn settings(read: AdmissionClassLimits, queue_timeout_ms: u64) -> Settings {
    let mut settings = Settings {
        rate_limit_enabled: false,
        metrics_enabled: true,
        metrics_require_auth: false,
        // The engine uses invented mock providers; readiness reports mocks as not ready otherwise.
        allow_mock: true,
        ..Settings::default()
    };
    settings.admission.read = read;
    settings.admission.queue_timeout_ms = queue_timeout_ms;
    settings.admission.retry_after_seconds = 2;
    settings
}

fn app_with(settings: Settings, embedding: Arc<dyn EmbeddingProvider>) -> (axum::Router, AppState) {
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        embedding,
    );
    let state = AppState::with_memory_engine(settings, Arc::new(engine));
    (create_app(state.clone()), state)
}

fn search(org: &str) -> Request<Body> {
    let (name, value) = authorization_header();
    Request::builder()
        .method("POST")
        .uri("/api/v1/memories/search")
        .header("content-type", "application/json")
        .header("X-Organization-ID", org)
        .header(name, value)
        .body(Body::from(
            r#"{"user_id":"user_t4","query":"invented query","limit":5}"#,
        ))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

async fn send(
    app: &axum::Router,
    request: Request<Body>,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_string());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, retry_after, json)
}

async fn metric(app: &axum::Router, series: &str) -> f64 {
    let response = app.clone().oneshot(get("/metrics")).await.unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    text.lines()
        .find(|l| l.starts_with(series))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

async fn wait_until(mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "condition not reached"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn code(json: &serde_json::Value) -> &str {
    json["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn under_at_and_above_capacity_with_queue_and_503() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    let limits = AdmissionClassLimits {
        max_in_flight: 2,
        max_queue: 1,
        deadline_ms: 30_000,
    };
    let (app, state) = app_with(settings(limits, 400), gated.clone());
    let gate = state.admission.as_ref().unwrap();
    let shed_capacity = r#"memcore_requests_shed_total{class="read",reason="capacity"}"#;
    let shed_timeout = r#"memcore_requests_shed_total{class="read",reason="queue_timeout"}"#;
    let capacity_before = metric(&app, shed_capacity).await;
    let timeout_before = metric(&app, shed_timeout).await;

    // Under / at capacity: two requests run (blocked in the provider).
    let running: Vec<_> = (0..2)
        .map(|_| tokio::spawn(send_owned(app.clone(), search("org_a"))))
        .collect();
    wait_until(|| gated.active.load(Ordering::SeqCst) == 2).await;
    assert_eq!(gate.gate("read").unwrap().available(), 0);

    // Third waits in the queue.
    let queued = tokio::spawn(send_owned(app.clone(), search("org_b")));
    wait_until(|| gate.gate("read").unwrap().waiting() == 1).await;

    // Above capacity: queue full -> immediate 503 with Retry-After.
    let started = Instant::now();
    let (status, retry_after, json) = send(&app, search("org_c")).await;
    let shed_ms = started.elapsed().as_millis();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&json), "OVERLOADED");
    assert_eq!(retry_after.as_deref(), Some("2"));
    assert!(shed_ms <= 50, "over-limit refusal took {shed_ms} ms");
    let body = json.to_string();
    assert!(
        !body.contains("org_c") && !body.contains("invented query"),
        "no tenant or query text: {body}"
    );

    // Health, readiness and metrics are not blocked by the full read class.
    for path in ["/health/live", "/health/ready", "/health", "/metrics"] {
        let started = Instant::now();
        let (status, _, _) = send(&app, get(path)).await;
        assert!(status.is_success(), "{path}: {status}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{path} answered promptly"
        );
    }

    // The queued request times out of the queue: 503 queue_timeout, Retry-After.
    let (status, retry_after, json) = queued.await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&json), "OVERLOADED");
    assert_eq!(retry_after.as_deref(), Some("2"));

    // Release the providers: both running requests succeed; every slot comes back.
    gated.open();
    for handle in running {
        let (status, _, _) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK);
    }
    wait_until(|| gate.gate("read").unwrap().available() == 2).await;
    assert_eq!(gate.gate("read").unwrap().waiting(), 0);

    assert_eq!(metric(&app, shed_capacity).await - capacity_before, 1.0);
    assert_eq!(metric(&app, shed_timeout).await - timeout_before, 1.0);
    assert_eq!(
        metric(&app, r#"memcore_admission_active{class="read"}"#).await,
        0.0
    );
    assert_eq!(
        metric(&app, r#"memcore_admission_waiting{class="read"}"#).await,
        0.0
    );
    // Labels are fixed sets only.
    let scrape = String::from_utf8_lossy(
        &app.clone()
            .oneshot(get("/metrics"))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .to_string();
    for line in scrape
        .lines()
        .filter(|l| l.contains("memcore_requests_shed_total") || l.contains("memcore_admission_"))
    {
        assert!(
            !line.contains("org_") && !line.contains("user_"),
            "tenant label leaked: {line}"
        );
    }
}

async fn send_owned(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, Option<String>, serde_json::Value) {
    send(&app, request).await
}

#[tokio::test]
async fn deadline_cancels_the_handler_and_frees_every_permit() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    let limits = AdmissionClassLimits {
        max_in_flight: 1,
        max_queue: 0,
        deadline_ms: 300,
    };
    let (app, state) = app_with(settings(limits, 100), gated.clone());
    let before = metric(
        &app,
        r#"memcore_request_deadline_exceeded_total{class="read"}"#,
    )
    .await;
    let started = Instant::now();
    let (status, retry_after, json) = send(&app, search("org_d")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&json), "REQUEST_DEADLINE_EXCEEDED");
    assert_eq!(retry_after.as_deref(), Some("2"));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(290) && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
    // The handler (and its provider call) was dropped; the slot is free.
    wait_until(|| gated.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(
        state
            .admission
            .as_ref()
            .unwrap()
            .gate("read")
            .unwrap()
            .available(),
        1
    );
    assert_eq!(
        metric(
            &app,
            r#"memcore_request_deadline_exceeded_total{class="read"}"#
        )
        .await
            - before,
        1.0
    );
    // The next request is admitted and succeeds once the provider answers.
    gated.open();
    assert_eq!(send(&app, search("org_d")).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_dropped_client_request_frees_its_permit() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    let limits = AdmissionClassLimits {
        max_in_flight: 1,
        max_queue: 4,
        deadline_ms: 30_000,
    };
    let (app, state) = app_with(settings(limits, 5_000), gated.clone());
    let gate = || state.admission.as_ref().unwrap().gate("read").unwrap();
    let running = tokio::spawn(send_owned(app.clone(), search("org_e")));
    wait_until(|| gated.active.load(Ordering::SeqCst) == 1).await;
    let queued = tokio::spawn(send_owned(app.clone(), search("org_e")));
    wait_until(|| gate().waiting() == 1).await;
    // The client goes away: both the running and the queued request futures are dropped.
    running.abort();
    queued.abort();
    wait_until(|| {
        gated.active.load(Ordering::SeqCst) == 0 && gate().waiting() == 0 && gate().available() == 1
    })
    .await;
    gated.open();
    assert_eq!(send(&app, search("org_e")).await.0, StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_429_is_distinct_from_overload_503() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    gated.open();
    let mut s = settings(
        AdmissionClassLimits {
            max_in_flight: 4,
            max_queue: 4,
            deadline_ms: 30_000,
        },
        1_000,
    );
    s.rate_limit_enabled = true;
    s.rate_limit_requests_per_minute = 1;
    let (app, state) = app_with(s, gated);
    let shed = r#"memcore_requests_shed_total{class="read",reason="capacity"}"#;
    let before = metric(&app, shed).await;
    assert_eq!(send(&app, search("org_rl")).await.0, StatusCode::OK);
    let (status, retry_after, json) = send(&app, search("org_rl")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&json), "RATE_LIMITED");
    assert!(retry_after.is_some());
    // A rate-limited request never reached admission and was not counted as shed.
    assert_eq!(metric(&app, shed).await, before);
    assert_eq!(
        state
            .admission
            .as_ref()
            .unwrap()
            .gate("read")
            .unwrap()
            .available(),
        4
    );
}

#[tokio::test]
async fn admission_off_is_a_rollback_switch() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    gated.open();
    let mut s = settings(
        AdmissionClassLimits {
            max_in_flight: 1,
            max_queue: 0,
            deadline_ms: 30_000,
        },
        1_000,
    );
    s.admission.enabled = false;
    let (app, state) = app_with(s, gated);
    assert!(state.admission.is_none());
    let results = futures_join(&app, 4).await;
    assert!(results.iter().all(|s| *s == StatusCode::OK));
}

async fn futures_join(app: &axum::Router, n: usize) -> Vec<StatusCode> {
    let handles: Vec<_> = (0..n)
        .map(|_| tokio::spawn(send_owned(app.clone(), search("org_off"))))
        .collect();
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap().0);
    }
    out
}

#[tokio::test]
async fn provider_permits_bound_concurrency_and_time_out_as_overloaded() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    let permits = ProviderPermits::new("embedding", 2, Duration::from_millis(200));
    let bounded = BoundedEmbeddingProvider::wrap(gated.clone(), permits.clone());
    let calls: Vec<_> = (0..6)
        .map(|i| {
            let bounded = bounded.clone();
            tokio::spawn(async move { bounded.embed_text(&format!("text {i}")).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        gated.active.load(Ordering::SeqCst),
        2,
        "only two calls reach the provider"
    );
    // The waiting calls give up after 200 ms with Overloaded (503), never reaching the provider.
    tokio::time::sleep(Duration::from_millis(250)).await;
    gated.open();
    let mut overloaded = 0;
    let mut ok = 0;
    for call in calls {
        match call.await.unwrap() {
            Ok(_) => ok += 1,
            Err(MemcoreError::Overloaded(_)) => overloaded += 1,
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!((ok, overloaded), (2, 4));
    assert_eq!(gated.max_active.load(Ordering::SeqCst), 2);
    assert_eq!(gated.calls.load(Ordering::SeqCst), 2);
    assert_eq!(permits.available(), 2, "every permit released");
}

/// A provider that fails with retryable errors: the retry loop runs inside one permit, so retries
/// never exceed the bound, and each attempt is still a provider call.
#[tokio::test]
async fn retries_stay_inside_the_provider_bound() {
    let _serial = SERIAL.lock().await;
    struct Flaky {
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }
    #[async_trait]
    impl EmbeddingProvider for Flaky {
        async fn embed_text(&self, _text: &str) -> MemcoreResult<Vec<f32>> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now, Ordering::SeqCst);
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            if n % 3 == 2 {
                Ok(vec![0.0; 4])
            } else {
                Err(MemcoreError::RateLimited)
            }
        }
        async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
            let mut out = Vec::new();
            for t in texts {
                out.push(self.embed_text(&t).await?);
            }
            Ok(out)
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    /// The retry loop a resilient provider runs (three attempts) around the flaky one.
    struct Retrying(Arc<Flaky>);
    #[async_trait]
    impl EmbeddingProvider for Retrying {
        async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
            let mut last = None;
            for _ in 0..3 {
                match self.0.embed_text(text).await {
                    Ok(v) => return Ok(v),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap())
        }
        async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
            self.0.embed_batch(texts).await
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    let flaky = Arc::new(Flaky {
        calls: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
    });
    let bounded = BoundedEmbeddingProvider::wrap(
        Arc::new(Retrying(flaky.clone())),
        ProviderPermits::new("embedding", 2, Duration::from_secs(5)),
    );
    let calls: Vec<_> = (0..8)
        .map(|i| {
            let bounded = bounded.clone();
            tokio::spawn(async move { bounded.embed_text(&format!("t{i}")).await })
        })
        .collect();
    for call in calls {
        let _ = call.await.unwrap();
    }
    assert!(
        flaky.max_active.load(Ordering::SeqCst) <= 2,
        "attempts never exceed the bound"
    );
    assert!(
        flaky.calls.load(Ordering::SeqCst) > 8,
        "retried attempts are real provider calls"
    );
}

#[tokio::test]
async fn concurrent_read_and_write_traffic_does_not_deadlock() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    gated.open();
    let mut s = settings(
        AdmissionClassLimits {
            max_in_flight: 2,
            max_queue: 64,
            deadline_ms: 30_000,
        },
        5_000,
    );
    s.admission.write = AdmissionClassLimits {
        max_in_flight: 2,
        max_queue: 64,
        deadline_ms: 30_000,
    };
    let (app, state) = app_with(s, gated);
    let (name, value) = authorization_header();
    let mut handles = Vec::new();
    for i in 0..40 {
        let app = app.clone();
        let request = if i % 2 == 0 {
            search("org_mix")
        } else {
            Request::builder()
                .method("POST")
                .uri("/api/v1/memories")
                .header("content-type", "application/json")
                .header("X-Organization-ID", "org_mix")
                .header(name, value.clone())
                .body(Body::from(
                    r#"{"user_id":"user_t4","messages":[{"role":"user","content":"An invented sentence."}],"metadata":{}}"#,
                ))
                .unwrap()
        };
        handles.push(tokio::spawn(async move { send(&app, request).await.0 }));
    }
    let all = tokio::time::timeout(Duration::from_secs(20), async {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    })
    .await
    .expect("no deadlock");
    assert!(all.iter().all(|s| *s == StatusCode::OK), "{all:?}");
    let a = state.admission.as_ref().unwrap();
    assert_eq!(a.gate("read").unwrap().available(), 2);
    assert_eq!(a.gate("write").unwrap().available(), 2);
}

// ------------------------------------------------------------------------------------------------
// Provider calls whose local future was abandoned while in flight.

const ABANDONED_DEADLINE: &str = r#"memcore_provider_calls_abandoned_total{role="embedding",operation="embedding_embed_query",cause="deadline"}"#;
const ABANDONED_CANCELLED: &str = r#"memcore_provider_calls_abandoned_total{role="embedding",operation="embedding_embed_query",cause="cancelled"}"#;

fn bounded(provider: Arc<dyn EmbeddingProvider>) -> Arc<dyn EmbeddingProvider> {
    BoundedEmbeddingProvider::wrap(
        provider,
        ProviderPermits::new("embedding", 4, Duration::from_secs(5)),
    )
}

/// An embedding provider that answers at once with an error (a completed provider error).
struct FailingEmbedding;

#[async_trait]
impl EmbeddingProvider for FailingEmbedding {
    async fn embed_text(&self, _text: &str) -> MemcoreResult<Vec<f32>> {
        Err(MemcoreError::ProviderError(
            "invented provider failure".to_string(),
        ))
    }
    async fn embed_batch(&self, _texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Err(MemcoreError::ProviderError(
            "invented provider failure".to_string(),
        ))
    }
    fn dimensions(&self) -> usize {
        4
    }
}

#[tokio::test]
async fn a_deadline_drop_counts_one_abandoned_call_with_cause_deadline() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    let limits = AdmissionClassLimits {
        max_in_flight: 1,
        max_queue: 0,
        deadline_ms: 300,
    };
    let (app, _state) = app_with(settings(limits, 100), bounded(gated.clone()));
    let (deadline_before, cancelled_before) = (
        metric(&app, ABANDONED_DEADLINE).await,
        metric(&app, ABANDONED_CANCELLED).await,
    );
    let (status, retry_after, json) = send(&app, search("org_ab")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&json), "REQUEST_DEADLINE_EXCEEDED");
    assert_eq!(retry_after.as_deref(), Some("2"));
    wait_until(|| gated.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(
        metric(&app, ABANDONED_DEADLINE).await - deadline_before,
        1.0
    );
    assert_eq!(
        metric(&app, ABANDONED_CANCELLED).await - cancelled_before,
        0.0
    );
    // A later call that completes counts nothing more (no double counting).
    gated.open();
    assert_eq!(send(&app, search("org_ab")).await.0, StatusCode::OK);
    assert_eq!(
        metric(&app, ABANDONED_DEADLINE).await - deadline_before,
        1.0
    );
    assert_eq!(
        metric(&app, ABANDONED_CANCELLED).await - cancelled_before,
        0.0
    );
    // Labels are fixed sets only.
    let scrape = String::from_utf8_lossy(
        &app.clone()
            .oneshot(get("/metrics"))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .to_string();
    for line in scrape
        .lines()
        .filter(|l| l.contains("memcore_provider_calls_abandoned_total"))
    {
        assert!(
            !line.contains("org_") && !line.contains("user_") && !line.contains("invented"),
            "{line}"
        );
    }
}

#[tokio::test]
async fn a_client_drop_counts_cause_cancelled_not_deadline() {
    let _serial = SERIAL.lock().await;
    let gated = GatedEmbedding::new();
    let limits = AdmissionClassLimits {
        max_in_flight: 1,
        max_queue: 4,
        deadline_ms: 30_000,
    };
    let (app, _state) = app_with(settings(limits, 5_000), bounded(gated.clone()));
    let (deadline_before, cancelled_before) = (
        metric(&app, ABANDONED_DEADLINE).await,
        metric(&app, ABANDONED_CANCELLED).await,
    );
    let running = tokio::spawn(send_owned(app.clone(), search("org_cd")));
    wait_until(|| gated.active.load(Ordering::SeqCst) == 1).await;
    running.abort();
    wait_until(|| gated.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(
        metric(&app, ABANDONED_CANCELLED).await - cancelled_before,
        1.0
    );
    assert_eq!(
        metric(&app, ABANDONED_DEADLINE).await - deadline_before,
        0.0
    );
}

#[tokio::test]
async fn completed_provider_calls_are_never_counted_as_abandoned() {
    let _serial = SERIAL.lock().await;
    let limits = AdmissionClassLimits {
        max_in_flight: 2,
        max_queue: 2,
        deadline_ms: 30_000,
    };
    // A completed provider error is an error, not an abandonment.
    let (failing, _) = app_with(settings(limits, 1_000), bounded(Arc::new(FailingEmbedding)));
    let (deadline_before, cancelled_before) = (
        metric(&failing, ABANDONED_DEADLINE).await,
        metric(&failing, ABANDONED_CANCELLED).await,
    );
    assert_ne!(send(&failing, search("org_ce")).await.0, StatusCode::OK);
    // A completed success is not an abandonment either.
    let gated = GatedEmbedding::new();
    gated.open();
    let (ok, _) = app_with(settings(limits, 1_000), bounded(gated));
    assert_eq!(send(&ok, search("org_ce")).await.0, StatusCode::OK);
    assert_eq!(metric(&ok, ABANDONED_DEADLINE).await - deadline_before, 0.0);
    assert_eq!(
        metric(&ok, ABANDONED_CANCELLED).await - cancelled_before,
        0.0
    );
}
