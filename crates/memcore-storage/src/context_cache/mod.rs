mod keys;

pub use keys::{redis_context_cache_key, redis_context_index_key};
pub use memcore_common::sanitize_redis_url_for_display;

#[cfg(feature = "redis-cache")]
mod redis;

#[cfg(feature = "redis-cache")]
pub use redis::RedisContextCache;
