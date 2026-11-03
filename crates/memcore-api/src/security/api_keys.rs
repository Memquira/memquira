use memcore_common::hash_api_key;
use memcore_core::ApiKeyScope;
use uuid::Uuid;

pub use memcore_common::hash_api_key as hash_raw_api_key;

/// Authenticated API key context attached after successful database auth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub org_id: String,
    pub api_key_id: Uuid,
    pub scopes: Vec<ApiKeyScope>,
}

impl AuthContext {
    pub fn has_scope(&self, scope: ApiKeyScope) -> bool {
        self.scopes.contains(&scope)
    }
}

pub fn hash_api_key_with_pepper(pepper: &str, raw_key: &str) -> String {
    hash_api_key(pepper, raw_key)
}

/// Generates a one-time raw API key in `mc_live_<token>` format.
pub fn generate_raw_api_key() -> String {
    format!("mc_live_{}", Uuid::new_v4().simple())
}
