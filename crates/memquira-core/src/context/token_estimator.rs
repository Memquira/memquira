/// Lightweight approximate token counting for context budget enforcement.
///
/// This is **not** a provider-specific tokenizer. Estimates are deterministic and
/// suitable for budget limits, not for billing or exact model window calculations.
pub trait TokenEstimator: Send + Sync {
    fn estimate_tokens(&self, text: &str) -> usize;
}

/// Approximates tokens as `ceil(chars / 4)` with a minimum of 1 for non-empty text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimpleTokenEstimator;

impl TokenEstimator for SimpleTokenEstimator {
    fn estimate_tokens(&self, text: &str) -> usize {
        let char_count = text.chars().count();
        if char_count == 0 {
            0
        } else {
            char_count.div_ceil(4).max(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_returns_zero() {
        assert_eq!(SimpleTokenEstimator.estimate_tokens(""), 0);
    }

    #[test]
    fn short_text_returns_at_least_one() {
        assert_eq!(SimpleTokenEstimator.estimate_tokens("a"), 1);
        assert_eq!(SimpleTokenEstimator.estimate_tokens("hi"), 1);
    }

    #[test]
    fn longer_text_returns_higher_estimate() {
        let short = SimpleTokenEstimator.estimate_tokens("hello");
        let long = SimpleTokenEstimator.estimate_tokens("hello world this is a longer string");
        assert!(long > short);
    }

    #[test]
    fn estimator_is_deterministic() {
        let text = "deterministic token estimate sample";
        let first = SimpleTokenEstimator.estimate_tokens(text);
        let second = SimpleTokenEstimator.estimate_tokens(text);
        assert_eq!(first, second);
    }
}
