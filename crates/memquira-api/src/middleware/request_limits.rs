use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::observability::error_response;
use crate::state::AppState;

/// Rejects requests whose `Content-Length` exceeds the configured max body size.
pub async fn enforce_request_body_limit(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(content_length) = content_length_bytes(request.headers())
        && content_length > state.settings.max_request_body_bytes as u64
    {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "PAYLOAD_TOO_LARGE",
            "request body is too large",
            &request,
        );
    }

    next.run(request).await
}

fn content_length_bytes(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

/// Requires `Content-Type: application/json` for JSON-body methods.
pub async fn enforce_json_content_type(request: Request, next: Next) -> Response {
    if should_validate_json_content_type(&request) && !is_allowed_json_request(request.headers()) {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Type must be application/json",
            &request,
        );
    }

    next.run(request).await
}

fn should_validate_json_content_type(request: &Request) -> bool {
    match *request.method() {
        Method::POST | Method::PUT | Method::PATCH => {}
        _ => return false,
    }

    let path = request.uri().path();
    !is_exempt_path(path)
}

fn is_exempt_path(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/ready"
            | "/metrics"
            | "/api/v1/version"
            | "/openapi.json"
            | "/docs"
            | "/docs/"
    ) || path.starts_with("/docs/")
}

fn is_allowed_json_request(headers: &HeaderMap) -> bool {
    match headers.get(header::CONTENT_TYPE) {
        Some(_) => is_json_content_type(headers),
        None => match content_length_bytes(headers) {
            // Empty-body POST/PUT/PATCH (e.g. trigger endpoints) may omit Content-Type.
            Some(0) | None => true,
            Some(_) => false,
        },
    }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let media_type = value.split(';').next().unwrap_or(value).trim();
    media_type.eq_ignore_ascii_case("application/json")
}
