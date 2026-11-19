//! Safe synthetic fixtures for E2E tests (no real PII / secrets).

pub const DEV_API_KEY: &str = "memcore_dev_key";
pub const DEFAULT_ORG: &str = "org_e2e";
pub const DEFAULT_USER: &str = "user_e2e";
pub const SMOKE_USER: &str = "smoke-test-user";

pub const MEMORY_GREEN_TEA: &str = "User likes green tea.";
pub const MEMORY_RUST_API: &str = "User is working on a Rust API test.";
pub const MEMORY_SUMMARIES: &str = "User prefers concise technical summaries.";

pub fn add_memory_json(user_id: &str, content: &str) -> String {
    format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{ "source": "e2e" }}
        }}"#
    )
}

pub fn search_json(user_id: &str, query: &str) -> String {
    format!(
        r#"{{
          "user_id": "{user_id}",
          "query": "{query}"
        }}"#
    )
}

pub fn context_json(user_id: &str, query: &str) -> String {
    format!(
        r#"{{
          "user_id": "{user_id}",
          "query": "{query}",
          "max_memories": 10
        }}"#
    )
}

pub fn import_json(export: &serde_json::Value, mode: &str, dry_run: bool) -> String {
    serde_json::json!({
        "export": export,
        "mode": mode,
        "restore_events": false,
        "dry_run": dry_run,
    })
    .to_string()
}
