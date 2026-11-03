use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use memcore_common::MemcoreError;
use memcore_core::{
    ApiKeyScope, ListMemoryEventsInput, MAX_LIST_MEMORY_EVENTS_LIMIT, TenantContext,
};
use uuid::Uuid;

use crate::dto::{
    ListMemoryEventsQuery, ListMemoryEventsResponse, parse_event_date_filters, parse_keyword_query,
    parse_memory_event_operation_label,
};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_scope};
use crate::security::AuthContext;
use crate::state::AppState;

pub async fn list_user_memory_events(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path(user_id): Path<String>,
    Query(query): Query<ListMemoryEventsQuery>,
) -> Result<Json<ListMemoryEventsResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::AuditRead,
    )?;
    validate_path_user_id(&user_id)?;

    let operation = query
        .operation
        .as_deref()
        .map(parse_memory_event_operation_label)
        .transpose()?;

    let fact_id = query.fact_id.as_deref().map(parse_fact_id).transpose()?;

    let (created_after, created_before) =
        parse_event_date_filters(query.created_after.as_ref(), query.created_before.as_ref())?;
    let query_text = parse_keyword_query(query.q)?;

    if query.limit == 0 {
        return Err(
            MemcoreError::ValidationError("limit must be greater than 0".to_string()).into(),
        );
    }

    if query.limit > MAX_LIST_MEMORY_EVENTS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_LIST_MEMORY_EVENTS_LIMIT}"
        ))
        .into());
    }

    let tenant = TenantContext::new(organization.org_id, user_id)?;

    let output = state
        .memory_engine
        .list_memory_events(ListMemoryEventsInput {
            tenant,
            fact_id,
            operation,
            created_after,
            created_before,
            query_text,
            limit: query.limit,
            cursor: query.cursor,
        })
        .await?;

    Ok(Json(ListMemoryEventsResponse::from(output)))
}

fn validate_path_user_id(user_id: &str) -> Result<(), MemcoreError> {
    if user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "user_id cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn parse_fact_id(value: &str) -> Result<Uuid, MemcoreError> {
    Uuid::parse_str(value.trim())
        .map_err(|_| MemcoreError::ValidationError("invalid fact_id".to_string()))
}
