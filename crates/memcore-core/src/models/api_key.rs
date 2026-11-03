use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyScope {
    MemoryRead,
    MemoryWrite,
    MemoryDelete,
    UserDelete,
    AuditRead,
    AdminRead,
    AdminWrite,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub org_id: String,
    pub name: String,
    pub key_hash: String,
    pub scopes: Vec<ApiKeyScope>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKeyRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}
