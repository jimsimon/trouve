//! Embedding model loading and chunk embedding.
//!
//! This is a self-contained model2vec inference engine (replacing the
//! `model2vec-rs` dependency) tuned for indexing throughput:
//!
//! - the embedding table is memory-mapped from `model.safetensors` instead of
//!   copied, so model "load" is nearly free;
//! - for the standard Bert pipeline (`BertNormalizer` + `BertPreTokenizer` +
//!   `WordPiece`) and pure-ASCII text — i.e. virtually all source code — a
//!   byte-level scanner replaces the HF normalizer/pre-tokenizer machinery,
//!   and WordPiece results are memoised per word in a sharded cache (code is
//!   extremely repetitive, so the hit rate is very high);
//! - token ids are mean-pooled straight out of the mapped table without
//!   intermediate allocations.
//!
//! Texts that are not pure ASCII (or that contain an added token like
//! `[UNK]`) go through the exact HF `tokenizers` pipeline, so output always
//! matches `model2vec` semantics for a batch of one. Unlike upstream
//! model2vec we never pad, which makes embeddings independent of how texts
//! are batched (upstream pooling absorbs `[PAD]` rows, so its output varies
//! with batch composition).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use anyhow::{anyhow, Context, Result};
use memmap2::Mmap;
use rayon::prelude::*;
use tokenizers::Tokenizer;

use crate::snapshot::Buf;
use crate::types::Chunk;
use crate::utils::resolve_model_name;

/// Token truncation length, matching model2vec defaults.
const MAX_TOKENS: usize = 512;
/// Number of shards for the word -> token-ids cache.
const CACHE_SHARDS: usize = 128;

/// A loaded embedding model plus the identifier it was loaded from.
pub struct EmbeddingModel {
    pub model_id: String,
    /// Full HF pipeline; the exactness fallback for non-ASCII input and the
    /// only path for non-Bert tokenizers (e.g. WordLevel test models).
    tokenizer: Tokenizer,
    /// Added-token strings (e.g. `[UNK]`, `[PAD]`); texts containing one are
    /// routed through the HF pipeline since it extracts them pre-splitting.
    added_tokens: Vec<String>,
    fast: Option<FastBert>,
    /// Row-major `rows x dim` embedding table (mmap-backed when possible).
    embeddings: Buf<f32>,
    dim: usize,
    /// Per-token-id row remap for vocabulary-quantized models.
    mapping: Option<Vec<u32>>,
    /// Per-token-id pooling weights for quantized models.
    weights: Option<Vec<f32>>,
    normalize: bool,
    median_token_length: usize,
    unk_token_id: Option<u32>,
    /// `truncation.max_length` from tokenizer.json (applied pre-unk-filter).
    tokenizer_truncation: Option<usize>,
}

/// One shard of the word -> token-ids memo.
type CacheShard = RwLock<HashMap<Box<[u8]>, Box<[u32]>, ahash::RandomState>>;

/// Byte-level reimplementation of BertNormalizer + BertPreTokenizer +
/// WordPiece, valid for pure-ASCII input, with a global word cache.
struct FastBert {
    /// piece -> id for word-initial pieces.
    head: HashMap<Box<[u8]>, u32, ahash::RandomState>,
    /// piece (continuation prefix stripped) -> id for word-internal pieces.
    cont: HashMap<Box<[u8]>, u32, ahash::RandomState>,
    unk_id: u32,
    max_input_chars: usize,
    lowercase: bool,
    hasher: ahash::RandomState,
    cache: Vec<CacheShard>,
}

impl FastBert {
    /// Tokenize one ASCII word (already normalized) through the cache.
    fn word_ids(&self, word: &[u8], out: &mut Vec<u32>) {
        if word.len() > self.max_input_chars {
            out.push(self.unk_id);
            return;
        }
        let shard = &self.cache[(self.hasher.hash_one(word) as usize) % CACHE_SHARDS];
        if let Some(ids) = shard.read().unwrap().get(word) {
            out.extend_from_slice(ids);
            return;
        }
        let mut ids: Vec<u32> = Vec::new();
        let mut start = 0usize;
        'outer: while start < word.len() {
            let mut end = word.len();
            while start < end {
                let vocab = if start == 0 { &self.head } else { &self.cont };
                if let Some(&id) = vocab.get(&word[start..end]) {
                    ids.push(id);
                    start = end;
                    continue 'outer;
                }
                end -= 1;
            }
            // No piece matched: the whole word becomes [UNK].
            ids.clear();
            ids.push(self.unk_id);
            break;
        }
        out.extend_from_slice(&ids);
        shard
            .write()
            .unwrap()
            .insert(word.into(), ids.into_boxed_slice());
    }

    /// Normalize + pre-tokenize + WordPiece an ASCII text into raw token ids
    /// (unk included). Stops early once `limit` ids are produced.
    fn tokenize_ascii(&self, text: &str, limit: usize, out: &mut Vec<u32>) {
        debug_assert!(text.is_ascii());
        let mut word: Vec<u8> = Vec::with_capacity(32);
        for &b in text.as_bytes() {
            if out.len() >= limit {
                return;
            }
            match b {
                // Word boundaries: ' ', \t, \n, \r (whitespace after cleaning).
                b' ' | b'\t' | b'\n' | b'\r' => {
                    if !word.is_empty() {
                        self.word_ids(&word, out);
                        word.clear();
                    }
                }
                // clean_text deletes NUL/control chars, joining neighbours.
                0x00..=0x1f | 0x7f => {}
                // ASCII punctuation is isolated as a single-char word.
                b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~' => {
                    if !word.is_empty() {
                        self.word_ids(&word, out);
                        word.clear();
                    }
                    self.word_ids(&[b], out);
                }
                _ => {
                    let c = if self.lowercase {
                        b.to_ascii_lowercase()
                    } else {
                        b
                    };
                    word.push(c);
                }
            }
        }
        if !word.is_empty() && out.len() < limit {
            self.word_ids(&word, out);
        }
    }
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
        let model = Self::from_files(&resolve_model_files(&id)?, id.clone())
            .with_context(|| format!("failed to load embedding model {id:?}"))?;
        let loaded = Arc::new(model);
        cache.lock().unwrap().push(loaded.clone());
        Ok(loaded)
    }

    fn from_files(files: &ModelFiles, model_id: String) -> Result<EmbeddingModel> {
        let tokenizer_bytes =
            std::fs::read(&files.tokenizer).context("failed to read tokenizer.json")?;
        let tokenizer = Tokenizer::from_bytes(&tokenizer_bytes)
            .map_err(|e| anyhow!("failed to load tokenizer: {e}"))?;
        let spec: serde_json::Value =
            serde_json::from_slice(&tokenizer_bytes).context("failed to parse tokenizer.json")?;

        let cfg: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&files.config).context("failed to read config.json")?,
        )
        .context("failed to parse config.json")?;
        let normalize = cfg
            .get("normalize")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);

        let file = std::fs::File::open(&files.model).context("failed to open model.safetensors")?;
        // Safety: the mmap'd model file is assumed not to be truncated
        // concurrently; same contract as the snapshot mmaps.
        let map = Arc::new(unsafe { Mmap::map(&file) }.context("failed to mmap model")?);
        let safet =
            safetensors::SafeTensors::deserialize(&map).context("failed to parse safetensors")?;

        let tensor = safet
            .tensor("embeddings")
            .or_else(|_| safet.tensor("0"))
            .or_else(|_| safet.tensor("embedding.weight"))
            .context("embeddings tensor not found")?;
        let [rows, dim]: [usize; 2] = tensor
            .shape()
            .try_into()
            .ok()
            .context("embedding tensor is not 2-D")?;
        let embeddings = embedding_buf(&map, &tensor, rows * dim)?;

        let weights = match safet.tensor("weights") {
            Ok(t) => Some(decode_f32s(&t)?),
            Err(_) => None,
        };
        let mapping = match safet.tensor("mapping") {
            Ok(t) => Some(decode_mapping(&t)?),
            Err(_) => None,
        };

        // Median token length over the model vocab, used for pre-truncation
        // (same computation as model2vec's compute_metadata).
        let vocab_obj = spec
            .pointer("/model/vocab")
            .and_then(serde_json::Value::as_object);
        let mut lens: Vec<usize> = vocab_obj
            .map(|v| v.keys().map(|k| k.len()).collect())
            .unwrap_or_default();
        lens.sort_unstable();
        let median_token_length = lens.get(lens.len() / 2).copied().unwrap_or(1);

        let unk_token_id = spec
            .pointer("/model/unk_token")
            .and_then(serde_json::Value::as_str)
            .and_then(|tok| tokenizer.token_to_id(tok));

        let tokenizer_truncation = spec
            .pointer("/truncation/max_length")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);

        let added_tokens = spec
            .get("added_tokens")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.get("content").and_then(serde_json::Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let fast = build_fast_bert(&spec, unk_token_id);

        Ok(EmbeddingModel {
            model_id,
            tokenizer,
            added_tokens,
            fast,
            embeddings,
            dim,
            mapping,
            weights,
            normalize,
            median_token_length,
            unk_token_id,
            tokenizer_truncation,
        })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Char-level pre-truncation to `max_tokens * median_token_length`,
    /// identical to model2vec's `truncate_str`.
    fn truncate_str<'a>(&self, s: &'a str) -> &'a str {
        s.char_indices()
            .nth(MAX_TOKENS.saturating_mul(self.median_token_length))
            .map_or(s, |(byte_idx, _)| &s[..byte_idx])
    }

    /// Tokenize one text into final token ids: tokenizer-level truncation,
    /// then unk removal, then truncation to `MAX_TOKENS` (model2vec order).
    fn token_ids(&self, text: &str) -> Vec<u32> {
        let text = self.truncate_str(text);
        let mut ids: Vec<u32> = Vec::new();

        let fast = self.fast.as_ref().filter(|_| {
            text.is_ascii()
                && !self
                    .added_tokens
                    .iter()
                    .any(|tok| text.contains(tok.as_str()))
        });
        if let Some(fast) = fast {
            // Early-stop is only safe at the tokenizer's own truncation
            // boundary (it applies before unk removal).
            let limit = self.tokenizer_truncation.unwrap_or(usize::MAX);
            fast.tokenize_ascii(text, limit, &mut ids);
            if let Some(max) = self.tokenizer_truncation {
                ids.truncate(max);
            }
        } else {
            let encoding = self
                .tokenizer
                .encode_fast(text, false)
                .expect("tokenization failed");
            ids.extend_from_slice(encoding.get_ids());
        }

        if let Some(unk) = self.unk_token_id {
            ids.retain(|&id| id != unk);
        }
        ids.truncate(MAX_TOKENS);
        ids
    }

    /// Mean-pool token ids into `out` (must be `dim` long), applying the
    /// quantization mapping/weights and optional L2 normalization exactly
    /// like model2vec's `pool_ids`.
    fn pool_into(&self, ids: &[u32], out: &mut [f32]) {
        out.fill(0.0);
        let table: &[f32] = &self.embeddings;
        for &id in ids {
            let tok = id as usize;
            let row_idx = self
                .mapping
                .as_ref()
                .and_then(|m| m.get(tok))
                .map(|&r| r as usize)
                .unwrap_or(tok);
            let scale = self
                .weights
                .as_ref()
                .and_then(|w| w.get(tok))
                .copied()
                .unwrap_or(1.0);
            let row = &table[row_idx * self.dim..(row_idx + 1) * self.dim];
            for (s, &v) in out.iter_mut().zip(row) {
                *s += v * scale;
            }
        }
        let denom = ids.len().max(1) as f32;
        for x in out.iter_mut() {
            *x /= denom;
        }
        if self.normalize {
            let norm = out.iter().map(|&v| v * v).sum::<f32>().sqrt().max(1e-12);
            for x in out.iter_mut() {
                *x /= norm;
            }
        }
    }

    /// Embed a single text.
    pub fn encode_one(&self, text: &str) -> Vec<f32> {
        let ids = self.token_ids(text);
        let mut out = vec![0.0f32; self.dim];
        self.pool_into(&ids, &mut out);
        out
    }

    /// Embed a batch of texts sequentially.
    pub fn encode(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.encode_one(t)).collect()
    }

    /// Embed chunk contents in parallel across all cores.
    pub fn embed_chunks(&self, chunks: &[Chunk]) -> Vec<Vec<f32>> {
        let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        self.embed_refs(&contents)
    }

    /// Embed arbitrary texts in parallel across all cores.
    pub fn embed_texts(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.embed_refs(&refs)
    }

    /// Embed borrowed texts in parallel across all cores.
    pub fn embed_refs(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.par_iter().map(|t| self.encode_one(t)).collect()
    }

    /// Embed borrowed texts in parallel into one flat row-major buffer,
    /// avoiding one small allocation per text.
    pub fn embed_refs_flat(&self, texts: &[&str]) -> Vec<f32> {
        let mut out = vec![0.0f32; texts.len() * self.dim];
        out.par_chunks_mut(self.dim)
            .zip(texts.par_iter())
            .for_each(|(row, t)| {
                let ids = self.token_ids(t);
                self.pool_into(&ids, row);
            });
        out
    }
}

/// Build the fast ASCII pipeline if the tokenizer is the standard Bert stack.
fn build_fast_bert(spec: &serde_json::Value, unk_token_id: Option<u32>) -> Option<FastBert> {
    let norm = spec.get("normalizer")?;
    if norm.get("type")?.as_str()? != "BertNormalizer" {
        return None;
    }
    // clean_text=false would leave control chars in words; not worth a
    // second code path since every published model2vec tokenizer sets it.
    if !norm
        .get("clean_text")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
    {
        return None;
    }
    let lowercase = norm
        .get("lowercase")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if spec.pointer("/pre_tokenizer/type")?.as_str()? != "BertPreTokenizer" {
        return None;
    }
    if spec.pointer("/model/type")?.as_str()? != "WordPiece" {
        return None;
    }
    let unk_id = unk_token_id?;
    let prefix = spec
        .pointer("/model/continuing_subword_prefix")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("##")
        .to_string();
    let max_input_chars = spec
        .pointer("/model/max_input_chars_per_word")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(100) as usize;
    let vocab = spec.pointer("/model/vocab")?.as_object()?;

    let mut head: HashMap<Box<[u8]>, u32, ahash::RandomState> = HashMap::default();
    let mut cont: HashMap<Box<[u8]>, u32, ahash::RandomState> = HashMap::default();
    for (piece, id) in vocab {
        let id = id.as_u64()? as u32;
        match piece.strip_prefix(&prefix) {
            Some(rest) => {
                cont.insert(rest.as_bytes().into(), id);
            }
            None => {
                head.insert(piece.as_bytes().into(), id);
            }
        }
    }

    Some(FastBert {
        head,
        cont,
        unk_id,
        max_input_chars,
        lowercase,
        hasher: ahash::RandomState::new(),
        cache: (0..CACHE_SHARDS)
            .map(|_| RwLock::new(HashMap::default()))
            .collect(),
    })
}

/// View the F32 embedding tensor zero-copy from the mmap when aligned;
/// otherwise (or for F16/I8 models) decode into owned memory.
fn embedding_buf(
    map: &Arc<Mmap>,
    tensor: &safetensors::tensor::TensorView<'_>,
    len: usize,
) -> Result<Buf<f32>> {
    let data = tensor.data();
    if tensor.dtype() == safetensors::tensor::Dtype::F32 {
        let offset = data.as_ptr() as usize - map.as_ptr() as usize;
        if offset.is_multiple_of(std::mem::align_of::<f32>()) {
            return Ok(Buf::mapped(map, offset, len));
        }
    }
    Ok(Buf::Owned(decode_f32s(tensor)?))
}

/// Decode a tensor of F32/F64/F16/I8 values into f32s (model2vec dtypes).
fn decode_f32s(tensor: &safetensors::tensor::TensorView<'_>) -> Result<Vec<f32>> {
    use safetensors::tensor::Dtype;
    let raw = tensor.data();
    Ok(match tensor.dtype() {
        Dtype::F32 => raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect(),
        Dtype::F64 => raw
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes(b.try_into().unwrap()) as f32)
            .collect(),
        Dtype::F16 => raw
            .chunks_exact(2)
            .map(|b| half::f16::from_le_bytes(b.try_into().unwrap()).to_f32())
            .collect(),
        Dtype::I8 => raw.iter().map(|&b| f32::from(b as i8)).collect(),
        other => return Err(anyhow!("unsupported tensor dtype: {other:?}")),
    })
}

/// Decode the vocabulary-quantization row mapping (I64 or I32).
fn decode_mapping(tensor: &safetensors::tensor::TensorView<'_>) -> Result<Vec<u32>> {
    use safetensors::tensor::Dtype;
    let raw = tensor.data();
    Ok(match tensor.dtype() {
        Dtype::I64 => raw
            .chunks_exact(8)
            .map(|b| i64::from_le_bytes(b.try_into().unwrap()) as u32)
            .collect(),
        Dtype::I32 => raw
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()) as u32)
            .collect(),
        other => return Err(anyhow!("unsupported mapping dtype: {other:?}")),
    })
}

struct ModelFiles {
    tokenizer: PathBuf,
    model: PathBuf,
    config: PathBuf,
}

fn match_local_layout(
    config_base: &Path,
    model_base: &Path,
    config_file: &str,
) -> Option<ModelFiles> {
    let config = config_base.join(config_file);
    let tokenizer = model_base.join("tokenizer.json");
    let model = model_base.join("model.safetensors");
    (config.exists() && tokenizer.exists() && model.exists()).then_some(ModelFiles {
        tokenizer,
        model,
        config,
    })
}

/// Resolve model files from a local folder or the Hugging Face Hub, trying
/// the same layouts as model2vec (plain and sentence-transformers).
fn resolve_model_files(id: &str) -> Result<ModelFiles> {
    let base = Path::new(id);
    if base.exists() {
        return match_local_layout(base, base, "config.json")
            .or_else(|| match_local_layout(base, base, "config_sentence_transformers.json"))
            .or_else(|| {
                match_local_layout(
                    base,
                    &base.join("0_StaticEmbedding"),
                    "config_sentence_transformers.json",
                )
            })
            .ok_or_else(|| anyhow!("no valid model layout found in {base:?}"));
    }

    let api = hf_hub::api::sync::Api::new().context("hf-hub API init failed")?;
    let repo = api.model(id.to_string());
    let fetch = |name: &str| repo.get(name);
    let config = fetch("config.json")
        .or_else(|_| fetch("config_sentence_transformers.json"))
        .with_context(|| format!("could not load '{id}' from HuggingFace Hub"))?;
    let tokenizer = fetch("tokenizer.json").context("tokenizer.json not found")?;
    let model = fetch("model.safetensors").context("model.safetensors not found")?;
    Ok(ModelFiles {
        tokenizer,
        model,
        config,
    })
}
