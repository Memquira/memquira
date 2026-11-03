//! Export the utoipa OpenAPI document to JSON (no server required).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use memcore_api::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let out = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("openapi/memcore.openapi.json"));

    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
        && let Err(error) = fs::create_dir_all(parent)
    {
        eprintln!("failed to create {}: {error}", parent.display());
        return ExitCode::FAILURE;
    }

    let spec = match ApiDoc::openapi().to_pretty_json() {
        Ok(json) => json,
        Err(error) => {
            eprintln!("failed to serialize OpenAPI: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Safety: refuse to write if the document somehow contains obvious secret placeholders.
    let lowered = spec.to_lowercase();
    for needle in [
        "sk-live-",
        "sk_test_",
        "postgres://",
        "redis://",
        "bearer memcore_",
    ] {
        if lowered.contains(needle) {
            eprintln!("refusing to write OpenAPI: unexpected secret-like content ({needle})");
            return ExitCode::FAILURE;
        }
    }

    if let Err(error) = fs::write(&out, format!("{spec}\n")) {
        eprintln!("failed to write {}: {error}", out.display());
        return ExitCode::FAILURE;
    }

    println!("wrote {}", out.display());
    ExitCode::SUCCESS
}
