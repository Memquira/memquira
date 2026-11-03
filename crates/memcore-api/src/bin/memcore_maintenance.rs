//! `memcore-maintenance`: operator-only maintenance.
//!
//! ```text
//! memcore-maintenance forget-user --org-id <org> --user-id <user> --confirm forget-user:<org>:<user> [--dry-run]
//! memcore-maintenance verify-vectors --org-id <org> --user-id <user> [--repair --confirm verify-vectors:<org>:<user>]
//! memcore-maintenance rollout-preflight --assessment first-c-plus-rollout|later-c-plus-upgrade
//! ```
//!
//! Reads the same environment as the API (database, Postgres, Qdrant settings).
//!
//! `forget-user` erases one user's facts, episodes, entities and their fact / episode vector
//! points WITHOUT starting the API and without any LLM or embedding provider, so it also works
//! when the API refuses to start because a collection's embedding identity no longer matches.
//! See `memcore_api::maintenance`. Exit codes: 0 complete (or dry run), 2 refused (bad scope,
//! confirmation or unsafe target), 3 incomplete (data may remain; re-run).
//!
//! `verify-vectors` compares one user's SQL rows with their fact / episode vector points
//! (`memcore_core::verify`). By default it is a DRY RUN: it reads only, embeds nothing and writes
//! nothing. `--repair` with the exact confirmation repairs only that user's points. Exit codes:
//! 0 clean / repair completed / attention required (only items not attributable to the user
//! remain), 1 repair needed, 2 refused, 3 repair incomplete, 4 blocked (incomplete scan).
//!
//! `rollout-preflight` inventories the configured SQL store read-only
//! (fact-id UUID versions, schema, a state fingerprint; works on schemas older than this build) and
//! reports this build's verdict on the environment. Exit codes: 0 PASS, 2 refused (arguments),
//! 4 BLOCKED. It never migrates, writes, opens Qdrant or builds a provider.
//!
//! All print one JSON report (ids, counts and hashes only; no content or credentials).

use memcore_api::maintenance;

fn usage() -> ! {
    eprintln!(
        "usage: memcore-maintenance forget-user --org-id <org> --user-id <user> \
         --confirm forget-user:<org>:<user> [--dry-run]\n       \
         memcore-maintenance verify-vectors --org-id <org> --user-id <user> \
         [--repair --confirm verify-vectors:<org>:<user>]\n       \
         memcore-maintenance rollout-preflight --assessment first-c-plus-rollout|later-c-plus-upgrade"
    );
    std::process::exit(2);
}

fn refuse(operation: &str, message: &str) -> ! {
    println!(
        "{}",
        serde_json::json!({ "operation": operation, "outcome": "refused", "error": message })
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("forget-user") => forget_user(args).await,
        Some("verify-vectors") => verify_vectors(args).await,
        Some("rollout-preflight") => rollout_preflight(args).await,
        _ => usage(),
    }
}

async fn forget_user(mut args: impl Iterator<Item = String>) {
    let (mut org, mut user, mut confirm, mut dry_run) = (None, None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--org-id" => org = args.next(),
            "--user-id" => user = args.next(),
            "--confirm" => confirm = args.next(),
            "--dry-run" => dry_run = true,
            _ => usage(),
        }
    }
    let (Some(org), Some(user), Some(confirm)) = (org, user, confirm) else {
        usage();
    };
    let tenant = match maintenance::validated_scope(&org, &user, &confirm) {
        Ok(tenant) => tenant,
        Err(error) => refuse("forget-user", &error.to_string()),
    };
    let settings = match memcore_config::load_settings() {
        Ok(settings) => settings,
        Err(error) => refuse("forget-user", &format!("configuration: {error}")),
    };
    let stores = match maintenance::open_stores(&settings).await {
        Ok(stores) => stores,
        Err(error) => refuse("forget-user", &error.to_string()),
    };
    let report = maintenance::forget_user(&stores, &tenant, dry_run).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    std::process::exit(if report.complete || report.dry_run {
        0
    } else {
        3
    });
}

async fn verify_vectors(mut args: impl Iterator<Item = String>) {
    let (mut org, mut user, mut confirm, mut repair) = (None, None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--org-id" => org = args.next(),
            "--user-id" => user = args.next(),
            "--confirm" => confirm = args.next(),
            "--repair" => repair = true,
            _ => usage(),
        }
    }
    let (Some(org), Some(user)) = (org, user) else {
        usage();
    };
    let (tenant, options) =
        match maintenance::validated_verify_scope(&org, &user, repair, confirm.as_deref()) {
            Ok(scope) => scope,
            Err(error) => refuse("verify-vectors", &error.to_string()),
        };
    let settings = match memcore_config::load_settings() {
        Ok(settings) => settings,
        Err(error) => refuse("verify-vectors", &format!("configuration: {error}")),
    };
    run_verify(&settings, &tenant, &options, repair).await;
}

async fn rollout_preflight(mut args: impl Iterator<Item = String>) {
    use memcore_storage::rollout_inventory::Assessment;
    let mut assessment = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--assessment" => {
                assessment = match args.next().as_deref() {
                    Some("first-c-plus-rollout") => Some(Assessment::FirstCPlusRollout),
                    Some("later-c-plus-upgrade") => Some(Assessment::LaterCPlusUpgrade),
                    _ => refuse(
                        "rollout-preflight",
                        "--assessment must be first-c-plus-rollout or later-c-plus-upgrade",
                    ),
                }
            }
            _ => usage(),
        }
    }
    let Some(assessment) = assessment else {
        refuse("rollout-preflight", "--assessment is required");
    };
    let (report, code) = maintenance::rollout_preflight(assessment).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    std::process::exit(code);
}

#[cfg(feature = "qdrant")]
async fn run_verify(
    settings: &memcore_config::Settings,
    tenant: &memcore_core::TenantContext,
    options: &memcore_core::verify::VerifyOptions,
    repair: bool,
) {
    let stores = match maintenance::open_verify_stores(settings, repair).await {
        Ok(stores) => stores,
        Err(error) => refuse("verify-vectors", &error.to_string()),
    };
    let report = maintenance::verify_vectors(&stores, tenant, options).await;
    let mut value = serde_json::to_value(&report).unwrap_or_default();
    value["stores"] = stores.description.clone();
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
    std::process::exit(report.exit_code);
}

#[cfg(not(feature = "qdrant"))]
async fn run_verify(
    _settings: &memcore_config::Settings,
    _tenant: &memcore_core::TenantContext,
    _options: &memcore_core::verify::VerifyOptions,
    _repair: bool,
) {
    refuse(
        "verify-vectors",
        "maintenance refused: verify-vectors needs a build with the `qdrant` feature",
    );
}
