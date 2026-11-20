//! Inject safe build metadata for the version endpoint.
//!
//! Environment variables (optional; default to `unknown`):
//! - `MEMQUIRA_BUILD_GIT_SHA`
//! - `MEMQUIRA_BUILD_TIMESTAMP`
//! - `MEMQUIRA_BUILD_PROFILE` (falls back to Cargo `PROFILE`)
//!
//! The pre-0.3 names `MEMCORE_BUILD_*` are still read when the new name is unset.

fn main() {
    let git_sha = env_or_unknown("MEMQUIRA_BUILD_GIT_SHA");
    let timestamp = env_or_unknown("MEMQUIRA_BUILD_TIMESTAMP");
    let profile = build_env("MEMQUIRA_BUILD_PROFILE")
        .or_else(|| std::env::var("PROFILE").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let rustc_version = rustc_version().unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=MEMQUIRA_BUILD_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=MEMQUIRA_BUILD_TIMESTAMP={timestamp}");
    println!("cargo:rustc-env=MEMQUIRA_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=MEMQUIRA_BUILD_RUSTC_VERSION={rustc_version}");

    for name in ["GIT_SHA", "TIMESTAMP", "PROFILE"] {
        println!("cargo:rerun-if-env-changed=MEMQUIRA_BUILD_{name}");
        println!("cargo:rerun-if-env-changed=MEMCORE_BUILD_{name}");
    }
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=RUSTC");
}

/// `key`, or its pre-0.3 `MEMCORE_*` name when `key` is unset.
fn build_env(key: &str) -> Option<String> {
    std::env::var(key).ok().or_else(|| {
        key.strip_prefix("MEMQUIRA_")
            .and_then(|rest| std::env::var(format!("MEMCORE_{rest}")).ok())
    })
}

fn env_or_unknown(key: &str) -> String {
    build_env(key).unwrap_or_else(|| "unknown".to_string())
}

fn rustc_version() -> Option<String> {
    let rustc = std::env::var("RUSTC").ok()?;
    let output = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}
