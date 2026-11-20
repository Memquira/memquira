use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Decoded cursor payload for forward-only DESC pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageCursor {
    /// Tie-breaker id (fact/event/api-key UUID string, or org user_id).
    pub last_id: String,
    /// Primary sort value from the last item on the previous page.
    pub last_sort_value: DateTime<Utc>,
}

/// Paginated result with opaque next cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
