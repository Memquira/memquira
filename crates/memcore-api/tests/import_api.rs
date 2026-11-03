mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use memcore_core::USER_EXPORT_FORMAT_VERSION;
use tower::ServiceExt;

use common::authorization_header;

const ORG_A: &str = "org_import_a";
const ORG_B: &str = "org_import_b";
const USER_A: &str = "user_import_a";
const MEMORY_CONTENT: &str = "Import API roundtrip memory";

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn post_request(uri: &str, body: &str, org_id: &str, with_auth: bool) -> Request<Body> {
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

fn get_request(uri: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::empty())
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

async fn export_user(app: &axum::Router, org_id: &str, user_id: &str) -> serde_json::Value {
    let (status, json) = response_parts(
        app.clone(),
        get_request(&format!("/api/v1/users/{user_id}/export"), org_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    json["export"].clone()
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

fn import_body(export: &serde_json::Value, mode: &str, restore_events: bool) -> String {
    import_body_with_dry_run(export, mode, restore_events, false)
}

fn import_body_with_dry_run(
    export: &serde_json::Value,
    mode: &str,
    restore_events: bool,
    dry_run: bool,
) -> String {
    serde_json::json!({
        "export": export,
        "mode": mode,
        "restore_events": restore_events,
        "dry_run": dry_run,
    })
    .to_string()
}

#[tokio::test]
async fn import_append_roundtrip_from_export() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;
    let export = export_user(&app, ORG_A, USER_A).await;

    // Into a separate server: the server rejects re-applying an unchanged export to the user
    // it came from (409 IMPORT_ALREADY_APPLIED, `retry_import_api.rs`); earlier versions duplicated it.
    let target = test_app();
    let body = import_body(&export, "append", false);
    let (status, json) = response_parts(
        target,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["summary"]["imported_facts"], 1);
}

#[tokio::test]
async fn import_replace_mode_works() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, "to be replaced").await;
    let mut export = export_user(&app, ORG_A, USER_A).await;
    export["facts"][0]["content"] = serde_json::json!("replaced via import");

    let body = import_body(&export, "replace", false);
    let (status, json) = response_parts(
        app.clone(),
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["replaced_existing"], true);
}

#[tokio::test]
async fn imported_memories_are_searchable() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;
    let export = export_user(&app, ORG_A, USER_A).await;

    // Into a separate server (an unchanged export is not re-applied to its own
    // user), whose search must then find the imported memory.
    let app = test_app();
    let import_uri = format!("/api/v1/users/{USER_A}/import");
    let body = import_body(&export, "append", false);
    let (import_status, _) =
        response_parts(app.clone(), post_request(&import_uri, &body, ORG_A, true)).await;
    assert_eq!(import_status, StatusCode::OK);

    let search_body = format!(
        r#"{{
          "user_id": "{USER_A}",
          "query": "{MEMORY_CONTENT}",
          "limit": 5
        }}"#
    );
    let (auth_name, auth_value) = authorization_header();
    let (search_status, search_json) = response_parts(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories/search")
            .header("content-type", "application/json")
            .header("X-Organization-ID", ORG_A)
            .header(auth_name, auth_value)
            .body(Body::from(search_body))
            .expect("request should build"),
    )
    .await;

    assert_eq!(search_status, StatusCode::OK);
    assert!(!search_json["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn import_rejects_mismatched_org_id() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_B,
        "user_id": USER_A,
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });

    let body = import_body(&export, "append", false);
    let (status, json) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn import_rejects_mismatched_user_id() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": "other_user",
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });

    let body = import_body(&export, "append", false);
    let (status, json) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn import_requires_authorization_header() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": USER_A,
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });
    let body = import_body(&export, "append", false);

    let (status, _) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            false,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn import_requires_organization_header() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": USER_A,
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });
    let body = import_body(&export, "append", false);

    let (auth_name, auth_value) = authorization_header();
    let (status, _) = response_parts(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/users/{USER_A}/import"))
            .header("content-type", "application/json")
            .header(auth_name, auth_value)
            .body(Body::from(body))
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn org_a_cannot_import_into_org_b() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_B,
        "user_id": USER_A,
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });
    let body = import_body(&export, "append", false);

    let (status, _) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn import_does_not_expose_api_key_fields() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": USER_A,
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [{
            "id": "00000000-0000-4000-8000-000000000001",
            "org_id": ORG_A,
            "user_id": USER_A,
            "content": "bad metadata",
            "summary": null,
            "memory_type": "Profile",
            "source": "api_import",
            "confidence": 0.9,
            "importance": 0.8,
            "valid_at": null,
            "invalid_at": null,
            "recorded_at": "2026-06-10T10:00:00Z",
            "updated_at": "2026-06-10T10:00:00Z",
            "metadata": { "api_key": "secret" }
        }],
        "memory_events": []
    });
    let body = import_body(&export, "append", false);

    let (status, json) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn dry_run_import_returns_validation_summary() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;
    let export = export_user(&app, ORG_A, USER_A).await;

    let body = import_body_with_dry_run(&export, "append", false, true);
    let (status, json) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["summary"]["dry_run"], true);
    assert_eq!(json["summary"]["validation"]["valid"], true);
    assert_eq!(json["summary"]["imported_facts"], 1);
}

#[tokio::test]
async fn dry_run_does_not_create_listed_memories() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": USER_A,
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [{
            "id": "00000000-0000-4000-8000-000000000002",
            "org_id": ORG_A,
            "user_id": USER_A,
            "content": "dry-run only",
            "summary": null,
            "memory_type": "Profile",
            "source": "api_import",
            "confidence": 0.9,
            "importance": 0.8,
            "valid_at": null,
            "invalid_at": null,
            "recorded_at": "2026-06-10T10:00:00Z",
            "updated_at": "2026-06-10T10:00:00Z",
            "metadata": {}
        }],
        "memory_events": []
    });

    let body = import_body_with_dry_run(&export, "append", false, true);
    let (status, _) = response_parts(
        app.clone(),
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (list_status, list_json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    assert!(list_json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn dry_run_replace_mode_does_not_delete_existing_memories() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, "keep during dry-run").await;
    let export = export_user(&app, ORG_A, USER_A).await;

    let body = import_body_with_dry_run(&export, "replace", false, true);
    let (status, json) = response_parts(
        app.clone(),
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["replaced_existing"], true);

    let (list_status, list_json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dry_run_invalid_payload_returns_validation_summary() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": "other_user",
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });
    let body = import_body_with_dry_run(&export, "append", false, true);

    let (status, json) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["validation"]["valid"], false);
    assert!(
        json["summary"]["validation"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "USER_ID_MISMATCH")
    );
}

#[tokio::test]
async fn non_dry_run_invalid_payload_returns_validation_error() {
    let app = test_app();
    let export = serde_json::json!({
        "format_version": USER_EXPORT_FORMAT_VERSION,
        "org_id": ORG_A,
        "user_id": "other_user",
        "exported_at": "2026-06-10T10:00:00Z",
        "facts": [],
        "memory_events": []
    });
    let body = import_body_with_dry_run(&export, "append", false, false);

    let (status, json) = response_parts(
        app,
        post_request(
            &format!("/api/v1/users/{USER_A}/import"),
            &body,
            ORG_A,
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}
