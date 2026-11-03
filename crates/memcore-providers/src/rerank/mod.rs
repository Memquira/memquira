//! Rerank providers. Each is optional and compiled only with its feature;
//! memcore never needs a reranker, and the default build contains neither.

#[cfg(feature = "bedrock-rerank")]
pub mod bedrock;
#[cfg(feature = "local-rerank")]
pub mod local;
