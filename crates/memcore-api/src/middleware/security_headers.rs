use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::state::AppState;

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    (
        "permissions-policy",
        "geolocation=(), microphone=(), camera=()",
    ),
    ("cross-origin-resource-policy", "same-origin"),
    ("cache-control", "no-store"),
];

/// Adds safe default security headers when enabled in settings.
///
/// Existing explicitly-set headers are left unchanged.
pub async fn apply_security_headers(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;

    if !state.settings.security_headers_enabled {
        return response;
    }

    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        if headers.contains_key(&header_name) {
            continue;
        }
        if let Ok(header_value) = HeaderValue::from_str(value) {
            headers.insert(header_name, header_value);
        }
    }

    // Prefer not to advertise a server fingerprint when unset.
    let _ = headers.get(header::SERVER);

    response
}
