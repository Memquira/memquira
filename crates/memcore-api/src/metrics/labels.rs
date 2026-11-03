//! Route label normalization (low cardinality).

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

/// The route template axum matched (e.g. `/api/v1/users/{user_id}/memories`), carried on the
/// response so the outer metrics middleware labels requests without ever seeing raw ids.
#[derive(Clone, Debug)]
pub struct MatchedRouteLabel(pub String);

/// Route-level middleware (`route_layer`): copies the matched template onto the response.
pub async fn capture_matched_route(request: Request, next: Next) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string());
    let mut response = next.run(request).await;
    if let Some(route) = matched {
        response.extensions_mut().insert(MatchedRouteLabel(route));
    }
    response
}

/// Bounded request class (in-flight gauges; admission control) from method and raw path.
pub fn request_class(method: &str, path: &str) -> &'static str {
    let path = path.split('?').next().unwrap_or(path);
    if path == "/health" || path.starts_with("/health/") || path == "/ready" || path == "/metrics" {
        return "health";
    }
    if path.starts_with("/api/v1/admin/") || path.starts_with("/api/v1/api-keys") {
        return "admin";
    }
    let read = matches!(
        path,
        "/api/v1/memories/search"
            | "/api/v1/memories/candidates"
            | "/api/v1/memories/context"
            | "/api/v1/episodes/search"
            | "/api/v1/context"
    ) || method == "GET";
    if read {
        return "read";
    }
    if matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") && path.starts_with("/api/v1/") {
        return "write";
    }
    "other"
}

/// Prefer Axum matched path; otherwise normalize dynamic segments.
pub fn normalize_route(matched: Option<&str>, raw_path: &str) -> String {
    if let Some(matched) = matched.filter(|value| !value.is_empty()) {
        return matched.to_string();
    }
    normalize_raw_path(raw_path)
}

fn normalize_raw_path(path: &str) -> String {
    let trimmed = path.split('?').next().unwrap_or(path);
    let mut out = Vec::new();
    for (idx, segment) in trimmed.split('/').enumerate() {
        if idx == 0 && segment.is_empty() {
            continue;
        }
        if segment.is_empty() {
            continue;
        }
        out.push(normalize_segment(segment));
    }
    if out.is_empty() {
        return "/".to_string();
    }
    format!("/{}", out.join("/"))
}

fn normalize_segment(segment: &str) -> &str {
    if Uuid::parse_str(segment).is_ok() {
        return "{id}";
    }
    // Common tenant path segments that are high-cardinality when literal.
    if looks_like_user_id(segment) {
        return "{user_id}";
    }
    segment
}

fn looks_like_user_id(segment: &str) -> bool {
    // Heuristic only used when MatchedPath is unavailable.
    segment.starts_with("user_")
        || segment.starts_with("perf-user-")
        || segment.starts_with("smoke-test-")
        || (segment.len() > 24
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_classes_are_bounded() {
        assert_eq!(request_class("POST", "/api/v1/memories/search"), "read");
        assert_eq!(request_class("POST", "/api/v1/memories/candidates"), "read");
        assert_eq!(request_class("POST", "/api/v1/memories"), "write");
        assert_eq!(request_class("POST", "/api/v1/users/alice/import"), "write");
        assert_eq!(request_class("GET", "/api/v1/users/alice/memories"), "read");
        assert_eq!(request_class("GET", "/api/v1/admin/org/summary"), "admin");
        assert_eq!(request_class("GET", "/health/ready"), "health");
        assert_eq!(request_class("GET", "/wp-admin/../etc"), "read");
        assert_eq!(request_class("OPTIONS", "/anything"), "other");
    }

    #[test]
    fn prefers_matched_path() {
        assert_eq!(
            normalize_route(
                Some("/api/v1/users/{user_id}/memories"),
                "/api/v1/users/alice/memories"
            ),
            "/api/v1/users/{user_id}/memories"
        );
    }

    #[test]
    fn strips_query_string() {
        assert_eq!(normalize_route(None, "/health?x=1"), "/health");
    }

    #[test]
    fn normalizes_uuid_segments() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            normalize_route(None, &format!("/api/v1/api-keys/{id}")),
            "/api/v1/api-keys/{id}"
        );
    }
}
