use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{delete, get, post};
use utoipa::OpenApi;

use crate::metrics::{metrics_handler, require_metrics_auth};
use crate::middleware::{
    apply_security_headers, build_cors_layer, enforce_json_content_type, enforce_rate_limit,
    enforce_request_body_limit, require_api_key, require_organization,
};
use crate::observability::{log_protected_request, observe_request_lifecycle};
use crate::openapi::ApiDoc;
use crate::provider_guardrails::apply_provider_test_source;
use crate::state::AppState;

pub mod admin;
pub mod api_keys;
pub mod candidates;
pub mod common;
pub mod context;
pub mod entities;
pub mod episodes;
pub mod health;
pub mod memories;
pub mod memory_events;
pub mod swagger_ui;
pub mod users;

/// Protected routes middleware order (incoming request):
/// `observe_request_lifecycle` → `require_api_key` → `require_organization` →
/// `enforce_rate_limit` → `log_protected_request` → `enforce_admission` → handler.
///
/// Global middleware order (incoming request):
/// request ID / tracing → security headers → CORS (optional) → body limit →
/// content-type validation → routes.
///
/// Axum applies `.layer()` / `.route_layer()` in reverse add order.
pub fn router(state: &AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/api/v1/memories", post(memories::add_memory))
        .route("/api/v1/memories/search", post(memories::search_memory))
        .route("/api/v1/episodes/search", post(episodes::search_episodes))
        .route(
            "/api/v1/memories/candidates",
            post(candidates::search_candidates),
        )
        .route("/api/v1/memories/context", post(candidates::memory_context))
        .route("/api/v1/context", post(context::build_context))
        .route(
            "/api/v1/users/{user_id}/memories",
            get(memories::list_user_memories),
        )
        .route(
            "/api/v1/users/{user_id}/memory-events",
            get(memory_events::list_user_memory_events),
        )
        .route(
            "/api/v1/users/{user_id}/memories/{memory_id}",
            delete(memories::delete_user_memory),
        )
        .route(
            "/api/v1/users/{user_id}/entities",
            get(entities::list_user_entities),
        )
        .route(
            "/api/v1/users/{user_id}/entities/{entity_id}",
            get(entities::get_user_entity),
        )
        .route(
            "/api/v1/users/{user_id}/export",
            get(users::export_user_data),
        )
        .route(
            "/api/v1/users/{user_id}/import",
            post(users::import_user_data),
        )
        .route(
            "/api/v1/users/{user_id}/retention/apply",
            post(users::apply_retention),
        )
        .route("/api/v1/users/{user_id}", delete(users::forget_user))
        .route("/api/v1/api-keys", post(api_keys::create_api_key))
        .route("/api/v1/api-keys", get(api_keys::list_api_keys))
        .route(
            "/api/v1/api-keys/{api_key_id}",
            delete(api_keys::revoke_api_key),
        )
        .route("/api/v1/admin/org/summary", get(admin::get_org_summary))
        .route("/api/v1/admin/jobs", get(admin::get_background_jobs))
        .route(
            "/api/v1/admin/jobs/runs",
            get(admin::query_background_job_runs),
        )
        .route(
            "/api/v1/admin/jobs/runs/retention/apply",
            post(admin::apply_background_job_run_retention),
        )
        .route(
            "/api/v1/admin/jobs/{job_kind}/run",
            post(admin::run_background_job),
        )
        .route("/api/v1/admin/org/users", get(admin::list_org_users))
        .route(
            "/api/v1/admin/org/memory-events",
            get(admin::search_org_memory_events),
        )
        .route(
            "/api/v1/admin/org/cache/context/metrics",
            get(admin::get_context_cache_metrics),
        )
        .route(
            "/api/v1/admin/org/cache/query-embedding/metrics",
            get(admin::get_query_embedding_cache_metrics),
        )
        .route(
            "/api/v1/admin/org/embedding/diagnostics",
            get(admin::get_embedding_diagnostics),
        )
        .route("/api/v1/admin/org/quotas", get(admin::get_org_quotas))
        .route(
            "/api/v1/admin/org/plan",
            get(admin::get_org_plan)
                .put(admin::upsert_org_plan)
                .delete(admin::delete_org_plan),
        )
        .route(
            "/api/v1/admin/org/provider-usage",
            get(admin::get_provider_usage),
        )
        .route(
            "/api/v1/admin/org/usage/dashboard",
            get(admin::get_org_usage_dashboard),
        )
        .route(
            "/api/v1/admin/org/usage/memory/snapshots",
            post(admin::create_memory_usage_snapshot).get(admin::query_memory_usage_snapshots),
        )
        .route(
            "/api/v1/admin/org/usage/provider/daily",
            get(admin::get_provider_usage_daily),
        )
        .route(
            "/api/v1/admin/org/provider-usage/retention/apply",
            post(admin::apply_provider_usage_retention),
        )
        .route(
            "/api/v1/admin/providers/guardrails",
            get(admin::get_provider_guardrails),
        )
        // Admission control, after auth, tenant and rate limiting.
        .route_layer(from_fn_with_state(
            state.clone(),
            crate::admission::enforce_admission,
        ))
        .route_layer(from_fn(log_protected_request))
        .route_layer(from_fn_with_state(state.clone(), enforce_rate_limit))
        .route_layer(from_fn(require_organization))
        .route_layer(from_fn_with_state(state.clone(), require_api_key))
        .route_layer(from_fn_with_state(
            state.call_source_slot.clone(),
            apply_provider_test_source,
        ));

    let metrics_path = state.settings.metrics_path.clone();
    let metrics_router = if state.settings.metrics_enabled {
        let router = Router::new().route(&metrics_path, get(metrics_handler));
        if state.settings.metrics_require_auth {
            router.route_layer(from_fn_with_state(state.clone(), require_metrics_auth))
        } else {
            router
        }
    } else {
        // Keep path registered so disabled scrapes get a stable 404 from the handler.
        Router::new().route(&metrics_path, get(metrics_handler))
    };

    let router = Router::new()
        .merge(swagger_ui::router(&ApiDoc::openapi()))
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::health_ready))
        .merge(metrics_router)
        .route("/api/v1/version", get(health::version))
        .merge(protected)
        // Metrics label requests by the matched route template, never by raw ids.
        .route_layer(from_fn(crate::metrics::labels::capture_matched_route))
        .layer(from_fn(enforce_json_content_type))
        .layer(from_fn_with_state(
            state.clone(),
            enforce_request_body_limit,
        ))
        .layer(DefaultBodyLimit::max(state.settings.max_request_body_bytes));

    let router = if let Some(cors_layer) = build_cors_layer(&state.settings) {
        router.layer(cors_layer)
    } else {
        router
    };

    router
        .layer(from_fn_with_state(state.clone(), apply_security_headers))
        .layer(from_fn_with_state(state.clone(), observe_request_lifecycle))
}
