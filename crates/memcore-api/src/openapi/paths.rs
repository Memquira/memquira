//! OpenAPI path definitions (documentation-only stubs; handlers live in `routes`).

#![allow(dead_code)]

use crate::dto::{
    AddMemoryRequest, AddMemoryResponse, ApplyBackgroundJobRunRetentionRequest,
    ApplyBackgroundJobRunRetentionResponse, ApplyProviderUsageRetentionRequest,
    ApplyProviderUsageRetentionResponse, ApplyRetentionRequest, ApplyRetentionResponse,
    BackgroundJobsResponse, BuildContextRequest, BuildContextResponse, ContextCacheMetricsResponse,
    CreateApiKeyRequest, CreateApiKeyResponse, CreateMemoryUsageSnapshotRequest,
    CreateMemoryUsageSnapshotResponse, DeleteMemoryResponse, DeleteOrgPlanResponse,
    EmbeddingDiagnosticsResponse, ExportUserResponse, ForgetUserResponse, GetOrgPlanResponse,
    ImportUserDataRequest, ImportUserDataResponse, ListApiKeysResponse, ListMemoriesResponse,
    ListMemoryEventsResponse, ListOrgUsersResponse, OrgQuotaStatusResponse, OrgSummaryResponse,
    OrgUsageDashboardResponse, ProviderUsageDailyResponse, ProviderUsageResponse,
    QueryBackgroundJobRunsResponse, QueryEmbeddingCacheMetricsResponse,
    QueryMemoryUsageSnapshotsResponse, RevokeApiKeyResponse, RunBackgroundJobResponse,
    SearchEpisodesRequest, SearchEpisodesResponse, SearchMemoryRequest, SearchMemoryResponse,
    SearchOrgMemoryEventsResponse, UpsertOrgPlanRequest, UpsertOrgPlanResponse,
};
use crate::dto::{EntityDetailResponse, ListEntitiesResponse};
use crate::dto::{
    MemoryContextRequest, MemoryContextResponse, SearchCandidatesRequest, SearchCandidatesResponse,
};
use crate::response::ErrorBody;
use crate::routes::health::{HealthReadyResponse, HealthResponse, ReadyResponse, VersionResponse};

/// Liveness probe.
#[utoipa::path(
    get,
    path = "/health",
    tag = "Health",
    responses(
        (status = 200, description = "Service is alive", body = HealthResponse),
    )
)]
pub fn health() {}

/// Readiness probe with configured backend labels.
#[utoipa::path(
    get,
    path = "/ready",
    tag = "Health",
    responses(
        (status = 200, description = "Service is ready", body = ReadyResponse),
    )
)]
pub fn ready() {}

/// Liveness probe for orchestrators: the process is up. Touches no dependency.
#[utoipa::path(
    get,
    path = "/health/live",
    tag = "Health",
    responses(
        (status = 200, description = "Process is alive", body = HealthResponse),
    )
)]
pub fn health_live() {}

/// Readiness probe for orchestrators: database startup state, Qdrant reachability, provider
/// configuration and shutdown draining. Never calls an LLM or embedding provider.
#[utoipa::path(
    get,
    path = "/health/ready",
    tag = "Health",
    responses(
        (status = 200, description = "Ready to serve requests", body = HealthReadyResponse),
        (status = 503, description = "Not ready or draining", body = HealthReadyResponse),
    )
)]
pub fn health_ready() {}

/// Prometheus scrape endpoint (path configurable; auth optional).
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "Metrics",
    responses(
        (status = 200, description = "Prometheus text metrics when MEMCORE_METRICS_ENABLED=true", content_type = "text/plain", body = String),
        (status = 401, description = "Unauthorized when metrics auth is required"),
        (status = 404, description = "Metrics endpoint disabled"),
    )
)]
pub fn metrics() {}

/// Safe build/version metadata (no secrets, no runtime env dump).
#[utoipa::path(
    get,
    path = "/api/v1/version",
    tag = "Health",
    responses(
        (status = 200, description = "Package and build metadata", body = VersionResponse),
    )
)]
pub fn version() {}

/// Extract and store memories from a user message conversation.
#[utoipa::path(
    post,
    path = "/api/v1/memories",
    tag = "Memories",
    request_body = AddMemoryRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Memories processed", body = AddMemoryResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 409, description = "MESSAGE_ID_CONFLICT: a message_id in the request already identifies a different message in this organization, user and session; nothing from the request was stored", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn add_memory() {}

/// Semantic search over stored memories.
#[utoipa::path(
    post,
    path = "/api/v1/memories/search",
    tag = "Memories",
    request_body = SearchMemoryRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Search results", body = SearchMemoryResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn search_memory() {}

/// Semantic search over a user's episodes (source-message-level memory). Requires
/// `MEMCORE_EPISODES_ENABLED`; `limit` follows the memory search rules. No full-text leg, reranking
/// or query rewriting.
#[utoipa::path(
    post,
    path = "/api/v1/episodes/search",
    tag = "Memories",
    request_body = SearchEpisodesRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Episodes ordered by vector similarity", body = SearchEpisodesResponse),
        (status = 400, description = "Validation error, or episodes are disabled", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn search_episodes() {}

/// EXPERIMENTAL: one user's facts and episodes as a single candidate list, ordered
/// by raw vector similarity with deterministic exact-tie rules (the E-merge order). Requires
/// `MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED` and `MEMCORE_EPISODES_ENABLED`. Not a stable retrieval
/// contract; it may change or be removed.
#[utoipa::path(
    post,
    path = "/api/v1/memories/candidates",
    tag = "Memories",
    request_body = SearchCandidatesRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Candidates in E-merge order", body = SearchCandidatesResponse),
        (status = 400, description = "Validation error, or the endpoint or episodes are disabled", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn search_candidates() {}

/// The validated server-side context for one user, rendered by the server: semantic facts @100
/// and episodes @100 merged by raw vector score with deterministic exact-tie rules (facts only when
/// episodes are disabled), lines `"<timestamp>: <text>"`, and the cl100k_base rank-order prefix at
/// `token_budget` (default 1,500): the first line that does not fit ends the context. Requires
/// `MEMCORE_SEARCH_MAX_LIMIT >= 100`.
#[utoipa::path(
    post,
    path = "/api/v1/memories/context",
    tag = "Memories",
    request_body = MemoryContextRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "The rendered context", body = MemoryContextResponse),
        (status = 400, description = "Validation error (budget, render mode, search depth limit)", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn memory_context() {}

/// Build a formatted context string from relevant memories.
#[utoipa::path(
    post,
    path = "/api/v1/context",
    tag = "Context",
    request_body = BuildContextRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Context assembled", body = BuildContextResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn build_context() {}

/// List memories for a user (forward-only opaque cursor pagination).
#[utoipa::path(
    get,
    path = "/api/v1/users/{user_id}/memories",
    tag = "Memories",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("memory_type" = Option<String>, Query, description = "Filter by memory type (PascalCase label)"),
        ("q" = Option<String>, Query, description = "Case-insensitive keyword search over content and summary (max 200 chars)"),
        ("limit" = Option<usize>, Query, description = "Maximum results to return"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a previous next_cursor"),
        ("include_deleted" = Option<bool>, Query, description = "Include soft-deleted memories"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "User memories", body = ListMemoriesResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn list_user_memories() {}

/// List a user's entities, newest first (requires
/// `MEMCORE_ENTITIES_ENABLED`). Forward-only opaque cursor pagination.
#[utoipa::path(
    get,
    path = "/api/v1/users/{user_id}/entities",
    tag = "Entities",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("limit" = Option<usize>, Query, description = "Maximum results (default 50, max 100)"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a previous next_cursor"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "User entities", body = ListEntitiesResponse),
        (status = 400, description = "Validation error, or entities are disabled", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn list_user_entities() {}

/// One of a user's entities with its linked facts (not deleted) and relations.
#[utoipa::path(
    get,
    path = "/api/v1/users/{user_id}/entities/{entity_id}",
    tag = "Entities",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("entity_id" = String, Path, description = "Entity identifier (UUID)"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Entity with linked facts and relations", body = EntityDetailResponse),
        (status = 400, description = "Validation error, or entities are disabled", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 404, description = "No such entity for this user", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_user_entity() {}

/// Soft-delete a single memory for a user.
#[utoipa::path(
    delete,
    path = "/api/v1/users/{user_id}/memories/{memory_id}",
    tag = "Memories",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("memory_id" = String, Path, description = "Memory (fact) UUID"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Memory deleted. Also returned for a retry of a memory this user already deleted, once any remaining search-index cleanup is complete", body = DeleteMemoryResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 404, description = "Memory not found: unknown, or another user's or organization's (indistinguishable)", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn delete_user_memory() {}

/// Export memory facts and optional audit events for a user as JSON. Does not include API keys or event `input_text`.
#[utoipa::path(
    get,
    path = "/api/v1/users/{user_id}/export",
    tag = "Users",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("include_events" = Option<bool>, Query, description = "Include memory audit events (default true)"),
        ("include_deleted" = Option<bool>, Query, description = "Include soft-deleted facts (default false)"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "User memory export", body = ExportUserResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing required scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn export_user_data() {}

/// Import user memory data from a `memcore.user_export.v1` JSON export. Set `dry_run=true` to validate without writes.
/// An exported fact id is kept only when this user already holds it (live or deleted); any other
/// fact is stored under an id derived from the organization, the user and the exported id, so restored ids can
/// differ from the export (see `docs/BACKUP_EXPORT.md`).
#[utoipa::path(
    post,
    path = "/api/v1/users/{user_id}/import",
    tag = "Users",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    request_body = ImportUserDataRequest,
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Import completed or dry-run validation summary", body = ImportUserDataResponse),
        (status = 400, description = "Validation error, nothing imported. Includes VALIDATION_ERROR with a message starting DUPLICATE_FACT_ID: two exported facts share one id, or would be stored under one id", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing required scope in database auth mode", body = ErrorBody),
        (status = 409, description = "mode=append only, checked before any write, nothing imported: IMPORT_ALREADY_APPLIED (a fact is already stored for this user under the id the import would use, with identical content) or IMPORT_ID_CONFLICT (that id belongs to a fact this user deleted)", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn import_user_data() {}

/// Apply user-scoped retention cleanup for facts and audit events. `dry_run` defaults to `true`.
#[utoipa::path(
    post,
    path = "/api/v1/users/{user_id}/retention/apply",
    tag = "Users",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    request_body = ApplyRetentionRequest,
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Retention dry-run or apply summary", body = ApplyRetentionResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing required scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn apply_retention() {}

/// Delete all memories and vectors for a user within the organization.
#[utoipa::path(
    delete,
    path = "/api/v1/users/{user_id}",
    tag = "Users",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "User data forgotten", body = ForgetUserResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn forget_user() {}

/// List memory audit events for a user. `input_text` is not exposed. Forward-only opaque cursor pagination.
#[utoipa::path(
    get,
    path = "/api/v1/users/{user_id}/memory-events",
    tag = "Memory Events",
    params(
        ("user_id" = String, Path, description = "User identifier"),
        ("operation" = Option<String>, Query, description = "Filter by operation (PascalCase label)"),
        ("fact_id" = Option<String>, Query, description = "Filter by fact UUID"),
        ("created_after" = Option<String>, Query, description = "Inclusive RFC3339 lower bound on created_at"),
        ("created_before" = Option<String>, Query, description = "Exclusive RFC3339 upper bound on created_at"),
        ("q" = Option<String>, Query, description = "Case-insensitive keyword search over event fields (max 200 chars; does not search input_text)"),
        ("limit" = Option<usize>, Query, description = "Maximum results to return"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a previous next_cursor"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Audit events", body = ListMemoryEventsResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn list_user_memory_events() {}

/// Create an API key for the organization. Returns the raw key once; it is never returned again in list responses.
#[utoipa::path(
    post,
    path = "/api/v1/api-keys",
    tag = "API Keys",
    request_body = CreateApiKeyRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "API key created (raw_key shown only in this response)", body = CreateApiKeyResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn create_api_key() {}

/// List API keys for the organization. Does not include raw keys or key hashes.
#[utoipa::path(
    get,
    path = "/api/v1/api-keys",
    tag = "API Keys",
    params(
        ("include_revoked" = Option<bool>, Query, description = "Include revoked keys when true"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization API keys", body = ListApiKeysResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn list_api_keys() {}

/// Revoke an API key. Revoked keys can no longer authenticate.
#[utoipa::path(
    delete,
    path = "/api/v1/api-keys/{api_key_id}",
    tag = "API Keys",
    params(
        ("api_key_id" = String, Path, description = "API key UUID"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "API key revoked", body = RevokeApiKeyResponse),
        (status = 400, description = "Invalid api_key_id", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 404, description = "API key not found in organization", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn revoke_api_key() {}

/// Organization-level aggregate counts for admin visibility. Does not return memory content.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/summary",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization summary", body = OrgSummaryResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_org_summary() {}

/// List in-process background job definitions and recent process-local runs.
#[utoipa::path(
    get,
    path = "/api/v1/admin/jobs",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Background job status", body = BackgroundJobsResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_background_jobs() {}

/// List persisted background job run history.
#[utoipa::path(
    get,
    path = "/api/v1/admin/jobs/runs",
    tag = "Admin",
    params(
        ("kind" = Option<String>, Query, description = "Optional job kind filter"),
        ("status" = Option<String>, Query, description = "Optional job status filter"),
        ("created_after" = Option<String>, Query, description = "Optional RFC3339 lower bound on started_at"),
        ("created_before" = Option<String>, Query, description = "Optional RFC3339 upper bound on started_at"),
        ("limit" = Option<usize>, Query, description = "Page size (default 50, max 100)"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Background job run history", body = QueryBackgroundJobRunsResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn query_background_job_runs() {}

/// Apply retention cleanup to persisted background job run history only.
#[utoipa::path(
    post,
    path = "/api/v1/admin/jobs/runs/retention/apply",
    tag = "Admin",
    request_body = ApplyBackgroundJobRunRetentionRequest,
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Background job run history retention summary", body = ApplyBackgroundJobRunRetentionResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn apply_background_job_run_retention() {}

/// Manually run one registered background job once.
#[utoipa::path(
    post,
    path = "/api/v1/admin/jobs/{job_kind}/run",
    tag = "Admin",
    params(
        ("job_kind" = String, Path, description = "Job kind: memory-usage-snapshot, provider-usage-retention, or memory-retention"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Background job run result", body = RunBackgroundJobResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn run_background_job() {}

/// List users in the organization with memory aggregates. Does not return memory content.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/users",
    tag = "Admin",
    params(
        ("limit" = Option<usize>, Query, description = "Page size (default 50, max 100)"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a previous next_cursor"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization users", body = ListOrgUsersResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn list_org_users() {}

/// Search memory audit events across the organization. Does not return input_text.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/memory-events",
    tag = "Admin",
    params(
        ("user_id" = Option<String>, Query, description = "Filter by user id"),
        ("fact_id" = Option<String>, Query, description = "Filter by fact UUID"),
        ("operation" = Option<String>, Query, description = "Filter by operation (Add, Update, Delete, NoOp, ForgetUser)"),
        ("created_after" = Option<String>, Query, description = "Inclusive RFC3339 lower bound on created_at"),
        ("created_before" = Option<String>, Query, description = "Exclusive RFC3339 upper bound on created_at"),
        ("q" = Option<String>, Query, description = "Case-insensitive keyword search over event fields and user_id (max 200 chars; does not search input_text)"),
        ("limit" = Option<usize>, Query, description = "Page size (default 50, max 100)"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a previous next_cursor"),
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization memory audit events", body = SearchOrgMemoryEventsResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead, AdminWrite, or AuditRead scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn search_org_memory_events() {}

/// Process-local aggregate context cache counters for debugging and operations.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/cache/context/metrics",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Process-local context cache metrics", body = ContextCacheMetricsResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_context_cache_metrics() {}

/// Process-local query-embedding cache counters.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/cache/query-embedding/metrics",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Process-local query-embedding cache metrics", body = QueryEmbeddingCacheMetricsResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_query_embedding_cache_metrics() {}

/// The embedding provider's process-local diagnostics.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/embedding/diagnostics",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Process-local embedding provider diagnostics", body = EmbeddingDiagnosticsResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_embedding_diagnostics() {}

/// Organization quota status and configured limits.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/quotas",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
        ("user_id" = Option<String>, Query, description = "Optional user id for per-user memory count"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization quota status", body = OrgQuotaStatusResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_org_quotas() {}

/// Current organization plan configuration and resolved quota defaults.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/plan",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization plan configuration", body = GetOrgPlanResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_org_plan() {}

/// Create or update the current organization's plan configuration.
#[utoipa::path(
    put,
    path = "/api/v1/admin/org/plan",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    request_body = UpsertOrgPlanRequest,
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization plan upserted", body = UpsertOrgPlanResponse),
        (status = 400, description = "Invalid tier, limits, or metadata", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn upsert_org_plan() {}

/// Delete the current organization's plan configuration.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/org/plan",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization plan deletion result", body = DeleteOrgPlanResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn delete_org_plan() {}

/// Provider usage events (persistent store) or process-local aggregates (`source=memory`).
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/provider-usage",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
        ("user_id" = Option<String>, Query, description = "Filter by user id"),
        ("provider_name" = Option<String>, Query, description = "Filter by provider name"),
        ("model_name" = Option<String>, Query, description = "Filter by model name"),
        ("capability" = Option<String>, Query, description = "Filter by capability: llm, embedding, summarization"),
        ("operation_name" = Option<String>, Query, description = "Filter by operation name"),
        ("created_after" = Option<String>, Query, description = "Inclusive lower bound (RFC3339)"),
        ("created_before" = Option<String>, Query, description = "Exclusive upper bound (RFC3339)"),
        ("limit" = Option<usize>, Query, description = "Page size (default 50, max 100)"),
        ("cursor" = Option<String>, Query, description = "Pagination cursor"),
        ("source" = Option<String>, Query, description = "persistent (default when store configured) or memory"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization provider usage", body = ProviderUsageResponse),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_provider_usage() {}

/// Dashboard-ready organization usage summary over a UTC time window.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/usage/dashboard",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
        ("created_after" = Option<String>, Query, description = "Inclusive RFC3339 lower bound. Must be paired with created_before."),
        ("created_before" = Option<String>, Query, description = "Exclusive RFC3339 upper bound. Must be paired with created_after."),
        ("days" = Option<u32>, Query, description = "Relative UTC window ending now (default 30, max 90). Ignored when both timestamps are provided."),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Organization usage dashboard", body = OrgUsageDashboardResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_org_usage_dashboard() {}

/// Create an org-scoped memory usage snapshot from current aggregate counts.
#[utoipa::path(
    post,
    path = "/api/v1/admin/org/usage/memory/snapshots",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    request_body = CreateMemoryUsageSnapshotRequest,
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Created memory usage snapshot", body = CreateMemoryUsageSnapshotResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn create_memory_usage_snapshot() {}

/// List org-scoped memory usage snapshots. Does not expose memory content.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/usage/memory/snapshots",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
        ("created_after" = Option<String>, Query, description = "Inclusive RFC3339 lower bound"),
        ("created_before" = Option<String>, Query, description = "Exclusive RFC3339 upper bound"),
        ("limit" = Option<usize>, Query, description = "Page size (default 50, max 100)"),
        ("cursor" = Option<String>, Query, description = "Pagination cursor"),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Memory usage snapshots", body = QueryMemoryUsageSnapshotsResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn query_memory_usage_snapshots() {}

/// Daily provider usage buckets over a UTC time window. Does not expose prompts or memory content.
#[utoipa::path(
    get,
    path = "/api/v1/admin/org/usage/provider/daily",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
        ("provider_name" = Option<String>, Query, description = "Filter by provider name"),
        ("model_name" = Option<String>, Query, description = "Filter by model name"),
        ("capability" = Option<String>, Query, description = "Filter by capability: llm, embedding, summarization"),
        ("created_after" = Option<String>, Query, description = "Inclusive RFC3339 lower bound. Must be paired with created_before."),
        ("created_before" = Option<String>, Query, description = "Exclusive RFC3339 upper bound. Must be paired with created_after."),
        ("days" = Option<u32>, Query, description = "Relative UTC window ending now (default 30, max 90). Ignored when both timestamps are provided."),
    ),
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Daily provider usage buckets", body = ProviderUsageDailyResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminRead or AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn get_provider_usage_daily() {}

/// Apply org-scoped provider usage event retention cleanup. `dry_run` defaults to `true`.
#[utoipa::path(
    post,
    path = "/api/v1/admin/org/provider-usage/retention/apply",
    tag = "Admin",
    params(
        ("X-Organization-ID" = String, Header, description = "Organization tenant id", example = "org_123"),
        ("X-Request-ID" = Option<String>, Header, description = "Optional request correlation id"),
    ),
    request_body = ApplyProviderUsageRetentionRequest,
    security(("BearerAuth" = [])),
    responses(
        (status = 200, description = "Provider usage retention dry-run or apply summary", body = ApplyProviderUsageRetentionResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 403, description = "Missing AdminWrite scope in database auth mode", body = ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = ErrorBody),
    )
)]
pub fn apply_provider_usage_retention() {}
