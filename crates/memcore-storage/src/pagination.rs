pub use memcore_core::pagination::page_fetch_limit as fetch_limit;

#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "sqlite")]
pub use sqlite::push_sqlite_desc_cursor;

#[cfg(feature = "postgres")]
pub use postgres::{push_postgres_desc_cursor_str_id, push_postgres_desc_cursor_uuid};
