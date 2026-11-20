use async_trait::async_trait;
use memquira_common::MemquiraResult;

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
    async fn find_by_hash(&self, key_hash: &str) -> MemquiraResult<Option<ApiKeyRecord>>;

    async fn insert_api_key(&self, record: ApiKeyRecord) -> MemquiraResult<ApiKeyRecord>;

    async fn revoke_api_key(&self, org_id: &str, key_id: uuid::Uuid) -> MemquiraResult<()>;

    async fn list_api_keys(&self, query: ApiKeyListQuery) -> MemquiraResult<Vec<ApiKeyRecord>>;
}
