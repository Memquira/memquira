use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitBreakerSnapshot {
    pub state: CircuitState,
    pub failure_count: usize,
    pub opened_at: Option<DateTime<Utc>>,
}
