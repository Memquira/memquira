//! Bounded fair FIFO inference queue for a local embedding model.
//!
//! One worker thread owns the model and runs one inference job at a time (the reference CPU is
//! compute-bound, and a second 2.4 GB copy would not help). Jobs wait in a bounded FIFO channel:
//!
//! - **fair:** first in, first out; no caller can starve another;
//! - **bounded:** at most `capacity` pending jobs; when full a request fails at once with
//!   `MemcoreError::RateLimited` (the overload path): nothing is buffered without bound, nothing is
//!   dropped silently;
//! - **cancellation:** a job whose caller has gone is skipped when the worker reaches it, so it never
//!   occupies capacity permanently and costs no inference;
//! - **shutdown:** dropping the queue fails every still-queued job with an explicit error and stops
//!   the worker; no task is created per request;
//! - **diagnostics:** queue depth, queue wait and inference time are recorded separately.
//!
//! A panic inside inference fails that job and leaves the worker serving later jobs.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use memcore_common::{MemcoreError, MemcoreResult};
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// Default pending-job capacity.
pub const DEFAULT_QUEUE_CAPACITY: usize = 16;

/// Latency samples kept for percentiles (the most recent ones).
const SAMPLE_WINDOW: usize = 10_000;

pub const QUEUE_SHUT_DOWN: &str = "local embedding queue shut down";

/// Encodes texts into vectors; owned by the worker thread.
pub trait BatchEncoder: Send + 'static {
    fn encode(&mut self, texts: &[String]) -> MemcoreResult<Vec<Vec<f32>>>;
}

struct Job {
    texts: Vec<String>,
    enqueued: Instant,
    reply: oneshot::Sender<MemcoreResult<Vec<Vec<f32>>>>,
}

#[derive(Default)]
struct Samples(Mutex<VecDeque<f64>>);

impl Samples {
    fn push(&self, value: f64) {
        let mut samples = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if samples.len() == SAMPLE_WINDOW {
            samples.pop_front();
        }
        samples.push_back(value);
    }

    fn summary(&self) -> Value {
        let mut values: Vec<f64> = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .copied()
            .collect();
        if values.is_empty() {
            return json!({ "count": 0 });
        }
        values.sort_by(f64::total_cmp);
        let pct = |q: f64| {
            let rank = q * (values.len() - 1) as f64;
            let (lo, hi) = (rank.floor() as usize, rank.ceil() as usize);
            values[lo] + (values[hi] - values[lo]) * (rank - lo as f64)
        };
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        json!({
            "count": values.len(),
            "p50": round(pct(0.5)), "p95": round(pct(0.95)), "p99": round(pct(0.99)),
            "mean": round(mean), "max": round(values[values.len() - 1]),
        })
    }
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[derive(Default)]
struct Stats {
    depth: AtomicUsize,
    max_depth: AtomicUsize,
    submitted: AtomicU64,
    rejected_full: AtomicU64,
    cancelled: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    texts: AtomicU64,
    wait_ms: Samples,
    inference_ms: Samples,
}

pub struct InferenceQueue {
    capacity: usize,
    sender: Mutex<Option<SyncSender<Job>>>,
    stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl InferenceQueue {
    pub fn start<E: BatchEncoder>(encoder: E, capacity: usize) -> MemcoreResult<Self> {
        let capacity = capacity.max(1);
        let (sender, receiver) = sync_channel::<Job>(capacity);
        let stats = Arc::new(Stats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker = {
            let (stats, shutdown) = (stats.clone(), shutdown.clone());
            std::thread::Builder::new()
                .name("memcore-local-embedding".to_string())
                .spawn(move || run_worker(encoder, receiver, stats, shutdown))
                .map_err(|error| {
                    MemcoreError::Internal(format!(
                        "failed to start local embedding worker: {error}"
                    ))
                })?
        };
        Ok(Self {
            capacity,
            sender: Mutex::new(Some(sender)),
            stats,
            shutdown,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Queues one job and waits for its vectors (one per text, in order).
    pub async fn submit(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let (reply, response) = oneshot::channel();
        let job = Job {
            texts,
            enqueued: Instant::now(),
            reply,
        };
        {
            let sender = self.sender.lock().unwrap_or_else(|p| p.into_inner());
            let Some(sender) = sender.as_ref() else {
                return Err(MemcoreError::ProviderError(QUEUE_SHUT_DOWN.to_string()));
            };
            let depth = self.stats.depth.fetch_add(1, Ordering::SeqCst) + 1;
            match sender.try_send(job) {
                Ok(()) => {
                    self.stats.submitted.fetch_add(1, Ordering::Relaxed);
                    self.stats.max_depth.fetch_max(depth, Ordering::Relaxed);
                }
                Err(TrySendError::Full(_)) => {
                    self.stats.depth.fetch_sub(1, Ordering::SeqCst);
                    self.stats.rejected_full.fetch_add(1, Ordering::Relaxed);
                    return Err(MemcoreError::RateLimited);
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.stats.depth.fetch_sub(1, Ordering::SeqCst);
                    return Err(MemcoreError::ProviderError(QUEUE_SHUT_DOWN.to_string()));
                }
            }
        }
        response
            .await
            .unwrap_or_else(|_| Err(MemcoreError::ProviderError(QUEUE_SHUT_DOWN.to_string())))
    }

    pub fn diagnostics(&self) -> Value {
        let s = &self.stats;
        json!({
            "queue_capacity": self.capacity,
            "queue_depth": s.depth.load(Ordering::SeqCst),
            "max_queue_depth": s.max_depth.load(Ordering::Relaxed),
            "submitted": s.submitted.load(Ordering::Relaxed),
            "rejected_full": s.rejected_full.load(Ordering::Relaxed),
            "cancelled": s.cancelled.load(Ordering::Relaxed),
            "completed": s.completed.load(Ordering::Relaxed),
            "failed": s.failed.load(Ordering::Relaxed),
            "texts_embedded": s.texts.load(Ordering::Relaxed),
            "queue_wait_ms": s.wait_ms.summary(),
            "inference_ms": s.inference_ms.summary(),
        })
    }
}

impl InferenceQueue {
    /// Stops accepting jobs; the job being inferred completes, every queued job fails with
    /// [`QUEUE_SHUT_DOWN`], and the worker exits.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Dropping the sender lets the worker drain what is queued (failing it) and exit.
        self.sender.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
}

impl Drop for InferenceQueue {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

fn run_worker<E: BatchEncoder>(
    mut encoder: E,
    receiver: Receiver<Job>,
    stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
) {
    while let Ok(job) = receiver.recv() {
        stats.depth.fetch_sub(1, Ordering::SeqCst);
        if shutdown.load(Ordering::SeqCst) {
            let _ = job.reply.send(Err(MemcoreError::ProviderError(
                QUEUE_SHUT_DOWN.to_string(),
            )));
            continue;
        }
        if job.reply.is_closed() {
            stats.cancelled.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let started = Instant::now();
        stats
            .wait_ms
            .push(started.duration_since(job.enqueued).as_secs_f64() * 1000.0);
        let outcome = catch_unwind(AssertUnwindSafe(|| encoder.encode(&job.texts)))
            .unwrap_or_else(|_| {
                Err(MemcoreError::ProviderError(
                    "local embedding inference panicked".to_string(),
                ))
            })
            .and_then(|vectors| {
                if vectors.len() == job.texts.len() {
                    Ok(vectors)
                } else {
                    Err(MemcoreError::ProviderError(format!(
                        "local embedding returned {} vectors for {} texts",
                        vectors.len(),
                        job.texts.len()
                    )))
                }
            });
        stats
            .inference_ms
            .push(started.elapsed().as_secs_f64() * 1000.0);
        match &outcome {
            Ok(_) => {
                stats.completed.fetch_add(1, Ordering::Relaxed);
                stats
                    .texts
                    .fetch_add(job.texts.len() as u64, Ordering::Relaxed);
            }
            Err(_) => {
                stats.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        let _ = job.reply.send(outcome);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    use super::*;

    /// Embeds each text as `[len, first byte]`, after an optional delay; can fail or panic.
    struct Fake {
        delay: Duration,
        calls: Arc<AtomicU32>,
        order: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn new(delay_ms: u64) -> (Self, Arc<AtomicU32>, Arc<Mutex<Vec<String>>>) {
            let calls = Arc::new(AtomicU32::new(0));
            let order = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    delay: Duration::from_millis(delay_ms),
                    calls: calls.clone(),
                    order: order.clone(),
                },
                calls,
                order,
            )
        }
    }

    impl BatchEncoder for Fake {
        fn encode(&mut self, texts: &[String]) -> MemcoreResult<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            self.order.lock().unwrap().extend(texts.iter().cloned());
            if texts.iter().any(|t| t == "panic") {
                panic!("boom");
            }
            if texts.iter().any(|t| t == "fail") {
                return Err(MemcoreError::ProviderError("bad input".to_string()));
            }
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32, f32::from(t.as_bytes()[0])])
                .collect())
        }
    }

    #[tokio::test]
    async fn a_job_returns_one_vector_per_text_in_order() {
        let (fake, _, _) = Fake::new(0);
        let queue = InferenceQueue::start(fake, 4).unwrap();
        let out = queue
            .submit(vec!["ab".to_string(), "xyz".to_string()])
            .await
            .unwrap();
        assert_eq!(out, vec![vec![2.0, 97.0], vec![3.0, 120.0]]);
        let d = queue.diagnostics();
        assert_eq!(
            (d["completed"].as_u64(), d["texts_embedded"].as_u64()),
            (Some(1), Some(2))
        );
        assert_eq!(d["queue_wait_ms"]["count"], 1);
        assert_eq!(d["inference_ms"]["count"], 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_are_served_first_in_first_out_one_at_a_time() {
        let (fake, _, order) = Fake::new(20);
        let queue = Arc::new(InferenceQueue::start(fake, 16).unwrap());
        let mut handles = Vec::new();
        for i in 0..6 {
            let queue = queue.clone();
            handles.push(tokio::spawn(async move {
                queue.submit(vec![format!("job{i}")]).await
            }));
            tokio::time::sleep(Duration::from_millis(3)).await; // submission order = i
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        let served = order.lock().unwrap().clone();
        assert_eq!(
            served,
            (0..6).map(|i| format!("job{i}")).collect::<Vec<_>>()
        );
        assert!(queue.diagnostics()["max_queue_depth"].as_u64().unwrap() >= 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_full_queue_rejects_at_once_with_the_overload_error() {
        let (fake, _, _) = Fake::new(200);
        let queue = Arc::new(InferenceQueue::start(fake, 2).unwrap());
        let mut pending = Vec::new();
        for i in 0..3 {
            // the first is taken by the worker; two more fill the queue
            let queue = queue.clone();
            pending.push(tokio::spawn(async move {
                queue.submit(vec![format!("p{i}")]).await
            }));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let started = Instant::now();
        let rejected = queue.submit(vec!["extra".to_string()]).await;
        assert_eq!(rejected.unwrap_err(), MemcoreError::RateLimited);
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "rejected without waiting"
        );
        for p in pending {
            p.await.unwrap().unwrap();
        }
        assert_eq!(queue.diagnostics()["rejected_full"], 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancelled_job_is_skipped_and_frees_its_slot() {
        let (fake, calls, order) = Fake::new(100);
        let queue = Arc::new(InferenceQueue::start(fake, 4).unwrap());
        let busy = {
            let queue = queue.clone();
            tokio::spawn(async move { queue.submit(vec!["busy".to_string()]).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        // queued behind "busy", then abandoned by its caller
        let abandoned = tokio::time::timeout(
            Duration::from_millis(10),
            queue.submit(vec!["abandoned".to_string()]),
        )
        .await;
        assert!(abandoned.is_err());
        busy.await.unwrap().unwrap();
        queue.submit(vec!["after".to_string()]).await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["busy", "after"], "never inferred");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let d = queue.diagnostics();
        assert_eq!(
            (d["cancelled"].as_u64(), d["queue_depth"].as_u64()),
            (Some(1), Some(0))
        );
    }

    #[tokio::test]
    async fn failures_and_panics_fail_only_their_job() {
        let (fake, _, _) = Fake::new(0);
        let queue = InferenceQueue::start(fake, 4).unwrap();
        assert!(queue.submit(vec!["fail".to_string()]).await.is_err());
        let panicked = queue.submit(vec!["panic".to_string()]).await.unwrap_err();
        assert_eq!(
            panicked,
            MemcoreError::ProviderError("local embedding inference panicked".to_string())
        );
        assert_eq!(queue.submit(vec!["ok".to_string()]).await.unwrap().len(), 1);
        let d = queue.diagnostics();
        assert_eq!(
            (d["failed"].as_u64(), d["completed"].as_u64()),
            (Some(2), Some(1))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_fails_queued_jobs_explicitly() {
        let (fake, calls, _) = Fake::new(150);
        let queue = Arc::new(InferenceQueue::start(fake, 4).unwrap());
        let mut waiting = Vec::new();
        for i in 0..3 {
            let queue = queue.clone();
            waiting.push(tokio::spawn(async move {
                queue.submit(vec![format!("q{i}")]).await
            }));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // shut down while q1 / q2 wait behind q0
        queue.shutdown();
        let results: Vec<_> = futures_join(waiting).await;
        assert_eq!(
            queue.submit(vec!["late".to_string()]).await.unwrap_err(),
            MemcoreError::ProviderError(QUEUE_SHUT_DOWN.to_string())
        );
        assert!(results[0].is_ok(), "the running job completes");
        for result in &results[1..] {
            assert_eq!(
                result.as_ref().unwrap_err(),
                &MemcoreError::ProviderError(QUEUE_SHUT_DOWN.to_string())
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    async fn futures_join(
        handles: Vec<tokio::task::JoinHandle<MemcoreResult<Vec<Vec<f32>>>>>,
    ) -> Vec<MemcoreResult<Vec<Vec<f32>>>> {
        let mut out = Vec::new();
        for handle in handles {
            out.push(handle.await.unwrap());
        }
        out
    }
}
