//! Local cross-encoder reranker: `BAAI/bge-reranker-v2-m3` INT8 on
//! ONNX Runtime, with its own `tokenizer.json`. Compiled only with the `local-rerank` feature.
//!
//! - The model and tokenizer are read from operator-supplied paths and verified against their
//!   SHA-256 before loading. Nothing is downloaded at startup, and memcore never bundles the files.
//! - Each (query, document) pair is encoded together, truncated to 512 tokens (longest first) and
//!   scored by the model's single relevance logit. Scoring runs in fixed-size batches on a blocking
//!   thread, with pinned intra-op threads and deterministic compute.
//! - Intended for quality characterisation, not latency-sensitive serving (tens of seconds per query on the
//!   reference laptop); it is not an online acceptance candidate.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::candidates::rerank::RerankProvider;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokenizers::{
    PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer, TruncationDirection,
    TruncationParams, TruncationStrategy,
};

/// At most 512 tokens per (query, document) pair.
pub const LOCAL_RERANK_MAX_TOKENS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRerankConfig {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    /// Expected lower-case hex SHA-256 of the model file.
    pub model_sha256: String,
    /// Expected lower-case hex SHA-256 of `tokenizer.json`.
    pub tokenizer_sha256: String,
    /// ONNX Runtime intra-op threads (pinned for determinism).
    pub intra_threads: usize,
    /// Pairs per inference batch; bounds memory.
    pub batch_size: usize,
}

/// What was loaded: recorded with every reranked run (cache key inputs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalRerankManifest {
    pub model_path: String,
    pub model_sha256: String,
    pub model_bytes: u64,
    pub tokenizer_path: String,
    pub tokenizer_sha256: String,
    pub max_tokens: usize,
    pub intra_threads: usize,
    pub batch_size: usize,
    pub inputs: Vec<String>,
}

pub struct LocalRerankProvider {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
    batch_size: usize,
    wants_token_type_ids: bool,
    manifest: LocalRerankManifest,
}

fn provider_error(message: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::ProviderError(format!("local reranker: {message}"))
}

/// Streams a file through SHA-256.
fn sha256_file(path: &Path) -> MemcoreResult<(String, u64)> {
    let mut file = File::open(path)
        .map_err(|error| provider_error(format!("cannot open {}: {error}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| provider_error(format!("cannot read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((hex::encode(hasher.finalize()), total))
}

fn verify(path: &Path, expected: &str, what: &str) -> MemcoreResult<(String, u64)> {
    let (actual, bytes) = sha256_file(path)?;
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        return Err(provider_error(format!(
            "{what} checksum mismatch for {}: expected {expected}, found {actual}",
            path.display()
        )));
    }
    Ok((actual, bytes))
}

impl LocalRerankProvider {
    /// Verifies both files' checksums, then loads the tokenizer and the ONNX session. Any mismatch
    /// or load error refuses to start the provider.
    pub fn load(config: &LocalRerankConfig) -> MemcoreResult<Self> {
        if config.intra_threads == 0 || config.batch_size == 0 {
            return Err(provider_error("threads and batch size must be at least 1"));
        }
        let (model_sha256, model_bytes) =
            verify(&config.model_path, &config.model_sha256, "model")?;
        let (tokenizer_sha256, _) = verify(
            &config.tokenizer_path,
            &config.tokenizer_sha256,
            "tokenizer",
        )?;

        let mut tokenizer = Tokenizer::from_file(&config.tokenizer_path)
            .map_err(|error| provider_error(format!("tokenizer: {error}")))?;
        let pad_id = tokenizer
            .token_to_id("<pad>")
            .ok_or_else(|| provider_error("tokenizer has no <pad> token"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: LOCAL_RERANK_MAX_TOKENS,
                strategy: TruncationStrategy::LongestFirst,
                stride: 0,
                direction: TruncationDirection::Right,
            }))
            .map_err(|error| provider_error(format!("tokenizer truncation: {error}")))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            direction: PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id,
            pad_type_id: 0,
            pad_token: "<pad>".to_string(),
        }));

        let options =
            |error: &dyn std::fmt::Display| provider_error(format!("session options: {error}"));
        let session = Session::builder()
            .map_err(|error| options(&error))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|error| options(&error))?
            .with_intra_threads(config.intra_threads)
            .map_err(|error| options(&error))?
            .with_inter_threads(1)
            .map_err(|error| options(&error))?
            .with_deterministic_compute(true)
            .map_err(|error| options(&error))?
            .commit_from_file(&config.model_path)
            .map_err(|error| provider_error(format!("model load: {error}")))?;
        let inputs: Vec<String> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();
        for required in ["input_ids", "attention_mask"] {
            if !inputs.iter().any(|name| name == required) {
                return Err(provider_error(format!("model has no `{required}` input")));
            }
        }
        let wants_token_type_ids = inputs.iter().any(|name| name == "token_type_ids");

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
            batch_size: config.batch_size,
            wants_token_type_ids,
            manifest: LocalRerankManifest {
                model_path: config.model_path.display().to_string(),
                model_sha256,
                model_bytes,
                tokenizer_path: config.tokenizer_path.display().to_string(),
                tokenizer_sha256,
                max_tokens: LOCAL_RERANK_MAX_TOKENS,
                intra_threads: config.intra_threads,
                batch_size: config.batch_size,
                inputs,
            },
        })
    }

    pub fn manifest(&self) -> &LocalRerankManifest {
        &self.manifest
    }

    /// Scores pairs synchronously (the blocking work behind [`RerankProvider::score`]).
    pub fn score_blocking(&self, query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
        score_batches(
            &self.session,
            &self.tokenizer,
            self.batch_size,
            self.wants_token_type_ids,
            query,
            documents,
        )
    }
}

fn score_batches(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    batch_size: usize,
    wants_token_type_ids: bool,
    query: &str,
    documents: &[String],
) -> MemcoreResult<Vec<f32>> {
    let mut scores = Vec::with_capacity(documents.len());
    for chunk in documents.chunks(batch_size) {
        let pairs: Vec<(String, String)> = chunk
            .iter()
            .map(|document| (query.to_string(), document.clone()))
            .collect();
        let encodings = tokenizer
            .encode_batch(pairs, true)
            .map_err(|error| provider_error(format!("tokenize: {error}")))?;
        let rows = encodings.len();
        let width = encodings.first().map_or(0, |encoding| encoding.len());
        if encodings.iter().any(|encoding| encoding.len() != width) {
            return Err(provider_error("batch was not padded to one length"));
        }
        let flat = |pick: &dyn Fn(&tokenizers::Encoding) -> &[u32]| -> Vec<i64> {
            encodings
                .iter()
                .flat_map(|encoding| pick(encoding).iter().map(|&value| i64::from(value)))
                .collect()
        };
        let shape = [rows as i64, width as i64];
        let ids = Tensor::from_array((shape, flat(&|e| e.get_ids())))
            .map_err(|error| provider_error(format!("input_ids: {error}")))?;
        let mask = Tensor::from_array((shape, flat(&|e| e.get_attention_mask())))
            .map_err(|error| provider_error(format!("attention_mask: {error}")))?;
        let mut session = session
            .lock()
            .map_err(|_| provider_error("session lock poisoned"))?;
        let outputs = if wants_token_type_ids {
            let types = Tensor::from_array((shape, flat(&|e| e.get_type_ids())))
                .map_err(|error| provider_error(format!("token_type_ids: {error}")))?;
            session.run(ort::inputs![
                "input_ids" => ids,
                "attention_mask" => mask,
                "token_type_ids" => types,
            ])
        } else {
            session.run(ort::inputs!["input_ids" => ids, "attention_mask" => mask])
        }
        .map_err(|error| provider_error(format!("inference: {error}")))?;
        let (out_shape, logits) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| provider_error(format!("output: {error}")))?;
        if logits.len() != rows {
            return Err(provider_error(format!(
                "expected {rows} logits, got {} (shape {out_shape:?})",
                logits.len()
            )));
        }
        scores.extend_from_slice(logits);
    }
    Ok(scores)
}

#[async_trait]
impl RerankProvider for LocalRerankProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn score(&self, query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let batch_size = self.batch_size;
        let wants_token_type_ids = self.wants_token_type_ids;
        let query = query.to_string();
        let documents = documents.to_vec();
        tokio::task::spawn_blocking(move || {
            score_batches(
                &session,
                &tokenizer,
                batch_size,
                wants_token_type_ids,
                &query,
                &documents,
            )
        })
        .await
        .map_err(|error| provider_error(format!("scoring task failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> LocalRerankConfig {
        LocalRerankConfig {
            model_path: dir.join("model.onnx"),
            tokenizer_path: dir.join("tokenizer.json"),
            model_sha256: "0".repeat(64),
            tokenizer_sha256: "0".repeat(64),
            intra_threads: 1,
            batch_size: 4,
        }
    }

    #[test]
    fn missing_files_refuse_to_load() {
        let dir = std::env::temp_dir().join("memcore-local-rerank-missing");
        let error = LocalRerankProvider::load(&config(&dir)).err().unwrap();
        assert!(error.to_string().contains("cannot open"), "{error}");
    }

    #[test]
    fn checksum_mismatch_refuses_to_load() {
        let dir = std::env::temp_dir().join(format!("memcore-local-rerank-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"not a model").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        let error = LocalRerankProvider::load(&config(&dir)).err().unwrap();
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zero_threads_or_batch_is_refused() {
        let dir = std::env::temp_dir();
        let mut bad = config(&dir);
        bad.batch_size = 0;
        assert!(LocalRerankProvider::load(&bad).is_err());
    }

    /// Real-model check, run manually with the pinned artifacts:
    /// `MEMCORE_TEST_RERANK_MODEL=<model_int8.onnx> MEMCORE_TEST_RERANK_TOKENIZER=<tokenizer.json>
    /// cargo test -p memcore-providers --features local-rerank -- --ignored local_model`.
    #[test]
    #[ignore = "needs the local bge-reranker-v2-m3 INT8 files"]
    fn local_model_ranks_the_relevant_invented_document_first_and_is_deterministic() {
        let model = PathBuf::from(std::env::var("MEMCORE_TEST_RERANK_MODEL").unwrap());
        let tokenizer = PathBuf::from(std::env::var("MEMCORE_TEST_RERANK_TOKENIZER").unwrap());
        let provider = LocalRerankProvider::load(&LocalRerankConfig {
            model_sha256: sha256_file(&model).unwrap().0,
            tokenizer_sha256: sha256_file(&tokenizer).unwrap().0,
            model_path: model,
            tokenizer_path: tokenizer,
            intra_threads: 4,
            batch_size: 2,
        })
        .unwrap();
        let documents = vec![
            "A ferry schedule lists three crossings every Sunday afternoon.".to_string(),
            "The retired lighthouse keeper spent the winter learning to play the cello."
                .to_string(),
            "Her neighbour repaired an old bicycle with a borrowed wrench.".to_string(),
        ];
        let query = "Which instrument did the retired lighthouse keeper learn to play?";
        let first = provider.score_blocking(query, &documents).unwrap();
        let second = provider.score_blocking(query, &documents).unwrap();
        assert_eq!(first, second, "deterministic");
        assert!(first[1] > first[0] && first[1] > first[2], "{first:?}");
        println!("scores {first:?}");
    }
}
