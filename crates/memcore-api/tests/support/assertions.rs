//! Shared assertions for API E2E / integration responses.

use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;

pub fn assert_status(status: StatusCode, expected: StatusCode) {
    assert_eq!(status, expected, "unexpected HTTP status");
}

pub fn assert_json_error_code(json: &Value, code: &str) {
    assert_eq!(
        json["error"]["code"].as_str(),
        Some(code),
        "unexpected error code in {json}"
    );
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "error message missing"
    );
}

pub fn assert_success_envelope(json: &Value) {
    assert_eq!(
        json["status"], "success",
        "expected success envelope: {json}"
    );
}

pub fn assert_has_security_headers(headers: &HeaderMap) {
    assert_eq!(
        headers.get("x-content-type-options").map(|v| v.as_bytes()),
        Some(b"nosniff".as_slice())
    );
    assert_eq!(
        headers.get("x-frame-options").map(|v| v.as_bytes()),
        Some(b"DENY".as_slice())
    );
    assert_eq!(
        headers.get("referrer-policy").map(|v| v.as_bytes()),
        Some(b"no-referrer".as_slice())
    );
    assert!(headers.get("permissions-policy").is_some());
    assert_eq!(
        headers
            .get("cross-origin-resource-policy")
            .map(|v| v.as_bytes()),
        Some(b"same-origin".as_slice())
    );
    assert_eq!(
        headers.get("cache-control").map(|v| v.as_bytes()),
        Some(b"no-store".as_slice())
    );
}

pub fn assert_no_secret_leak(body: &str) {
    let lower = body.to_lowercase();
    for forbidden in [
        "openai_api_key",
        "sk-live-",
        "sk-proj-",
        "database_url",
        "postgres_url",
        "redis_url",
        "api_key_pepper",
        "bearer memcore",
        "password=",
        "authorization: bearer",
    ] {
        assert!(
            !lower.contains(forbidden),
            "response must not contain secret-like pattern: {forbidden}"
        );
    }
}

pub fn assert_error_contract(json: &Value) {
    assert!(json.get("error").is_some(), "missing error object: {json}");
    assert!(json["error"]["code"].is_string());
    assert!(json["error"]["message"].is_string());
}
