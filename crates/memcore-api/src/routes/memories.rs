use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use memcore_common::MemcoreError;
use memcore_core::{
    AddMemoryInput, ApiKeyScope, CheckMemoryWriteQuotaInput, DeleteMemoryInput, ListMemoriesInput,
    MemoryMessage, MemorySession, MessageRole, SearchMemoryInput, TenantContext,
};
use uuid::Uuid;

use crate::dto::{
    AddMemoryRequest, AddMemoryResponse, DeleteMemoryResponse, ListMemoriesQuery,
    ListMemoriesResponse, MemoryMessageRequest, SearchMemoryRequest, SearchMemoryResponse,
    parse_keyword_query, parse_memory_type_label,
};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_scope};
use crate::security::AuthContext;
use crate::state::AppState;

pub async fn add_memory(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<AddMemoryRequest>,
) -> Result<Json<AddMemoryResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryWrite,
    )?;
    validate_add_memory_request(&request)?;

    // Map the session and messages first: they borrow the request, and user_id is moved below.
    let session = map_session(&request)?;
    let messages = map_messages(&request.messages)?;
    let tenant = organization.tenant_with_user_id(request.user_id)?;

    let quota = state
        .memory_engine
        .check_memory_write_quota(CheckMemoryWriteQuotaInput {
            org_id: tenant.org_id.clone(),
            user_id: tenant.user_id.clone(),
            requested_new_memories: 1,
        })
        .await?;
    if let Some(violation) = quota.violations.into_iter().next() {
        crate::metrics::ops::record_quota_rejection("memory_write");
        return Err(violation.into_error().into());
    }

    let output = state
        .memory_engine
        .add_memory(AddMemoryInput {
            tenant,
            messages,
            metadata: request.metadata,
            session,
        })
        .await
        .inspect_err(|_| crate::metrics::ops::record_memory_create("error"))?;
    crate::metrics::ops::record_memory_create("success");
    crate::metrics::ops::record_fact_outcomes(&output);

    Ok(Json(AddMemoryResponse::from(output)))
}

pub async fn search_memory(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<SearchMemoryRequest>,
) -> Result<Json<SearchMemoryResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    validate_search_memory_request(&request, state.memory_engine.search_max_limit())?;

    let tenant = organization.tenant_with_user_id(request.user_id)?;
    let memory_types = request.filters.parse_memory_types()?;

    let output = state
        .memory_engine
        .search_memory_with_options(
            SearchMemoryInput {
                tenant,
                query: request.query,
                limit: request.limit,
                memory_types,
                metadata_filter: None,
            },
            memcore_core::SearchOptions {
                mode: request.retrieval_mode.map(Into::into),
                user_names: request.user_names,
                include_history: request.include_history,
            },
        )
        .await
        .inspect_err(|_| crate::metrics::ops::record_memory_search("error"))?;
    crate::metrics::ops::record_memory_search("success");

    Ok(Json(SearchMemoryResponse::from(output)))
}

pub async fn list_user_memories(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path(user_id): Path<String>,
    Query(query): Query<ListMemoriesQuery>,
) -> Result<Json<ListMemoriesResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    validate_path_user_id(&user_id)?;

    let memory_type = query
        .memory_type
        .as_deref()
        .map(parse_memory_type_label)
        .transpose()?;

    if query.limit == 0 {
        return Err(
            MemcoreError::ValidationError("limit must be greater than 0".to_string()).into(),
        );
    }

    if query.limit > memcore_core::MAX_LIST_MEMORIES_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {}",
            memcore_core::MAX_LIST_MEMORIES_LIMIT
        ))
        .into());
    }

    let query_text = parse_keyword_query(query.q)?;

    let tenant = TenantContext::new(organization.org_id, user_id)?;

    let output = state
        .memory_engine
        .list_memories(ListMemoriesInput {
            tenant,
            memory_type,
            query_text,
            limit: query.limit,
            cursor: query.cursor,
            include_deleted: query.include_deleted,
        })
        .await?;

    Ok(Json(ListMemoriesResponse::from(output)))
}

pub async fn delete_user_memory(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path((user_id, memory_id)): Path<(String, String)>,
) -> Result<Json<DeleteMemoryResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryDelete,
    )?;
    validate_path_user_id(&user_id)?;

    let memory_id = Uuid::parse_str(memory_id.trim())
        .map_err(|_| MemcoreError::ValidationError("invalid memory_id".to_string()))?;

    let tenant = TenantContext::new(organization.org_id, user_id)?;

    let output = state
        .memory_engine
        .delete_memory(DeleteMemoryInput { tenant, memory_id })
        .await
        .inspect_err(|_| crate::metrics::ops::record_memory_delete("error"))?;
    crate::metrics::ops::record_memory_delete("success");

    Ok(Json(DeleteMemoryResponse::from(output)))
}

fn validate_path_user_id(user_id: &str) -> Result<(), MemcoreError> {
    if user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "user_id cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_add_memory_request(request: &AddMemoryRequest) -> Result<(), MemcoreError> {
    if request.user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "user_id cannot be empty".to_string(),
        ));
    }

    if request.messages.is_empty() {
        return Err(MemcoreError::ValidationError(
            "messages cannot be empty".to_string(),
        ));
    }

    for message in &request.messages {
        if message.content.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "message content cannot be empty".to_string(),
            ));
        }
    }

    validate_optional_label("session_id", request.session_id.as_deref())?;
    let mut seen_message_ids = std::collections::HashSet::new();
    for (index, message) in request.messages.iter().enumerate() {
        validate_optional_label(&format!("messages[{index}].name"), message.name.as_deref())?;
        validate_optional_label(
            &format!("messages[{index}].message_id"),
            message.message_id.as_deref(),
        )?;
        if let Some(id) = message.message_id.as_deref()
            && !seen_message_ids.insert(id.trim())
        {
            // Two messages sharing an ID would make a fact's provenance ambiguous.
            return Err(MemcoreError::ValidationError(format!(
                "messages[{index}].message_id duplicates an earlier message_id in this request"
            )));
        }
    }

    Ok(())
}

/// Longest accepted `session_id`, `name` or `message_id`.
const MAX_LABEL_CHARS: usize = 200;

fn validate_optional_label(field: &str, value: Option<&str>) -> Result<(), MemcoreError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.trim().is_empty() {
        return Err(MemcoreError::ValidationError(format!(
            "{field} cannot be empty when provided"
        )));
    }
    if value.chars().count() > MAX_LABEL_CHARS {
        return Err(MemcoreError::ValidationError(format!(
            "{field} cannot exceed {MAX_LABEL_CHARS} characters"
        )));
    }
    Ok(())
}

/// Parses an optional client timestamp. A value that is present but unreadable is a 400, never a
/// silent fallback: the client asked for a specific time and would otherwise get a different one.
fn parse_client_timestamp(
    field: &str,
    value: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, MemcoreError> {
    let Some(value) = value else {
        return Ok(None);
    };
    memcore_core::temporal::parse_lenient_timestamp(value)
        .map(Some)
        .ok_or_else(|| {
            MemcoreError::ValidationError(format!(
                "{field} is not a recognised date or time; use RFC 3339, e.g. 2023-05-08T13:56:00Z"
            ))
        })
}

fn map_session(request: &AddMemoryRequest) -> Result<MemorySession, MemcoreError> {
    Ok(MemorySession {
        id: request
            .session_id
            .as_deref()
            .map(|id| id.trim().to_string()),
        timestamp: parse_client_timestamp(
            "session_timestamp",
            request.session_timestamp.as_deref(),
        )?,
    })
}

const MAX_SEARCH_USER_NAMES: usize = 10;
const MAX_SEARCH_USER_NAME_LENGTH: usize = 200;

fn validate_search_memory_request(
    request: &SearchMemoryRequest,
    max_limit: usize,
) -> Result<(), MemcoreError> {
    if request.user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "user_id cannot be empty".to_string(),
        ));
    }

    if request.query.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "query cannot be empty".to_string(),
        ));
    }

    if request.limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if request.user_names.len() > MAX_SEARCH_USER_NAMES
        || request
            .user_names
            .iter()
            .any(|name| name.chars().count() > MAX_SEARCH_USER_NAME_LENGTH)
    {
        return Err(MemcoreError::ValidationError(format!(
            "user_names allows at most {MAX_SEARCH_USER_NAMES} names of up to \
             {MAX_SEARCH_USER_NAME_LENGTH} characters"
        )));
    }

    // `MEMCORE_SEARCH_MAX_LIMIT`: 50 unless an operator raised it.
    if request.limit > max_limit {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {max_limit}"
        )));
    }

    Ok(())
}

fn map_messages(messages: &[MemoryMessageRequest]) -> Result<Vec<MemoryMessage>, MemcoreError> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| map_message(index, message))
        .collect()
}

fn map_message(
    index: usize,
    message: &MemoryMessageRequest,
) -> Result<MemoryMessage, MemcoreError> {
    let role = match message.role.trim().to_ascii_lowercase().as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        other => {
            return Err(MemcoreError::ValidationError(format!(
                "invalid message role: {other}"
            )));
        }
    };

    Ok(MemoryMessage {
        role,
        content: message.content.clone(),
        name: message.name.as_deref().map(|name| name.trim().to_string()),
        timestamp: parse_client_timestamp(
            &format!("messages[{index}].timestamp"),
            message.timestamp.as_deref(),
        )?,
        message_id: message
            .message_id
            .as_deref()
            .map(|id| id.trim().to_string()),
    })
}
