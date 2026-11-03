use memcore_core::{
    ContextBudget, ContextCompressionMode, ContextCompressionOptions, ContextFormat,
    ContextFormatOptions, MemorySearchResult, MemoryType, SimpleTokenEstimator,
    assemble_context_with_budget,
};
use serde_json::json;
use uuid::Uuid;

fn sample_result(content: &str, memory_type: MemoryType) -> MemorySearchResult {
    MemorySearchResult {
        fact_id: Uuid::new_v4(),
        content: content.to_string(),
        memory_type,
        score: 0.82,
        vector_score: None,
        confidence: 0.9,
        importance: 0.85,
        valid_at: None,
        metadata: json!({}),
        structure: Default::default(),
    }
}

#[test]
fn compression_summary_uses_skipped_memories_only() {
    let memories = vec![
        sample_result("included alpha", MemoryType::Skill),
        sample_result(
            "skipped memory beta one two three four five six seven",
            MemoryType::Skill,
        ),
        sample_result(
            "skipped memory gamma one two three four five six seven",
            MemoryType::Skill,
        ),
    ];
    let budget = ContextBudget {
        max_tokens: 70,
        reserved_tokens: 10,
    };
    let compression = ContextCompressionOptions {
        mode: ContextCompressionMode::SimpleExtractive,
        summary_max_tokens: 35,
        include_summary_section: true,
    };

    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &ContextFormatOptions::default(),
        &budget,
        &compression,
        &SimpleTokenEstimator,
    );

    assert!(assembled.context.contains("included alpha"));
    assert!(assembled.compression.enabled);
    assert!(assembled.compression.summarized_memories > 0);
    assert_eq!(assembled.budget.included_memories, 1);
    assert_eq!(assembled.budget.skipped_memories, 2);
}

#[test]
fn compression_plain_text_markdown_and_json_modes() {
    let memories: Vec<_> = (0..6)
        .map(|index| {
            sample_result(
                &format!("compression format memory {index}"),
                MemoryType::Skill,
            )
        })
        .collect();
    let budget = ContextBudget {
        max_tokens: 70,
        reserved_tokens: 10,
    };
    let compression = ContextCompressionOptions {
        mode: ContextCompressionMode::SimpleExtractive,
        summary_max_tokens: 60,
        include_summary_section: true,
    };

    for format in [
        ContextFormat::PlainText,
        ContextFormat::Markdown,
        ContextFormat::Json,
    ] {
        let assembled = assemble_context_with_budget(
            &memories,
            false,
            &ContextFormatOptions {
                format,
                ..ContextFormatOptions::default()
            },
            &budget,
            &compression,
            &SimpleTokenEstimator,
        );

        assert!(assembled.compression.enabled);
        match format {
            ContextFormat::PlainText => {
                assert!(assembled.context.contains("Compressed Memory Summary:"));
            }
            ContextFormat::Markdown => {
                assert!(assembled.context.contains("## Compressed Memory Summary"));
            }
            ContextFormat::Json => {
                let value: serde_json::Value =
                    serde_json::from_str(&assembled.context).expect("json context");
                assert!(
                    value.get("compressed_summary").is_some() || value.get("summary").is_some(),
                    "json context should include a summary field"
                );
            }
        }
    }
}

#[test]
fn compression_does_not_expose_sensitive_metadata() {
    let mut skipped = sample_result("safe user preference", MemoryType::Profile);
    skipped.metadata = json!({
        "input_text": "secret audit text",
        "api_key": "mc_live_secret"
    });
    let memories = vec![sample_result("tiny included", MemoryType::Profile), skipped];
    let budget = ContextBudget {
        max_tokens: 50,
        reserved_tokens: 5,
    };
    let compression = ContextCompressionOptions {
        mode: ContextCompressionMode::SimpleExtractive,
        summary_max_tokens: 80,
        include_summary_section: true,
    };

    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &ContextFormatOptions::default(),
        &budget,
        &compression,
        &SimpleTokenEstimator,
    );

    assert!(!assembled.context.contains("mc_live_secret"));
    assert!(!assembled.context.contains("secret audit text"));
    assert!(assembled.context.contains("safe user preference"));
}

#[test]
fn skipped_memories_count_unchanged_with_compression() {
    let memories: Vec<_> = (0..5)
        .map(|index| {
            sample_result(
                &format!("ranked memory item {index} extra words"),
                MemoryType::Skill,
            )
        })
        .collect();
    let budget = ContextBudget {
        max_tokens: 80,
        reserved_tokens: 10,
    };
    let compression = ContextCompressionOptions {
        mode: ContextCompressionMode::SimpleExtractive,
        summary_max_tokens: 40,
        include_summary_section: true,
    };

    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &ContextFormatOptions::default(),
        &budget,
        &compression,
        &SimpleTokenEstimator,
    );

    assert_eq!(
        assembled.budget.included_memories + assembled.budget.skipped_memories,
        memories.len()
    );
    assert!(assembled.budget.used_tokens <= assembled.budget.available_tokens);
}
