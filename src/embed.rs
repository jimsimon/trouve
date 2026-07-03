//! Embedding model loading and chunk embedding (port of `semble/index/dense.py`
//! model handling, backed by the official `model2vec-rs` engine).

use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use model2vec_rs::model::StaticModel;
use rayon::prelude::*;

use crate::types::Chunk;
use crate::utils::resolve_model_name;

/// Batch size for embedding calls; chunks are embedded in parallel batches.
const EMBED_BATCH_SIZE: usize = 512;
/// Token truncation length, matching model2vec defaults.
const MAX_TOKENS: usize = 512;

/// A loaded embedding model plus the identifier it was loaded from.
pub struct EmbeddingModel {
    model: StaticModel,
    pub model_id: String,
}

static MODEL_CACHE: OnceLock<Mutex<Vec<Arc<EmbeddingModel>>>> = OnceLock::new();

impl EmbeddingModel {
    /// Load a model from the Hugging Face Hub or a local path, caching per id.
    pub fn load(model_id: Option<&str>) -> Result<Arc<EmbeddingModel>> {
        let id = model_id
            .map(|s| s.to_string())
            .unwrap_or_else(resolve_model_name);
        let cache = MODEL_CACHE.get_or_init(|| Mutex::new(Vec::new()));
        {
            let cached = cache.lock().unwrap();
            if let Some(found) = cached.iter().find(|m| m.model_id == id) {
                return Ok(found.clone());
            }
        }
        let model = StaticModel::from_pretrained(&id, None, None, None)
            .with_context(|| format!("failed to load embedding model {id:?}"))?;
        let loaded = Arc::new(EmbeddingModel {
            model,
            model_id: id,
        });
        cache.lock().unwrap().push(loaded.clone());
        Ok(loaded)
    }

    /// Embed a batch of texts.
    pub fn encode(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.model
            .encode_with_args(texts, Some(MAX_TOKENS), EMBED_BATCH_SIZE)
    }

    /// Embed a single query string.
    pub fn encode_one(&self, text: &str) -> Vec<f32> {
        self.encode(std::slice::from_ref(&text.to_string()))
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    /// Embed chunk contents in parallel batches across all cores.
    pub fn embed_chunks(&self, chunks: &[Chunk]) -> Vec<Vec<f32>> {
        let contents: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        self.embed_texts(&contents)
    }

    /// Embed arbitrary texts in parallel batches across all cores.
    pub fn embed_texts(&self, texts: &[String]) -> Vec<Vec<f32>> {
        if texts.is_empty() {
            return Vec::new();
        }
        texts
            .par_chunks(EMBED_BATCH_SIZE)
            .flat_map(|batch| self.encode(batch))
            .collect()
    }
}
