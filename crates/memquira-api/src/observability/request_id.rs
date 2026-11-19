use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use uuid::Uuid;

/// Maximum accepted inbound request ID length.
pub const MAX_REQUEST_ID_LENGTH: usize = 128;

/// Correlation ID attached to each HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn resolve_request_id(headers: &HeaderMap, header_name: &str) -> RequestId {
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| is_safe_request_id(value))
        .map(RequestId::new)
        .unwrap_or_else(RequestId::generate)
}

fn is_safe_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_LENGTH
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
}

pub fn request_id_from_extensions(request: &Request) -> Option<RequestId> {
    request.extensions().get::<RequestId>().cloned()
}

pub fn insert_response_request_id_header(
    response: &mut axum::response::Response,
    header_name: &str,
    request_id: &RequestId,
) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::try_from(header_name),
        HeaderValue::from_str(request_id.as_str()),
    ) {
        response.headers_mut().insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn generates_when_missing() {
        let headers = HeaderMap::new();
        let id = resolve_request_id(&headers, "X-Request-ID");
        assert!(!id.as_str().is_empty());
    }

    #[test]
    fn preserves_safe_request_id() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Request-ID", HeaderValue::from_static("req_abc-123"));
        let id = resolve_request_id(&headers, "X-Request-ID");
        assert_eq!(id.as_str(), "req_abc-123");
    }

    #[test]
    fn replaces_overly_long_request_id() {
        let mut headers = HeaderMap::new();
        let long = "a".repeat(MAX_REQUEST_ID_LENGTH + 1);
        headers.insert(
            "X-Request-ID",
            HeaderValue::from_str(&long).expect("header value"),
        );
        let id = resolve_request_id(&headers, "X-Request-ID");
        assert_ne!(id.as_str(), long);
        assert!(id.as_str().len() <= MAX_REQUEST_ID_LENGTH);
    }
}
