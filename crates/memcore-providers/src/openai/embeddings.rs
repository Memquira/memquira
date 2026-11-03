use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};

use crate::traits::EmbeddingProvider;
use memcore_core::ports::EmbeddingIdentity;

use super::client::OpenAiClient;
use super::types::{EmbeddingsCreateRequest, EmbeddingsCreateResponse};

#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingProvider {
    client: OpenAiClient,
    model: String,
    dimensions: usize,
    options: EmbeddingOptions,
}

/// Per-deployment embedding options. The defaults reproduce the reference request
/// bodies exactly. Everything that changes vectors is part of the embedding identity.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingOptions {
    /// Send the `dimensions` request field (some OpenAI-compatible servers reject it).
    pub send_dimensions: bool,
    /// L2-normalise vectors in memcore (for endpoints that do not).
    pub normalize: bool,
    /// Prepended to search queries (`embed_query`).
    pub query_prefix: Option<String>,
    /// Prepended to stored text (`embed_text` / `embed_batch`).
    pub document_prefix: Option<String>,
    pub revision: Option<String>,
    pub serving_variant: Option<String>,
    /// The store's embedding-dedup policy label (D-L3), recorded in the identity.
    pub dedup_policy: Option<String>,
}

impl Default for EmbeddingOptions {
    fn default() -> Self {
        Self {
            send_dimensions: true,
            normalize: false,
            query_prefix: None,
            document_prefix: None,
            revision: None,
            serving_variant: None,
            dedup_policy: None,
        }
    }
}

/// Default output dimensions for common OpenAI embedding models (used at API bootstrap).
pub fn default_embedding_dimensions_for_model(model: &str) -> usize {
    match model.trim().to_ascii_lowercase().as_str() {
        "text-embedding-3-large" => 3072,
        "text-embedding-3-small" | "text-embedding-ada-002" => 1536,
        // Google AI Studio / Gemini OpenAI-compat default output size.
        "gemini-embedding-001" | "gemini-embedding-2" | "gemini-embedding-2-preview" => 3072,
        _ => 1536,
    }
}

impl OpenAiEmbeddingProvider {
    pub fn new(
        client: OpenAiClient,
        model: impl Into<String>,
        dimensions: usize,
    ) -> MemcoreResult<Self> {
        if dimensions == 0 {
            return Err(MemcoreError::ValidationError(
                "embedding dimensions must be greater than 0".to_string(),
            ));
        }

        Ok(Self {
            client,
            model: model.into(),
            dimensions,
            options: EmbeddingOptions::default(),
        })
    }

    /// Applies deployment options.
    pub fn with_options(mut self, options: EmbeddingOptions) -> Self {
        self.options = options;
        self
    }

    async fn embed_prefixed(
        &self,
        texts: Vec<String>,
        prefix: Option<&str>,
    ) -> MemcoreResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        for text in &texts {
            validate_non_empty_text(text)?;
        }
        let input = match prefix {
            Some(prefix) => texts.into_iter().map(|t| format!("{prefix}{t}")).collect(),
            None => texts,
        };
        let request = EmbeddingsCreateRequest {
            model: self.model.clone(),
            input,
            dimensions: self.options.send_dimensions.then_some(self.dimensions),
        };

        let response: EmbeddingsCreateResponse = self.client.create_embeddings(&request).await?;

        if response.data.is_empty() {
            return Err(MemcoreError::ProviderError(
                "OpenAI embeddings response contained no data".to_string(),
            ));
        }

        let mut ordered = vec![Vec::new(); response.data.len()];
        for item in response.data {
            validate_embedding_dimensions(&item.embedding, self.dimensions)?;
            validate_finite(&item.embedding)?;
            if item.index >= ordered.len() {
                return Err(MemcoreError::ProviderError(
                    "OpenAI embeddings response index out of range".to_string(),
                ));
            }
            let mut vector = item.embedding;
            if self.options.normalize {
                l2_normalize(&mut vector)?;
            }
            ordered[item.index] = vector;
        }

        if ordered.iter().any(Vec::is_empty) {
            return Err(MemcoreError::ProviderError(
                "OpenAI embeddings response missing one or more indices".to_string(),
            ));
        }

        Ok(ordered)
    }
}

fn validate_finite(embedding: &[f32]) -> MemcoreResult<()> {
    if embedding.iter().any(|value| !value.is_finite()) {
        return Err(MemcoreError::ProviderError(
            "embedding provider returned a non-finite value".to_string(),
        ));
    }
    Ok(())
}

fn l2_normalize(vector: &mut [f32]) -> MemcoreResult<()> {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return Err(MemcoreError::ProviderError(
            "embedding provider returned a zero vector; it cannot be normalised".to_string(),
        ));
    }
    vector.iter_mut().for_each(|v| *v /= norm);
    Ok(())
}

fn validate_non_empty_text(text: &str) -> MemcoreResult<()> {
    if text.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "embedding text cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_embedding_dimensions(embedding: &[f32], expected: usize) -> MemcoreResult<()> {
    if embedding.len() != expected {
        return Err(MemcoreError::ProviderError(format!(
            "OpenAI embedding dimension mismatch: expected {expected}, got {}",
            embedding.len()
        )));
    }
    Ok(())
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let mut batch = self.embed_batch(vec![text.to_string()]).await?;
        batch
            .pop()
            .ok_or_else(|| MemcoreError::ProviderError("OpenAI returned no embedding".to_string()))
    }

    fn embedding_identity(&self) -> Option<EmbeddingIdentity> {
        use sha2::{Digest, Sha256};
        let mut identity =
            EmbeddingIdentity::remote("openai_compatible", &self.model, self.dimensions);
        let o = &self.options;
        identity.revision = o.revision.clone();
        identity.normalisation = o.normalize.then(|| "l2_by_memcore".to_string());
        identity.query_instruction_sha256 = o
            .query_prefix
            .as_ref()
            .map(|p| hex::encode(Sha256::digest(p.as_bytes())));
        identity.document_prefix = o.document_prefix.clone();
        identity.serving_variant = o.serving_variant.clone();
        identity.dedup_policy = o.dedup_policy.clone();
        Some(identity)
    }

    async fn embed_query(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let prefix = self.options.query_prefix.as_deref();
        let mut batch = self.embed_prefixed(vec![text.to_string()], prefix).await?;
        batch
            .pop()
            .ok_or_else(|| MemcoreError::ProviderError("OpenAI returned no embedding".to_string()))
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let prefix = self.options.document_prefix.clone();
        self.embed_prefixed(texts, prefix.as_deref()).await
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::EmbeddingProvider;

    #[test]
    fn default_dimensions_for_known_models() {
        assert_eq!(
            default_embedding_dimensions_for_model("text-embedding-3-small"),
            1536
        );
        assert_eq!(
            default_embedding_dimensions_for_model("text-embedding-3-large"),
            3072
        );
        assert_eq!(
            default_embedding_dimensions_for_model("unknown-model"),
            1536
        );
        assert_eq!(
            default_embedding_dimensions_for_model("gemini-embedding-001"),
            3072
        );
    }

    #[test]
    fn zero_dimensions_rejected_at_construction() {
        let client = OpenAiClient::new("key", "https://api.openai.com/v1").expect("client");
        let error =
            OpenAiEmbeddingProvider::new(client, "text-embedding-3-small", 0).expect_err("fail");
        assert_eq!(
            error,
            MemcoreError::ValidationError(
                "embedding dimensions must be greater than 0".to_string()
            )
        );
    }

    #[tokio::test]
    async fn empty_text_returns_validation_error() {
        let client = OpenAiClient::new("key", "https://api.openai.com/v1").expect("client");
        let provider =
            OpenAiEmbeddingProvider::new(client, "text-embedding-3-small", 4).expect("provider");
        let error = provider.embed_text("   ").await.expect_err("should fail");
        assert_eq!(
            error,
            MemcoreError::ValidationError("embedding text cannot be empty".to_string())
        );
    }

    #[test]
    fn parses_embedding_response_and_validates_dimensions() {
        let payload = serde_json::json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3, 0.4], "index": 0 }
            ]
        });
        let response: EmbeddingsCreateResponse =
            serde_json::from_value(payload).expect("deserialize");
        validate_embedding_dimensions(&response.data[0].embedding, 4).expect("valid");
        let error = validate_embedding_dimensions(&response.data[0].embedding, 8).expect_err("bad");
        assert!(matches!(error, MemcoreError::ProviderError(_)));
    }

    #[test]
    fn parses_embedding_response_when_index_omitted() {
        let payload = serde_json::json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3, 0.4] }
            ]
        });
        let response: EmbeddingsCreateResponse =
            serde_json::from_value(payload).expect("deserialize without index");
        assert_eq!(response.data[0].index, 0);
        assert_eq!(response.data[0].embedding.len(), 4);
    }
}
