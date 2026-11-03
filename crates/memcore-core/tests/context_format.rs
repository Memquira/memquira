use memcore_core::{
    ContextBudget, ContextCompressionOptions, ContextFormat, ContextFormatOptions,
    MemorySearchResult, MemoryType, SimpleTokenEstimator, assemble_context_with_budget,
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
fn markdown_sections_group_by_memory_type() {
    let memories = vec![
        sample_result("User is building memcore.", MemoryType::Project),
        sample_result("User is a developer.", MemoryType::Profile),
    ];
    let options = ContextFormatOptions::structured_markdown();
    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &options,
        &ContextBudget {
            max_tokens: 2000,
            reserved_tokens: 0,
        },
        &ContextCompressionOptions::default(),
        &SimpleTokenEstimator,
    );

    assert!(assembled.context.contains("## Profile"));
    assert!(assembled.context.contains("## Projects"));
    assert_eq!(assembled.memories.len(), 2);
}

#[test]
fn flat_format_preserves_ranked_order_without_sections() {
    let memories = vec![
        sample_result("first ranked", MemoryType::Project),
        sample_result("second ranked", MemoryType::Profile),
    ];
    let options = ContextFormatOptions {
        format: ContextFormat::Markdown,
        section_by_memory_type: false,
        ..ContextFormatOptions::default()
    };
    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &options,
        &ContextBudget {
            max_tokens: 2000,
            reserved_tokens: 0,
        },
        &ContextCompressionOptions::default(),
        &SimpleTokenEstimator,
    );

    let first = assembled.context.find("first ranked").unwrap();
    let second = assembled.context.find("second ranked").unwrap();
    assert!(first < second);
    assert!(!assembled.context.contains("## "));
}

#[test]
fn json_format_returns_string_context_payload() {
    let memories = vec![sample_result(
        "User is building memcore.",
        MemoryType::Project,
    )];
    let options = ContextFormatOptions {
        format: ContextFormat::Json,
        ..ContextFormatOptions::default()
    };
    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &options,
        &ContextBudget {
            max_tokens: 2000,
            reserved_tokens: 0,
        },
        &ContextCompressionOptions::default(),
        &SimpleTokenEstimator,
    );

    let value: serde_json::Value = serde_json::from_str(&assembled.context).expect("json context");
    assert_eq!(value["memories"][0]["content"], "User is building memcore.");
}

#[test]
fn metadata_flags_affect_formatted_output() {
    let memories = vec![sample_result(
        "User is building memcore.",
        MemoryType::Project,
    )];
    let options = ContextFormatOptions {
        format: ContextFormat::Markdown,
        include_scores: true,
        include_memory_types: true,
        ..ContextFormatOptions::default()
    };
    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &options,
        &ContextBudget {
            max_tokens: 2000,
            reserved_tokens: 0,
        },
        &ContextCompressionOptions::default(),
        &SimpleTokenEstimator,
    );

    assert!(assembled.context.contains("Project"));
    assert!(assembled.context.contains("0.82"));
}

#[test]
fn oversized_formatted_memory_is_skipped_with_sections() {
    let memories = vec![
        sample_result(&"x".repeat(5000), MemoryType::Profile),
        sample_result("tiny profile note", MemoryType::Profile),
    ];
    let options = ContextFormatOptions::structured_markdown();
    let assembled = assemble_context_with_budget(
        &memories,
        false,
        &options,
        &ContextBudget {
            max_tokens: 80,
            reserved_tokens: 10,
        },
        &ContextCompressionOptions::default(),
        &SimpleTokenEstimator,
    );

    assert_eq!(assembled.budget.included_memories, 1);
    assert_eq!(assembled.budget.skipped_memories, 1);
    assert!(assembled.context.contains("tiny profile note"));
    assert!(assembled.budget.used_tokens <= assembled.budget.available_tokens);
}
