use chrono::{DateTime, Utc};
use memcore_common::MemcoreError;
use memcore_core::{ApiKeyRecord, ApiKeyScope};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreateApiKeyResponse {
    pub status: &'static str,
    pub api_key: ApiKeyItemResponse,
    /// Raw API key returned only once at creation time.
    pub raw_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListApiKeysQuery {
    #[serde(default)]
    pub include_revoked: bool,
    #[serde(default = "default_list_api_keys_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

pub fn default_list_api_keys_limit() -> usize {
    50
}

pub const MAX_LIST_API_KEYS_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListApiKeysResponse {
    pub status: &'static str,
    pub api_keys: Vec<ApiKeyItemResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevokeApiKeyResponse {
    pub status: &'static str,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiKeyItemResponse {
    pub id: Uuid,
    pub org_id: String,
    pub name: String,
    pub scopes: Vec<ApiKeyScopeResponse>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// API-facing scope labels (PascalCase) separate from core snake_case serde.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "PascalCase")]
pub enum ApiKeyScopeResponse {
    MemoryRead,
    MemoryWrite,
    MemoryDelete,
    UserDelete,
    AuditRead,
    AdminRead,
    AdminWrite,
}

impl From<ApiKeyScope> for ApiKeyScopeResponse {
    fn from(value: ApiKeyScope) -> Self {
        match value {
            ApiKeyScope::MemoryRead => Self::MemoryRead,
            ApiKeyScope::MemoryWrite => Self::MemoryWrite,
            ApiKeyScope::MemoryDelete => Self::MemoryDelete,
            ApiKeyScope::UserDelete => Self::UserDelete,
            ApiKeyScope::AuditRead => Self::AuditRead,
            ApiKeyScope::AdminRead => Self::AdminRead,
            ApiKeyScope::AdminWrite => Self::AdminWrite,
        }
    }
}

impl From<&ApiKeyRecord> for ApiKeyItemResponse {
    fn from(record: &ApiKeyRecord) -> Self {
        Self {
            id: record.id,
            org_id: record.org_id.clone(),
            name: record.name.clone(),
            scopes: record.scopes.iter().copied().map(Into::into).collect(),
            created_at: record.created_at,
            revoked_at: record.revoked_at,
        }
    }
}

pub fn parse_api_key_scope_label(value: &str) -> Result<ApiKeyScope, MemcoreError> {
    match value.trim() {
        "MemoryRead" => Ok(ApiKeyScope::MemoryRead),
        "MemoryWrite" => Ok(ApiKeyScope::MemoryWrite),
        "MemoryDelete" => Ok(ApiKeyScope::MemoryDelete),
        "UserDelete" => Ok(ApiKeyScope::UserDelete),
        "AuditRead" => Ok(ApiKeyScope::AuditRead),
        "AdminRead" => Ok(ApiKeyScope::AdminRead),
        "AdminWrite" => Ok(ApiKeyScope::AdminWrite),
        _ => Err(MemcoreError::ValidationError(
            "invalid API key scope".to_string(),
        )),
    }
}

pub fn parse_create_api_key_request(
    request: CreateApiKeyRequest,
) -> Result<(String, Vec<ApiKeyScope>), MemcoreError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(MemcoreError::ValidationError(
            "name cannot be empty".to_string(),
        ));
    }

    if request.scopes.is_empty() {
        return Err(MemcoreError::ValidationError(
            "scopes cannot be empty".to_string(),
        ));
    }

    let scopes = request
        .scopes
        .iter()
        .map(|scope| parse_api_key_scope_label(scope))
        .collect::<Result<Vec<_>, _>>()?;

    Ok((name.to_string(), scopes))
}
