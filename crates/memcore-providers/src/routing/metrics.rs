use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct ProviderRoutingMetrics {
    call_success: AtomicU64,
    call_failure: AtomicU64,
    fallback_attempted: AtomicU64,
    fallback_succeeded: AtomicU64,
    circuit_opened: AtomicU64,
    circuit_blocked: AtomicU64,
    circuit_half_opened: AtomicU64,
    circuit_closed: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderRoutingMetricsSnapshot {
    pub call_success: u64,
    pub call_failure: u64,
    pub fallback_attempted: u64,
    pub fallback_succeeded: u64,
    pub circuit_opened: u64,
    pub circuit_blocked: u64,
    pub circuit_half_opened: u64,
    pub circuit_closed: u64,
}

impl ProviderRoutingMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_call_success(&self) {
        self.call_success.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_call_failure(&self) {
        self.call_failure.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fallback_attempted(&self) {
        self.fallback_attempted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fallback_succeeded(&self) {
        self.fallback_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_circuit_opened(&self) {
        self.circuit_opened.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_circuit_blocked(&self) {
        self.circuit_blocked.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_circuit_half_opened(&self) {
        self.circuit_half_opened.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_circuit_closed(&self) {
        self.circuit_closed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ProviderRoutingMetricsSnapshot {
        ProviderRoutingMetricsSnapshot {
            call_success: self.call_success.load(Ordering::Relaxed),
            call_failure: self.call_failure.load(Ordering::Relaxed),
            fallback_attempted: self.fallback_attempted.load(Ordering::Relaxed),
            fallback_succeeded: self.fallback_succeeded.load(Ordering::Relaxed),
            circuit_opened: self.circuit_opened.load(Ordering::Relaxed),
            circuit_blocked: self.circuit_blocked.load(Ordering::Relaxed),
            circuit_half_opened: self.circuit_half_opened.load(Ordering::Relaxed),
            circuit_closed: self.circuit_closed.load(Ordering::Relaxed),
        }
    }
}
