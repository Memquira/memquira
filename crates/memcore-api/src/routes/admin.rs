use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use chrono::{Duration, Utc};
use memcore_common::MemcoreError;
use memcore_core::{
    ApiKeyScope, GetOrgQuotaStatusInput, ListOrgUsersInput, ProviderUsageDailyInput,
    ProviderUsageQuery, SearchOrgMemoryEventsInput, parse_optional_cursor,
    validate_background_job_run_limit, validate_provider_usage_limit,
};
use uuid::Uuid;

use crate::dto::{
    ApplyBackgroundJobRunRetentionRequest, ApplyBackgroundJobRunRetentionResponse,
    ContextCacheMetricsResponse, CreateMemoryUsageSnapshotRequest,
    CreateMemoryUsageSnapshotResponse, DeleteOrgPlanResponse, EmbeddingDiagnosticsResponse,
    GetOrgPlanResponse, ListOrgUsersQuery, ListOrgUsersResponse, OrgQuotaStatusResponse,
    OrgQuotasQuery, OrgSummaryResponse, OrgUsageDashboardResponse, OrgUsageDateRangeQuery,
    ProviderGuardrailsResponse, ProviderUsageDailyQueryParams, ProviderUsageDailyResponse,
    ProviderUsageQueryParams, ProviderUsageResponse, QueryBackgroundJobRunsParams,
    QueryBackgroundJobRunsResponse, QueryEmbeddingCacheMetricsResponse,
    QueryMemoryUsageSnapshotsParams, QueryMemoryUsageSnapshotsResponse, RunBackgroundJobResponse,
    SearchOrgMemoryEventsQuery, SearchOrgMemoryEventsResponse, UpsertOrgPlanRequest,
    UpsertOrgPlanResponse, background_job_run_retention_response, background_jobs_response,
    background_jobs_response_with_persisted_runs_and_locks, context_cache_metrics_response,
    get_org_plan_response, org_quota_status_response, org_summary_input, org_usage_dashboard_input,
    parse_background_job_kind, parse_event_date_filters, parse_keyword_query,
    parse_memory_event_operation_label, parse_org_usage_window, parse_provider_usage_capability,
    provider_usage_memory_response, provider_usage_persisted_response,
    query_background_job_runs_input, query_background_job_runs_response,
    query_embedding_cache_metrics_response, query_memory_usage_snapshots_input,
    run_background_job_response, upsert_org_plan_response, validate_list_org_users_limit,
    validate_search_org_memory_events_limit,
};
use crate::middleware::OrganizationContext;
use crate::routes::common::{ApiError, check_any_scope};
use crate::security::AuthContext;
use crate::state::AppState;

pub async fn get_background_jobs(
    State(state): State<AppState>,
    Extension(_organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<crate::dto::BackgroundJobsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let snapshot = state.background_jobs.snapshot();
    let mut latest_persisted_runs = None;
    if let Some(store) = &state.background_job_run_store {
        let query = memcore_core::BackgroundJobRunQuery {
            kind: None,
            status: None,
            created_after: None,
            created_before: None,
            limit: 10,
            cursor: None,
        };
        match store.query_runs(query).await {
            Ok(result) => {
                tracing::info!(
                    run_count = result.runs.len(),
                    "background job history queried"
                );
                latest_persisted_runs = Some(result.runs);
            }
            Err(error) => {
                tracing::warn!(
                    error_code = error.code(),
                    "background job history query failed for jobs snapshot"
                );
            }
        }
    }

    if state.settings.background_job_lock_enabled {
        let mut lock_statuses = Vec::new();
        if let Some(store) = &state.background_job_lock_store {
            for definition in &snapshot.jobs {
                match store.get_lock(definition.kind).await {
                    Ok(lock) => lock_statuses.push((definition.kind, lock)),
                    Err(error) => {
                        tracing::warn!(
                            job_kind = %definition.kind,
                            error_code = error.code(),
                            "background job lock status query failed"
                        );
                        lock_statuses.push((definition.kind, None));
                    }
                }
            }
        }
        return Ok(Json(
            background_jobs_response_with_persisted_runs_and_locks(
                snapshot,
                latest_persisted_runs,
                true,
                lock_statuses,
            ),
        ));
    }

    if let Some(runs) = latest_persisted_runs {
        return Ok(Json(
            background_jobs_response_with_persisted_runs_and_locks(
                snapshot,
                Some(runs),
                false,
                Vec::new(),
            ),
        ));
    }

    Ok(Json(background_jobs_response(snapshot)))
}

pub async fn run_background_job(
    State(state): State<AppState>,
    Extension(_organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Path(job_kind): Path<String>,
) -> Result<Json<RunBackgroundJobResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let kind = parse_background_job_kind(&job_kind)?;
    tracing::info!(job_kind = %kind, "manual background job trigger requested");
    let started = std::time::Instant::now();
    let run = state.background_jobs.run_manual(kind).await?;
    let status = format!("{:?}", run.status);
    let kind_label = format!("{kind:?}");
    crate::metrics::ops::record_background_job_run(
        &kind_label,
        &status,
        started.elapsed().as_secs_f64(),
    );
    if run.attempt_count > 1 {
        crate::metrics::ops::record_background_job_retries(
            &kind_label,
            (run.attempt_count - 1) as u64,
        );
    }

    Ok(Json(run_background_job_response(run)))
}

pub async fn query_background_job_runs(
    State(state): State<AppState>,
    Extension(_organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(params): Query<QueryBackgroundJobRunsParams>,
) -> Result<Json<QueryBackgroundJobRunsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    validate_background_job_run_limit(params.limit)?;
    let query = query_background_job_runs_input(params)?;
    let Some(store) = &state.background_job_run_store else {
        return Ok(Json(query_background_job_runs_response(
            memcore_core::BackgroundJobRunQueryResult {
                runs: Vec::new(),
                next_cursor: None,
            },
        )));
    };

    let result = store.query_runs(query).await?;
    tracing::info!(
        run_count = result.runs.len(),
        "background job history queried"
    );
    Ok(Json(query_background_job_runs_response(result)))
}

pub async fn apply_background_job_run_retention(
    State(state): State<AppState>,
    Extension(_organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    body: Option<Json<ApplyBackgroundJobRunRetentionRequest>>,
) -> Result<Json<ApplyBackgroundJobRunRetentionResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let request =
        body.map(|Json(request)| request)
            .unwrap_or(ApplyBackgroundJobRunRetentionRequest {
                dry_run: true,
                retention_days: None,
            });
    let retention_days = request.retention_days(&state.settings);
    let cutoff = Utc::now() - Duration::days(retention_days as i64);
    if retention_days == 0 {
        tracing::info!(
            dry_run = request.dry_run,
            deleted_count = 0usize,
            "background job history cleanup skipped because retention is disabled"
        );
        return Ok(Json(background_job_run_retention_response(
            request, cutoff, 0,
        )));
    }

    let Some(store) = &state.background_job_run_store else {
        return Ok(Json(background_job_run_retention_response(
            request, cutoff, 0,
        )));
    };

    let matched_or_deleted = store
        .delete_runs_older_than(cutoff, request.dry_run)
        .await?;
    if request.dry_run {
        tracing::info!(
            dry_run = true,
            deleted_count = 0usize,
            "background job history cleanup dry-run"
        );
    } else {
        tracing::info!(
            dry_run = false,
            deleted_count = matched_or_deleted,
            "background job history cleanup applied"
        );
    }

    Ok(Json(background_job_run_retention_response(
        request,
        cutoff,
        matched_or_deleted,
    )))
}

pub async fn get_org_summary(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<OrgSummaryResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let input = org_summary_input(organization.org_id);
    let output = state.memory_engine.get_org_summary(input).await?;

    Ok(Json(OrgSummaryResponse::from(output)))
}

pub async fn list_org_users(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<ListOrgUsersQuery>,
) -> Result<Json<ListOrgUsersResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    validate_list_org_users_limit(query.limit)?;

    let input = ListOrgUsersInput {
        org_id: organization.org_id,
        limit: query.limit,
        cursor: query.cursor,
    };
    let output = state.memory_engine.list_org_users(input).await?;

    Ok(Json(ListOrgUsersResponse::from(output)))
}

pub async fn search_org_memory_events(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<SearchOrgMemoryEventsQuery>,
) -> Result<Json<SearchOrgMemoryEventsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[
            ApiKeyScope::AdminRead,
            ApiKeyScope::AdminWrite,
            ApiKeyScope::AuditRead,
        ],
    )?;

    validate_search_org_memory_events_limit(query.limit)?;

    let operation = query
        .operation
        .as_deref()
        .map(parse_memory_event_operation_label)
        .transpose()?;

    let fact_id = query.fact_id.as_deref().map(parse_fact_id).transpose()?;

    let (created_after, created_before) =
        parse_event_date_filters(query.created_after.as_ref(), query.created_before.as_ref())?;
    let query_text = parse_keyword_query(query.q)?;

    let input = SearchOrgMemoryEventsInput {
        org_id: organization.org_id,
        user_id: query.user_id,
        fact_id,
        operation,
        created_after,
        created_before,
        query_text,
        limit: query.limit,
        cursor: query.cursor,
    };

    let output = state.memory_engine.search_org_memory_events(input).await?;

    Ok(Json(SearchOrgMemoryEventsResponse::from(output)))
}

pub async fn get_context_cache_metrics(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<ContextCacheMetricsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let _ = organization.org_id;
    let snapshot = state.memory_engine.context_cache_metrics_snapshot();
    Ok(Json(context_cache_metrics_response(snapshot)))
}

/// The embedding provider's process-local diagnostics (local model queue depth,
/// queue wait and inference time, reported separately).
pub async fn get_embedding_diagnostics(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<EmbeddingDiagnosticsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let _ = organization.org_id;
    Ok(Json(EmbeddingDiagnosticsResponse {
        status: "success",
        scope: "process_local",
        diagnostics: state.memory_engine.embedding_diagnostics(),
    }))
}

/// Process-local query-embedding cache counters (leaders, followers, hits and
/// the actual provider operations), for verifying the one-embedding-per-query behaviour.
pub async fn get_query_embedding_cache_metrics(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<QueryEmbeddingCacheMetricsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let _ = organization.org_id;
    let stats = state.memory_engine.query_embedding_cache_stats();
    Ok(Json(query_embedding_cache_metrics_response(stats)))
}

pub async fn get_org_quotas(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<OrgQuotasQuery>,
) -> Result<Json<OrgQuotaStatusResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let result = state
        .memory_engine
        .get_org_quota_status(GetOrgQuotaStatusInput {
            org_id: organization.org_id,
            user_id: query.user_id,
        })
        .await?;

    Ok(Json(org_quota_status_response(result)))
}

pub async fn get_org_plan(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<GetOrgPlanResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let org_id = organization.org_id;
    let plan = state.org_plan_store.get_org_plan(&org_id).await?;
    let resolved = state
        .memory_engine
        .resolve_org_quota_limits(&org_id)
        .await?;

    Ok(Json(get_org_plan_response(plan, resolved)))
}

pub async fn upsert_org_plan(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(body): Json<UpsertOrgPlanRequest>,
) -> Result<Json<UpsertOrgPlanResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let org_id = organization.org_id;
    let existing = state.org_plan_store.get_org_plan(&org_id).await?;
    let plan = body.into_plan(org_id, existing.as_ref())?;
    let plan = state.org_plan_store.upsert_org_plan(plan).await?;

    Ok(Json(upsert_org_plan_response(plan)))
}

pub async fn delete_org_plan(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<DeleteOrgPlanResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let deleted = state
        .org_plan_store
        .delete_org_plan(&organization.org_id)
        .await?;

    Ok(Json(DeleteOrgPlanResponse {
        status: "success",
        deleted,
    }))
}

pub async fn get_provider_guardrails(
    State(state): State<AppState>,
    Extension(_organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
) -> Result<Json<ProviderGuardrailsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let snapshot = state.provider_guardrails.status_snapshot();
    Ok(Json(ProviderGuardrailsResponse {
        status: "success",
        enabled: snapshot.enabled,
        real_provider_calls_enabled: snapshot.real_provider_calls_enabled,
        test_mode: snapshot.test_mode,
        max_calls_per_run: snapshot.max_calls_per_run,
        used_calls: snapshot.used_calls,
        remaining_calls: snapshot.remaining_calls,
        max_input_chars: snapshot.max_input_chars,
        max_output_tokens: snapshot.max_output_tokens,
        max_retries_per_call: snapshot.max_retries_per_call,
        timeout_seconds: snapshot.timeout_seconds,
        allow_real_providers_during_load_tests: snapshot.allow_real_providers_during_load_tests,
        background_jobs_allow_real_providers: snapshot.background_jobs_allow_real_providers,
        multi_provider_validation_enabled: snapshot.multi_provider_validation_enabled,
    }))
}

pub async fn get_provider_usage(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<ProviderUsageQueryParams>,
) -> Result<Json<ProviderUsageResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let limit = validate_provider_usage_limit(query.limit)?;
    let capability = query
        .capability
        .as_deref()
        .map(parse_provider_usage_capability)
        .transpose()?;
    let (created_after, created_before) =
        parse_event_date_filters(query.created_after.as_ref(), query.created_before.as_ref())?;
    let cursor = parse_optional_cursor(query.cursor)?;

    let use_persistent = match query.source.as_deref() {
        Some("memory") => false,
        Some("persistent") => true,
        None => state.provider_usage_store.is_some(),
        Some(other) => {
            return Err(MemcoreError::ValidationError(format!("invalid source: {other}")).into());
        }
    };

    if use_persistent && let Some(store) = &state.provider_usage_store {
        let result = store
            .query_usage(ProviderUsageQuery {
                org_id: organization.org_id.clone(),
                user_id: query.user_id,
                provider_name: query.provider_name,
                model_name: query.model_name,
                capability,
                operation_name: query.operation_name,
                created_after,
                created_before,
                limit,
                cursor,
            })
            .await?;
        return Ok(Json(provider_usage_persisted_response(
            "persistent",
            result,
        )));
    }

    let snapshot = state.provider_usage.snapshot();
    Ok(Json(provider_usage_memory_response(snapshot)))
}

pub async fn get_org_usage_dashboard(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<OrgUsageDateRangeQuery>,
) -> Result<Json<OrgUsageDashboardResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let input = org_usage_dashboard_input(organization.org_id, query)?;
    let output = state.memory_engine.get_org_usage_dashboard(input).await?;

    Ok(Json(OrgUsageDashboardResponse::from(output)))
}

pub async fn create_memory_usage_snapshot(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    body: Option<Json<CreateMemoryUsageSnapshotRequest>>,
) -> Result<Json<CreateMemoryUsageSnapshotResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let body = body
        .map(|Json(body)| body)
        .unwrap_or(CreateMemoryUsageSnapshotRequest { captured_at: None });
    let output = state
        .memory_engine
        .create_memory_usage_snapshot(body.into_input(organization.org_id)?)
        .await?;

    Ok(Json(CreateMemoryUsageSnapshotResponse::from(output)))
}

pub async fn query_memory_usage_snapshots(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<QueryMemoryUsageSnapshotsParams>,
) -> Result<Json<QueryMemoryUsageSnapshotsResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let input = query_memory_usage_snapshots_input(organization.org_id, query)?;
    let output = state
        .memory_engine
        .query_memory_usage_snapshots(input)
        .await?;

    Ok(Json(QueryMemoryUsageSnapshotsResponse::from(output)))
}

pub async fn get_provider_usage_daily(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<ProviderUsageDailyQueryParams>,
) -> Result<Json<ProviderUsageDailyResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminRead, ApiKeyScope::AdminWrite],
    )?;

    let capability = query
        .capability
        .as_deref()
        .map(parse_provider_usage_capability)
        .transpose()?;
    let (created_after, created_before) = parse_org_usage_window(
        query.created_after.as_ref(),
        query.created_before.as_ref(),
        query.days,
    )?;

    let output = state
        .memory_engine
        .get_provider_usage_daily(ProviderUsageDailyInput {
            org_id: organization.org_id,
            created_after,
            created_before,
            provider_name: query.provider_name,
            model_name: query.model_name,
            capability,
        })
        .await?;

    Ok(Json(ProviderUsageDailyResponse::from(output)))
}

pub async fn apply_provider_usage_retention(
    State(state): State<AppState>,
    Extension(organization): Extension<OrganizationContext>,
    auth: Option<Extension<AuthContext>>,
    Json(body): Json<crate::dto::ApplyProviderUsageRetentionRequest>,
) -> Result<Json<crate::dto::ApplyProviderUsageRetentionResponse>, ApiError> {
    check_any_scope(
        auth.as_ref().map(|extension| &extension.0),
        &[ApiKeyScope::AdminWrite],
    )?;

    let output = state
        .memory_engine
        .apply_provider_usage_retention(body.into_input(organization.org_id, &state.settings))
        .await?;

    Ok(Json(output.into()))
}

fn parse_fact_id(value: &str) -> Result<Uuid, MemcoreError> {
    Uuid::parse_str(value.trim())
        .map_err(|_| MemcoreError::ValidationError("invalid fact_id".to_string()))
}
