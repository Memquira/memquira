use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};

/// Default total token budget for assembled context.
pub const DEFAULT_CONTEXT_MAX_TOKENS: usize = 2000;

/// Default tokens reserved for the assistant reply and system overhead.
pub const DEFAULT_CONTEXT_RESERVED_TOKENS: usize = 300;

/// Hard upper bound for `max_tokens` on context requests.
pub const MAX_CONTEXT_MAX_TOKENS: usize = 16000;

/// Token budget for context assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_tokens: usize,
    pub reserved_tokens: usize,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_CONTEXT_MAX_TOKENS,
            reserved_tokens: DEFAULT_CONTEXT_RESERVED_TOKENS,
        }
    }
}

impl ContextBudget {
    /// Tokens available for memory context after reserving reply/overhead space.
    pub fn available_tokens(&self) -> usize {
        self.max_tokens.saturating_sub(self.reserved_tokens)
    }

    pub fn validate(&self) -> MemcoreResult<()> {
        if self.reserved_tokens >= self.max_tokens {
            return Err(MemcoreError::ValidationError(
                "reserved_tokens must be less than max_tokens".to_string(),
            ));
        }

        if self.max_tokens > MAX_CONTEXT_MAX_TOKENS {
            return Err(MemcoreError::ValidationError(format!(
                "max_tokens cannot exceed {MAX_CONTEXT_MAX_TOKENS}"
            )));
        }

        Ok(())
    }
}

/// Budget consumption metadata returned with assembled context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudgetUsage {
    pub max_tokens: usize,
    pub reserved_tokens: usize,
    pub available_tokens: usize,
    pub used_tokens: usize,
    pub included_memories: usize,
    pub skipped_memories: usize,
}

impl ContextBudgetUsage {
    pub fn from_budget(budget: &ContextBudget) -> Self {
        Self {
            max_tokens: budget.max_tokens,
            reserved_tokens: budget.reserved_tokens,
            available_tokens: budget.available_tokens(),
            used_tokens: 0,
            included_memories: 0,
            skipped_memories: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_values() {
        let budget = ContextBudget::default();
        assert_eq!(budget.max_tokens, 2000);
        assert_eq!(budget.reserved_tokens, 300);
        assert_eq!(budget.available_tokens(), 1700);
    }

    #[test]
    fn reserved_tokens_reduce_available_tokens() {
        let budget = ContextBudget {
            max_tokens: 1000,
            reserved_tokens: 250,
        };
        assert_eq!(budget.available_tokens(), 750);
    }

    #[test]
    fn invalid_reserved_gte_max_is_rejected() {
        let budget = ContextBudget {
            max_tokens: 500,
            reserved_tokens: 500,
        };
        assert!(budget.validate().is_err());
    }

    #[test]
    fn max_tokens_above_safe_max_is_rejected() {
        let budget = ContextBudget {
            max_tokens: MAX_CONTEXT_MAX_TOKENS + 1,
            reserved_tokens: 100,
        };
        assert!(budget.validate().is_err());
    }
}
