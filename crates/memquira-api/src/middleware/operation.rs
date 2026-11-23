//! `X-Memquira-Operation`: scopes a caller's operation token over the request, so its provider calls carry it
//! (see `memquira_providers::operation`). Only when `MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED=true`; otherwise the
//! header is ignored. Memquira does not interpret the token; a provider-side proxy verifies it.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use memquira_providers::operation::{
    OPERATION_HEADER, is_valid_operation_token, with_operation_token,
};

use crate::observability::{attach_error_code, error_response};
use crate::state::AppState;

pub async fn forward_operation_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.settings.provider_operation_header_enabled {
        return next.run(request).await;
    }
    let token = match request.headers().get(OPERATION_HEADER) {
        None => None,
        Some(value) => match value.to_str().ok().filter(|t| is_valid_operation_token(t)) {
            Some(token) => Some(token.to_string()),
            None => {
                let mut response = error_response(
                    StatusCode::BAD_REQUEST,
                    "INVALID_OPERATION_TOKEN",
                    "X-Memquira-Operation must be 1 to 512 characters of A-Z, a-z, 0-9, '.', '_', '-' or ':'",
                    &request,
                );
                attach_error_code(&mut response, "INVALID_OPERATION_TOKEN");
                return response;
            }
        },
    };
    with_operation_token(token, next.run(request)).await
}
