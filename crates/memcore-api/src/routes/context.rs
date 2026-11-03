use std::time::Instant;

use axum::Json;
use axum::extract::{Extension, State};
use memcore_core::{ApiKeyScope, BuildContextInput, ContextBudget};

use crate::dto::{
    BuildContextRequest, BuildContextResponse, compression_options_from_request,
    format_options_from_request, validate_build_context_request,
};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_scope};
use crate::security::AuthContext;
use crate::state::AppState;

pub async fn build_context(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<BuildContextRequest>,
) -> Result<Json<BuildContextResponse>, ApiError> {
    check_scope(
        auth.as_ref().map(|extension| &extension.0),
        ApiKeyScope::MemoryRead,
    )?;
    validate_build_context_request(&request)?;

    let format_options = format_options_from_request(&request)?;
    let budget = ContextBudget {
        max_tokens: request.max_tokens,
        reserved_tokens: request.reserved_tokens,
    };
    let compression_options =
        compression_options_from_request(&request, budget.available_tokens())?;
    let user_id = request.user_id.clone();
    let tenant = organization.tenant_with_user_id(request.user_id)?;
    let memory_types = request.filters.parse_memory_types()?;

    let build_input = BuildContextInput {
        tenant,
        query: request.query.clone(),
        max_memories: request.max_memories,
        memory_types,
        include_metadata: request.include_metadata,
        budget,
        format_options,
        compression_options,
        include_history: request.include_history,
    };

    let started_at = Instant::now();
    let output = state
        .memory_engine
        .build_context(build_input.clone())
        .await
        .inspect_err(|_| crate::metrics::ops::record_context_request("error"))?;
    crate::metrics::ops::record_context_request("success");
    let duration_ms = started_at.elapsed().as_millis() as u64;

    let cache_backend = match state.settings.context_cache_backend {
        memcore_config::ContextCacheBackend::Redis => "redis",
        memcore_config::ContextCacheBackend::Memory => "memory",
        memcore_config::ContextCacheBackend::Disabled => "none",
    };
    if output.cache.hit {
        if output.cache.served_stale {
            crate::metrics::ops::record_context_cache_stale_hit(cache_backend);
        } else {
            crate::metrics::ops::record_context_cache_hit(cache_backend);
        }
    } else if output.cache.enabled {
        crate::metrics::ops::record_context_cache_miss(cache_backend);
    }
    if output.cache.refresh_started {
        crate::metrics::ops::record_context_cache_refresh(cache_backend, "started");
    }

    tracing::debug!(
        event = "context_build_completed",
        org_id = %organization.org_id,
        user_id = %user_id,
        cache_enabled = output.cache.enabled,
        cache_hit = output.cache.hit,
        served_stale = output.cache.served_stale,
        refresh_started = output.cache.refresh_started,
        waited_for_inflight = output.cache.waited_for_inflight,
        duration_ms,
        "context build completed"
    );

    // At most a fixed number of these refresh tasks exist per process (the stale hit only
    // reserves a slot when one is free); each runs under a deadline and releases its slot on every
    // exit. A reserved slot whose refresh will not run is released here.
    if output.cache.refresh_started {
        let engine = state.memory_engine.clone();
        if state.shutdown_token.is_cancelled() {
            engine.release_stale_refresh(&build_input).await;
        } else {
            let shutdown_token = state.shutdown_token.child_token();
            tokio::spawn(async move {
                if shutdown_token.is_cancelled() {
                    tracing::info!("stale context refresh skipped because shutdown is requested");
                    engine.release_stale_refresh(&build_input).await;
                    return;
                }
                let _ = engine.refresh_stale_context(build_input).await;
            });
        }
    }

    Ok(Json(BuildContextResponse::from(output)))
}
