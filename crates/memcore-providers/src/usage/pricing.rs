/// Static per-model pricing hints for estimated cost (USD per 1M tokens).
///
/// Prices are approximate and must be reviewed manually — not authoritative billing data.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderPricing {
    pub provider_name: String,
    pub model_name: String,
    pub input_cost_per_1m_tokens_usd: f64,
    pub output_cost_per_1m_tokens_usd: f64,
}

/// Estimates provider cost from token counts and static pricing hints.
pub struct ProviderCostCalculator;

impl ProviderCostCalculator {
    pub fn estimate_cost_usd(
        pricing: &ProviderPricing,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) -> Option<f64> {
        let input = input_tokens?;
        let output = output_tokens?;
        let cost = (input as f64 / 1_000_000.0) * pricing.input_cost_per_1m_tokens_usd
            + (output as f64 / 1_000_000.0) * pricing.output_cost_per_1m_tokens_usd;
        Some(cost)
    }

    /// Embedding models typically bill input tokens only.
    pub fn estimate_embedding_cost_usd(
        pricing: &ProviderPricing,
        input_tokens: Option<u64>,
    ) -> Option<f64> {
        let input = input_tokens?;
        Some((input as f64 / 1_000_000.0) * pricing.input_cost_per_1m_tokens_usd)
    }
}

/// Built-in pricing hints for common OpenAI models (manually maintained).
pub fn lookup_pricing(provider_name: &str, model_name: &str) -> Option<ProviderPricing> {
    if provider_name != "openai" {
        return None;
    }

    let table: &[(&str, f64, f64)] = &[
        ("gpt-4.1-mini", 0.40, 1.60),
        ("gpt-4.1-nano", 0.10, 0.40),
        ("gpt-4.1", 2.00, 8.00),
        ("gpt-4o-mini", 0.15, 0.60),
        ("gpt-4o", 2.50, 10.00),
        ("text-embedding-3-small", 0.02, 0.0),
        ("text-embedding-3-large", 0.13, 0.0),
        ("text-embedding-ada-002", 0.10, 0.0),
    ];

    table
        .iter()
        .find(|(name, _, _)| *name == model_name)
        .map(|(name, input, output)| ProviderPricing {
            provider_name: provider_name.to_string(),
            model_name: (*name).to_string(),
            input_cost_per_1m_tokens_usd: *input,
            output_cost_per_1m_tokens_usd: *output,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pricing() -> ProviderPricing {
        ProviderPricing {
            provider_name: "openai".to_string(),
            model_name: "gpt-4.1-mini".to_string(),
            input_cost_per_1m_tokens_usd: 0.40,
            output_cost_per_1m_tokens_usd: 1.60,
        }
    }

    #[test]
    fn missing_token_usage_returns_none() {
        assert!(
            ProviderCostCalculator::estimate_cost_usd(&sample_pricing(), None, Some(100)).is_none()
        );
        assert!(
            ProviderCostCalculator::estimate_cost_usd(&sample_pricing(), Some(100), None).is_none()
        );
    }

    #[test]
    fn llm_cost_uses_input_and_output_tokens() {
        let cost = ProviderCostCalculator::estimate_cost_usd(
            &sample_pricing(),
            Some(1_000_000),
            Some(500_000),
        )
        .expect("cost");
        assert!((cost - (0.40 + 0.80)).abs() < f64::EPSILON);
    }

    #[test]
    fn embedding_cost_uses_input_tokens_only() {
        let pricing = ProviderPricing {
            provider_name: "openai".to_string(),
            model_name: "text-embedding-3-small".to_string(),
            input_cost_per_1m_tokens_usd: 0.02,
            output_cost_per_1m_tokens_usd: 0.0,
        };
        let cost = ProviderCostCalculator::estimate_embedding_cost_usd(&pricing, Some(2_000_000))
            .expect("cost");
        assert!((cost - 0.04).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_calculation_is_deterministic() {
        let a =
            ProviderCostCalculator::estimate_cost_usd(&sample_pricing(), Some(10_000), Some(2_000));
        let b =
            ProviderCostCalculator::estimate_cost_usd(&sample_pricing(), Some(10_000), Some(2_000));
        assert_eq!(a, b);
    }

    #[test]
    fn unknown_model_returns_none_from_lookup() {
        assert!(lookup_pricing("openai", "unknown-model-xyz").is_none());
        assert!(lookup_pricing("mock", "mock-llm").is_none());
    }
}
