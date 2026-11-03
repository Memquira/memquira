mod client;
mod embeddings;
mod llm;
mod types;

pub use client::{OpenAiClient, parse_retry_after_ms};
pub use embeddings::{
    EmbeddingOptions, OpenAiEmbeddingProvider, default_embedding_dimensions_for_model,
};
pub use llm::{
    CLASSIFICATION_INSTRUCTIONS_OPERATIONS_V1, ChatJsonMode, ClassificationPrompt,
    OpenAiLlmProvider, OpenAiLlmTransport, OutputTokenLimits,
};
// Exported so the extraction-bug regression tests can drive the parser directly with real
// captured model output, without standing up an HTTP mock.
pub use types::{parse_fact_extraction_response, parse_memory_type_label};

#[cfg(test)]
mod integration_tests {
    use memcore_core::{CandidateFact, Fact, MemorySource, MemoryType, TenantContext};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::inputs::{
        FactClassificationInput, FactExtractionInput, MemoryMessage, MessageRole,
        SummarizationInput,
    };
    use crate::traits::{EmbeddingProvider, LlmProvider};

    fn tenant() -> TenantContext {
        TenantContext::new("org_test", "user_test").expect("tenant")
    }

    /// OpenAI's Responses API contract for `input`: a string or an array of input items (an object
    /// is rejected with HTTP 400). Mocks mounted with this matcher answer only valid requests, so a
    /// malformed body falls through to wiremock's 404 and the call fails.
    struct ValidResponsesInput;

    impl wiremock::Match for ValidResponsesInput {
        fn matches(&self, request: &wiremock::Request) -> bool {
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .map(|v| v["input"].is_string() || v["input"].is_array())
                .unwrap_or(false)
        }
    }

    fn responses_reply(text: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}]
        }))
    }

    #[tokio::test]
    async fn responses_transport_sends_a_valid_input_for_every_json_operation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(ValidResponsesInput)
            .respond_with(responses_reply(
                "{\"operation\":\"Add\",\"target_fact_id\":null,\"reason\":\"ok\",\"confidence\":0.85}",
            ))
            .mount(&server)
            .await;
        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider = OpenAiLlmProvider::new(client, "gpt-4o-mini");
        assert_eq!(provider.transport(), OpenAiLlmTransport::Responses);
        let candidate = CandidateFact::new(
            "User likes Rust.",
            MemoryType::Preference,
            0.9,
            0.8,
            None,
            json!({}),
        )
        .expect("candidate");
        let decision = provider
            .classify_fact_operation(FactClassificationInput {
                tenant: tenant(),
                candidate_fact: candidate,
                existing_facts: vec![],
            })
            .await
            .expect("classification must send a string or array `input`");
        assert_eq!(decision.operation, memcore_core::FactOperation::Add);
    }

    /// OpenAI structured outputs (`strict: true`, the Responses transport and `json_schema` mode)
    /// require every object schema to set `additionalProperties: false` and to list every property
    /// in `required` (optional fields are expressed as a `null` type). Returns the violations.
    fn strict_violations(schema: &serde_json::Value, at: &str, out: &mut Vec<String>) {
        if let Some(obj) = schema.as_object() {
            let is_object = obj
                .get("type")
                .map(|t| {
                    t == "object"
                        || t.as_array()
                            .is_some_and(|a| a.iter().any(|x| x == "object"))
                })
                .unwrap_or(false);
            if is_object && let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
                if obj.get("additionalProperties") != Some(&serde_json::Value::Bool(false)) {
                    out.push(format!("{at}: additionalProperties is not false"));
                }
                let required: Vec<&str> = obj
                    .get("required")
                    .and_then(|r| r.as_array())
                    .map(|r| r.iter().filter_map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                for key in props.keys() {
                    if !required.contains(&key.as_str()) {
                        out.push(format!("{at}.{key}: not in required"));
                    }
                }
            }
            for (key, value) in obj {
                strict_violations(value, &format!("{at}/{key}"), out);
            }
        } else if let Some(items) = schema.as_array() {
            for (i, value) in items.iter().enumerate() {
                strict_violations(value, &format!("{at}[{i}]"), out);
            }
        }
    }

    #[test]
    fn every_schema_sent_with_strict_true_satisfies_openai_strict_mode() {
        let schemas = [
            ("classification", super::types::classification_json_schema()),
            (
                "fact_extraction",
                crate::prompts::fact_extraction_json_schema(),
            ),
            (
                "structured_fact_extraction",
                crate::prompts::structured_fact_extraction_json_schema(),
            ),
            ("consolidation", crate::prompts::consolidation_json_schema()),
            (
                "entity_annotation",
                crate::prompts::entity_annotation_json_schema(),
            ),
            ("entity_match", crate::prompts::entity_match_json_schema()),
        ];
        let mut all = Vec::new();
        for (name, schema) in &schemas {
            strict_violations(schema, name, &mut all);
        }
        assert!(all.is_empty(), "strict-mode violations: {all:#?}");
    }

    #[tokio::test]
    async fn responses_transport_sends_a_valid_input_for_both_extraction_modes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(ValidResponsesInput)
            .respond_with(responses_reply("{\"facts\":[]}"))
            .mount(&server)
            .await;
        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider = OpenAiLlmProvider::new(client, "gpt-4o-mini");
        for mode in [
            memcore_core::ExtractionMode::default(),
            memcore_core::ExtractionMode::Structured,
        ] {
            provider
                .extract_facts(FactExtractionInput {
                    tenant: tenant(),
                    messages: vec![MemoryMessage {
                        role: MessageRole::User,
                        content: "I like Rust.".to_string(),
                        ..Default::default()
                    }],
                    metadata: json!({}),
                    mode,
                })
                .await
                .expect("extraction must send a string or array `input`");
        }
    }

    #[tokio::test]
    async fn extract_facts_calls_responses_endpoint() {
        let server = MockServer::start().await;
        let response_body = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "{\"facts\":[{\"content\":\"User likes Rust.\",\"memory_type\":\"Preference\",\"confidence\":0.9,\"importance\":0.8,\"valid_at\":null,\"metadata\":{}}]}"
                }]
            }]
        });

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider = OpenAiLlmProvider::new(client, "gpt-4.1-mini");

        let facts = provider
            .extract_facts(FactExtractionInput {
                tenant: tenant(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "I like Rust.".to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                mode: Default::default(),
            })
            .await
            .expect("extraction");

        assert_eq!(facts.candidates.len(), 1);
        assert_eq!(facts.candidates[0].content, "User likes Rust.");
        assert_eq!(facts.skipped.total(), 0);
    }

    #[tokio::test]
    async fn classify_fact_operation_parses_model_json() {
        let server = MockServer::start().await;
        let response_body = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "{\"operation\":\"Add\",\"target_fact_id\":null,\"reason\":\"ok\",\"confidence\":0.85}"
                }]
            }]
        });

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider = OpenAiLlmProvider::new(client, "gpt-4.1-mini");

        let candidate = CandidateFact::new(
            "User likes Rust.",
            MemoryType::Preference,
            0.9,
            0.8,
            None,
            json!({}),
        )
        .expect("candidate");

        let decision = provider
            .classify_fact_operation(FactClassificationInput {
                tenant: tenant(),
                candidate_fact: candidate,
                existing_facts: vec![],
            })
            .await
            .expect("classification");

        assert_eq!(decision.operation, memcore_core::FactOperation::Add);
        assert_eq!(decision.confidence, 0.85);
    }

    #[tokio::test]
    async fn summarize_memory_returns_plain_text() {
        let server = MockServer::start().await;
        let response_body = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "User prefers Rust for systems work."
                }]
            }]
        });

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider = OpenAiLlmProvider::new(client, "gpt-4.1-mini");

        let now = chrono::Utc::now();
        let fact = Fact::new(
            uuid::Uuid::new_v4(),
            "org_test",
            "user_test",
            MemoryType::Preference,
            "User likes Rust",
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            now,
            now,
            json!({}),
        )
        .expect("fact");

        let summary = provider
            .summarize_memory(SummarizationInput {
                tenant: tenant(),
                facts: vec![fact],
                max_tokens: None,
            })
            .await
            .expect("summary");

        assert_eq!(summary, "User prefers Rust for systems work.");
    }

    #[tokio::test]
    async fn embed_batch_calls_embeddings_endpoint() {
        let server = MockServer::start().await;
        let response_body = json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3, 0.4], "index": 0 },
                { "embedding": [0.5, 0.6, 0.7, 0.8], "index": 1 }
            ]
        });

        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider =
            OpenAiEmbeddingProvider::new(client, "text-embedding-3-small", 4).expect("provider");

        let embeddings = provider
            .embed_batch(vec!["alpha".to_string(), "beta".to_string()])
            .await
            .expect("embeddings");

        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), 4);
    }

    #[tokio::test]
    async fn extract_facts_calls_chat_completions_for_compat_transport() {
        let server = MockServer::start().await;
        let response_body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "{\"facts\":[{\"content\":\"User likes Rust.\",\"memory_type\":\"Preference\",\"confidence\":0.9,\"importance\":0.8,\"valid_at\":null,\"metadata\":{}}]}"
                }
            }]
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let client = OpenAiClient::new("test-key", server.uri()).expect("client");
        let provider = OpenAiLlmProvider::new(client, "gemini-3.6-flash")
            .with_transport(crate::openai::OpenAiLlmTransport::ChatCompletions);

        let facts = provider
            .extract_facts(FactExtractionInput {
                tenant: tenant(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "I like Rust.".to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                mode: Default::default(),
            })
            .await
            .expect("extraction");

        assert_eq!(facts.candidates.len(), 1);
        assert_eq!(facts.candidates[0].content, "User likes Rust.");
        assert_eq!(facts.skipped.total(), 0);
    }

    #[test]
    fn prefers_chat_completions_outside_openai_host() {
        assert!(OpenAiClient::prefers_chat_completions(
            "https://generativelanguage.googleapis.com/v1beta/openai/"
        ));
        assert!(OpenAiClient::prefers_chat_completions(
            "https://api.groq.com/openai/v1"
        ));
        assert!(OpenAiClient::prefers_chat_completions(
            "https://api.z.ai/api/paas/v4/"
        ));
        assert!(!OpenAiClient::prefers_chat_completions(
            "https://api.openai.com/v1"
        ));
    }

    #[test]
    fn prefers_disabled_thinking_for_zai_host() {
        assert!(OpenAiClient::prefers_disabled_thinking(
            "https://api.z.ai/api/paas/v4/"
        ));
        assert!(!OpenAiClient::prefers_disabled_thinking(
            "https://api.groq.com/openai/v1"
        ));
        assert!(!OpenAiClient::prefers_disabled_thinking(
            "https://api.openai.com/v1"
        ));
    }

    #[tokio::test]
    async fn unauthorized_api_key_maps_to_provider_error() {
        let server = MockServer::start().await;
        let response_body = json!({
            "error": {
                "message": "Incorrect API key provided",
                "type": "invalid_request_error"
            }
        });

        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(401).set_body_json(response_body))
            .mount(&server)
            .await;

        let client = OpenAiClient::new("bad-key", server.uri()).expect("client");
        let provider =
            OpenAiEmbeddingProvider::new(client, "text-embedding-3-small", 4).expect("provider");

        let error = provider.embed_text("hello").await.expect_err("should fail");
        assert!(matches!(
            error,
            memcore_common::MemcoreError::ProviderError(_)
        ));
        assert!(error.to_string().contains("unauthorized"));
    }

    #[tokio::test]
    #[ignore = "requires OPENAI_API_KEY; run with: cargo test -p memcore-providers openai_live -- --ignored --nocapture"]
    async fn openai_live_smoke_test() {
        let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
        let client = OpenAiClient::new(api_key, "https://api.openai.com/v1").expect("client");
        let provider =
            OpenAiEmbeddingProvider::new(client, "text-embedding-3-small", 256).expect("provider");
        let embedding = provider
            .embed_text("memcore smoke test")
            .await
            .expect("embed");
        assert_eq!(embedding.len(), 256);
    }

    /// The candidate prompt defines exactly the operations the parser accepts and the
    /// ingest path applies, and none it does not.
    #[test]
    fn the_operations_prompt_names_only_supported_operations() {
        use memcore_core::FactOperation;
        let prompt = CLASSIFICATION_INSTRUCTIONS_OPERATIONS_V1;
        for (label, operation) in [
            ("Add", FactOperation::Add),
            ("Update", FactOperation::Update),
            ("Delete", FactOperation::Delete),
            ("NoOp", FactOperation::NoOp),
        ] {
            assert!(prompt.contains(&format!("- {label}: ")), "{label} defined");
            assert_eq!(types::parse_fact_operation_label(label).unwrap(), operation);
        }
        for absent in ["Archive", "Summarize", "Invalidate", "Prefer Add"] {
            assert!(!prompt.contains(absent), "{absent:?}");
        }
        assert_eq!(
            ClassificationPrompt::default().instructions(),
            ClassificationPrompt::Legacy.instructions()
        );
        assert!(
            ClassificationPrompt::Legacy
                .instructions()
                .contains("Prefer Add when there is no clear conflict.")
        );
    }

    /// The selected prompt is the only difference between the two requests: same model, schema and
    /// payload; the default request is the production one.
    #[tokio::test]
    async fn a_classification_request_carries_the_selected_prompt_and_the_same_payload() {
        fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
            match value {
                serde_json::Value::String(text) => out.push(text.clone()),
                serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
                serde_json::Value::Object(map) => map.values().for_each(|v| strings(v, out)),
                _ => {}
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "completed",
                "output": [{
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "text": "{\"operation\":\"Add\",\"target_fact_id\":null,\"reason\":\"ok\",\"confidence\":0.8}"
                    }]
                }]
            })))
            .mount(&server)
            .await;
        let existing = Fact::new(
            uuid::Uuid::nil(),
            "org_test",
            "user_test",
            MemoryType::Profile,
            "Jonas drives a red hatchback.",
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            chrono::Utc::now(),
            chrono::Utc::now(),
            json!({}),
        )
        .expect("fact");
        let input = || FactClassificationInput {
            tenant: tenant(),
            candidate_fact: CandidateFact::new(
                "Jonas now drives a van.",
                MemoryType::Profile,
                0.9,
                0.8,
                None,
                json!({}),
            )
            .expect("candidate"),
            existing_facts: vec![existing.clone()],
        };
        for prompt in [None, Some(ClassificationPrompt::OperationsV1)] {
            let client = OpenAiClient::new("test-key", server.uri()).expect("client");
            let mut provider = OpenAiLlmProvider::new(client, "gpt-4o-mini");
            if let Some(prompt) = prompt {
                provider = provider.with_classification_prompt(prompt);
            }
            provider
                .classify_fact_operation(input())
                .await
                .expect("classification");
        }
        let requests = server.received_requests().await.expect("recorded");
        assert_eq!(requests.len(), 2);
        let bodies: Vec<serde_json::Value> = requests
            .iter()
            .map(|request| serde_json::from_slice(&request.body).expect("json body"))
            .collect();
        let texts: Vec<Vec<String>> = bodies
            .iter()
            .map(|body| {
                let mut out = Vec::new();
                strings(body, &mut out);
                out
            })
            .collect();
        let has = |texts: &[String], needle: &str| texts.iter().any(|text| text.contains(needle));
        assert!(has(
            &texts[0],
            "Prefer Add when there is no clear conflict."
        ));
        assert!(!has(&texts[0], "Choose exactly one operation:"));
        assert!(has(&texts[1], "Choose exactly one operation:"));
        assert!(!has(
            &texts[1],
            "Prefer Add when there is no clear conflict."
        ));
        let payload = |texts: &[String]| {
            texts
                .iter()
                .find(|text| text.contains("candidate_fact"))
                .cloned()
                .expect("payload")
        };
        assert_eq!(payload(&texts[0]), payload(&texts[1]), "payload unchanged");
        assert_eq!(bodies[0]["model"], bodies[1]["model"]);
    }

    /// Malformed decisions stay invalid output (the engine rejects the candidate, the request
    /// goes on) under either prompt: an unknown operation, a non-UUID target of an Update, an
    /// out-of-range confidence, broken JSON.
    #[test]
    fn malformed_decisions_are_rejected_as_invalid_output() {
        for text in [
            "{\"operation\":\"Replace\",\"target_fact_id\":null,\"reason\":null,\"confidence\":0.9}",
            "{\"operation\":\"Update\",\"target_fact_id\":\"fact-1\",\"reason\":null,\"confidence\":0.9}",
            "{\"operation\":\"Update\",\"target_fact_id\":null,\"reason\":null,\"confidence\":1.5}",
            "{\"operation\":\"Update\"",
        ] {
            let error = types::parse_classification_response(text).expect_err(text);
            assert!(error.is_provider_output_invalid(), "{text}: {error}");
        }
        // An Update without a target parses; the ingest path decides (the semantic guards store an
        // Add: `target_not_offered`).
        let decision = types::parse_classification_response(
            "{\"operation\":\"Update\",\"target_fact_id\":null,\"reason\":null,\"confidence\":0.9}",
        )
        .expect("parses");
        assert_eq!(decision.target_fact_id, None);
    }
}
