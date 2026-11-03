use memcore_core::MemoryEvent;

pub fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

pub fn optional_contains(haystack: &Option<String>, needle: &str) -> bool {
    haystack
        .as_ref()
        .is_some_and(|value| contains_case_insensitive(value, needle))
}

pub fn event_matches_keyword(event: &MemoryEvent, needle: &str, include_user_id: bool) -> bool {
    optional_contains(&event.previous_content, needle)
        || optional_contains(&event.new_content, needle)
        || optional_contains(&event.provider_name, needle)
        || optional_contains(&event.model_name, needle)
        || (include_user_id && contains_case_insensitive(&event.user_id, needle))
}

#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(feature = "sqlite")]
pub use sqlite::{push_sqlite_event_keyword_filter, push_sqlite_fact_keyword_filter};

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "postgres")]
pub use postgres::{push_postgres_event_keyword_filter, push_postgres_fact_keyword_filter};
