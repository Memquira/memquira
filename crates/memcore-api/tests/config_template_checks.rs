//! Every shipped environment / Compose template resolves, through
//! the real configuration parser, to a context-cache configuration the startup policy accepts as
//! safe (`disabled`), and no template suggests a refused backend (`redis`, or `memory` outside the
//! documented local single-process opt-in). One test: it mutates the process environment.

use std::path::{Path, PathBuf};

use memcore_config::{ContextCacheBackend, Settings};

const TEMPLATES: &[&str] = &[
    ".env.example",
    ".env.production.example",
    "docker/local-starter.env.example",
    "docker/docker-compose.local-starter.ollama-embed.yml",
    "docker/docker-compose.local-starter.yml",
    "docker/docker-compose.local.yml",
    "docker/docker-compose.prod.yml",
    "profiles/byom.env.example",
    "profiles/local-starter.env.example",
];

/// The variables that decide the context-cache policy.
const KEYS: &[&str] = &[
    "MEMCORE_CONTEXT_CACHE_BACKEND",
    "MEMCORE_CONTEXT_CACHE_ENABLED",
    "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS",
    "MEMCORE_ENV",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `KEY=value`, `KEY: value`, `export KEY=value`, `: "${KEY:=value}"`; `${X:-default}` resolves
/// to its default. Returns (key, value, commented).
fn assignments(text: &str) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let commented = trimmed.starts_with('#');
        let body = trimmed.trim_start_matches('#').trim();
        for key in KEYS.iter().chain(["MEMCORE_REDIS_URL"].iter()) {
            // Shell default form: : "${KEY:=value}"
            if let Some(rest) = body.strip_prefix(&format!(": \"${{{key}:=")) {
                out.push((
                    key.to_string(),
                    rest.trim_end_matches("}\"").to_string(),
                    commented,
                ));
                continue;
            }
            let body = body.strip_prefix("export ").unwrap_or(body);
            let Some(rest) = body.strip_prefix(key) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) else {
                continue;
            };
            let mut value = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if let Some(inner) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
                value = inner
                    .split_once(":-")
                    .map(|(_, default)| default.to_string())
                    .unwrap_or_default();
            }
            out.push((key.to_string(), value, commented));
        }
    }
    out
}

#[test]
fn cg13_every_shipped_template_resolves_to_a_safe_context_cache() {
    let mut report = Vec::new();
    for template in TEMPLATES {
        let path = root().join(template);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{template}: {e}"));
        let found = assignments(&text);
        // No template may suggest a refused backend, even commented out.
        for (key, value, commented) in &found {
            if key == "MEMCORE_CONTEXT_CACHE_BACKEND" {
                let value = value.trim().to_ascii_lowercase();
                assert!(
                    value == "disabled" || value.is_empty(),
                    "{template}: {key}={value} (commented: {commented})"
                );
            }
            if key == "MEMCORE_CONTEXT_CACHE_ENABLED" {
                assert!(
                    !*commented || value != "true",
                    "{template}: suggests the legacy flag"
                );
            }
        }
        // The real parser on the template's own (uncommented) values.
        // SAFETY: this file's single test is the only code touching the environment.
        unsafe {
            for (key, _) in std::env::vars() {
                if key.starts_with("MEMCORE_") {
                    std::env::remove_var(key);
                }
            }
            for (key, value, commented) in &found {
                if !commented && KEYS.contains(&key.as_str()) && !value.is_empty() {
                    std::env::set_var(key, value);
                }
            }
        }
        let settings = Settings::from_env()
            .unwrap_or_else(|error| panic!("{template}: the real parser refused it: {error}"));
        assert_eq!(
            settings.context_cache_backend,
            ContextCacheBackend::Disabled,
            "{template}"
        );
        settings
            .check_context_cache_policy()
            .unwrap_or_else(|error| panic!("{template}: {error}"));
        report.push(format!(
            "{template}: {} cache assignment(s) -> disabled",
            found
                .iter()
                .filter(|(k, _, c)| !c && k.contains("CONTEXT_CACHE"))
                .count()
        ));
    }
    eprintln!("{}", report.join("\n"));
}
