mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

const ORG_A: &str = "org_rate_a";
const ORG_B: &str = "org_rate_b";
const USER_A: &str = "user_rate_a";

const ADD_BODY: &str = r#"{
  "user_id": "user_rate_a",
  "messages": [{ "role": "user", "content": "rate limit test" }],
  "metadata": {}
}"#;

fn rate_limited_app(requests_per_minute: u32, enabled: bool) -> axum::Router {
    let settings = Settings {
        rate_limit_enabled: enabled,
        rate_limit_requests_per_minute: requests_per_minute,
        ..Settings::default()
    };
    create_app(AppState::new(settings))
}

fn post_request(uri: &str, body: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::from(body.to_string()))
        .expect("request should build")
}

fn get_request(uri: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }

    builder.body(Body::empty()).expect("request should build")
}

async fn response_parts(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.expect("router should respond");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, json)
}

#[tokio::test]
async fn protected_route_succeeds_under_limit() {
    let app = rate_limited_app(2, true);

    let (status, json) =
        response_parts(app, post_request("/api/v1/memories", ADD_BODY, ORG_A)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn protected_route_returns_429_after_exceeding_limit() {
    let app = rate_limited_app(2, true);

    for _ in 0..2 {
        let (status, _) = response_parts(
            app.clone(),
            post_request("/api/v1/memories", ADD_BODY, ORG_A),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, json) =
        response_parts(app, post_request("/api/v1/memories", ADD_BODY, ORG_A)).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["error"]["code"], "RATE_LIMITED");
    assert_eq!(json["error"]["message"], "rate limit exceeded");
}

#[tokio::test]
async fn rate_limit_is_scoped_by_org_id() {
    let app = rate_limited_app(1, true);

    let (status, _) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", ADD_BODY, ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", ADD_BODY, ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["error"]["code"], "RATE_LIMITED");
}

#[tokio::test]
async fn org_a_hitting_limit_does_not_block_org_b() {
    let app = rate_limited_app(1, true);

    let (status, _) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", ADD_BODY, ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", ADD_BODY, ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    let (status, json) =
        response_parts(app, post_request("/api/v1/memories", ADD_BODY, ORG_B)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn health_route_is_not_rate_limited() {
    let app = rate_limited_app(1, true);

    for _ in 0..5 {
        let (status, json) = response_parts(app.clone(), get_request("/health", None, false)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
    }
}

#[tokio::test]
async fn ready_route_is_not_rate_limited() {
    let app = rate_limited_app(1, true);

    for _ in 0..5 {
        let (status, json) = response_parts(app.clone(), get_request("/ready", None, false)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ready");
    }
}

#[tokio::test]
async fn rate_limiting_disabled_allows_repeated_requests() {
    let app = rate_limited_app(1, false);

    for _ in 0..5 {
        let (status, json) = response_parts(
            app.clone(),
            post_request("/api/v1/memories", ADD_BODY, ORG_A),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "success");
    }
}

#[tokio::test]
async fn list_memories_route_is_rate_limited() {
    let app = rate_limited_app(1, true);

    let (status, _) = response_parts(
        app.clone(),
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["error"]["code"], "RATE_LIMITED");
}
