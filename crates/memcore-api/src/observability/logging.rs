use memcore_common::MemcoreResult;
use memcore_config::{LogFormat, Settings};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_logging(settings: &Settings) -> MemcoreResult<()> {
    let level = settings.log_level.as_filter_str();
    let filter = EnvFilter::try_new(format!("{level},memcore_api={level},tower_http=warn"))
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(format!("{level},memcore_api={level}")));

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
        service = "memcore",
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
        "memcore-api starting"
    );
}

fn environment_label(environment: &memcore_config::Environment) -> &'static str {
    match environment {
        memcore_config::Environment::Development => "development",
        memcore_config::Environment::Production => "production",
    }
}

fn storage_mode_label(storage_mode: &memcore_config::StorageMode) -> &'static str {
    match storage_mode {
        memcore_config::StorageMode::Embedded => "embedded",
        memcore_config::StorageMode::Production => "production",
    }
}

fn fact_backend_label(fact_backend: &memcore_config::FactBackend) -> &'static str {
    match fact_backend {
        memcore_config::FactBackend::Mock => "mock",
        memcore_config::FactBackend::Sqlite => "sqlite",
        memcore_config::FactBackend::Postgres => "postgres",
    }
}

fn vector_backend_label(vector_backend: &memcore_config::VectorBackend) -> &'static str {
    match vector_backend {
        memcore_config::VectorBackend::Mock => "mock",
        memcore_config::VectorBackend::LanceDb => "lancedb",
        memcore_config::VectorBackend::Qdrant => "qdrant",
    }
}

fn llm_provider_label(llm_provider: &memcore_config::LlmProviderKind) -> &'static str {
    match llm_provider {
        memcore_config::LlmProviderKind::Mock => "mock",
        memcore_config::LlmProviderKind::OpenAi => "openai",
        memcore_config::LlmProviderKind::OpenRouter => "openrouter",
        memcore_config::LlmProviderKind::Anthropic => "anthropic",
        memcore_config::LlmProviderKind::Groq => "groq",
    }
}

fn embedding_provider_label(
    embedding_provider: &memcore_config::EmbeddingProviderKind,
) -> &'static str {
    match embedding_provider {
        memcore_config::EmbeddingProviderKind::Mock => "mock",
        memcore_config::EmbeddingProviderKind::OpenAi => "openai",
    }
}

fn log_format_label(log_format: LogFormat) -> &'static str {
    match log_format {
        LogFormat::Json => "json",
        LogFormat::Pretty => "pretty",
    }
}
