//! Runtime quality tier.
//!
//! The tier comes from the configuration (`Settings::quality_tier`, the exact reference contract)
//! and, for the reference tier, from a free check that the endpoint is the reference gateway in its
//! reference configuration. `reference-validated` is HISTORICAL evidence (the measurement); the
//! separate `live_provider_verified` field is true only after an opt-in startup chat probe passed.

use std::time::Duration;

use memcore_config::{QualityTier, Settings};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QualityStatus {
    /// `reference-validated`, `compatible-unvalidated` or `development-mock`.
    pub tier: String,
    /// The tier the configuration alone would claim.
    pub configured_tier: String,
    /// Reference-contract settings this configuration changes (names only).
    pub reference_mismatches: Vec<String>,
    /// What the tier rests on.
    pub evidence: String,
    /// `confirmed`, `failed`, `not_checked` or `not_applicable`.
    pub reference_gateway: String,
    /// `passed`, `failed` or `skipped` (`MEMCORE_LLM_STARTUP_PROBE`, off by default).
    pub llm_startup_probe: String,
    /// True only when a live chat probe passed in this process.
    pub live_provider_verified: bool,
    /// `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED` (default false).
    pub classification_skip_when_no_related: bool,
    /// Validated variants of the frozen reference that this configuration enables, each with its own
    /// validation record. A tier is never inherited silently: an enabled variant is listed here with
    /// what was and was not shown equivalent. Empty when none is enabled.
    pub validated_variants: Vec<ValidatedVariant>,
}

/// The validation record of a variant of the frozen reference.
/// `output-equivalent` means the stored state, contexts and answer prompts were shown identical to
/// the reference on the frozen replay; it does not mean operationally equivalent (provider calls,
/// usage, call budgets, circuit breaking and failure propagation differ by design).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ValidatedVariant {
    /// Variant name.
    pub name: String,
    /// The setting that enables it.
    pub setting: String,
    /// The frozen reference it was validated against.
    pub frozen_reference: String,
    /// The implementation, with this build's version and source revision.
    pub implementation: String,
    /// `output-equivalent`: stored state, contexts and prompts identical to the reference.
    pub equivalence: String,
    /// Always false for this variant: it changes provider traffic by design.
    pub operationally_equivalent: bool,
    /// Answer-output parity and the scope of its evidence.
    pub output_parity: String,
    /// Stored-state parity and the scope of its evidence.
    pub stored_state_parity: String,
    /// Context parity and the scope of its evidence.
    pub context_parity: String,
    /// Prompt parity and the scope of its evidence.
    pub prompt_parity: String,
    /// The operational behaviour that differs from the reference, by design.
    pub intentional_operational_differences: Vec<String>,
    /// Where the validation is recorded.
    pub evidence: String,
}

const REFERENCE_EVIDENCE: &str = "historical: the maintainers' reference evaluation of this configuration (figures not \n     published), extended to its query-time beam (hnsw_ef 256) by a retrieval parity check; not a live verification";

/// The validation record of the classification-skip variant.
pub fn classification_skip_variant() -> ValidatedVariant {
    let scope = "verified by the maintainers on a re-ingest replay of the reference evaluation, SQLite \n                 and PostgreSQL";
    ValidatedVariant {
        name: "classification-skip".to_string(),
        setting: "MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED=true".to_string(),
        frozen_reference: "the reference configuration (frozen store dumps)"
            .to_string(),
        implementation: format!(
            "memcore {} source {}: legacy ingest adds a candidate without the \
             classification call when the related-fact lookup returns no fact",
            env!("CARGO_PKG_VERSION"),
            env!("MEMCORE_BUILD_GIT_SHA")
        ),
        equivalence: "output-equivalent".to_string(),
        operationally_equivalent: false,
        output_parity: format!(
            "identical answer prompts with their cached answers, so the reference outcomes apply; {scope}"
        ),
        stored_state_parity: format!(
            "identical facts, events, episodes and vector points (generated ids and wall-clock \
             times excluded); {scope}"
        ),
        context_parity: format!("all C0 and E-merge reference contexts identical; {scope}"),
        prompt_parity: format!("all reference answer prompts identical; {scope}"),
        intentional_operational_differences: vec![
            "classification provider calls: none for a candidate without related facts".to_string(),
            "usage reporting: a skipped call makes no provider-usage record and no provider metric; \
             it is counted by memcore_ingest_classification_skipped_total"
                .to_string(),
            "call-budget accounting: a skipped call is not charged to the provider call guard"
                .to_string(),
            "circuit-breaker behaviour: a skipped call is not sampled by the classification breaker"
                .to_string(),
            "classification-failure propagation: a classification error can no longer fail an \
             ingest whose candidate has no related facts"
                .to_string(),
        ],
        evidence: "the maintainers' validation records (not published)"
            .to_string(),
    }
}

impl QualityStatus {
    pub fn from_settings(settings: &Settings) -> Self {
        let tier = settings.quality_tier();
        Self {
            tier: tier.as_str().to_string(),
            configured_tier: tier.as_str().to_string(),
            reference_mismatches: settings
                .reference_contract_mismatches()
                .into_iter()
                .map(str::to_string)
                .collect(),
            evidence: evidence(tier).to_string(),
            reference_gateway: if tier == QualityTier::ReferenceValidated {
                "not_checked"
            } else {
                "not_applicable"
            }
            .to_string(),
            llm_startup_probe: "skipped".to_string(),
            live_provider_verified: false,
            classification_skip_when_no_related: settings.classification_skip_when_no_related,
            validated_variants: if settings.classification_skip_when_no_related {
                vec![classification_skip_variant()]
            } else {
                Vec::new()
            },
        }
    }

    /// `memcore_quality_tier_info{tier, reference_gateway, llm_startup_probe} 1` (bounded labels).
    pub fn record_metrics(&self) {
        metrics::gauge!(
            "memcore_quality_tier_info",
            "tier" => self.tier.clone(),
            "reference_gateway" => self.reference_gateway.clone(),
            "llm_startup_probe" => self.llm_startup_probe.clone(),
        )
        .set(1.0);
        for variant in &self.validated_variants {
            metrics::gauge!(
                "memcore_quality_variant_info",
                "variant" => variant.name.clone(),
                "equivalence" => variant.equivalence.clone(),
            )
            .set(1.0);
        }
    }
}

fn evidence(tier: QualityTier) -> &'static str {
    match tier {
        QualityTier::ReferenceValidated => REFERENCE_EVIDENCE,
        QualityTier::CompatibleUnvalidated => {
            "none: works with this configuration, quality not measured"
        }
        QualityTier::DevelopmentMock => "none: mock providers (tests and development only)",
    }
}

/// Startup quality checks (`run`): the reference gateway check for a reference configuration, the
/// optional chat probe, then `MEMCORE_REQUIRE_QUALITY_TIER` on the resulting tier. An error refuses
/// startup.
pub async fn startup_quality(settings: &Settings) -> Result<QualityStatus, String> {
    let mut status = QualityStatus::from_settings(settings);
    if settings.quality_tier() == QualityTier::ReferenceValidated {
        let endpoint = settings.llm_endpoint();
        match memcore_providers::reference_gateway::check_reference_gateway(
            &endpoint.base_url,
            Duration::from_secs(5),
        )
        .await
        {
            Ok(()) => status.reference_gateway = "confirmed".to_string(),
            Err(reason) => {
                tracing::warn!(reason = %reason, "reference gateway not confirmed: quality tier is compatible-unvalidated");
                status.reference_gateway = "failed".to_string();
                status.tier = QualityTier::CompatibleUnvalidated.as_str().to_string();
                status.evidence = evidence(QualityTier::CompatibleUnvalidated).to_string();
            }
        }
    }
    if settings.providers.llm_startup_probe {
        match llm_startup_probe(settings).await {
            Ok(()) => {
                status.llm_startup_probe = "passed".to_string();
                status.live_provider_verified = true;
            }
            Err(reason) => {
                status.llm_startup_probe = "failed".to_string();
                return Err(format!("MEMCORE_LLM_STARTUP_PROBE failed: {reason}"));
            }
        }
    }
    if let Some(required) = settings.providers.require_quality_tier
        && required.as_str() != status.tier
    {
        return Err(format!(
            "MEMCORE_REQUIRE_QUALITY_TIER={} but the runtime tier is {} (reference gateway: {}; contract differences: {})",
            required.as_str(),
            status.tier,
            status.reference_gateway,
            if status.reference_mismatches.is_empty() {
                "none".to_string()
            } else {
                status.reference_mismatches.join(", ")
            }
        ));
    }
    Ok(status)
}

/// One classification call through the configured provider stack (guardrails and usage
/// accounting included); passes when the reply parses as the expected JSON.
async fn llm_startup_probe(settings: &Settings) -> Result<(), String> {
    use memcore_core::{CandidateFact, FactClassificationInput, MemoryType, TenantContext};
    let provider =
        crate::state::create_llm_provider_for_tool(settings).map_err(|e| e.to_string())?;
    let input = FactClassificationInput {
        tenant: TenantContext::new("memcore_startup_probe", "probe").map_err(|e| e.to_string())?,
        candidate_fact: CandidateFact::new(
            "The user likes green tea.",
            MemoryType::Preference,
            0.9,
            0.5,
            None,
            serde_json::json!({}),
        )
        .map_err(|e| e.to_string())?,
        existing_facts: vec![],
    };
    provider
        .classify_fact_operation(input)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_configuration_lists_no_variant() {
        let status = QualityStatus::from_settings(&Settings::default());
        assert!(!status.classification_skip_when_no_related);
        assert!(status.validated_variants.is_empty());
    }

    #[test]
    fn classification_skip_is_qualified_never_silently_inherited() {
        let mut settings = Settings::default();
        let before = QualityStatus::from_settings(&settings);
        settings.classification_skip_when_no_related = true;
        let status = QualityStatus::from_settings(&settings);

        // The tier and the reference-contract comparison are those of the same configuration
        // without S1; S1 is reported beside them as a qualified variant.
        assert_eq!(status.tier, before.tier);
        assert_eq!(status.reference_mismatches, before.reference_mismatches);
        assert!(status.classification_skip_when_no_related);
        assert_eq!(status.validated_variants.len(), 1);
        let v = &status.validated_variants[0];
        assert_eq!(v.name, "classification-skip");
        assert_eq!(
            v.setting,
            "MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED=true"
        );
        assert!(
            v.frozen_reference
                .starts_with("the reference configuration")
        );
        assert_eq!(v.equivalence, "output-equivalent");
        assert!(!v.operationally_equivalent);
        for parity in [
            &v.output_parity,
            &v.stored_state_parity,
            &v.context_parity,
            &v.prompt_parity,
        ] {
            assert!(parity.contains("reference evaluation"), "{parity}");
        }
        let differences = v.intentional_operational_differences.join(" | ");
        for expected in [
            "classification provider calls",
            "usage reporting",
            "call-budget accounting",
            "circuit-breaker behaviour",
            "classification-failure propagation",
        ] {
            assert!(differences.contains(expected), "{expected}");
        }
        assert!(v.implementation.contains(env!("CARGO_PKG_VERSION")));
    }
}
