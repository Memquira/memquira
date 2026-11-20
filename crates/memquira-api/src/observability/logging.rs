use memquira_common::MemquiraResult;
use memquira_config::{LogFormat, Settings};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_logging(settings: &Settings) -> MemquiraResult<()> {
    let level = settings.log_level.as_filter_str();
    let filter = EnvFilter::try_new(format!("{level},memquira_api={level},tower_http=warn"))
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(format!("{level},memquira_api={level}")));

    let registry = tracing_subscriber::registry().with(filter);

    match settings.log_format {
        LogFormat::Json => {
            registry.with(fmt::layer().json()).init();
        }
        LogFormat::Pretty => {
            registry.with(fmt::layer().pretty()).init();
        }
    }

    Ok(())
}

pub fn log_startup(settings: &Settings) {
    tracing::info!(
        service = "memquira",
        version = env!("CARGO_PKG_VERSION"),
        environment = environment_label(&settings.environment),
        host = %settings.host,
        port = settings.port,
        storage_mode = storage_mode_label(&settings.storage_mode),
        fact_backend = fact_backend_label(&settings.fact_backend),
        vector_backend = vector_backend_label(&settings.vector_backend),
        llm_provider = llm_provider_label(&settings.llm_provider),
        embedding_provider = embedding_provider_label(&settings.embedding_provider),
        log_format = log_format_label(settings.log_format),
        log_level = settings.log_level.as_filter_str(),
        metrics_enabled = settings.metrics_enabled,
        rate_limit_enabled = settings.rate_limit_enabled,
        security_headers_enabled = settings.security_headers_enabled,
        max_request_body_bytes = settings.max_request_body_bytes,
        cors_enabled = settings.cors_enabled,
        auth_mode = ?settings.auth_mode,
        openai_api_key_configured = settings
            .openai_api_key
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        postgres_url_configured = settings
            .postgres_url
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        redis_url_configured = settings
            .redis_url
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        "memquira-api starting"
    );
    // Names only, never values.
    for warning in memquira_common::env::legacy_report().deprecation_warnings() {
        tracing::warn!(deprecated_config = true, "{warning}");
    }
}

fn environment_label(environment: &memquira_config::Environment) -> &'static str {
    match environment {
        memquira_config::Environment::Development => "development",
        memquira_config::Environment::Production => "production",
    }
}

fn storage_mode_label(storage_mode: &memquira_config::StorageMode) -> &'static str {
    match storage_mode {
        memquira_config::StorageMode::Embedded => "embedded",
        memquira_config::StorageMode::Production => "production",
    }
}

fn fact_backend_label(fact_backend: &memquira_config::FactBackend) -> &'static str {
    match fact_backend {
        memquira_config::FactBackend::Mock => "mock",
        memquira_config::FactBackend::Sqlite => "sqlite",
        memquira_config::FactBackend::Postgres => "postgres",
    }
}

fn vector_backend_label(vector_backend: &memquira_config::VectorBackend) -> &'static str {
    match vector_backend {
        memquira_config::VectorBackend::Mock => "mock",
        memquira_config::VectorBackend::LanceDb => "lancedb",
        memquira_config::VectorBackend::Qdrant => "qdrant",
    }
}

fn llm_provider_label(llm_provider: &memquira_config::LlmProviderKind) -> &'static str {
    match llm_provider {
        memquira_config::LlmProviderKind::Mock => "mock",
        memquira_config::LlmProviderKind::OpenAi => "openai",
        memquira_config::LlmProviderKind::OpenRouter => "openrouter",
        memquira_config::LlmProviderKind::Anthropic => "anthropic",
        memquira_config::LlmProviderKind::Groq => "groq",
    }
}

fn embedding_provider_label(
    embedding_provider: &memquira_config::EmbeddingProviderKind,
) -> &'static str {
    match embedding_provider {
        memquira_config::EmbeddingProviderKind::Mock => "mock",
        memquira_config::EmbeddingProviderKind::OpenAi => "openai",
    }
}

fn log_format_label(log_format: LogFormat) -> &'static str {
    match log_format {
        LogFormat::Json => "json",
        LogFormat::Pretty => "pretty",
    }
}
