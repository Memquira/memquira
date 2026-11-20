use axum::Json;
use axum::extract::{Extension, State};
use memquira_common::MemquiraError;
use memquira_core::{
    ApiKeyScope, DEFAULT_CANDIDATE_EPISODE_LIMIT, DEFAULT_CANDIDATE_FACT_LIMIT,
    SearchCandidatesInput,
};

use crate::dto::{
    MemoryContextRequest, MemoryContextResponse, SearchCandidatesRequest, SearchCandidatesResponse,
};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_scope};
use crate::security::AuthContext;
use crate::state::AppState;

/// `POST /api/v1/memories/candidates`: EXPERIMENTAL. 400 unless
/// `MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED` and `MEMQUIRA_EPISODES_ENABLED` are on.
pub async fn search_candidates(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<SearchCandidatesRequest>,
) -> Result<Json<SearchCandidatesResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    if !state.settings.experimental_candidates_enabled {
        return Err(MemquiraError::ValidationError(
            "the experimental candidates endpoint is not enabled \
             (MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED)"
                .to_string(),
        )
        .into());
    }
    if request.user_id.trim().is_empty() {
        return Err(MemquiraError::ValidationError("user_id cannot be empty".to_string()).into());
    }
    let reference_time = request
        .reference_time
        .as_deref()
        .map(|text| {
            chrono::DateTime::parse_from_rfc3339(text.trim()).map_err(|_| {
                MemquiraError::ValidationError(
                    "reference_time must be an RFC 3339 timestamp".to_string(),
                )
            })
        })
        .transpose()?;
    let max_limit = state.memory_engine.search_max_limit();
    let tenant = organization.tenant_with_user_id(request.user_id)?;

    let output = state
        .memory_engine
        .search_candidates(SearchCandidatesInput {
            tenant,
            query: request.query,
            fact_limit: request
                .fact_limit
                .unwrap_or(DEFAULT_CANDIDATE_FACT_LIMIT.min(max_limit)),
            episode_limit: request
                .episode_limit
                .unwrap_or(DEFAULT_CANDIDATE_EPISODE_LIMIT.min(max_limit)),
            reference_time,
        })
        .await?;

    Ok(Json(SearchCandidatesResponse::new(
        output,
        request.include_analysis,
    )))
}

/// `POST /api/v1/memories/context`: the server-side context for one user, rendered by the
/// server: facts @100 + episodes @100 merged by raw vector score (facts only when episodes are
/// off), the reference line format, and the cl100k rank-order prefix at `token_budget`.
pub async fn memory_context(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<MemoryContextRequest>,
) -> Result<Json<MemoryContextResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    if request.user_id.trim().is_empty() {
        return Err(MemquiraError::ValidationError("user_id cannot be empty".to_string()).into());
    }
    if request.render_mode != "d76" {
        return Err(MemquiraError::ValidationError("render_mode must be `d76`".to_string()).into());
    }
    // Omitted: the server default (`MEMQUIRA_CONTEXT_DEFAULT_PACKING`, default `prefix`).
    let requested = request
        .packing
        .as_deref()
        .unwrap_or(state.settings.context_default_packing.as_str());
    let packing = match requested {
        "prefix" => memquira_core::d76_context::D76Packing::Prefix,
        "fill" => memquira_core::d76_context::D76Packing::Fill,
        "evidence" => memquira_core::d76_context::D76Packing::Evidence,
        _ => {
            return Err(MemquiraError::ValidationError(
                "packing must be `prefix`, `fill` or `evidence`".to_string(),
            )
            .into());
        }
    };
    let tenant = organization.tenant_with_user_id(request.user_id)?;
    let exclusion_requested = !request.exclude_message_ids.is_empty();
    let output = state
        .memory_engine
        .d76_context_with_options(
            memquira_core::d76_context::D76ContextInput {
                tenant,
                query: request.query,
                token_budget: request.token_budget,
            },
            packing,
            memquira_core::d76_context::D76ContextOptions {
                exclude_message_ids: request.exclude_message_ids,
            },
        )
        .await?;
    let excluded = output.excluded_candidates;
    Ok(Json(
        MemoryContextResponse::from_packed(output, request.include_items)
            .with_excluded(exclusion_requested, excluded),
    ))
}
