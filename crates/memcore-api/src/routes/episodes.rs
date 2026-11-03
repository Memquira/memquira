use axum::Json;
use axum::extract::{Extension, State};
use memcore_common::MemcoreError;
use memcore_core::{ApiKeyScope, SearchEpisodesInput};

use crate::dto::{SearchEpisodesRequest, SearchEpisodesResponse};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_scope};
use crate::security::AuthContext;
use crate::state::AppState;

/// `POST /api/v1/episodes/search`. 400 when episodes are disabled.
pub async fn search_episodes(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<SearchEpisodesRequest>,
) -> Result<Json<SearchEpisodesResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    if request.user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError("user_id cannot be empty".to_string()).into());
    }
    let tenant = organization.tenant_with_user_id(request.user_id)?;

    let output = state
        .memory_engine
        .search_episodes(SearchEpisodesInput {
            tenant,
            query: request.query,
            limit: request.limit,
        })
        .await?;

    Ok(Json(SearchEpisodesResponse::from(output)))
}
