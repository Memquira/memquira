use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use memcore_common::MemcoreError;
use serde_json::Value;

use crate::metrics::labels::{MatchedRouteLabel, request_class};
use crate::metrics::{InFlightGuard, record_http_request};
use crate::middleware::OrganizationContext;
use crate::response::ErrorBody;
use crate::state::AppState;

use super::request_id::{
    RequestId, insert_response_request_id_header, request_id_from_extensions, resolve_request_id,
};

/// Attaches a request ID, records metrics, enriches error responses, and logs latency.
pub async fn observe_request_lifecycle(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let header_name = state.settings.request_id_header.clone();
    let request_id = resolve_request_id(request.headers(), &header_name);
    request
        .extensions_mut()
        .insert(RequestId::new(request_id.as_str()));

    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started_at = Instant::now();
    let in_flight = state
        .settings
        .metrics_enabled
        .then(|| InFlightGuard::new(request_class(method.as_str(), &path)));

    let mut response = next.run(request).await;
    drop(in_flight);
    let latency_ms = started_at.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    insert_response_request_id_header(&mut response, &header_name, &request_id);

    if state.settings.metrics_enabled {
        // The matched route template travels on the response (`route_layer`); a
        // request that matched no route is labelled `unmatched`, never by its raw path.
        let route = response
            .extensions()
            .get::<MatchedRouteLabel>()
            .map_or_else(|| "unmatched".to_string(), |label| label.0.clone());
        record_http_request(
            method.as_str(),
            &route,
            status,
            started_at.elapsed().as_secs_f64(),
        );
        state.metrics.record_request(&path, status, latency_ms);
    }

    if status >= 400 {
        enrich_error_response(&mut response, request_id.as_str()).await;
    }

    let error_code = error_code_from_response(&response);
    let org_id = response
        .extensions()
        .get::<LoggedOrgId>()
        .map(|value| value.0.as_str());

    tracing::info!(
        request_id = %request_id.as_str(),
        method = %method,
        path = %path,
        status_code = status,
        latency_ms = latency_ms,
        org_id = org_id,
        error_code = error_code.as_deref(),
        "http_request_completed"
    );

    response
}

/// Logs protected-route requests with organization context after tenant middleware runs.
pub async fn log_protected_request(request: Request, next: Next) -> Response {
    let request_id = request_id_from_extensions(&request).map(|id| id.as_str().to_string());
    let org_id = request
        .extensions()
        .get::<OrganizationContext>()
        .map(|org| org.org_id.clone());

    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started_at = Instant::now();

    let mut response = next.run(request).await;
    let latency_ms = started_at.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    if let Some(org_id) = org_id {
        response
            .extensions_mut()
            .insert(LoggedOrgId(org_id.clone()));
        tracing::debug!(
            request_id = request_id.as_deref(),
            method = %method,
            path = %path,
            status_code = status,
            latency_ms = latency_ms,
            org_id = %org_id,
            "protected_request_completed"
        );
    }

    response
}

/// Organization id captured after tenant middleware for outer request logging.
#[derive(Debug, Clone)]
pub struct LoggedOrgId(pub String);

pub fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
    request: &Request,
) -> Response {
    let request_id = request_id_from_extensions(request).map(|id| id.as_str().to_string());
    let body = ErrorBody::new(code, message).with_request_id(request_id);
    (status, axum::Json(body)).into_response()
}

pub fn memcore_error_response(error: MemcoreError, request: &Request) -> Response {
    let (status, body) = ErrorBody::from_memcore_error(error);
    let request_id = request_id_from_extensions(request).map(|id| id.as_str().to_string());
    (status, axum::Json(body.with_request_id(request_id))).into_response()
}

async fn enrich_error_response(response: &mut Response, request_id: &str) {
    let headers = response.headers().clone();
    let status = response.status();

    let body = std::mem::replace(response.body_mut(), Body::empty());
    let collected = match body.collect().await {
        Ok(bytes) => bytes.to_bytes(),
        Err(_) => return,
    };

    let Ok(mut json) = serde_json::from_slice::<Value>(&collected) else {
        *response.body_mut() = Body::from(collected);
        return;
    };

    let Some(error_obj) = json.get_mut("error").and_then(Value::as_object_mut) else {
        *response.body_mut() = Body::from(collected);
        return;
    };

    if !error_obj.contains_key("request_id") {
        error_obj.insert(
            "request_id".to_string(),
            Value::String(request_id.to_string()),
        );
    }

    let enriched = match serde_json::to_vec(&json) {
        Ok(bytes) => bytes,
        Err(_) => collected.to_vec(),
    };

    *response.body_mut() = Body::from(enriched);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
}

fn error_code_from_response(response: &Response) -> Option<String> {
    if response.status().as_u16() < 400 {
        return None;
    }

    response
        .extensions()
        .get::<LoggedErrorCode>()
        .map(|value| value.0.clone())
}

/// Error code attached to responses for structured request logging.
#[derive(Debug, Clone)]
pub struct LoggedErrorCode(pub String);

pub fn attach_error_code(response: &mut Response, code: impl Into<String>) {
    response
        .extensions_mut()
        .insert(LoggedErrorCode(code.into()));
}
