//! Read-only entity API. 400 when entities are disabled.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use memcore_common::MemcoreError;
use memcore_core::{ApiKeyScope, ListEntitiesInput, TenantContext};
use uuid::Uuid;

use crate::dto::{EntityDetailResponse, ListEntitiesQuery, ListEntitiesResponse};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_scope};
use crate::security::AuthContext;
use crate::state::AppState;

fn tenant(organization: OrganizationContext, user_id: String) -> Result<TenantContext, ApiError> {
    if user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError("user_id cannot be empty".to_string()).into());
    }
    Ok(TenantContext::new(organization.org_id, user_id)?)
}

/// `GET /api/v1/users/{user_id}/entities`: the user's entities, newest first, paginated.
pub async fn list_user_entities(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path(user_id): Path<String>,
    Query(query): Query<ListEntitiesQuery>,
) -> Result<Json<ListEntitiesResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    let output = state
        .memory_engine
        .list_entities(ListEntitiesInput {
            tenant: tenant(organization, user_id)?,
            limit: query.limit,
            cursor: query.cursor,
        })
        .await?;
    Ok(Json(ListEntitiesResponse::from(output)))
}

/// `GET /api/v1/users/{user_id}/entities/{entity_id}`: one entity with its linked facts and
/// relations. 404 when the user has no such entity.
pub async fn get_user_entity(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path((user_id, entity_id)): Path<(String, Uuid)>,
) -> Result<Json<EntityDetailResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    let tenant = tenant(organization, user_id)?;
    let detail = state
        .memory_engine
        .get_entity(&tenant, entity_id)
        .await?
        .ok_or_else(|| MemcoreError::NotFound("entity not found".to_string()))?;
    Ok(Json(EntityDetailResponse::from(detail)))
}
