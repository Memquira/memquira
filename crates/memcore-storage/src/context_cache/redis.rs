use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult, sanitize_redis_url_for_display};
use memcore_core::{CachedContextEntry, ContextCache, ContextCacheKey};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

use super::keys::{redis_context_cache_key, redis_context_index_key};

/// Redis-backed tenant-scoped context cache.
#[derive(Clone)]
pub struct RedisContextCache {
    manager: ConnectionManager,
    key_prefix: String,
    ttl_seconds: u64,
}

impl RedisContextCache {
    pub async fn connect(
        redis_url: &str,
        key_prefix: impl Into<String>,
        ttl_seconds: u64,
    ) -> MemcoreResult<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|err| map_redis_error("failed to open redis client", err, redis_url))?;
        let manager = client
            .get_connection_manager()
            .await
            .map_err(|err| map_redis_error("failed to connect to redis", err, redis_url))?;

        Ok(Self {
            manager,
            key_prefix: key_prefix.into(),
            ttl_seconds,
        })
    }
}

#[async_trait]
impl ContextCache for RedisContextCache {
    async fn get(&self, key: &ContextCacheKey) -> MemcoreResult<Option<CachedContextEntry>> {
        let redis_key = redis_context_cache_key(&self.key_prefix, key);
        let mut conn = self.manager.clone();
        let payload: Option<String> = conn
            .get(&redis_key)
            .await
            .map_err(|err| map_redis_command_error("context cache get failed", err))?;

        let Some(payload) = payload else {
            return Ok(None);
        };

        let entry: CachedContextEntry = serde_json::from_str(&payload).map_err(|err| {
            MemcoreError::StorageError(format!("context cache deserialization failed: {err}"))
        })?;

        let now = Utc::now();
        if entry.is_fully_expired(now) {
            let _: () = conn.del(&redis_key).await.map_err(|err| {
                map_redis_command_error("context cache delete stale entry failed", err)
            })?;
            return Ok(None);
        }

        if !entry.is_fresh(now) {
            return Ok(None);
        }

        Ok(Some(entry))
    }

    async fn get_any(&self, key: &ContextCacheKey) -> MemcoreResult<Option<CachedContextEntry>> {
        let redis_key = redis_context_cache_key(&self.key_prefix, key);
        let mut conn = self.manager.clone();
        let payload: Option<String> = conn
            .get(&redis_key)
            .await
            .map_err(|err| map_redis_command_error("context cache get failed", err))?;

        let Some(payload) = payload else {
            return Ok(None);
        };

        let entry: CachedContextEntry = serde_json::from_str(&payload).map_err(|err| {
            MemcoreError::StorageError(format!("context cache deserialization failed: {err}"))
        })?;

        let now = Utc::now();
        if entry.is_fully_expired(now) {
            let _: () = conn.del(&redis_key).await.map_err(|err| {
                map_redis_command_error("context cache delete stale entry failed", err)
            })?;
            return Ok(None);
        }

        Ok(Some(entry))
    }

    async fn set(&self, key: ContextCacheKey, entry: CachedContextEntry) -> MemcoreResult<()> {
        let redis_key = redis_context_cache_key(&self.key_prefix, &key);
        let index_key = redis_context_index_key(&self.key_prefix, &key.org_id, &key.user_id);
        let payload = serde_json::to_string(&entry).map_err(|err| {
            MemcoreError::StorageError(format!("context cache serialization failed: {err}"))
        })?;
        let redis_ttl = redis_entry_ttl_seconds(&entry, self.ttl_seconds);

        let mut conn = self.manager.clone();
        conn.set_ex::<_, _, ()>(&redis_key, payload, redis_ttl)
            .await
            .map_err(|err| map_redis_command_error("context cache set failed", err))?;
        conn.sadd::<_, _, ()>(&index_key, &redis_key)
            .await
            .map_err(|err| map_redis_command_error("context cache index update failed", err))?;
        conn.expire::<_, ()>(&index_key, redis_ttl as i64)
            .await
            .map_err(|err| map_redis_command_error("context cache index ttl failed", err))?;

        Ok(())
    }

    async fn invalidate_user(&self, org_id: &str, user_id: &str) -> MemcoreResult<usize> {
        let index_key = redis_context_index_key(&self.key_prefix, org_id, user_id);
        let mut conn = self.manager.clone();
        let members: Vec<String> = conn
            .smembers(&index_key)
            .await
            .map_err(|err| map_redis_command_error("context cache index read failed", err))?;

        if members.is_empty() {
            let _: () = conn
                .del(&index_key)
                .await
                .map_err(|err| map_redis_command_error("context cache index delete failed", err))?;
            return Ok(0);
        }

        let mut pipe = redis::pipe();
        for member in &members {
            pipe.del(member);
        }
        pipe.del(&index_key);
        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|err| map_redis_command_error("context cache invalidation failed", err))?;

        Ok(members.len())
    }
}

fn map_redis_command_error(context: &str, err: redis::RedisError) -> MemcoreError {
    MemcoreError::StorageError(format!("{context}: {err}"))
}

fn redis_entry_ttl_seconds(entry: &CachedContextEntry, default_ttl_seconds: u64) -> u64 {
    let now = Utc::now();
    let until = entry.effective_stale_until();
    if until > now {
        (until - now).num_seconds().max(1) as u64
    } else {
        default_ttl_seconds.max(1)
    }
}

fn map_redis_error(context: &str, err: redis::RedisError, redis_url: &str) -> MemcoreError {
    MemcoreError::StorageError(format!(
        "{context} (redis_url={}): {err}",
        sanitize_redis_url_for_display(redis_url)
    ))
}

#[cfg(test)]
mod serde_tests {
    use chrono::{Duration, Utc};
    use memcore_core::{
        CachedContextEntry, ContextBudgetUsage, ContextCompressionUsage, MemorySearchResult,
        MemoryType,
    };
    use uuid::Uuid;

    #[test]
    fn cached_context_entry_serializes_and_deserializes() {
        let entry = CachedContextEntry {
            context: "assembled context".to_string(),
            memories: vec![MemorySearchResult {
                fact_id: Uuid::new_v4(),
                content: "User likes Rust.".to_string(),
                memory_type: MemoryType::Preference,
                score: 0.5,
                vector_score: None,
                confidence: 0.9,
                importance: 0.8,
                valid_at: None,
                metadata: serde_json::json!({"source": "test"}),
                structure: Default::default(),
            }],
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::seconds(300),
            stale_until: None,
            budget: ContextBudgetUsage {
                max_tokens: 2000,
                reserved_tokens: 300,
                available_tokens: 1700,
                used_tokens: 42,
                included_memories: 1,
                skipped_memories: 0,
            },
            compression: ContextCompressionUsage::disabled(),
        };

        let encoded = serde_json::to_string(&entry).expect("serialize");
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("api_key"));
        let decoded: CachedContextEntry = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.context, entry.context);
        assert_eq!(decoded.memories.len(), 1);
        assert_eq!(decoded.budget.used_tokens, 42);
        assert!(!decoded.compression.enabled);
    }
}
