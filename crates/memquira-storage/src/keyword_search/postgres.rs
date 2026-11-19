use sqlx::{Postgres, QueryBuilder};

pub fn postgres_ilike_pattern(query: &str) -> String {
    format!("%{}%", query)
}

pub fn push_postgres_fact_keyword_filter(builder: &mut QueryBuilder<Postgres>, query_text: &str) {
    let pattern = postgres_ilike_pattern(query_text);
    builder.push(" AND (content ILIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR COALESCE(summary, '') ILIKE ");
    builder.push_bind(pattern);
    builder.push(")");
}

pub fn push_postgres_event_keyword_filter(
    builder: &mut QueryBuilder<Postgres>,
    query_text: &str,
    include_user_id: bool,
) {
    let pattern = postgres_ilike_pattern(query_text);
    builder.push(" AND (COALESCE(previous_content, '') ILIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR COALESCE(new_content, '') ILIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR COALESCE(provider_name, '') ILIKE ");
    builder.push_bind(pattern.clone());
    builder.push(" OR COALESCE(model_name, '') ILIKE ");
    builder.push_bind(pattern.clone());
    if include_user_id {
        builder.push(" OR user_id ILIKE ");
        builder.push_bind(pattern);
    }
    builder.push(")");
}
