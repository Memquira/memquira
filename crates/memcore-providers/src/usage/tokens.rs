use std::sync::{Arc, Mutex};

use memcore_core::ports::{FactExtractionInput, SummarizationInput};

use super::types::ProviderTokenUsage;

/// Optional slot for providers to publish token usage from the latest call.
pub type TokenUsageSlot = Arc<Mutex<Option<ProviderTokenUsage>>>;

pub fn new_token_usage_slot() -> TokenUsageSlot {
    Arc::new(Mutex::new(None))
}

pub fn take_token_usage(slot: &TokenUsageSlot) -> Option<ProviderTokenUsage> {
    slot.lock().ok().and_then(|mut guard| guard.take())
}

pub fn store_token_usage(slot: &TokenUsageSlot, usage: ProviderTokenUsage) {
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(usage);
    }
}

/// Rough token estimate from character length (chars / 4).
pub fn estimate_tokens_from_text(text: &str) -> u64 {
    let chars = text.chars().count();
    chars.div_ceil(4).max(1) as u64
}

pub fn estimate_llm_extraction_tokens(input: &FactExtractionInput) -> ProviderTokenUsage {
    let input_chars: usize = input
        .messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum();
    let input_tokens = input_chars.div_ceil(4).max(1) as u64;
    ProviderTokenUsage::from_counts(Some(input_tokens), Some(input_tokens / 4))
}

pub fn estimate_llm_classification_tokens(
    candidate_content: &str,
    existing_count: usize,
) -> ProviderTokenUsage {
    let input_tokens =
        estimate_tokens_from_text(candidate_content).saturating_add(existing_count as u64 * 20);
    ProviderTokenUsage::from_counts(Some(input_tokens), Some(32))
}

pub fn estimate_llm_summarization_tokens(input: &SummarizationInput) -> ProviderTokenUsage {
    let input_chars: usize = input
        .facts
        .iter()
        .map(|fact| fact.content.chars().count())
        .sum();
    let input_tokens = input_chars.div_ceil(4).max(1) as u64;
    let output_tokens = input.max_tokens.map(|t| t as u64).unwrap_or(128);
    ProviderTokenUsage::from_counts(Some(input_tokens), Some(output_tokens))
}

pub fn estimate_embedding_tokens(text: &str) -> ProviderTokenUsage {
    ProviderTokenUsage::from_counts(Some(estimate_tokens_from_text(text)), None)
}

pub fn estimate_embedding_batch_tokens(texts: &[String]) -> ProviderTokenUsage {
    let total: u64 = texts
        .iter()
        .map(|text| estimate_tokens_from_text(text))
        .sum();
    ProviderTokenUsage::from_counts(Some(total), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_from_text_is_deterministic() {
        assert_eq!(
            estimate_tokens_from_text("hello"),
            estimate_tokens_from_text("hello")
        );
        assert!(estimate_tokens_from_text("abcd") >= 1);
    }
}
