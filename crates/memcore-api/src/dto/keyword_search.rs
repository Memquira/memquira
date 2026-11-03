use memcore_common::MemcoreError;

/// Maximum length for keyword search query parameter `q`.
pub const MAX_KEYWORD_QUERY_LENGTH: usize = 200;

/// Trims `q`, treats empty as no search, validates max length.
pub fn parse_keyword_query(q: Option<String>) -> Result<Option<String>, MemcoreError> {
    match q {
        None => Ok(None),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > MAX_KEYWORD_QUERY_LENGTH {
                return Err(MemcoreError::ValidationError(
                    "q must be 200 characters or less".to_string(),
                ));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_q_treated_as_none() {
        assert_eq!(parse_keyword_query(None).expect("parse"), None);
        assert_eq!(
            parse_keyword_query(Some(String::new())).expect("parse"),
            None
        );
        assert_eq!(
            parse_keyword_query(Some("   ".to_string())).expect("parse"),
            None
        );
    }

    #[test]
    fn trims_q() {
        assert_eq!(
            parse_keyword_query(Some("  rust  ".to_string())).expect("parse"),
            Some("rust".to_string())
        );
    }

    #[test]
    fn long_q_returns_validation_error() {
        let long = "a".repeat(201);
        let error = parse_keyword_query(Some(long)).expect_err("too long");
        assert_eq!(
            error,
            MemcoreError::ValidationError("q must be 200 characters or less".to_string())
        );
    }
}
