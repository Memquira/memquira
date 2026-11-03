use std::sync::atomic::{AtomicUsize, Ordering};

/// In-memory per-process call budget for real-provider safety during tests/staging.
///
/// Restarting the process resets the counter. This is not a billing system.
#[derive(Debug)]
pub struct ProviderCallBudget {
    max_calls_per_run: usize,
    used_calls: AtomicUsize,
}

impl ProviderCallBudget {
    pub fn new(max_calls_per_run: usize) -> Self {
        Self {
            max_calls_per_run,
            used_calls: AtomicUsize::new(0),
        }
    }

    /// Atomically reserve one call. Returns false if the budget is exhausted.
    pub fn try_consume(&self) -> bool {
        loop {
            let used = self.used_calls.load(Ordering::Relaxed);
            if used >= self.max_calls_per_run {
                return false;
            }
            if self
                .used_calls
                .compare_exchange_weak(used, used + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub fn used(&self) -> usize {
        self.used_calls.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> usize {
        self.max_calls_per_run.saturating_sub(self.used())
    }

    pub fn max_calls(&self) -> usize {
        self.max_calls_per_run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_consumes_until_exhausted() {
        let budget = ProviderCallBudget::new(2);
        assert!(budget.try_consume());
        assert!(budget.try_consume());
        assert!(!budget.try_consume());
        assert_eq!(budget.used(), 2);
        assert_eq!(budget.remaining(), 0);
    }
}
