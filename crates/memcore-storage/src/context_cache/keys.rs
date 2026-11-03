use memcore_core::{ContextCacheKey, stable_sha256_hex};

/// Builds the Redis cache key for a tenant-scoped context entry.
///
/// Format: `{prefix}:context:{org_hash}:{user_hash}:{query_hash}:{options_hash}`
pub fn redis_context_cache_key(prefix: &str, key: &ContextCacheKey) -> String {
    let org_hash = stable_sha256_hex(&key.org_id);
    let user_hash = stable_sha256_hex(&key.user_id);
    format!(
        "{}:context:{}:{}:{}:{}",
        sanitize_key_prefix(prefix),
        org_hash,
        user_hash,
        key.query_hash,
        key.options_hash
    )
}

/// Builds the per-user Redis index set key listing all cached context keys.
///
/// Format: `{prefix}:context:index:{org_hash}:{user_hash}`
pub fn redis_context_index_key(prefix: &str, org_id: &str, user_id: &str) -> String {
    format!(
        "{}:context:index:{}:{}",
        sanitize_key_prefix(prefix),
        stable_sha256_hex(org_id),
        stable_sha256_hex(user_id)
    )
}

fn sanitize_key_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        "memcore".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore_core::ContextCacheKey;

    fn sample_key(org_id: &str, user_id: &str, query: &str, options: &str) -> ContextCacheKey {
        ContextCacheKey {
            org_id: org_id.to_string(),
            user_id: user_id.to_string(),
            query_hash: stable_sha256_hex(query),
            options_hash: stable_sha256_hex(options),
        }
    }

    #[test]
    fn redis_key_builder_is_deterministic() {
        let key = sample_key("org_a", "user_a", "query", "options");
        let first = redis_context_cache_key("memcore", &key);
        let second = redis_context_cache_key("memcore", &key);
        assert_eq!(first, second);
    }

    #[test]
    fn redis_key_includes_org_user_isolation() {
        let base = sample_key("org_a", "user_a", "same", "same");
        let other_org = sample_key("org_b", "user_a", "same", "same");
        let other_user = sample_key("org_a", "user_b", "same", "same");

        let base_key = redis_context_cache_key("memcore", &base);
        assert_ne!(base_key, redis_context_cache_key("memcore", &other_org));
        assert_ne!(base_key, redis_context_cache_key("memcore", &other_user));
        assert_ne!(
            redis_context_index_key("memcore", "org_a", "user_a"),
            redis_context_index_key("memcore", "org_b", "user_a")
        );
    }

    #[test]
    fn redis_key_changes_when_query_or_options_hash_changes() {
        let base = sample_key("org_a", "user_a", "alpha", "opts");
        let query_changed = sample_key("org_a", "user_a", "beta", "opts");
        let options_changed = sample_key("org_a", "user_a", "alpha", "other_opts");

        assert_ne!(
            redis_context_cache_key("memcore", &base),
            redis_context_cache_key("memcore", &query_changed)
        );
        assert_ne!(
            redis_context_cache_key("memcore", &base),
            redis_context_cache_key("memcore", &options_changed)
        );
    }

    #[test]
    fn redis_key_does_not_include_raw_org_user_or_query() {
        let raw_org = "org_with_secret_name";
        let raw_user = "user_with_secret_id";
        let raw_query = "very long sensitive query string that should not appear verbatim";
        let key = sample_key(raw_org, raw_user, raw_query, "opts");
        let redis_key = redis_context_cache_key("memcore", &key);

        assert!(!redis_key.contains(raw_org));
        assert!(!redis_key.contains(raw_user));
        assert!(!redis_key.contains(raw_query));
        assert!(!redis_key.contains("mc_live_"));
        assert!(!redis_key.contains("Bearer "));
    }

    #[test]
    fn empty_prefix_defaults_to_memcore() {
        let key = sample_key("org", "user", "q", "o");
        assert!(redis_context_cache_key("", &key).starts_with("memcore:context:"));
    }
}
