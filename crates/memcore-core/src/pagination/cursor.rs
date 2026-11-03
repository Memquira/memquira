use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};

use super::types::{Page, PageCursor};

const INVALID_CURSOR_MESSAGE: &str = "invalid cursor";

/// Returns `limit + 1` for internal over-fetch pagination.
pub fn page_fetch_limit(limit: usize) -> usize {
    limit.saturating_add(1)
}

/// Returns true when `sort_value`/`id` should appear after `cursor` in DESC pagination.
pub fn is_after_cursor_in_desc_order(
    sort_value: DateTime<Utc>,
    id: &str,
    cursor: &PageCursor,
) -> bool {
    sort_value < cursor.last_sort_value
        || (sort_value == cursor.last_sort_value && id < cursor.last_id.as_str())
}

/// Parses an optional opaque API cursor string into a decoded cursor.
pub fn parse_optional_cursor(cursor: Option<String>) -> MemcoreResult<Option<PageCursor>> {
    match cursor {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Ok(None),
        Some(value) => decode_cursor(&value).map(Some),
    }
}

/// Encodes a cursor payload as opaque base64url JSON.
pub fn encode_cursor(cursor: &PageCursor) -> MemcoreResult<String> {
    let json = serde_json::to_string(cursor)
        .map_err(|error| MemcoreError::Internal(format!("failed to encode cursor: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(json.as_bytes()))
}

/// Decodes an opaque base64url JSON cursor.
pub fn decode_cursor(cursor: &str) -> MemcoreResult<PageCursor> {
    let trimmed = cursor.trim();
    if trimmed.is_empty() {
        return Err(MemcoreError::ValidationError(
            INVALID_CURSOR_MESSAGE.to_string(),
        ));
    }

    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed)
        .map_err(|_| MemcoreError::ValidationError(INVALID_CURSOR_MESSAGE.to_string()))?;

    serde_json::from_slice(&bytes)
        .map_err(|_| MemcoreError::ValidationError(INVALID_CURSOR_MESSAGE.to_string()))
}

/// Builds a page from an over-fetched item list (`limit + 1` items max).
pub fn build_page<T, F>(mut items: Vec<T>, limit: usize, to_cursor: F) -> MemcoreResult<Page<T>>
where
    F: Fn(&T) -> PageCursor,
{
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        let last = items
            .last()
            .expect("page truncated to a non-empty limit-sized slice");
        Some(encode_cursor(&to_cursor(last))?)
    } else {
        None
    };

    Ok(Page { items, next_cursor })
}
