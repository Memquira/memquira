//! Auth gate for metrics scrape endpoint.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::middleware::Next;
use axum::response::Response;
use memcore_config::AuthMode;
use memcore_core::ApiKeyScope;

use crate::observability::{attach_error_code, error_response};
use crate::security::{AuthContext, hash_api_key_with_pepper};
use crate::state::AppState;

const BEARER_PREFIX: &str = "Bearer ";

/// Requires a valid API key when `metrics_require_auth` is true.
///
/// Database mode additionally requires [`ApiKeyScope::AdminRead`].
pub async fn require_metrics_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.settings.metrics_require_auth {
        return next.run(request).await;
    }

    match validate_metrics_authorization(&state, request.headers()).await {
        Ok(()) => next.run(request).await,
        Err((status, code, message)) => {
            let mut response = error_response(status, code, message, &request);
            attach_error_code(&mut response, code);
            crate::metrics::record_auth_failure(
                auth_reason(code, message),
                request.method().as_str(),
                state.settings.metrics_path.as_str(),
            );
            response
        }
    }
}

async fn validate_metrics_authorization(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, &'static str, &'static str)> {
    match state.settings.auth_mode {
        AuthMode::Dev => {
            let token = extract_bearer_token(headers)?;
            if token != state.settings.dev_api_key {
                return Err((StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "invalid api key"));
            }
            Ok(())
        }
        AuthMode::Database => {
            let token = extract_bearer_token(headers)?;
            let pepper = state
                .settings
                .api_key_pepper
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "api key pepper is not configured",
                ))?;
            let key_hash = hash_api_key_with_pepper(pepper, token);
            let record = state
                .api_key_store
                .find_by_hash(&key_hash)
                .await
                .map_err(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL_ERROR",
                        "failed to validate api key",
                    )
                })?
                .filter(|record| record.is_active())
                .ok_or((StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "invalid api key"))?;

            let auth = AuthContext {
                org_id: record.org_id,
                api_key_id: record.id,
                scopes: record.scopes,
            };
            if !auth.has_scope(ApiKeyScope::AdminRead) && !auth.has_scope(ApiKeyScope::AdminWrite) {
                return Err((
                    StatusCode::FORBIDDEN,
                    "FORBIDDEN",
                    "admin read scope required for metrics",
                ));
            }
            Ok(())
        }
    }
}

fn extract_bearer_token(
    headers: &HeaderMap,
) -> Result<&str, (StatusCode, &'static str, &'static str)> {
    let header_value = headers.get(AUTHORIZATION).ok_or((
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "missing authorization header",
    ))?;
    let header_str = header_value.to_str().map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "invalid authorization header",
        )
    })?;
    let token = header_str.strip_prefix(BEARER_PREFIX).ok_or((
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "invalid authorization header",
    ))?;
    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "invalid authorization header",
        ));
    }
    Ok(token)
}

fn auth_reason(code: &str, message: &str) -> &'static str {
    match code {
        "FORBIDDEN" => "missing_scope",
        "UNAUTHORIZED" if message.contains("missing") => "missing_key",
        "UNAUTHORIZED" => "invalid_key",
        _ => "forbidden",
    }
}
