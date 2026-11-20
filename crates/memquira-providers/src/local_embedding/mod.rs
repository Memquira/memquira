//! Local embedding models: model-independent infrastructure. Model providers are
//! compiled only with their own feature.

pub mod queue;

pub use queue::{BatchEncoder, DEFAULT_QUEUE_CAPACITY, InferenceQueue, QUEUE_SHUT_DOWN};
