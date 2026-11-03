mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

const ORG_A: &str = "org_retention_a";
const USER_A: &str = "user_retention_a";

fn retention_app() -> axum::Router {
    let mut settings = Settings::default();
    settings.retention_enabled = true;
    settings.fact_retention_days = 365;
    settings.event_retention_days = 90;
    create_app(AppState::new(settings))
}

fn post_retention(uri: &str, body: &str, org_id: &str, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id);

    if with_auth {
        let (auth_name, auth_value) = authorization_header();
        builder = builder.header(auth_name, auth_value);
    }

    builder
        .body(Body::from(body.to_string()))
        .expect("request should build")
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

async fn seed_memory(app: &axum::Router, org_id: &str, user_id: &str, content: &str) {
    let add_body = format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{}}
        }}"#
    );
    let (auth_name, auth_value) = authorization_header();
    let (status, _) = response_parts(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header("content-type", "application/json")
            .header("X-Organization-ID", org_id)
            .header(auth_name, auth_value)
            .body(Body::from(add_body))
            .expect("request should build"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn retention_requires_authorization_header() {
    let app = retention_app();
    let (status, _) = response_parts(
        app,
        post_retention(
            &format!("/api/v1/users/{USER_A}/retention/apply"),
            r#"{"fact_retention_days":365}"#,
            ORG_A,
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn retention_requires_organization_header() {
    let app = retention_app();
    let (auth_name, auth_value) = authorization_header();
    let (status, _) = response_parts(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/users/{USER_A}/retention/apply"))
            .header("content-type", "application/json")
            .header(auth_name, auth_value)
            .body(Body::from(r#"{"fact_retention_days":365}"#))
            .expect("request should build"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dry_run_defaults_to_true() {
    let app = retention_app();
    seed_memory(&app, ORG_A, USER_A, "retention memory").await;

    let (status, json) = response_parts(
        app,
        post_retention(
            &format!("/api/v1/users/{USER_A}/retention/apply"),
            r#"{}"#,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["dry_run"], true);
    assert_eq!(json["summary"]["facts_deleted"], 0);
}

#[tokio::test]
async fn disabled_retention_config_returns_zero_counts() {
    let app = create_app(AppState::new(Settings::default()));
    seed_memory(&app, ORG_A, USER_A, "retention memory").await;

    let (status, json) = response_parts(
        app,
        post_retention(
            &format!("/api/v1/users/{USER_A}/retention/apply"),
            r#"{"dry_run":false,"fact_retention_days":1}"#,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["facts_matched"], 0);
    assert_eq!(json["summary"]["facts_deleted"], 0);
}

#[tokio::test]
async fn zero_days_disables_fact_cleanup_category() {
    let app = retention_app();
    seed_memory(&app, ORG_A, USER_A, "retention memory").await;

    let (status, json) = response_parts(
        app,
        post_retention(
            &format!("/api/v1/users/{USER_A}/retention/apply"),
            r#"{"dry_run":true,"fact_retention_days":0,"event_retention_days":90}"#,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["facts_matched"], 0);
}
