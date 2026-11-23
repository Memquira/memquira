//! `X-Memquira-Operation` (MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED): ignored when off; when on, a valid token is
//! accepted and an invalid one is refused with 400 before the handler runs.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use memquira_api::{AppState, create_app};
use memquira_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

fn app(enabled: bool) -> axum::Router {
    let mut settings = Settings::default();
    settings.provider_operation_header_enabled = enabled;
    create_app(AppState::new(settings))
}

async fn search(app: axum::Router, token: Option<&str>) -> StatusCode {
    let (name, value) = authorization_header();
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/memories/search")
        .header("content-type", "application/json")
        .header("X-Organization-ID", "org_123")
        .header(name, value);
    if let Some(token) = token {
        builder = builder.header("X-Memquira-Operation", token);
    }
    let request = builder
        .body(Body::from(
            r#"{"user_id":"user_123","query":"rust","limit":5}"#,
        ))
        .expect("request");
    app.oneshot(request).await.expect("response").status()
}

#[tokio::test]
async fn invalid_tokens_are_refused_only_when_forwarding_is_enabled() {
    let invalid = "not a valid token!";
    assert_eq!(
        search(app(true), Some(invalid)).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        search(app(true), Some(&"a".repeat(513))).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        search(app(true), Some("v1.0f8e2c1a.1763800000.Sig-_")).await,
        StatusCode::OK
    );
    assert_eq!(search(app(true), None).await, StatusCode::OK);
    // Off (the default): the header is ignored.
    assert_eq!(search(app(false), Some(invalid)).await, StatusCode::OK);
}
