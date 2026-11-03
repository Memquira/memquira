//! Classification skip: provider-usage accounting through the real provider router. A
//! classification call skipped because no related fact exists makes no provider request, so it
//! leaves no usage record; extraction and embedding usage are unchanged.

use std::sync::Arc;

use memcore_api::{ProviderWiring, create_mock_memory_engine_with_wiring};
use memcore_config::Settings;
use memcore_core::{AddMemoryInput, MemoryMessage, MessageRole, TenantContext};
use memcore_providers::{InMemoryProviderUsageRecorder, ProviderUsageRecorder};
use serde_json::json;

async fn requests_by_operation(skip: bool) -> Vec<(String, u64, u64)> {
    let mut settings = Settings::default();
    settings.classification_skip_when_no_related = skip;
    let usage = InMemoryProviderUsageRecorder::new();
    let wiring = ProviderWiring::for_tests(usage.clone());
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: TenantContext::new("org_s1", "user_s1").expect("tenant"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "I am learning Rust.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");
    assert_eq!(output.added, 1);

    let mut rows: Vec<(String, u64, u64)> = usage
        .snapshot()
        .records
        .into_iter()
        .map(|r| (r.operation_name, r.request_count, r.error_count))
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn a_skipped_classification_leaves_no_usage_record() {
    let previous = requests_by_operation(false).await;
    let skipped = requests_by_operation(true).await;

    let classify = |rows: &[(String, u64, u64)]| {
        rows.iter()
            .filter(|(op, _, _)| op == "llm_classify_fact_operation")
            .map(|(_, n, _)| *n)
            .sum::<u64>()
    };
    assert_eq!(
        classify(&previous),
        1,
        "previous path: one classification request"
    );
    assert_eq!(
        classify(&skipped),
        0,
        "S1: no classification request recorded"
    );

    let without_classify = |rows: &[(String, u64, u64)]| {
        rows.iter()
            .filter(|(op, _, _)| op != "llm_classify_fact_operation")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(without_classify(&previous), without_classify(&skipped));
    assert!(skipped.iter().all(|(_, _, errors)| *errors == 0));
}
