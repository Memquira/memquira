use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use memcore_common::MemcoreResult;
use memcore_core::TenantContext;

use crate::observability::{attach_error_code, error_response};
use crate::security::AuthContext;

pub const ORG_HEADER: &str = "X-Organization-ID";

/// Organization scope extracted from `X-Organization-ID` (tenant middleware only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationContext {
    pub org_id: String,
}

impl OrganizationContext {
    pub fn tenant_with_user_id(&self, user_id: impl Into<String>) -> MemcoreResult<TenantContext> {
        TenantContext::new(self.org_id.clone(), user_id)
    }
}

/// Validates `X-Organization-ID` and attaches [`OrganizationContext`] to the request.
///
/// Runs after auth middleware on protected routes. In database auth mode, the header must
/// match the authenticated API key's `org_id`.
pub async fn require_organization(request: Request, next: Next) -> Response {
    let auth_context = request.extensions().get::<AuthContext>().cloned();

    match extract_organization(request.headers()) {
        Ok(org) => {
            if let Some(auth) = &auth_context
                && auth.org_id != org.org_id
            {
                let mut response = error_response(
                    StatusCode::FORBIDDEN,
                    "FORBIDDEN",
                    "organization header does not match api key",
                    &request,
                );
                attach_error_code(&mut response, "FORBIDDEN");
                return response;
            }

            let mut request = request;
            request.extensions_mut().insert(org);
            next.run(request).await
        }
        Err((status, code, message)) => {
            let mut response = error_response(status, code, message, &request);
            attach_error_code(&mut response, code);
            response
        }
    }
}

fn extract_organization(
    headers: &HeaderMap,
) -> Result<OrganizationContext, (StatusCode, &'static str, &'static str)> {
    let Some(value) = headers.get(ORG_HEADER) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "missing X-Organization-ID header",
        ));
    };

    let header_str = value.to_str().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "X-Organization-ID must be valid UTF-8",
        )
    })?;

    let org_id = header_str.trim();
    if org_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "X-Organization-ID cannot be empty",
        ));
    }

    Ok(OrganizationContext {
        org_id: org_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::{ORG_HEADER, extract_organization};

    #[test]
    fn missing_header_returns_error() {
        let headers = HeaderMap::new();
        assert!(extract_organization(&headers).is_err());
    }

    #[test]
    fn empty_header_returns_error() {
        let mut headers = HeaderMap::new();
        headers.insert(ORG_HEADER, "   ".parse().unwrap());
        assert!(extract_organization(&headers).is_err());
    }

    #[test]
    fn trims_whitespace_from_org_id() {
        let mut headers = HeaderMap::new();
        headers.insert(ORG_HEADER, "  org_abc  ".parse().unwrap());
        let org = extract_organization(&headers).expect("org should parse");
        assert_eq!(org.org_id, "org_abc");
    }
}
