//! The shipped deployment profiles (`profiles/*.env.example`) and the reference-tier fixture
//! (`tests/fixtures/reference.env`) load through the real settings validation and claim the right
//! quality tier. One test (it sets process environment variables).

use std::path::PathBuf;

use memcore_config::{
    EventBackend, FactBackend, LlmJsonMode, QualityTier, Settings, VectorBackend, load_settings,
};

const SETTINGS_SOURCE: &str = include_str!("../src/settings.rs");

fn profile(name: &str) -> Vec<(String, String)> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // The reference configuration is a test fixture: its provider gateway is not distributed.
    let path = if name == "reference" {
        base.join("tests/fixtures/reference.env")
    } else {
        base.join(format!("../../profiles/{name}.env.example"))
    };
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            let (k, v) = l
                .split_once('=')
                .unwrap_or_else(|| panic!("not KEY=value: {l}"));
            // Docker Compose env-file rule: surrounding double quotes are stripped.
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            (k.trim().to_string(), v.to_string())
        })
        .collect()
}

fn load(vars: &[(String, String)]) -> memcore_common::MemcoreResult<Settings> {
    for (key, _) in std::env::vars() {
        if key.starts_with("MEMCORE_") || key.starts_with("OPENAI_") || key.starts_with("AWS_") {
            unsafe { std::env::remove_var(&key) };
        }
    }
    for (k, v) in vars {
        unsafe { std::env::set_var(k, v) };
    }
    load_settings()
}

fn with(vars: &[(String, String)], key: &str, value: &str) -> Vec<(String, String)> {
    let mut out: Vec<_> = vars.iter().filter(|(k, _)| k != key).cloned().collect();
    out.push((key.to_string(), value.to_string()));
    out
}

#[test]
fn the_three_profiles_load_and_claim_the_right_tier() {
    for name in ["reference", "byom", "local-starter"] {
        let vars = profile(name);
        for (key, _) in &vars {
            assert!(
                SETTINGS_SOURCE.contains(&format!("\"{key}\"")),
                "{name}: {key} is not a setting memcore reads (typo?)"
            );
            if name != "reference" {
                assert!(!key.starts_with("AWS_"), "{name} must not need AWS: {key}");
            }
        }
        assert!(
            vars.iter()
                .any(|(k, _)| k == "MEMCORE_REQUIRE_QUALITY_TIER"),
            "{name} must pin its tier"
        );
    }

    // Reference: the exact contract, required at startup.
    let reference = profile("reference");
    let s = load(&reference).expect("reference profile loads");
    assert_eq!(s.reference_contract_mismatches(), Vec::<&str>::new());
    assert_eq!(s.quality_tier(), QualityTier::ReferenceValidated);
    assert_eq!(
        s.check_required_quality_tier().unwrap(),
        QualityTier::ReferenceValidated
    );
    assert_eq!(
        (s.fact_backend, s.event_backend),
        (FactBackend::Postgres, EventBackend::Postgres)
    );
    assert_eq!(s.vector_backend, VectorBackend::Qdrant);
    assert_eq!(s.providers.llm_json_mode, LlmJsonMode::JsonObject);
    assert_eq!(s.providers.embedding_options.dimensions, None);
    assert_eq!(s.query_embedding_cache_capacity, 10_000);
    // The reference profile searches with the accepted beam.
    assert_eq!(
        s.qdrant_search_hnsw_ef,
        Some(memcore_config::REFERENCE_QDRANT_SEARCH_HNSW_EF)
    );
    assert_eq!(s.qdrant_search_hnsw_ef, Some(256));
    // The experimental bounded exact search is not part of the launch reference.
    assert_eq!(s.qdrant_exact_search_max_tenant_points, None);
    // The reference profile pins the measured prefix packing (also the server default since 2026-10-28).
    assert_eq!(
        s.context_default_packing,
        memcore_config::ContextPacking::Prefix
    );
    assert!(s.metrics_enabled && s.rate_limit_enabled && s.admission.enabled);
    assert!(
        !s.providers.llm_startup_probe,
        "no paid call per start by default"
    );
    // Changing a reference parameter is refused by the pinned tier.
    for (key, value) in [
        ("MEMCORE_LLM_MODEL", "other-model"),
        ("MEMCORE_LLM_JSON_MODE", "json_schema"),
        ("MEMCORE_EMBEDDING_DIMENSIONS", "1024"),
        ("MEMCORE_SEARCH_MAX_LIMIT", "50"),
        ("MEMCORE_CONSOLIDATION_MODE", "llm_v2"),
        ("MEMCORE_EPISODES_ENABLED", "false"),
        // Any beam other than the accepted 256: the unapproved 128 and others.
        ("MEMCORE_QDRANT_SEARCH_HNSW_EF", "128"),
        ("MEMCORE_QDRANT_SEARCH_HNSW_EF", "512"),
        ("MEMCORE_QDRANT_SEARCH_HNSW_EF", "64"),
        // The experimental bounded exact-search option (never reference-validated).
        ("MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS", "12000"),
        // Opting the server default into fill / evidence leaves the measured reference.
        ("MEMCORE_CONTEXT_DEFAULT_PACKING", "evidence"),
        ("MEMCORE_CONTEXT_DEFAULT_PACKING", "fill"),
    ] {
        let s = load(&with(&reference, key, value)).expect("still a valid configuration");
        assert_eq!(
            s.quality_tier(),
            QualityTier::CompatibleUnvalidated,
            "{key}"
        );
        assert!(
            s.check_required_quality_tier().is_err(),
            "{key} must be refused"
        );
    }

    // Rollback: the reference profile with the beam removed (unset, the earlier request) is a
    // valid configuration but compatible-unvalidated; the pinned reference tier refuses it, naming the
    // setting, and it starts once the deployment requires the tier it actually has.
    let rollback: Vec<_> = reference
        .iter()
        .filter(|(k, _)| k != "MEMCORE_QDRANT_SEARCH_HNSW_EF")
        .cloned()
        .collect();
    assert_eq!(
        rollback.len() + 1,
        reference.len(),
        "the profile sets the beam once"
    );
    let s = load(&rollback).expect("rollback is a valid configuration");
    assert_eq!(s.qdrant_search_hnsw_ef, None);
    assert_eq!(
        s.reference_contract_mismatches(),
        vec!["qdrant_search_hnsw_ef"]
    );
    assert_eq!(s.quality_tier(), QualityTier::CompatibleUnvalidated);
    let error = s.check_required_quality_tier().unwrap_err().to_string();
    assert!(error.contains("qdrant_search_hnsw_ef"), "{error}");
    let s = load(&with(
        &rollback,
        "MEMCORE_REQUIRE_QUALITY_TIER",
        "compatible-unvalidated",
    ))
    .expect("rollback with its own tier loads");
    assert_eq!(
        s.check_required_quality_tier().unwrap(),
        QualityTier::CompatibleUnvalidated
    );
    // Invalid beams are refused at load, whatever the profile.
    for bad in ["0", "4097", "abc"] {
        assert!(
            load(&with(&reference, "MEMCORE_QDRANT_SEARCH_HNSW_EF", bad)).is_err(),
            "{bad}"
        );
    }

    // BYOM: independent endpoints and keys, compatible-unvalidated, exact-only dedup.
    let byom = profile("byom");
    let s = load(&byom).expect("byom profile loads");
    assert_eq!(
        s.check_required_quality_tier().unwrap(),
        QualityTier::CompatibleUnvalidated
    );
    let (llm, emb) = (s.llm_endpoint(), s.embedding_endpoint());
    assert_ne!(llm.base_url, emb.base_url);
    assert_ne!(llm.api_key, emb.api_key);
    assert_eq!(s.providers.embedding_options.dedup_threshold, None);
    assert!(s.providers.llm_startup_probe);

    // Local starter: its shipped settings, no keys, compatible-unvalidated.
    let local = profile("local-starter");
    let s = load(&local).expect("local-starter profile loads");
    assert_eq!(
        s.check_required_quality_tier().unwrap(),
        QualityTier::CompatibleUnvalidated
    );
    assert_eq!(s.providers.llm_json_mode, LlmJsonMode::JsonSchema);
    assert!(!s.structured_extraction_enabled);
    assert_eq!(s.llm_endpoint().api_key, None);
    assert_eq!(s.embedding_endpoint().api_key, None);
    assert_eq!(s.providers.embedding_options.dimensions, Some(384));
    assert_eq!(s.providers.embedding_options.dedup_threshold, None);
    assert_eq!(
        s.providers.embedding_options.query_prefix.as_deref(),
        Some("Represent this sentence for searching relevant passages: ")
    );
}
