use axum::http::StatusCode;
use memquira_common::{MemquiraError, safe_error_message};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
                details: None,
                request_id: None,
            },
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.error.details = Some(details);
        self
    }

    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.error.request_id = request_id;
        self
    }

    pub fn from_memquira_error(error: MemquiraError) -> (StatusCode, Self) {
        // Every MemquiraError that becomes an HTTP response passes through here exactly once, which
        // makes this the one place the truncation counter cannot be missed.
        if let MemquiraError::ProviderOutputTruncated { operation, .. } = &error {
            crate::metrics::ops::record_provider_output_truncated(operation);
        }
        let status = match &error {
            MemquiraError::Unauthorized => StatusCode::UNAUTHORIZED,
            MemquiraError::Forbidden => StatusCode::FORBIDDEN,
            MemquiraError::BadRequest(_) => StatusCode::BAD_REQUEST,
            MemquiraError::NotFound(_) => StatusCode::NOT_FOUND,
            MemquiraError::Conflict(_) => StatusCode::CONFLICT,
            MemquiraError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            MemquiraError::ProviderError(_) => StatusCode::BAD_GATEWAY,
            MemquiraError::StorageError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MemquiraError::MigrationError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MemquiraError::ValidationError(_) => StatusCode::BAD_REQUEST,
            MemquiraError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MemquiraError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            MemquiraError::Overloaded(_) => StatusCode::SERVICE_UNAVAILABLE,
            MemquiraError::ProviderThrottled { .. } => StatusCode::SERVICE_UNAVAILABLE,
            MemquiraError::QuotaExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
            MemquiraError::ProviderGuardrailViolation { .. } => StatusCode::FORBIDDEN,
            // The upstream model returned an unusable (incomplete) response.
            MemquiraError::ProviderOutputTruncated { .. } => StatusCode::BAD_GATEWAY,
            MemquiraError::ProviderOutputInvalid { .. } => StatusCode::BAD_GATEWAY,
        };

        let code = match &error {
            MemquiraError::Unauthorized => "UNAUTHORIZED",
            MemquiraError::Forbidden => "FORBIDDEN",
            MemquiraError::BadRequest(_) => "BAD_REQUEST",
            MemquiraError::NotFound(_) => "NOT_FOUND",
            // A reused client message_id that identifies a different message.
            error @ MemquiraError::Conflict(_) if error.is_message_id_conflict() => {
                "MESSAGE_ID_CONFLICT"
            }
            // Append-import retry / re-application and deleted-id conflicts.
            error @ MemquiraError::Conflict(_) if error.is_import_already_applied() => {
                "IMPORT_ALREADY_APPLIED"
            }
            error @ MemquiraError::Conflict(_) if error.is_import_id_conflict() => {
                "IMPORT_ID_CONFLICT"
            }
            MemquiraError::Conflict(_) => "CONFLICT",
            MemquiraError::RateLimited => "RATE_LIMITED",
            MemquiraError::ProviderError(_) => "PROVIDER_ERROR",
            MemquiraError::StorageError(_) => "STORAGE_ERROR",
            MemquiraError::MigrationError(_) => "MIGRATION_ERROR",
            MemquiraError::ValidationError(_) => "VALIDATION_ERROR",
            MemquiraError::Internal(_) => "INTERNAL_ERROR",
            MemquiraError::Timeout(_) => "TIMEOUT",
            MemquiraError::Overloaded(_) => "OVERLOADED",
            MemquiraError::ProviderThrottled { .. } => "PROVIDER_THROTTLED",
            MemquiraError::QuotaExceeded { .. } => "QUOTA_EXCEEDED",
            MemquiraError::ProviderGuardrailViolation { .. } => "PROVIDER_GUARDRAIL_VIOLATION",
            MemquiraError::ProviderOutputTruncated { .. } => "PROVIDER_OUTPUT_TRUNCATED",
            MemquiraError::ProviderOutputInvalid { .. } => "PROVIDER_OUTPUT_INVALID",
        };

        let body = Self::new(code, safe_error_message(&error));
        let body = match &error {
            MemquiraError::QuotaExceeded {
                kind,
                limit,
                current,
                requested,
                ..
            } => body.with_details(json!({
                "kind": kind,
                "limit": limit,
                "current": current,
                "requested": requested,
            })),
            MemquiraError::ProviderGuardrailViolation {
                code: guard_code, ..
            } => body.with_details(json!({ "guardrail_code": guard_code })),
            MemquiraError::ProviderOutputTruncated {
                operation,
                max_output_tokens,
            } => body.with_details(json!({
                "operation": operation,
                "max_output_tokens": max_output_tokens,
            })),
            MemquiraError::ProviderOutputInvalid { operation, reason } => {
                body.with_details(json!({
                    "operation": operation,
                    "reason": reason,
                }))
            }
            _ => body,
        };

        (status, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_a_distinct_bad_gateway_error_with_its_operation_and_limit() {
        let (status, body) = ErrorBody::from_memquira_error(
            MemquiraError::provider_output_truncated("extract_facts", Some(2_048)),
        );

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body.error.code, "PROVIDER_OUTPUT_TRUNCATED");
        assert_eq!(
            body.error.details,
            Some(json!({ "operation": "extract_facts", "max_output_tokens": 2048 }))
        );
        // The message is built from the operation name alone, never from model output.
        assert_eq!(
            body.error.message,
            "provider output truncated at the output-token limit during extract_facts"
        );
    }

    #[test]
    fn a_message_id_conflict_is_a_409_naming_only_the_position() {
        let (status, body) = ErrorBody::from_memquira_error(MemquiraError::message_id_conflict(2));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.error.code, "MESSAGE_ID_CONFLICT");
        assert!(body.error.message.contains("messages[2].message_id"));
        assert!(body.error.details.is_none());

        // Any other conflict keeps its old code.
        let (status, body) =
            ErrorBody::from_memquira_error(MemquiraError::Conflict("already exists".to_string()));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.error.code, "CONFLICT");
    }

    #[test]
    fn a_generic_provider_error_keeps_its_old_code() {
        let (status, body) = ErrorBody::from_memquira_error(MemquiraError::ProviderError(
            "upstream 500".to_string(),
        ));
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body.error.code, "PROVIDER_ERROR");
    }
}
