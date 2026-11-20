use sqlx::{QueryBuilder, Sqlite};

pub fn sqlite_like_pattern(query: &str) -> String {
    format!("%{}%", query.to_ascii_lowercase())
}

pub fn push_sqlite_fact_keyword_filter(builder: &mut QueryBuilder<Sqlite>, query_text: &str) {
    let pattern = sqlite_like_pattern(query_text);
    builder.push(" AND (LOWER(content) LIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR LOWER(COALESCE(summary, '')) LIKE ");
    builder.push_bind(pattern);
    builder.push(")");
}

pub fn push_sqlite_event_keyword_filter(
    builder: &mut QueryBuilder<Sqlite>,
    query_text: &str,
    include_user_id: bool,
) {
    let pattern = sqlite_like_pattern(query_text);
    builder.push(" AND (LOWER(COALESCE(previous_content, '')) LIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR LOWER(COALESCE(new_content, '')) LIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR LOWER(COALESCE(provider_name, '')) LIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR LOWER(COALESCE(model_name, '')) LIKE ");
    builder.push_bind(pattern.clone());
    if include_user_id {
        builder.push(" OR LOWER(user_id) LIKE ");
        builder.push_bind(pattern);
    }
    builder.push(")");
}
