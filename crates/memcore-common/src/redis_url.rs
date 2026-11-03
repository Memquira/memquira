/// Masks Redis URL credentials for safe error messages and debug output.
pub fn sanitize_redis_url_for_display(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(rest) = trimmed
        .strip_prefix("redis://")
        .or_else(|| trimmed.strip_prefix("rediss://"))
    {
        let scheme = if trimmed.starts_with("rediss://") {
            "rediss://"
        } else {
            "redis://"
        };
        if let Some(at_pos) = rest.find('@') {
            let host_part = &rest[at_pos + 1..];
            return format!("{scheme}***@{host_part}");
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_redis_url_for_display;

    #[test]
    fn redis_password_not_exposed_in_sanitized_url() {
        let sanitized = sanitize_redis_url_for_display("redis://:secret_password@localhost:6379/0");
        assert!(!sanitized.contains("secret_password"));
        assert!(sanitized.contains("***@"));
        assert!(sanitized.contains("localhost:6379"));
    }
}
