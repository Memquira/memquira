use memcore_common::{MemcoreError, MemcoreResult};

/// Parses a comma-separated provider fallback order into normalized names.
pub fn parse_provider_fallback_order(order: &str) -> Vec<String> {
    order
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

pub fn validate_llm_provider_name(name: &str) -> MemcoreResult<()> {
    match name.trim().to_ascii_lowercase().as_str() {
        "mock" | "openai" => Ok(()),
        _ => Err(MemcoreError::ValidationError(format!(
            "unknown LLM provider in fallback order: {name}"
        ))),
    }
}

pub fn validate_embedding_provider_name(name: &str) -> MemcoreResult<()> {
    match name.trim().to_ascii_lowercase().as_str() {
        "mock" | "openai" => Ok(()),
        _ => Err(MemcoreError::ValidationError(format!(
            "unknown embedding provider in fallback order: {name}"
        ))),
    }
}

pub fn validate_summarizer_provider_name(name: &str) -> MemcoreResult<()> {
    validate_llm_provider_name(name)
}

pub fn validate_provider_fallback_order(
    names: &[String],
    validate_name: fn(&str) -> MemcoreResult<()>,
) -> MemcoreResult<()> {
    if names.is_empty() {
        return Err(MemcoreError::ValidationError(
            "provider fallback order cannot be empty when fallback is enabled".to_string(),
        ));
    }
    for name in names {
        validate_name(name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_order() {
        assert_eq!(
            parse_provider_fallback_order("mock, openai"),
            vec!["mock".to_string(), "openai".to_string()]
        );
    }

    #[test]
    fn unknown_llm_name_fails_validation() {
        let error = validate_llm_provider_name("anthropic").expect_err("unknown");
        assert!(matches!(error, MemcoreError::ValidationError(_)));
    }
}
