use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use memcore_config::{ProviderStartupMode, load_settings};
use memcore_core::ShutdownToken;
use tokio::net::TcpListener;

use crate::observability::{init_logging, log_startup};
use crate::routes;
use crate::shutdown::shutdown_signal_with_token;
use crate::startup::wait_for_dependencies;
use crate::state::AppState;

pub fn create_app(state: AppState) -> Router {
    routes::router(&state).with_state(state)
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings = load_settings()?;
    init_logging(&settings)?;
    log_startup(&settings);
    // The recorder exists before the stores are built, so the startup
    // embedding-identity probes (real provider calls) are counted in /metrics too.
    if settings.metrics_enabled
        && let Err(error) = crate::metrics::install_metrics_recorder(&settings)
    {
        tracing::warn!(error = %error, "metrics recorder not installed before startup probes");
    }

    // Never run on mock providers by accident: they silently corrupt stored memories.
    match settings.check_startup_providers() {
        Ok(ProviderStartupMode::Real) => {}
        Ok(ProviderStartupMode::MockAllowed) => tracing::warn!(
            allow_mock = true,
            "MEMCORE_ALLOW_MOCK=true: mock LLM / embedding providers are active; for tests and \
             local development only, stored memories are not real"
        ),
        Err(error) => {
            tracing::error!(error = %error, "startup refused");
            return Err(error.into());
        }
    }

    // Qdrant / Postgres may start after memcore (compose, Kubernetes): bounded wait, then fail.
    if let Err(error) = wait_for_dependencies(&settings).await {
        tracing::error!(error = %error, "startup refused");
        return Err(error.into());
    }

    // The quality tier, the reference-gateway check, the optional chat probe
    // and MEMCORE_REQUIRE_QUALITY_TIER, before any store is opened.
    let quality = match crate::quality::startup_quality(&settings).await {
        Ok(quality) => quality,
        Err(error) => {
            tracing::error!(error = %error, "startup refused");
            return Err(error.into());
        }
    };
    tracing::info!(
        quality_tier = %quality.tier,
        configured_tier = %quality.configured_tier,
        reference_gateway = %quality.reference_gateway,
        llm_startup_probe = %quality.llm_startup_probe,
        reference_mismatches = ?quality.reference_mismatches,
        validated_variants = ?quality
            .validated_variants
            .iter()
            .map(|v| format!(
                "{} ({}, operationally_equivalent={})",
                v.name, v.equivalence, v.operationally_equivalent
            ))
            .collect::<Vec<_>>(),
        "quality tier"
    );

    let shutdown_token = ShutdownToken::new();
    let mut state =
        AppState::initialize_with_shutdown(settings.clone(), shutdown_token.child_token()).await?;
    quality.record_metrics();
    state.quality = quality;
    let app = create_app(state);
    let addr: SocketAddr = format!("{}:{}", settings.host, settings.port).parse()?;

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "memcore-api listening");
    serve_until_shutdown(
        listener,
        app,
        shutdown_signal_with_token(shutdown_token.child_token()),
        shutdown_token,
        Duration::from_secs(settings.graceful_shutdown_timeout_seconds),
    )
    .await
}

/// Serves `app` until `signal` resolves, then drains: the listener stops accepting connections,
/// in-flight requests may finish for up to `drain_timeout`, and the function returns after that
/// even if a request is still running. A request that never finished was never acknowledged.
pub async fn serve_until_shutdown<S>(
    listener: TcpListener,
    app: Router,
    signal: S,
    shutdown_token: ShutdownToken,
    drain_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Future<Output = ()> + Send + 'static,
{
    // `into_make_service` finalises the router ONCE. Serving the `Router` directly
    // makes axum (0.8, `impl Service<IncomingStream> for Router`) run `with_state(())` for every
    // accepted connection, rebuilding every route's service stack: about 2.2 MB per open connection
    // for this router, which drove overload RSS to 2.3 GB. Routing and middleware are unchanged.
    let server = axum::serve(listener, app.into_make_service()).with_graceful_shutdown(signal);
    let server = server.into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result?;
        }
        _ = shutdown_token.cancelled() => {
            tracing::info!(
                timeout_seconds = drain_timeout.as_secs(),
                "server graceful shutdown started"
            );
            match tokio::time::timeout(drain_timeout, &mut server).await {
                Ok(result) => {
                    result?;
                    tracing::info!("server graceful shutdown complete");
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_seconds = drain_timeout.as_secs(),
                        "server graceful shutdown timeout reached; unfinished requests were not acknowledged"
                    );
                }
            }
        }
    }

    Ok(())
}
