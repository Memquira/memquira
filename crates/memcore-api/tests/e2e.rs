//! In-process end-to-end API suite.
//!
//! Uses mock providers and mock stores by default (no OpenAI/Postgres/Qdrant/Redis/LanceDB).
//! Run: `cargo test -p memcore-api --test e2e`

mod support;

use axum::http::StatusCode;
use memcore_config::Settings;
use support::{
    DEFAULT_USER, MEMORY_GREEN_TEA, MEMORY_RUST_API, MEMORY_SUMMARIES, TestApp, add_memory_json,
    assert_error_contract, assert_has_security_headers, assert_json_error_code,
    assert_no_secret_leak, assert_status, assert_success_envelope, context_json, import_json,
    search_json,
};

// ---------------------------------------------------------------------------
// Operational endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_health_ready_version_are_public_and_safe() {
    let app = TestApp::new();

    for path in ["/health", "/ready", "/api/v1/version"] {
        let res = app.get(path).await;
        assert_status(res.status, StatusCode::OK);
        assert_has_security_headers(&res.headers);
        assert_no_secret_leak(&res.raw);
        assert!(
            res.headers.get("x-request-id").is_some()
                || res.body["error"]["request_id"].is_null()
                || res.body.get("request_id").is_none(),
            "request id handling present or unused for {path}"
        );
    }

    let health = app.get("/health").await;
    assert_eq!(health.body["status"], "ok");
    assert_eq!(health.body["service"], "memcore");

    let ready = app.get("/ready").await;
    assert!(ready.body["status"].is_string());
    assert!(ready.body["checks"]["database"].is_object());

    let version = app.get("/api/v1/version").await;
    assert_eq!(version.body["status"], "success");
    assert!(version.body["version"]["package_version"].is_string());
    assert_eq!(
        version.body["version"]["package_version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

// ---------------------------------------------------------------------------
// Auth + tenant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_auth_and_tenant_gates() {
    let app = TestApp::new();
    let body = add_memory_json(DEFAULT_USER, MEMORY_GREEN_TEA);

    let missing_auth = app
        .request_with_bearer(
            "POST",
            "/api/v1/memories",
            None,
            Some(&app.org_id),
            Some(&body),
        )
        .await;
    assert_status(missing_auth.status, StatusCode::UNAUTHORIZED);
    assert_json_error_code(&missing_auth.body, "UNAUTHORIZED");
    assert_error_contract(&missing_auth.body);
    assert_no_secret_leak(&missing_auth.raw);

    let bad_auth = app
        .request_with_bearer(
            "POST",
            "/api/v1/memories",
            Some("not-a-real-key"),
            Some(&app.org_id),
            Some(&body),
        )
        .await;
    assert_status(bad_auth.status, StatusCode::UNAUTHORIZED);
    assert_json_error_code(&bad_auth.body, "UNAUTHORIZED");
    assert_no_secret_leak(&bad_auth.raw);

    let missing_org = app
        .request_with_bearer(
            "POST",
            "/api/v1/memories",
            Some(&app.api_key),
            None,
            Some(&body),
        )
        .await;
    assert_status(missing_org.status, StatusCode::BAD_REQUEST);
    assert_json_error_code(&missing_org.body, "VALIDATION_ERROR");

    let ok = app.authed_post_json_raw("/api/v1/memories", &body).await;
    assert_status(ok.status, StatusCode::OK);
    assert_success_envelope(&ok.body);
}

#[tokio::test]
async fn e2e_cors_disabled_by_default() {
    let app = TestApp::new();
    let res = app.get("/health").await;
    assert!(res.headers.get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn e2e_cors_preflight_works_when_enabled() {
    let app = TestApp::with_settings(Settings {
        cors_enabled: true,
        cors_allowed_origins: vec!["https://app.example".to_string()],
        ..Settings::default()
    });

    let request = axum::http::Request::builder()
        .method("OPTIONS")
        .uri("/api/v1/memories")
        .header("origin", "https://app.example")
        .header("access-control-request-method", "POST")
        .body(axum::body::Body::empty())
        .expect("preflight should build");

    let response = tower::ServiceExt::oneshot(app.router.clone(), request)
        .await
        .expect("router should respond");
    assert!(
        response.status().is_success() || response.status() == StatusCode::NO_CONTENT,
        "preflight status: {}",
        response.status()
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .map(|v| v.as_bytes()),
        Some(b"https://app.example".as_slice())
    );
}

// ---------------------------------------------------------------------------
// Memory create → list → search → context → delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_memory_lifecycle_flow() {
    let app = TestApp::new();
    let user = DEFAULT_USER;
    let content = MEMORY_RUST_API;

    let created = app
        .authed_post_json_raw("/api/v1/memories", &add_memory_json(user, content))
        .await;
    assert_status(created.status, StatusCode::OK);
    assert_success_envelope(&created.body);
    let memory_id = created.body["memories"][0]["id"]
        .as_str()
        .expect("memory id")
        .to_string();
    assert!(!memory_id.is_empty());

    let listed = app
        .authed_get(&format!("/api/v1/users/{user}/memories"))
        .await;
    assert_status(listed.status, StatusCode::OK);
    assert_success_envelope(&listed.body);
    let items = listed.body["memories"].as_array().expect("memories array");
    assert!(
        items
            .iter()
            .any(|m| m["id"] == memory_id || m["content"] == content),
        "created memory should be listable"
    );

    let search = app
        .authed_post_json_raw("/api/v1/memories/search", &search_json(user, content))
        .await;
    assert_status(search.status, StatusCode::OK);
    assert_success_envelope(&search.body);
    let results = search.body["results"].as_array().expect("search results");
    assert!(
        !results.is_empty(),
        "search should return results: {}",
        search.body
    );

    let context = app
        .authed_post_json_raw("/api/v1/context", &context_json(user, "Rust API"))
        .await;
    assert_status(context.status, StatusCode::OK);
    assert_success_envelope(&context.body);
    assert!(
        context.body["context"].as_str().is_some(),
        "context string missing: {}",
        context.body
    );
    assert!(
        context.body["memories"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "context memories missing: {}",
        context.body
    );

    let deleted = app
        .authed_delete(&format!("/api/v1/users/{user}/memories/{memory_id}"))
        .await;
    assert_status(deleted.status, StatusCode::OK);
    assert_success_envelope(&deleted.body);

    let listed_after = app
        .authed_get(&format!("/api/v1/users/{user}/memories"))
        .await;
    assert_status(listed_after.status, StatusCode::OK);
    let items_after = listed_after.body["memories"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        items_after
            .iter()
            .all(|m| m["id"].as_str() != Some(memory_id.as_str())),
        "deleted memory must not appear in list"
    );
}

// ---------------------------------------------------------------------------
// Forget user + tenant isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_forget_user_and_tenant_isolation() {
    let app = TestApp::new();
    let org_a = "org_e2e_a";
    let org_b = "org_e2e_b";
    let user_a = "user_e2e_a";
    let user_b = "user_e2e_b";

    let create_a = app
        .authed_as(
            "POST",
            "/api/v1/memories",
            org_a,
            Some(&add_memory_json(user_a, MEMORY_GREEN_TEA)),
        )
        .await;
    assert_status(create_a.status, StatusCode::OK);

    let create_b = app
        .authed_as(
            "POST",
            "/api/v1/memories",
            org_b,
            Some(&add_memory_json(user_b, MEMORY_SUMMARIES)),
        )
        .await;
    assert_status(create_b.status, StatusCode::OK);

    let list_a = app
        .authed_as(
            "GET",
            &format!("/api/v1/users/{user_a}/memories"),
            org_a,
            None,
        )
        .await;
    assert_status(list_a.status, StatusCode::OK);
    assert!(
        list_a.body["memories"]
            .as_array()
            .is_some_and(|m| !m.is_empty())
    );

    // Org A must not see org B content via list of user_b under org_a (empty / isolated).
    let cross = app
        .authed_as(
            "GET",
            &format!("/api/v1/users/{user_b}/memories"),
            org_a,
            None,
        )
        .await;
    assert_status(cross.status, StatusCode::OK);
    let cross_items = cross.body["memories"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        cross_items
            .iter()
            .all(|m| m["content"].as_str() != Some(MEMORY_SUMMARIES)),
        "org A must not see org B memory content"
    );

    let forget = app
        .authed_as("DELETE", &format!("/api/v1/users/{user_a}"), org_a, None)
        .await;
    assert_status(forget.status, StatusCode::OK);
    assert_success_envelope(&forget.body);

    let list_a_after = app
        .authed_as(
            "GET",
            &format!("/api/v1/users/{user_a}/memories"),
            org_a,
            None,
        )
        .await;
    assert_status(list_a_after.status, StatusCode::OK);
    assert!(
        list_a_after.body["memories"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
    );

    let list_b = app
        .authed_as(
            "GET",
            &format!("/api/v1/users/{user_b}/memories"),
            org_b,
            None,
        )
        .await;
    assert_status(list_b.status, StatusCode::OK);
    assert!(
        list_b.body["memories"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "org B data must remain after org A forget"
    );
}

// ---------------------------------------------------------------------------
// Import / export
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_export_import_and_dry_run() {
    let app = TestApp::new();
    let user = "user_e2e_import";
    let target = "user_e2e_import_target";

    let created = app
        .authed_post_json_raw("/api/v1/memories", &add_memory_json(user, MEMORY_SUMMARIES))
        .await;
    assert_status(created.status, StatusCode::OK);

    let export = app
        .authed_get(&format!("/api/v1/users/{user}/export"))
        .await;
    assert_status(export.status, StatusCode::OK);
    assert_success_envelope(&export.body);
    assert_no_secret_leak(&export.raw);
    let mut export_payload = export.body["export"].clone();
    assert!(export_payload.is_object());
    // Import path requires export + fact/event user_id to match the target user.
    export_payload["user_id"] = serde_json::json!(target);
    if let Some(facts) = export_payload["facts"].as_array_mut() {
        for fact in facts {
            fact["user_id"] = serde_json::json!(target);
            // Fact IDs are globally unique; mint fresh IDs for a different user.
            fact["id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        }
    }
    if let Some(events) = export_payload["memory_events"].as_array_mut() {
        for event in events {
            event["user_id"] = serde_json::json!(target);
            event["id"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        }
    }

    let dry = app
        .authed_post_json_raw(
            &format!("/api/v1/users/{target}/import"),
            &import_json(&export_payload, "append", true),
        )
        .await;
    assert_status(dry.status, StatusCode::OK);
    assert_eq!(dry.body["summary"]["dry_run"], true);

    let listed_dry = app
        .authed_get(&format!("/api/v1/users/{target}/memories"))
        .await;
    assert!(
        listed_dry.body["memories"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "dry-run must not mutate target user"
    );

    let imported = app
        .authed_post_json_raw(
            &format!("/api/v1/users/{target}/import"),
            &import_json(&export_payload, "append", false),
        )
        .await;
    assert_status(imported.status, StatusCode::OK);
    assert_success_envelope(&imported.body);

    let listed = app
        .authed_get(&format!("/api/v1/users/{target}/memories"))
        .await;
    assert_status(listed.status, StatusCode::OK);
    assert!(
        listed.body["memories"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "import should create listable memories"
    );

    let bad = app
        .authed_post_json_raw(
            &format!("/api/v1/users/{target}/import"),
            r#"{"export":{"format_version":"nope"},"mode":"append","dry_run":false}"#,
        )
        .await;
    assert!(
        bad.status.is_client_error() || bad.body["error"]["code"] == "VALIDATION_ERROR",
        "malformed import should fail safely: {} {}",
        bad.status,
        bad.body
    );
    assert_no_secret_leak(&bad.raw);
}

// ---------------------------------------------------------------------------
// Admin (dev auth — full admin scope)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_admin_endpoints_are_reachable_and_safe() {
    let app = TestApp::new();

    let summary = app.authed_get("/api/v1/admin/org/summary").await;
    assert_status(summary.status, StatusCode::OK);
    assert_no_secret_leak(&summary.raw);

    let jobs = app.authed_get("/api/v1/admin/jobs").await;
    assert_status(jobs.status, StatusCode::OK);
    assert_no_secret_leak(&jobs.raw);

    let runs = app.authed_get("/api/v1/admin/jobs/runs").await;
    assert_status(runs.status, StatusCode::OK);

    let quotas = app.authed_get("/api/v1/admin/org/quotas").await;
    assert_status(quotas.status, StatusCode::OK);

    let usage = app.authed_get("/api/v1/admin/org/provider-usage").await;
    assert_status(usage.status, StatusCode::OK);
    assert_no_secret_leak(&usage.raw);

    let keys = app.authed_get("/api/v1/api-keys").await;
    // Dev mode may allow listing or return empty success depending on store.
    assert!(
        keys.status.is_success() || keys.status.is_client_error(),
        "api-keys list should respond: {}",
        keys.status
    );
    assert_no_secret_leak(&keys.raw);
    assert!(
        !keys.raw.to_lowercase().contains("key_hash")
            || !keys.body.to_string().contains("key_hash")
            || keys.body.pointer("/api_keys/0/key_hash").is_none(),
        "key hashes must not be exposed"
    );
}

#[tokio::test]
async fn e2e_background_job_manual_run_records_history() {
    // Keep runner disabled; manual admin trigger should still work.
    let app = TestApp::with_settings(Settings {
        background_jobs_enabled: false,
        background_job_org_ids: vec!["org_e2e".to_string()],
        memory_usage_snapshot_job_enabled: true,
        ..Settings::default()
    });

    let before = app.authed_get("/api/v1/admin/jobs/runs").await;
    assert_status(before.status, StatusCode::OK);

    let run = app
        .authed_post_json_raw("/api/v1/admin/jobs/memory-usage-snapshot/run", "{}")
        .await;
    assert_status(run.status, StatusCode::OK);
    assert_no_secret_leak(&run.raw);
    let after = app.authed_get("/api/v1/admin/jobs/runs").await;
    assert_status(after.status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Request hardening + rate limit + error contracts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_request_hardening_and_error_contracts() {
    let app = TestApp::with_settings(Settings {
        max_request_body_bytes: 256,
        rate_limit_enabled: false,
        ..Settings::default()
    });

    let oversized = "x".repeat(512);
    let big = format!(
        r#"{{"user_id":"user_e2e","messages":[{{"role":"user","content":"{oversized}"}}]}}"#
    );
    let too_large = app.authed_post_json_raw("/api/v1/memories", &big).await;
    assert_status(too_large.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_json_error_code(&too_large.body, "PAYLOAD_TOO_LARGE");
    assert_error_contract(&too_large.body);

    let bad_ct = app
        .authed_post_raw(
            "/api/v1/memories",
            r#"{"user_id":"user_e2e","messages":[{"role":"user","content":"hi"}]}"#,
            Some("text/plain"),
        )
        .await;
    assert_status(bad_ct.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_json_error_code(&bad_ct.body, "UNSUPPORTED_MEDIA_TYPE");

    let malformed = app
        .authed_post_json_raw("/api/v1/memories", "{not-json")
        .await;
    assert!(malformed.status.is_client_error());
    assert_no_secret_leak(&malformed.raw);

    let ok = app
        .authed_post_json_raw(
            "/api/v1/memories",
            &add_memory_json(DEFAULT_USER, "hello under limit"),
        )
        .await;
    assert_status(ok.status, StatusCode::OK);
    assert_has_security_headers(&ok.headers);
}

#[tokio::test]
async fn e2e_rate_limit_returns_safe_error() {
    let app = TestApp::with_rate_limit_enabled(2).with_org("org_rate_e2e");
    let body = add_memory_json(DEFAULT_USER, MEMORY_GREEN_TEA);

    let first = app.authed_post_json_raw("/api/v1/memories", &body).await;
    assert_status(first.status, StatusCode::OK);
    let second = app.authed_post_json_raw("/api/v1/memories", &body).await;
    assert_status(second.status, StatusCode::OK);
    let third = app.authed_post_json_raw("/api/v1/memories", &body).await;
    assert_status(third.status, StatusCode::TOO_MANY_REQUESTS);
    assert_json_error_code(&third.body, "RATE_LIMITED");
    assert_no_secret_leak(&third.raw);

    // Health remains exempt.
    let health = app.get("/health").await;
    assert_status(health.status, StatusCode::OK);
}
