use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use chrono::Utc;
use memcore_common::MemcoreError;
use memcore_config::AuthMode;
use memcore_core::{
    ApiKeyListQuery, ApiKeyRecord, ApiKeyScope, PageCursor, build_page, parse_optional_cursor,
};
use uuid::Uuid;

use crate::dto::{
    ApiKeyItemResponse, CreateApiKeyRequest, CreateApiKeyResponse, ListApiKeysQuery,
    ListApiKeysResponse, MAX_LIST_API_KEYS_LIMIT, RevokeApiKeyResponse,
    parse_create_api_key_request,
};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_any_scope};
use crate::security::{AuthContext, generate_raw_api_key, hash_api_key_with_pepper};
use crate::state::AppState;

const DEV_MODE_PEPPER: &str = "memcore-dev-pepper";

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let (name, scopes) = parse_create_api_key_request(request)?;
    let pepper = resolve_api_key_pepper(&state)?;
    let raw_key = generate_raw_api_key();
    let key_hash = hash_api_key_with_pepper(pepper, &raw_key);

    let record = ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: organization.org_id,
        name,
        key_hash,
        scopes,
        created_at: Utc::now(),
        revoked_at: None,
    };

    let stored = state.api_key_store.insert_api_key(record).await?;

    Ok(Json(CreateApiKeyResponse {
        status: "success",
        api_key: ApiKeyItemResponse::from(&stored),
        raw_key,
    }))
}

pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<ListApiKeysQuery>,
) -> Result<Json<ListApiKeysResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    if query.limit == 0 {
        return Err(
            MemcoreError::ValidationError("limit must be greater than 0".to_string()).into(),
        );
    }

    if query.limit > MAX_LIST_API_KEYS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_LIST_API_KEYS_LIMIT}"
        ))
        .into());
    }

    let cursor = parse_optional_cursor(query.cursor)?;

    let records = state
        .api_key_store
        .list_api_keys(ApiKeyListQuery {
            org_id: organization.org_id,
            include_revoked: query.include_revoked,
            limit: query.limit,
            cursor,
        })
        .await?;

    let page = build_page(records, query.limit, |record| PageCursor {
        last_id: record.id.to_string(),
        last_sort_value: record.created_at,
    })?;

    Ok(Json(ListApiKeysResponse {
        status: "success",
        api_keys: page.items.iter().map(ApiKeyItemResponse::from).collect(),
        next_cursor: page.next_cursor,
    }))
}

pub async fn revoke_api_key(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path(api_key_id): Path<String>,
) -> Result<Json<RevokeApiKeyResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let key_id = Uuid::parse_str(api_key_id.trim())
        .map_err(|_| MemcoreError::ValidationError("invalid api_key_id".to_string()))?;

    state
        .api_key_store
        .revoke_api_key(&organization.org_id, key_id)
        .await?;

    Ok(Json(RevokeApiKeyResponse {
        status: "success",
        revoked: true,
    }))
}

fn resolve_api_key_pepper(state: &AppState) -> Result<&str, ApiError> {
    if let Some(pepper) = state
        .settings
        .api_key_pepper
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(pepper);
    }

    if state.settings.auth_mode == AuthMode::Dev {
        return Ok(DEV_MODE_PEPPER);
    }

    Err(MemcoreError::Internal("api key pepper is not configured".to_string()).into())
}
