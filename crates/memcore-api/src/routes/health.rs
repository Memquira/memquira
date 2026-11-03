use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use memcore_config::{Environment, FactBackend, ProviderStartupMode, StorageMode, VectorBackend};
use memcore_storage::ProbeOutcome;
use serde::Serialize;
use utoipa::ToSchema;

use crate::startup::{Dependency, probe_dependency};
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReadyResponse {
    pub status: String,
    pub environment: String,
    pub storage_mode: String,
    pub vector_backend: String,
    pub fact_backend: String,
    pub checks: ReadyChecks,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReadyChecks {
    pub database: DatabaseReadyCheck,
    pub providers: ProvidersReadyCheck,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DatabaseReadyCheck {
    pub connected: bool,
    pub migrations_clean: bool,
    pub applied_migrations: Option<usize>,
    pub pending_migrations: Option<usize>,
    pub warning_count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProvidersReadyCheck {
    pub configured: bool,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "memcore",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VersionResponse {
    pub status: &'static str,
    pub version: VersionInfo,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VersionInfo {
    pub package_version: &'static str,
    pub git_sha: &'static str,
    pub build_timestamp: &'static str,
    pub rustc_version: &'static str,
    pub profile: &'static str,
}

/// Public build metadata. Never reads process environment at request time.
pub async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        status: "success",
        version: VersionInfo {
            package_version: env!("CARGO_PKG_VERSION"),
            git_sha: env!("MEMCORE_BUILD_GIT_SHA"),
            build_timestamp: env!("MEMCORE_BUILD_TIMESTAMP"),
            rustc_version: env!("MEMCORE_BUILD_RUSTC_VERSION"),
            profile: env!("MEMCORE_BUILD_PROFILE"),
        },
    })
}

pub async fn ready(State(state): State<AppState>) -> Json<ReadyResponse> {
    let settings = &state.settings;

    let storage = &state.storage_startup;
    let migration_report = storage.migration_report.as_ref();
    let database_connected = database_connected_now(&state).await;
    let ready = database_connected && storage.migrations_clean;

    Json(ReadyResponse {
        status: if ready { "ready" } else { "not_ready" }.to_string(),
        environment: environment_label(&settings.environment).to_string(),
        storage_mode: storage_mode_label(&settings.storage_mode).to_string(),
        vector_backend: vector_backend_label(&settings.vector_backend).to_string(),
        fact_backend: fact_backend_label(&settings.fact_backend).to_string(),
        checks: ReadyChecks {
            database: DatabaseReadyCheck {
                connected: database_connected,
                migrations_clean: storage.migrations_clean,
                applied_migrations: migration_report.map(|report| report.applied_count),
                pending_migrations: migration_report.map(|report| report.pending_count),
                warning_count: storage.warnings.len(),
            },
            providers: ProvidersReadyCheck { configured: true },
        },
    })
}

/// Liveness (`GET /health/live`): the process is up and serving HTTP. Touches no dependency.
pub async fn live() -> Json<HealthResponse> {
    health().await
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthReadyResponse {
    /// `ready`, `not_ready`, or `draining` (shutdown started).
    pub status: String,
    pub draining: bool,
    pub checks: HealthReadyChecks,
    /// The runtime quality tier and what it rests on.
    pub quality: crate::quality::QualityStatus,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthReadyChecks {
    pub database: DatabaseReadyCheck,
    pub vector_store: VectorStoreReadyCheck,
    pub providers: ProviderModeCheck,
    /// closure: ingestion recovery (omitted without an ingest journal). Informational:
    /// it never makes the service unready; alert on `stalled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_recovery: Option<IngestRecoveryCheck>,
}

/// closure: the ingest journal backlog.
#[derive(Debug, Serialize, ToSchema)]
pub struct IngestRecoveryCheck {
    /// `idle`, `recovering` (normal backlog: in-flight requests, rollbacks within their window),
    /// `stalled` (an operation older than three stale windows or claimed three times) or
    /// `unavailable` (the journal could not be read).
    pub state: String,
    pub pending: usize,
    pub rolling_back: usize,
    pub oldest_age_seconds: Option<u64>,
    pub max_attempts: u32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VectorStoreReadyCheck {
    pub backend: String,
    /// Live reachability for network backends (Qdrant); `null` when nothing needs probing.
    pub reachable: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderModeCheck {
    pub ready: bool,
    /// `real`, `mock_allowed` (`MEMCORE_ALLOW_MOCK=true`), or `refused`.
    pub mode: String,
    pub llm_provider: String,
    pub embedding_provider: String,
}

/// The startup check AND, for PostgreSQL, a live bounded `SELECT 1`
/// ([`memcore_storage::DatabaseReadinessProbe`]). SQLite and mock storage keep the startup check.
async fn database_connected_now(state: &AppState) -> bool {
    let probe = &state.database_readiness;
    let live = if probe.is_live() {
        // A check that does not finish within the probe timeout counts as "not answering".
        tokio::time::timeout(crate::startup::PROBE_TIMEOUT, probe.check())
            .await
            .unwrap_or(Some(false))
    } else {
        None
    };
    if live == Some(false) {
        tracing::debug!("readiness: database did not answer the live check");
    }
    state.storage_startup.database_connected && live.unwrap_or(true)
}

/// Readiness (`GET /health/ready`): `200` when the server can serve requests, `503` otherwise.
/// Checks the database startup state plus a live bounded database check (PostgreSQL), a
/// bounded Qdrant reachability probe, the provider configuration and shutdown draining. Never calls
/// an LLM or embedding provider.
pub async fn health_ready(
    State(state): State<AppState>,
) -> (StatusCode, Json<HealthReadyResponse>) {
    let settings = &state.settings;
    let storage = &state.storage_startup;
    let migration_report = storage.migration_report.as_ref();
    let draining = state.shutdown_token.is_cancelled();

    // The vector-store and database probes run concurrently, so one request stays within one
    // probe timeout.
    let vector_probe = async {
        match settings.vector_backend {
            VectorBackend::Qdrant if !settings.qdrant_url.trim().is_empty() => {
                let dependency = Dependency::Qdrant(settings.qdrant_url.trim().to_string());
                let outcome = probe_dependency(&dependency).await;
                if outcome != ProbeOutcome::Ready {
                    tracing::debug!(?outcome, "readiness: vector store not reachable");
                }
                Some(outcome == ProbeOutcome::Ready)
            }
            _ => None,
        }
    };
    let (reachable, database_connected, recovery) = tokio::join!(
        vector_probe,
        database_connected_now(&state),
        state.memory_engine.ingest_recovery_status()
    );
    let ingest_recovery = match recovery {
        Ok(status) => status.map(|s| IngestRecoveryCheck {
            state: s.state.to_string(),
            pending: s.pending,
            rolling_back: s.rolling_back,
            oldest_age_seconds: s.oldest_age_seconds,
            max_attempts: s.max_attempts,
        }),
        Err(_) => None,
    };

    let (provider_ready, mode) = match settings.check_startup_providers() {
        Ok(ProviderStartupMode::Real) => (true, "real"),
        Ok(ProviderStartupMode::MockAllowed) => (true, "mock_allowed"),
        Err(_) => (false, "refused"),
    };

    let database_ready = database_connected && storage.migrations_clean;
    let ready = !draining && database_ready && reachable != Some(false) && provider_ready;
    let status = if draining {
        "draining"
    } else if ready {
        "ready"
    } else {
        "not_ready"
    };

    let body = HealthReadyResponse {
        status: status.to_string(),
        draining,
        quality: state.quality.clone(),
        checks: HealthReadyChecks {
            database: DatabaseReadyCheck {
                connected: database_connected,
                migrations_clean: storage.migrations_clean,
                applied_migrations: migration_report.map(|report| report.applied_count),
                pending_migrations: migration_report.map(|report| report.pending_count),
                warning_count: storage.warnings.len(),
            },
            vector_store: VectorStoreReadyCheck {
                backend: vector_backend_label(&settings.vector_backend).to_string(),
                reachable,
            },
            providers: ProviderModeCheck {
                ready: provider_ready,
                mode: mode.to_string(),
                llm_provider: format!("{:?}", settings.llm_provider).to_ascii_lowercase(),
                embedding_provider: format!("{:?}", settings.embedding_provider)
                    .to_ascii_lowercase(),
            },
            ingest_recovery,
        },
    };
    let code = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
}

fn environment_label(environment: &Environment) -> &'static str {
    match environment {
        Environment::Development => "development",
        Environment::Production => "production",
    }
}

fn storage_mode_label(storage_mode: &StorageMode) -> &'static str {
    match storage_mode {
        StorageMode::Embedded => "embedded",
        StorageMode::Production => "production",
    }
}

fn vector_backend_label(vector_backend: &VectorBackend) -> &'static str {
    match vector_backend {
        VectorBackend::Mock => "mock",
        VectorBackend::LanceDb => "lancedb",
        VectorBackend::Qdrant => "qdrant",
    }
}

fn fact_backend_label(fact_backend: &FactBackend) -> &'static str {
    match fact_backend {
        FactBackend::Mock => "mock",
        FactBackend::Sqlite => "sqlite",
        FactBackend::Postgres => "postgres",
    }
}
