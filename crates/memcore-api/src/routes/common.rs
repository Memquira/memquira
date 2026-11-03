use axum::Json;
use axum::response::{IntoResponse, Response};
use memcore_common::MemcoreError;
use memcore_core::ApiKeyScope;

use crate::response::ErrorBody;
use crate::security::{AuthContext, ensure_any_scope, ensure_scope};

#[derive(Debug)]
pub struct ApiError((axum::http::StatusCode, Json<ErrorBody>));

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = self.0;
        (status, body).into_response()
    }
}

impl From<MemcoreError> for ApiError {
    fn from(error: MemcoreError) -> Self {
        let (status, body) = ErrorBody::from_memcore_error(error);
        Self((status, Json(body)))
    }
}

pub fn check_scope(auth: Option<&AuthContext>, scope: ApiKeyScope) -> Result<(), ApiError> {
    if let Some(auth) = auth {
        ensure_scope(auth, scope).map_err(|error| {
            ApiError((
                error.status,
                Json(ErrorBody::new(error.code, error.message)),
            ))
        })?;
    }
    Ok(())
}

pub fn check_any_scope(auth: Option<&AuthContext>, scopes: &[ApiKeyScope]) -> Result<(), ApiError> {
    if let Some(auth) = auth {
        ensure_any_scope(auth, scopes).map_err(|error| {
            ApiError((
                error.status,
                Json(ErrorBody::new(error.code, error.message)),
            ))
        })?;
    }
    Ok(())
}
