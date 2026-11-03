//! Amazon Bedrock reranker: Amazon Rerank 1.0 (`amazon.rerank-v1:0`)
//! through the documented Agents Runtime `Rerank` API, signed with SigV4 from the standard AWS
//! credential chain (optionally a named profile). Compiled only with the `bedrock-rerank` feature.
//!
//! Credentials are resolved on the first call, never at startup, so a memcore that does not rerank
//! needs no AWS configuration. Every submitted document is requested back (`numberOfResults` = the
//! document count); the returned indices are mapped to the submitted order and must cover every
//! document exactly once, or the call fails and the caller keeps its prior order.

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_config::Region;
use aws_sdk_bedrockagentruntime::Client;
use aws_sdk_bedrockagentruntime::types::{
    BedrockRerankingConfiguration, BedrockRerankingModelConfiguration, RerankDocument,
    RerankDocumentType, RerankQuery, RerankQueryContentType, RerankSource, RerankSourceType,
    RerankTextDocument, RerankingConfiguration, RerankingConfigurationType,
};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::candidates::rerank::{RerankProvider, scores_from_indexed};

/// The Agents Runtime API accepts at most this many sources per request.
pub const MAX_BEDROCK_RERANK_SOURCES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockRerankConfig {
    pub region: String,
    pub model_arn: String,
    /// A named AWS profile; `None` uses the default credential chain.
    pub profile: Option<String>,
}

pub struct BedrockRerankProvider {
    client: Client,
    model_arn: String,
}

fn provider_error(message: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::ProviderError(format!("bedrock reranker: {message}"))
}

impl BedrockRerankProvider {
    /// Builds the client. No credential is fetched until the first call.
    pub async fn new(config: &BedrockRerankConfig) -> Self {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()));
        if let Some(profile) = &config.profile {
            loader = loader.profile_name(profile);
        }
        let shared = loader.load().await;
        Self {
            client: Client::new(&shared),
            model_arn: config.model_arn.clone(),
        }
    }

    fn text(text: &str) -> RerankTextDocument {
        RerankTextDocument::builder().text(text).build()
    }

    fn request_parts(
        &self,
        query: &str,
        documents: &[String],
    ) -> MemcoreResult<(RerankQuery, Vec<RerankSource>, RerankingConfiguration)> {
        let count =
            i32::try_from(documents.len()).map_err(|_| provider_error("too many documents"))?;
        let query = RerankQuery::builder()
            .r#type(RerankQueryContentType::Text)
            .text_query(Self::text(query))
            .build()
            .map_err(provider_error)?;
        let sources = documents
            .iter()
            .map(|document| {
                let inline = RerankDocument::builder()
                    .r#type(RerankDocumentType::Text)
                    .text_document(Self::text(document))
                    .build()
                    .map_err(provider_error)?;
                RerankSource::builder()
                    .r#type(RerankSourceType::Inline)
                    .inline_document_source(inline)
                    .build()
                    .map_err(provider_error)
            })
            .collect::<MemcoreResult<Vec<_>>>()?;
        let configuration = RerankingConfiguration::builder()
            .r#type(RerankingConfigurationType::BedrockRerankingModel)
            .bedrock_reranking_configuration(
                BedrockRerankingConfiguration::builder()
                    .number_of_results(count)
                    .model_configuration(
                        BedrockRerankingModelConfiguration::builder()
                            .model_arn(&self.model_arn)
                            .build()
                            .map_err(provider_error)?,
                    )
                    .build(),
            )
            .build()
            .map_err(provider_error)?;
        Ok((query, sources, configuration))
    }
}

#[async_trait]
impl RerankProvider for BedrockRerankProvider {
    fn name(&self) -> &'static str {
        "bedrock"
    }

    async fn score(&self, query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        if documents.len() > MAX_BEDROCK_RERANK_SOURCES {
            return Err(provider_error(format!(
                "{} documents exceed the {MAX_BEDROCK_RERANK_SOURCES}-source limit",
                documents.len()
            )));
        }
        let (query, sources, configuration) = self.request_parts(query, documents)?;
        let response = self
            .client
            .rerank()
            .queries(query)
            .set_sources(Some(sources))
            .reranking_configuration(configuration)
            .send()
            .await
            .map_err(|error| {
                provider_error(aws_sdk_bedrockagentruntime::error::DisplayErrorContext(
                    error,
                ))
            })?;
        if response.next_token().is_some() {
            return Err(provider_error("paginated response: results are incomplete"));
        }
        let results: Vec<(i64, f32)> = response
            .results()
            .iter()
            .map(|result| (i64::from(result.index()), result.relevance_score()))
            .collect();
        scores_from_indexed(documents.len(), &results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Any configured region works; the tests use a fixed one.
    const REGION: &str = "eu-central-1";
    const MODEL_ARN: &str = "arn:aws:bedrock:eu-central-1::foundation-model/amazon.rerank-v1:0";

    #[tokio::test]
    async fn building_the_client_needs_no_credentials_and_requests_every_document() {
        // No credential lookup happens here: a machine without AWS configuration can construct it.
        let provider = BedrockRerankProvider::new(&BedrockRerankConfig {
            region: REGION.to_string(),
            model_arn: MODEL_ARN.to_string(),
            profile: None,
        })
        .await;
        let documents: Vec<String> = (0..60).map(|i| format!("invented document {i}")).collect();
        let (query, sources, configuration) = provider
            .request_parts("an invented question?", &documents)
            .unwrap();
        assert_eq!(
            query.text_query().unwrap().text(),
            Some("an invented question?")
        );
        assert_eq!(sources.len(), 60);
        assert_eq!(
            sources[7]
                .inline_document_source()
                .and_then(|document| document.text_document())
                .and_then(|text| text.text()),
            Some("invented document 7")
        );
        let bedrock = configuration.bedrock_reranking_configuration().unwrap();
        assert_eq!(bedrock.number_of_results(), Some(60));
        assert_eq!(
            bedrock.model_configuration().unwrap().model_arn(),
            MODEL_ARN
        );
    }

    #[tokio::test]
    async fn empty_and_oversized_requests_are_handled_before_any_call() {
        let provider = BedrockRerankProvider::new(&BedrockRerankConfig {
            region: REGION.to_string(),
            model_arn: MODEL_ARN.to_string(),
            profile: None,
        })
        .await;
        assert!(provider.score("q", &[]).await.unwrap().is_empty());
        let too_many: Vec<String> = (0..1001).map(|i| i.to_string()).collect();
        assert!(provider.score("q", &too_many).await.is_err());
    }
}
