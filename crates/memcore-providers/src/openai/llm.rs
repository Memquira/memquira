use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{ExtractionMode, FactExtractionOutput, FactOperationDecision};

use crate::structured::{StructuredWindow, parse_structured_fact_extraction_response};
use serde_json::json;

use crate::inputs::{
    FactClassificationInput, FactExtractionInput, MemoryMessage, MessageRole, SummarizationInput,
};
use crate::traits::LlmProvider;

use super::client::OpenAiClient;
use super::types::{
    ChatCompletionMessage, ChatCompletionsRequest, classification_json_schema,
    extract_chat_completion_text, extract_output_text, parse_classification_response,
    parse_fact_extraction_response,
};

const CLASSIFICATION_INSTRUCTIONS: &str = r#"You classify how a candidate memory fact should be stored relative to existing facts.
Return valid JSON only matching the schema.
Prefer Add when there is no clear conflict."#;

/// candidate classification instructions (`MEMCORE_CLASSIFICATION_PROMPT=operations_v1`):
/// the same JSON contract with each operation defined. V2.2 found gpt-4o-mini answering Add for 26 of
/// 42 corrections it was shown the old fact for, often while naming the change in its reason, under
/// instructions that define no operation and say "Prefer Add". Experimental: not measured with a
/// real model. The examples are invented; none comes from an evaluation set.
pub const CLASSIFICATION_INSTRUCTIONS_OPERATIONS_V1: &str = r#"You decide how a candidate memory fact relates to a user's existing facts.
Return valid JSON only matching the schema.
Choose exactly one operation:
- Add: the candidate is new information that can be true at the same time as every existing fact: another hobby, a second project, a different person, a different attribute.
- Update: the candidate gives a new or corrected current value for the same attribute of the same subject as one existing fact, so that fact is no longer current: a new job or address, a moved date, a changed preference, "actually it is X". Set target_fact_id to that fact's id.
- Delete: the candidate says an existing fact is no longer true and gives no new value. Set target_fact_id to that fact's id.
- NoOp: the candidate says nothing that an existing fact does not already say.
Rules:
- Same subject, same attribute, different current value is Update even when the wording or the memory type differs. Do not keep both with Add.
- Different subjects (the user and a relative, two projects, two pets) are never Update or Delete.
- A new event of the same kind as a past event is Add: the past event still happened.
- If you are unsure whether the candidate is about the same attribute of the same subject, choose Add.
- target_fact_id is the id of one existing fact for Update and Delete, and null for Add and NoOp.
- confidence is a number from 0.0 to 1.0; reason is one short sentence.
Examples:
- existing "Jonas drives a red hatchback." / candidate "Jonas sold the hatchback and now drives a van." -> Update
- existing "Ines plays the accordion." / candidate "Ines has started playing the harmonica too." -> Add
- existing "The user's uncle lives in Graz." / candidate "The user lives in Linz." -> Add
- existing "Ines goes to a barber in the old town." / candidate "Ines now goes to a barber near the station." -> Update
- existing "Kofi has a parking permit." / candidate "Kofi no longer has a parking permit." -> Delete
- existing "Jonas enjoys trail running." / candidate "Jonas likes trail running." -> NoOp"#;

/// Which instructions a fact-classification request carries (`MEMCORE_CLASSIFICATION_PROMPT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClassificationPrompt {
    /// The production instructions, unchanged. Default.
    #[default]
    Legacy,
    /// [`CLASSIFICATION_INSTRUCTIONS_OPERATIONS_V1`]: experimental, opt-in.
    OperationsV1,
}

impl ClassificationPrompt {
    /// The system instructions of a classification request.
    pub fn instructions(self) -> &'static str {
        match self {
            Self::Legacy => CLASSIFICATION_INSTRUCTIONS,
            Self::OperationsV1 => CLASSIFICATION_INSTRUCTIONS_OPERATIONS_V1,
        }
    }
}

const SUMMARIZATION_INSTRUCTIONS: &str = r#"You summarize memory facts into concise plain text for downstream context.
Keep the summary short and factual.
Do not use markdown unless it materially improves clarity."#;

/// HTTP shape used for OpenAI-compatible LLM calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiLlmTransport {
    /// Native OpenAI `/responses` API (default for `api.openai.com`).
    Responses,
    /// OpenAI-compatible `/chat/completions` (Gemini, Groq, and most vendor proxies).
    ChatCompletions,
}

/// How chat-completions JSON calls ask for JSON (`MEMCORE_LLM_JSON_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatJsonMode {
    /// `response_format: {"type": "json_object"}`; the schema is only in the system prompt.
    /// Default: request bodies are unchanged from earlier releases.
    #[default]
    JsonObject,
    /// `response_format: {"type": "json_schema", "json_schema": {name, schema, strict}}` with the
    /// same schema the prompt already shows: schema-constrained decoding (OpenAI structured
    /// outputs; Ollama >= 0.5). The system prompt is unchanged.
    JsonSchema,
    /// No `response_format` field; the system prompt already demands a single JSON object and shows
    /// the schema. For servers that reject `response_format`.
    Instruction,
}

/// Output-token limits per operation.
///
/// Extraction and classification need very different budgets — a fact list versus one small JSON
/// decision — so they are configured separately (`MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS`,
/// `MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS`). The defaults here mirror `memcore-config`; the API
/// always sets both from `Settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputTokenLimits {
    pub extraction: u32,
    pub classification: u32,
}

impl Default for OutputTokenLimits {
    fn default() -> Self {
        Self {
            extraction: 2_048,
            classification: 300,
        }
    }
}

/// Summarization keeps its previous fixed limit; it is outside the extraction reliability fix.
const SUMMARIZATION_MAX_OUTPUT_TOKENS: u32 = 300;

/// One model call: which operation it is, for errors and logs, and its output-token limit.
#[derive(Debug, Clone, Copy)]
struct CallSpec {
    operation: &'static str,
    max_output_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct OpenAiLlmProvider {
    client: OpenAiClient,
    model: String,
    transport: OpenAiLlmTransport,
    limits: OutputTokenLimits,
    extraction_policy: crate::prompts::StructuredExtractionPolicy,
    json_mode: ChatJsonMode,
    classification_prompt: ClassificationPrompt,
}

impl OpenAiLlmProvider {
    /// Defaults to the OpenAI `/responses` transport (used by unit tests and direct OpenAI).
    pub fn new(client: OpenAiClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            transport: OpenAiLlmTransport::Responses,
            limits: OutputTokenLimits::default(),
            extraction_policy: crate::prompts::StructuredExtractionPolicy::default(),
            json_mode: ChatJsonMode::default(),
            classification_prompt: ClassificationPrompt::default(),
        }
    }

    /// Select transport from base URL (chat completions for Gemini/Groq-style OpenAI-compat hosts).
    pub fn for_base_url(client: OpenAiClient, model: impl Into<String>) -> Self {
        let transport = if OpenAiClient::prefers_chat_completions(client.base_url()) {
            OpenAiLlmTransport::ChatCompletions
        } else {
            OpenAiLlmTransport::Responses
        };
        Self {
            client,
            model: model.into(),
            transport,
            limits: OutputTokenLimits::default(),
            extraction_policy: crate::prompts::StructuredExtractionPolicy::default(),
            json_mode: ChatJsonMode::default(),
            classification_prompt: ClassificationPrompt::default(),
        }
    }

    /// How chat-completions JSON calls request JSON (default `JsonObject`).
    pub fn with_json_mode(mut self, json_mode: ChatJsonMode) -> Self {
        self.json_mode = json_mode;
        self
    }

    /// The instructions of classification requests (default `Legacy`, the production prompt).
    pub fn with_classification_prompt(mut self, prompt: ClassificationPrompt) -> Self {
        self.classification_prompt = prompt;
        self
    }

    pub fn with_output_token_limits(mut self, limits: OutputTokenLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The retention policy of structured extraction requests. Default `Retentive`.
    pub fn with_structured_extraction_policy(
        mut self,
        policy: crate::prompts::StructuredExtractionPolicy,
    ) -> Self {
        self.extraction_policy = policy;
        self
    }

    pub fn output_token_limits(&self) -> OutputTokenLimits {
        self.limits
    }

    pub fn with_transport(mut self, transport: OpenAiLlmTransport) -> Self {
        self.transport = transport;
        self
    }

    pub fn transport(&self) -> OpenAiLlmTransport {
        self.transport
    }

    async fn request_json(
        &self,
        call: CallSpec,
        instructions: &str,
        input: serde_json::Value,
        schema_name: &str,
        schema: serde_json::Value,
    ) -> MemcoreResult<String> {
        match self.transport {
            OpenAiLlmTransport::Responses => {
                self.responses_json(call, instructions, input, schema_name, schema)
                    .await
            }
            OpenAiLlmTransport::ChatCompletions => {
                self.chat_json(call, instructions, input, schema_name, schema)
                    .await
            }
        }
    }

    async fn request_plain_text(
        &self,
        call: CallSpec,
        instructions: &str,
        input: serde_json::Value,
    ) -> MemcoreResult<String> {
        match self.transport {
            OpenAiLlmTransport::Responses => {
                self.responses_plain_text(call, instructions, input).await
            }
            OpenAiLlmTransport::ChatCompletions => {
                self.chat_plain_text(call, instructions, input).await
            }
        }
    }

    async fn responses_json(
        &self,
        call: CallSpec,
        instructions: &str,
        input: serde_json::Value,
        schema_name: &str,
        schema: serde_json::Value,
    ) -> MemcoreResult<String> {
        let request = self.client.responses_request_body(
            &self.model,
            instructions,
            input,
            schema_name,
            schema,
        );
        let response = self.client.create_response(&request).await?;
        // The Responses transport has never sent an output limit (reasoning models count their
        // reasoning tokens against it), so a truncation here is reported without one.
        if response.is_truncated() {
            return Err(truncated(call.operation, None));
        }
        let text = extract_output_text(&response)?;
        Ok(text)
    }

    async fn responses_plain_text(
        &self,
        call: CallSpec,
        instructions: &str,
        input: serde_json::Value,
    ) -> MemcoreResult<String> {
        let request = self
            .client
            .responses_text_request_body(&self.model, instructions, input);
        let response = self.client.create_response(&request).await?;
        if response.is_truncated() {
            return Err(truncated(call.operation, None));
        }
        extract_output_text(&response)
    }

    /// Structured extraction. The legacy request above is left exactly as it was, so
    /// with the flag off nothing about what is sent to the model changes.
    async fn extract_structured_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        let window = StructuredWindow::new(&input.messages);
        let text = self
            .request_json(
                CallSpec {
                    operation: "extract_facts",
                    max_output_tokens: self.limits.extraction,
                },
                crate::prompts::structured_fact_extraction_instructions_for(self.extraction_policy),
                window.payload().clone(),
                "memcore_structured_fact_extraction",
                crate::prompts::structured_fact_extraction_json_schema(),
            )
            .await?;
        parse_structured_fact_extraction_response(&text, &window)
    }

    fn chat_thinking_option(&self) -> Option<serde_json::Value> {
        if OpenAiClient::prefers_disabled_thinking(self.client.base_url()) {
            Some(json!({ "type": "disabled" }))
        } else {
            None
        }
    }

    async fn chat_json(
        &self,
        call: CallSpec,
        instructions: &str,
        input: serde_json::Value,
        schema_name: &str,
        schema: serde_json::Value,
    ) -> MemcoreResult<String> {
        let response_format = match self.json_mode {
            ChatJsonMode::Instruction => None,
            ChatJsonMode::JsonObject => Some(json!({ "type": "json_object" })),
            ChatJsonMode::JsonSchema => Some(json!({
                "type": "json_schema",
                "json_schema": { "name": schema_name, "schema": schema.clone(), "strict": true },
            })),
        };
        let system = format!(
            "{instructions}\nReturn a single JSON object only that matches this JSON Schema:\n{schema}"
        );
        let request = ChatCompletionsRequest {
            model: self.model.clone(),
            messages: vec![
                ChatCompletionMessage {
                    role: "system".to_string(),
                    content: system,
                },
                ChatCompletionMessage {
                    role: "user".to_string(),
                    content: input.to_string(),
                },
            ],
            response_format,
            max_tokens: Some(call.max_output_tokens),
            temperature: None,
            top_p: None,
            thinking: self.chat_thinking_option(),
        };
        let response = self.client.create_chat_completion(&request).await?;
        if response.is_truncated() {
            return Err(truncated(call.operation, Some(call.max_output_tokens)));
        }
        extract_chat_completion_text(&response)
    }

    async fn chat_plain_text(
        &self,
        call: CallSpec,
        instructions: &str,
        input: serde_json::Value,
    ) -> MemcoreResult<String> {
        let request = ChatCompletionsRequest {
            model: self.model.clone(),
            messages: vec![
                ChatCompletionMessage {
                    role: "system".to_string(),
                    content: instructions.to_string(),
                },
                ChatCompletionMessage {
                    role: "user".to_string(),
                    content: input.to_string(),
                },
            ],
            response_format: None,
            max_tokens: Some(call.max_output_tokens),
            temperature: None,
            top_p: None,
            thinking: self.chat_thinking_option(),
        };
        let response = self.client.create_chat_completion(&request).await?;
        if response.is_truncated() {
            return Err(truncated(call.operation, Some(call.max_output_tokens)));
        }
        extract_chat_completion_text(&response)
    }
}

/// Builds the truncation error and logs it once, where it is detected. No model text is logged —
/// only the operation name and the limit, both of which come from this code.
fn truncated(operation: &'static str, max_output_tokens: Option<u32>) -> MemcoreError {
    tracing::warn!(
        event = "provider_output_truncated",
        operation,
        max_output_tokens,
        "model output hit the output-token limit; the response is incomplete and is not retried"
    );
    MemcoreError::provider_output_truncated(operation, max_output_tokens)
}

fn messages_to_input(messages: &[MemoryMessage]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = messages
        .iter()
        .map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
            };
            json!({
                "role": role,
                "content": message.content,
            })
        })
        .collect();

    serde_json::Value::Array(items)
}

#[async_trait]
impl LlmProvider for OpenAiLlmProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        if input.mode == ExtractionMode::Structured {
            return self.extract_structured_facts(input).await;
        }

        let payload = json!({
            "tenant": {
                "org_id": input.tenant.org_id,
                "user_id": input.tenant.user_id,
            },
            "messages": messages_to_input(&input.messages),
            "metadata": input.metadata,
        });

        let text = self
            .request_json(
                CallSpec {
                    operation: "extract_facts",
                    max_output_tokens: self.limits.extraction,
                },
                crate::prompts::fact_extraction_instructions(),
                payload,
                "memcore_fact_extraction",
                crate::prompts::fact_extraction_json_schema(),
            )
            .await?;

        parse_fact_extraction_response(&text)
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        let existing: Vec<serde_json::Value> = input
            .existing_facts
            .iter()
            .map(|fact| {
                json!({
                    "id": fact.id,
                    "content": fact.content,
                    "memory_type": fact.memory_type,
                    "confidence": fact.confidence,
                    "importance": fact.importance,
                })
            })
            .collect();

        let payload = json!({
            "tenant": {
                "org_id": input.tenant.org_id,
                "user_id": input.tenant.user_id,
            },
            "candidate_fact": {
                "content": input.candidate_fact.content,
                "memory_type": input.candidate_fact.memory_type,
                "confidence": input.candidate_fact.confidence,
                "importance": input.candidate_fact.importance,
            },
            "existing_facts": existing,
        });

        let text = self
            .request_json(
                CallSpec {
                    operation: "classify_fact_operation",
                    max_output_tokens: self.limits.classification,
                },
                self.classification_prompt.instructions(),
                payload,
                "memcore_fact_classification",
                classification_json_schema(),
            )
            .await?;

        parse_classification_response(&text)
    }

    async fn decide_consolidation(
        &self,
        input: memcore_core::ports::ConsolidationInput,
    ) -> MemcoreResult<memcore_core::ports::ConsolidationDecision> {
        let text = self
            .request_json(
                CallSpec {
                    operation: "decide_consolidation",
                    max_output_tokens: self.limits.classification,
                },
                crate::prompts::CONSOLIDATION_INSTRUCTIONS,
                crate::prompts::consolidation_payload(&input),
                "memcore_consolidation_decision",
                crate::prompts::consolidation_json_schema(),
            )
            .await?;

        crate::prompts::parse_consolidation_response(&text)
    }

    async fn annotate_entities(
        &self,
        input: memcore_core::ports::EntityAnnotationInput,
    ) -> MemcoreResult<memcore_core::ports::EntityAnnotationOutput> {
        // The retry is its own operation, so usage, circuits and truncation reports keep
        // the two requests apart. The output-token limit is the same for both.
        let operation = match input.attempt {
            memcore_core::ports::AnnotationAttempt::First => "annotate_entities",
            memcore_core::ports::AnnotationAttempt::Retry => "annotate_entities_retry",
        };
        let text = self
            .request_json(
                CallSpec {
                    operation,
                    max_output_tokens: self.limits.extraction,
                },
                &crate::prompts::entity_annotation_instructions(input.attempt),
                crate::prompts::entity_annotation_payload(&input),
                "memcore_entity_annotation",
                crate::prompts::entity_annotation_json_schema(),
            )
            .await?;

        crate::prompts::parse_entity_annotation_response(&text)
    }

    /// Exactly the given system and user text, a JSON-object
    /// response format and the given sampling settings; the raw text is returned for the caller's
    /// contract parser. Chat-completions transport only.
    async fn decompose_query(
        &self,
        input: memcore_core::ports::QueryDecompositionInput,
    ) -> MemcoreResult<String> {
        const OPERATION: &str = "decompose_query";
        if self.transport != OpenAiLlmTransport::ChatCompletions {
            return Err(MemcoreError::ProviderError(
                "query decomposition needs the chat-completions transport".to_string(),
            ));
        }
        let request = ChatCompletionsRequest {
            model: self.model.clone(),
            messages: vec![
                ChatCompletionMessage {
                    role: "system".to_string(),
                    content: input.system,
                },
                ChatCompletionMessage {
                    role: "user".to_string(),
                    content: input.user,
                },
            ],
            response_format: Some(json!({ "type": "json_object" })),
            max_tokens: Some(input.max_output_tokens),
            temperature: Some(input.temperature),
            top_p: Some(input.top_p),
            thinking: self.chat_thinking_option(),
        };
        let response = self.client.create_chat_completion(&request).await?;
        if response.is_truncated() {
            return Err(truncated(OPERATION, Some(input.max_output_tokens)));
        }
        extract_chat_completion_text(&response)
    }

    async fn confirm_entity_match(
        &self,
        input: memcore_core::ports::EntityMatchInput,
    ) -> MemcoreResult<bool> {
        let text = self
            .request_json(
                CallSpec {
                    operation: "confirm_entity_match",
                    max_output_tokens: self.limits.classification,
                },
                crate::prompts::ENTITY_MATCH_INSTRUCTIONS,
                crate::prompts::entity_match_payload(&input),
                "memcore_entity_match",
                crate::prompts::entity_match_json_schema(),
            )
            .await?;

        crate::prompts::parse_entity_match_response(&text)
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        let facts: Vec<serde_json::Value> = input
            .facts
            .iter()
            .map(|fact| {
                json!({
                    "content": fact.content,
                    "memory_type": fact.memory_type,
                    "importance": fact.importance,
                })
            })
            .collect();

        let payload = json!({
            "tenant": {
                "org_id": input.tenant.org_id,
                "user_id": input.tenant.user_id,
            },
            "facts": facts,
            "max_tokens": input.max_tokens,
        });

        let summary = self
            .request_plain_text(
                CallSpec {
                    operation: "summarize_memory",
                    max_output_tokens: SUMMARIZATION_MAX_OUTPUT_TOKENS,
                },
                SUMMARIZATION_INSTRUCTIONS,
                payload,
            )
            .await?;

        if let Some(max_tokens) = input.max_tokens {
            let max_chars = max_tokens.saturating_mul(4);
            if summary.len() > max_chars {
                return Ok(summary[..max_chars].to_string());
            }
        }

        Ok(summary)
    }
}
