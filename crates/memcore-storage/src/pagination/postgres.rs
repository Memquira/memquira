use memcore_core::pagination::PageCursor;
use sqlx::{Postgres, QueryBuilder};

pub fn push_postgres_desc_cursor_uuid(
    builder: &mut QueryBuilder<Postgres>,
    sort_column: &str,
    id_column: &str,
    cursor: &PageCursor,
) {
    let last_id = uuid::Uuid::parse_str(&cursor.last_id).unwrap_or(uuid::Uuid::nil());
    builder.push(" AND (");
    builder.push(sort_column);
    builder.push(" < ");
    builder.push_bind(cursor.last_sort_value);
    builder.push(" OR (");
    builder.push(sort_column);
    builder.push(" = ");
    builder.push_bind(cursor.last_sort_value);
    builder.push(" AND ");
    builder.push(id_column);
    builder.push(" < ");
    builder.push_bind(last_id);
    builder.push("))");
}

pub fn push_postgres_desc_cursor_str_id(
    builder: &mut QueryBuilder<Postgres>,
    sort_column: &str,
    id_column: &str,
    cursor: &PageCursor,
) {
    builder.push(" AND (");
    builder.push(sort_column);
    builder.push(" < ");
    builder.push_bind(cursor.last_sort_value);
    builder.push(" OR (");
    builder.push(sort_column);
    builder.push(" = ");
    builder.push_bind(cursor.last_sort_value);
    builder.push(" AND ");
    builder.push(id_column);
    builder.push(" < ");
    builder.push_bind(cursor.last_id.clone());
    builder.push("))");
}
