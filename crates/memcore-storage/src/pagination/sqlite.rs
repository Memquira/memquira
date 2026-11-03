use memcore_core::pagination::PageCursor;
use sqlx::{QueryBuilder, Sqlite};

fn datetime_to_str(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339()
}

pub fn push_sqlite_desc_cursor(
    builder: &mut QueryBuilder<Sqlite>,
    sort_column: &str,
    id_column: &str,
    cursor: &PageCursor,
) {
    let sort_value = datetime_to_str(cursor.last_sort_value);
    builder.push(" AND (");
    builder.push(sort_column);
    builder.push(" < ");
    builder.push_bind(sort_value.clone());
    builder.push(" OR (");
    builder.push(sort_column);
    builder.push(" = ");
    builder.push_bind(sort_value);
    builder.push(" AND ");
    builder.push(id_column);
    builder.push(" < ");
    builder.push_bind(cursor.last_id.clone());
    builder.push("))");
}
