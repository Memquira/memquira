//! Inject safe build metadata for the version endpoint.
//!
//! Environment variables (optional; default to `unknown`):
//! - `MEMCORE_BUILD_GIT_SHA`
//! - `MEMCORE_BUILD_TIMESTAMP`
//! - `MEMCORE_BUILD_PROFILE` (falls back to Cargo `PROFILE`)

fn main() {
    let git_sha = env_or_unknown("MEMCORE_BUILD_GIT_SHA");
    let timestamp = env_or_unknown("MEMCORE_BUILD_TIMESTAMP");
    let profile = std::env::var("MEMCORE_BUILD_PROFILE")
        .or_else(|_| std::env::var("PROFILE"))
        .unwrap_or_else(|_| "unknown".to_string());
    let rustc_version = rustc_version().unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=MEMCORE_BUILD_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=MEMCORE_BUILD_TIMESTAMP={timestamp}");
    println!("cargo:rustc-env=MEMCORE_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=MEMCORE_BUILD_RUSTC_VERSION={rustc_version}");

    println!("cargo:rerun-if-env-changed=MEMCORE_BUILD_GIT_SHA");
    println!("cargo:rerun-if-env-changed=MEMCORE_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=MEMCORE_BUILD_PROFILE");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=RUSTC");
}

fn env_or_unknown(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| "unknown".to_string())
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
