use async_trait::async_trait;
use memcore_common::MemcoreResult;

use crate::ApiKeyRecord;
use crate::pagination::PageCursor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyListQuery {
    pub org_id: String,
    pub include_revoked: bool,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

#[async_trait]
pub trait ApiKeyStore: Send + Sync {
    async fn find_by_hash(&self, key_hash: &str) -> MemcoreResult<Option<ApiKeyRecord>>;

    async fn insert_api_key(&self, record: ApiKeyRecord) -> MemcoreResult<ApiKeyRecord>;

    async fn revoke_api_key(&self, org_id: &str, key_id: uuid::Uuid) -> MemcoreResult<()>;

    async fn list_api_keys(&self, query: ApiKeyListQuery) -> MemcoreResult<Vec<ApiKeyRecord>>;
}
