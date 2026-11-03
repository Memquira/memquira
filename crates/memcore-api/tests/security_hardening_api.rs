mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{DEFAULT_REQUEST_ID_HEADER, Settings};
use tower::ServiceExt;

use common::authorization_header;

async fn send(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let response = app.oneshot(request).await.expect("router should respond");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, headers, json)
}

fn app_with(settings: Settings) -> axum::Router {
    create_app(AppState::new(settings))
}

#[tokio::test]
async fn security_headers_are_present_by_default() {
    let app = app_with(Settings::default());
    let (status, headers, json) = send(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    assert_eq!(
        headers.get("permissions-policy").unwrap(),
        "geolocation=(), microphone=(), camera=()"
    );
    assert_eq!(
        headers.get("cross-origin-resource-policy").unwrap(),
        "same-origin"
    );
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn security_headers_omitted_when_disabled() {
    let settings = Settings {
        security_headers_enabled: false,
        ..Settings::default()
    };
    let app = app_with(settings);
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    assert!(headers.get("x-content-type-options").is_none());
    assert!(headers.get("x-frame-options").is_none());
}

#[tokio::test]
async fn request_under_body_limit_succeeds() {
    let settings = Settings {
        max_request_body_bytes: 4096,
        ..Settings::default()
    };
    let app = app_with(settings);
    let (auth_header, auth_value) = authorization_header();
    let body = r#"{
  "user_id": "user_123",
  "messages": [{"role":"user","content":"hello under limit"}]
}"#;
    let (status, _, _) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_sec")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(Body::from(body))
            .expect("request"),
    )
    .await;

    assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(status.is_success());
}

#[tokio::test]
async fn request_over_body_limit_returns_payload_too_large() {
    let settings = Settings {
        max_request_body_bytes: 32,
        ..Settings::default()
    };
    let app = app_with(settings);
    let (auth_header, auth_value) = authorization_header();
    let body =
        r#"{"user_id":"u1","content":"this body is intentionally larger than thirty two bytes"}"#;
    let (status, _, json) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_sec")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(Body::from(body))
            .expect("request"),
    )
    .await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json["error"]["code"], "PAYLOAD_TOO_LARGE");
    assert_eq!(json["error"]["message"], "request body is too large");
    assert!(
        !json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("postgres://")
    );
}

#[tokio::test]
async fn health_get_still_works_with_body_limit() {
    let settings = Settings {
        max_request_body_bytes: 8,
        ..Settings::default()
    };
    let app = app_with(settings);
    let (status, _, json) = send(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn json_endpoint_accepts_application_json() {
    let app = app_with(Settings::default());
    let (auth_header, auth_value) = authorization_header();
    let (status, _, _) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories/search")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_sec")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"user_id":"u1","query":"hi"}"#))
            .expect("request"),
    )
    .await;
    assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn json_endpoint_accepts_charset_utf8() {
    let app = app_with(Settings::default());
    let (auth_header, auth_value) = authorization_header();
    let (status, _, _) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories/search")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_sec")
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(Body::from(r#"{"user_id":"u1","query":"hi"}"#))
            .expect("request"),
    )
    .await;
    assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn json_endpoint_rejects_text_plain() {
    let app = app_with(Settings::default());
    let (auth_header, auth_value) = authorization_header();
    let (status, _, json) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_sec")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("hello"))
            .expect("request"),
    )
    .await;

    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(json["error"]["code"], "UNSUPPORTED_MEDIA_TYPE");
    assert_eq!(
        json["error"]["message"],
        "Content-Type must be application/json"
    );
}

#[tokio::test]
async fn get_endpoint_does_not_require_content_type() {
    let app = app_with(Settings::default());
    let (auth_header, auth_value) = authorization_header();
    let (status, _, _) = send(
        app,
        Request::builder()
            .uri("/api/v1/users/u1/memories")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_sec")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn cors_disabled_by_default_has_no_allow_origin() {
    let app = app_with(Settings::default());
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[tokio::test]
async fn cors_enabled_allows_configured_origin() {
    let settings = Settings {
        cors_enabled: true,
        cors_allowed_origins: vec!["https://app.example.com".to_string()],
        ..Settings::default()
    };
    let app = app_with(settings);
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .header(header::ORIGIN, "https://app.example.com")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "https://app.example.com"
    );
}

#[tokio::test]
async fn cors_disallowed_origin_has_no_allow_origin() {
    let settings = Settings {
        cors_enabled: true,
        cors_allowed_origins: vec!["https://app.example.com".to_string()],
        ..Settings::default()
    };
    let app = app_with(settings);
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .header(header::ORIGIN, "https://evil.example.com")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[tokio::test]
async fn cors_preflight_works_without_auth() {
    let settings = Settings {
        cors_enabled: true,
        cors_allowed_origins: vec!["https://app.example.com".to_string()],
        ..Settings::default()
    };
    let app = app_with(settings);
    let (status, headers, _) = send(
        app,
        Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/memories")
            .header(header::ORIGIN, "https://app.example.com")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,content-type,x-organization-id",
            )
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    assert!(status.is_success() || status == StatusCode::NO_CONTENT);
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "https://app.example.com"
    );
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cors_wildcard_without_credentials() {
    let settings = Settings {
        cors_enabled: true,
        cors_allowed_origins: vec!["*".to_string()],
        cors_allow_credentials: false,
        ..Settings::default()
    };
    let app = app_with(settings);
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .header(header::ORIGIN, "https://any.example.com")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "*"
    );
}

#[tokio::test]
async fn request_id_is_present_and_overlong_replaced() {
    let app = app_with(Settings::default());
    let long_id = "a".repeat(200);
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .header(DEFAULT_REQUEST_ID_HEADER, &long_id)
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    let returned = headers
        .get(DEFAULT_REQUEST_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap();
    assert_ne!(returned, long_id);
    assert!(returned.len() <= 128);
}

#[tokio::test]
async fn storage_error_message_is_sanitized() {
    use memcore_api::response::ErrorBody;
    use memcore_common::MemcoreError;

    let (_, body) = ErrorBody::from_memcore_error(MemcoreError::StorageError(
        "failed postgres://user:secret@db/memcore".to_string(),
    ));
    assert!(!body.error.message.contains("secret"));
    assert_eq!(body.error.message, "database operation failed");
}
