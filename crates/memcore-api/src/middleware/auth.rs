use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::middleware::Next;
use axum::response::Response;

use memcore_config::AuthMode;

use crate::observability::{attach_error_code, error_response};
use crate::security::{AuthContext, hash_api_key_with_pepper};
use crate::state::AppState;

const BEARER_PREFIX: &str = "Bearer ";

pub async fn require_api_key(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if !state.settings.auth_enabled {
        return next.run(request).await;
    }

    let result = match state.settings.auth_mode {
        AuthMode::Dev => {
            validate_dev_authorization(request.headers(), &state.settings.dev_api_key).map(|_| ())
        }
        AuthMode::Database => {
            match validate_database_authorization(&state, request.headers()).await {
                Ok(auth) => {
                    request.extensions_mut().insert(auth);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    };

    match result {
        Ok(()) => next.run(request).await,
        Err((status, code, message)) => {
            let reason = match (status, code) {
                (StatusCode::UNAUTHORIZED, "UNAUTHORIZED") if message.contains("missing") => {
                    "missing_key"
                }
                (StatusCode::UNAUTHORIZED, _) => "invalid_key",
                (StatusCode::FORBIDDEN, _) => "forbidden",
                _ => "invalid_key",
            };
            if state.settings.metrics_enabled {
                let route = crate::metrics::normalize_route(None, request.uri().path());
                crate::metrics::record_auth_failure(reason, request.method().as_str(), &route);
            }
            let mut response = error_response(status, code, message, &request);
            attach_error_code(&mut response, code);
            response
        }
    }
}

fn validate_dev_authorization(
    headers: &HeaderMap,
    expected_key: &str,
) -> Result<(), (StatusCode, &'static str, &'static str)> {
    let token = extract_bearer_token(headers)?;

    if token != expected_key {
        return Err((StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "invalid api key"));
    }

    Ok(())
}

async fn validate_database_authorization(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthContext, (StatusCode, &'static str, &'static str)> {
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

    Ok(AuthContext {
        org_id: record.org_id,
        api_key_id: record.id,
        scopes: record.scopes,
    })
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

    let token = header_str.strip_prefix(BEARER_PREFIX).ok_or({
        (
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "invalid authorization header",
        )
    })?;

    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "invalid authorization header",
        ));
    }

    Ok(token)
}
