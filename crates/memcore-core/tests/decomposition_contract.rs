//! The frozen decomposition contract fixture: every invented case in
//! `fixtures/phase4_decomposition_contract_v1.json` runs through the real `LlmDecompositionProvider`
//! and `DecompositionService` over a canned LLM, so the prompt, parser, validation and fallback are
//! tested without any model. The exact request sent to the LLM port is checked too.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::FactOperationDecision;
use memcore_core::candidates::decomposition::{
    DecompositionIdentity, DecompositionService, DecompositionState, FailureClass,
    LlmDecompositionProvider, MAX_OUTPUT_TOKENS, SYSTEM_PROMPT, TEMPERATURE, TOP_P,
};
use memcore_core::ports::{
    FactClassificationInput, FactExtractionInput, FactExtractionOutput, LlmProvider,
    QueryDecompositionInput, SummarizationInput,
};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(10);

enum Canned {
    Text(String),
    Error,
    Hang,
}

struct CannedLlm {
    reply: Canned,
    requests: Mutex<Vec<QueryDecompositionInput>>,
}

#[async_trait]
impl LlmProvider for CannedLlm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        Err(MemcoreError::ProviderError("not used".to_string()))
    }
    async fn classify_fact_operation(
        &self,
        _: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        Err(MemcoreError::ProviderError("not used".to_string()))
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Err(MemcoreError::ProviderError("not used".to_string()))
    }
    async fn decompose_query(&self, input: QueryDecompositionInput) -> MemcoreResult<String> {
        self.requests.lock().unwrap().push(input);
        match &self.reply {
            Canned::Text(text) => Ok(text.clone()),
            Canned::Error => Err(MemcoreError::ProviderError(
                "invented connection reset".to_string(),
            )),
            Canned::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(String::new())
            }
        }
    }
}

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/phase4_decomposition_contract_v1.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture json")
}

#[tokio::test(start_paused = true)]
async fn every_fixture_case_gets_its_frozen_outcome() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 31);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let question = case["question"].as_str().unwrap();
        let reply = match &case["response"] {
            Value::String(text) => Canned::Text(text.clone()),
            Value::Object(object) if object.contains_key("provider_error") => Canned::Error,
            Value::Object(object) if object.contains_key("provider_timeout_ms") => Canned::Hang,
            Value::Null => Canned::Text(String::new()),
            other => panic!("{id}: unexpected response {other}"),
        };
        let llm = Arc::new(CannedLlm {
            reply,
            requests: Mutex::new(Vec::new()),
        });
        let provider = LlmDecompositionProvider::new(
            llm.clone(),
            DecompositionIdentity {
                provider: "openai".to_string(),
                model: "bedrock-nova-lite".to_string(),
            },
        );
        let service = DecompositionService::new(Arc::new(provider));
        let audit = service
            .decompose("org-fixture", question, None, &[], TIMEOUT)
            .await;
        let expected = case["expected"].as_str().unwrap();
        match expected {
            "accepted" => {
                assert_eq!(audit.state, DecompositionState::Called, "{id}");
                let want: Vec<String> =
                    serde_json::from_value(case["expected_sub_queries"].clone()).unwrap();
                assert_eq!(audit.accepted, want, "{id}");
                assert!(audit.failure_class.is_none(), "{id}");
            }
            "fallback:input_too_long" => {
                assert_eq!(
                    audit.state,
                    DecompositionState::InputTooLongFallback,
                    "{id}"
                );
                assert!(llm.requests.lock().unwrap().is_empty(), "{id}: never sent");
            }
            _ => {
                let class = match expected {
                    "fallback:schema" => FailureClass::Validation,
                    "fallback:invalid_json" => FailureClass::InvalidJson,
                    "fallback:model_error" => FailureClass::ProviderError,
                    "fallback:model_timeout" => FailureClass::Timeout,
                    other => panic!("{id}: unknown expectation {other}"),
                };
                assert_eq!(audit.state, DecompositionState::FailedFallback, "{id}");
                assert_eq!(audit.failure_class, Some(class), "{id}");
                assert!(audit.accepted.is_empty(), "{id}: nothing partially used");
            }
        }
        if expected != "fallback:input_too_long" {
            assert_eq!(audit.operations, 1, "{id}");
            let requests = llm.requests.lock().unwrap();
            assert_eq!(requests.len(), 1, "{id}: one operation");
            assert_eq!(
                requests[0],
                QueryDecompositionInput {
                    system: SYSTEM_PROMPT.to_string(),
                    user: format!("Question: {question}"),
                    max_output_tokens: MAX_OUTPUT_TOKENS,
                    temperature: TEMPERATURE,
                    top_p: TOP_P,
                },
                "{id}: the exact frozen request"
            );
        }
    }
}

#[tokio::test]
async fn a_truncated_reply_is_unusable_output() {
    struct Truncates;
    #[async_trait]
    impl LlmProvider for Truncates {
        async fn extract_facts(
            &self,
            _: FactExtractionInput,
        ) -> MemcoreResult<FactExtractionOutput> {
            unreachable!()
        }
        async fn classify_fact_operation(
            &self,
            _: FactClassificationInput,
        ) -> MemcoreResult<FactOperationDecision> {
            unreachable!()
        }
        async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
            unreachable!()
        }
        async fn decompose_query(&self, _: QueryDecompositionInput) -> MemcoreResult<String> {
            Err(MemcoreError::provider_output_truncated(
                "decompose_query",
                Some(MAX_OUTPUT_TOKENS),
            ))
        }
    }
    let provider = LlmDecompositionProvider::new(
        Arc::new(Truncates),
        DecompositionIdentity {
            provider: "openai".to_string(),
            model: "bedrock-nova-lite".to_string(),
        },
    );
    let service = DecompositionService::new(Arc::new(provider));
    let audit = service
        .decompose(
            "org",
            "Did Rowena or Dunstan bake more?",
            None,
            &[],
            TIMEOUT,
        )
        .await;
    assert_eq!(audit.failure_class, Some(FailureClass::UnusableOutput));
}

/// The frozen `decompose-v1` contract hash. Changing the prompt or template fails here; a
/// new prompt needs a new contract version and maintainer review.
#[test]
fn the_frozen_contract_hash_is_pinned() {
    assert_eq!(
        memcore_core::candidates::decomposition::contract_hash(),
        "9886f51f850f60d1df703f7455555e5e05ecfc4c4e074bdb348d141d5e933ff2"
    );
    assert_eq!(
        memcore_core::candidates::decomposition::PROMPT_VERSION,
        "decompose-v1"
    );
}
