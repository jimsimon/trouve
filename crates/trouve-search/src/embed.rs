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
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
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
/// Environment override for the model-repair lock wait, in positive seconds.
const HUB_MODEL_LOCK_TIMEOUT_ENV: &str = "TROUVE_HUB_MODEL_LOCK_TIMEOUT_SECS";
/// Default time a loader waits for another process's model-repair lock.
const DEFAULT_HUB_MODEL_LOCK_WAIT: Duration = Duration::from_secs(300);
/// Poll interval while waiting for a model repository's repair lock.
const HUB_MODEL_LOCK_RETRY: Duration = Duration::from_millis(50);

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
static MODEL_LOAD_LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();

fn lock_ignore_poison<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn model_load_lock(id: &str) -> Arc<Mutex<()>> {
    let mut locks = lock_ignore_poison(MODEL_LOAD_LOCKS.get_or_init(|| Mutex::new(HashMap::new())));
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        return lock;
    }

    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(id.to_string(), Arc::downgrade(&lock));
    lock
}

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

        // Model validation may invalidate and redownload Hub snapshot
        // pointers. Serialize that whole load/refresh sequence per model id,
        // while allowing unrelated models to load concurrently.
        let load_lock = model_load_lock(&id);
        let _load_guard = lock_ignore_poison(&load_lock);
        {
            let cached = cache.lock().unwrap();
            if let Some(found) = cached.iter().find(|m| m.model_id == id) {
                return Ok(found.clone());
            }
        }

        let model = load_model_with_cache_coordination(&id)
            .with_context(|| format!("failed to load embedding model {id:?}"))?;
        let loaded = Arc::new(model);
        cache.lock().unwrap().push(loaded.clone());
        Ok(loaded)
    }

    fn from_files(files: &ModelFiles, model_id: String) -> Result<EmbeddingModel> {
        let tokenizer_bytes =
            std::fs::read(&files.tokenizer).context("failed to read tokenizer.json")?;
        let tokenizer = Tokenizer::from_bytes(&tokenizer_bytes).map_err(|error| {
            invalid_model_artifact(format!("failed to load tokenizer: {error}"))
        })?;
        let spec: serde_json::Value =
            serde_json::from_slice(&tokenizer_bytes).map_err(|error| {
                invalid_model_artifact(format!("failed to parse tokenizer.json: {error}"))
            })?;

        let config_bytes = std::fs::read(&files.config).context("failed to read config.json")?;
        let cfg: serde_json::Value = serde_json::from_slice(&config_bytes).map_err(|error| {
            invalid_model_artifact(format!("failed to parse config.json: {error}"))
        })?;
        let normalize = cfg
            .get("normalize")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);

        let file = std::fs::File::open(&files.model).context("failed to open model.safetensors")?;
        if files.origin == ModelOrigin::Hub
            && file
                .metadata()
                .context("failed to inspect model.safetensors")?
                .len()
                == 0
        {
            return Err(invalid_model_artifact("cached model.safetensors is empty"));
        }
        // Safety: the mmap'd model file is assumed not to be truncated
        // concurrently; same contract as the snapshot mmaps.
        let map = Arc::new(unsafe { Mmap::map(&file) }.context("failed to mmap model")?);
        let safet = safetensors::SafeTensors::deserialize(&map).map_err(|error| {
            invalid_model_artifact(format!("failed to parse safetensors: {error}"))
        })?;

        let tensor = safet
            .tensor("embeddings")
            .or_else(|_| safet.tensor("0"))
            .or_else(|_| safet.tensor("embedding.weight"))
            .map_err(|error| {
                invalid_model_artifact(format!("embeddings tensor not found: {error}"))
            })?;
        let [rows, dim]: [usize; 2] = tensor
            .shape()
            .try_into()
            .ok()
            .ok_or_else(|| invalid_model_artifact("embedding tensor is not 2-D"))?;
        let embeddings = embedding_buf(&map, &tensor, rows * dim)
            .map_err(|error| invalid_model_artifact(format!("{error:#}")))?;

        let weights = match safet.tensor("weights") {
            Ok(t) => Some(
                decode_f32s(&t).map_err(|error| invalid_model_artifact(format!("{error:#}")))?,
            ),
            Err(_) => None,
        };
        let mapping = match safet.tensor("mapping") {
            Ok(t) => Some(
                decode_mapping(&t, rows)
                    .map_err(|error| invalid_model_artifact(format!("{error:#}")))?,
            ),
            Err(_) => None,
        };

        // Every token id the tokenizer can emit must resolve to an embedding
        // row and, when present, a weight. Validating here keeps the pooling
        // hot path free of bounds checks and turns a corrupt or mismatched
        // model file into a load error instead of a panic or silent fallback
        // mid-index. Ids are validated against the highest assigned id, not
        // the vocabulary *count*: token ids may have gaps, so the id space can
        // be larger than the count.
        let id_space = tokenizer
            .get_vocab(true)
            .values()
            .copied()
            .max()
            .map(|max_id| max_id as usize + 1)
            .unwrap_or(0);
        if let Some(w) = &weights
            && w.len() < id_space
        {
            return Err(invalid_model_artifact(format!(
                "weights tensor covers {} entries but token ids reach {id_space}",
                w.len()
            )));
        }
        match &mapping {
            Some(m) if m.len() < id_space => {
                return Err(invalid_model_artifact(format!(
                    "mapping tensor covers {} entries but token ids reach {id_space}",
                    m.len()
                )));
            }
            None if id_space > rows => {
                return Err(invalid_model_artifact(format!(
                    "token ids reach {id_space} but the embedding table only has {rows} rows"
                )));
            }
            _ => {}
        }

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
            match self.tokenizer.encode_fast(text, false) {
                Ok(encoding) => ids.extend_from_slice(encoding.get_ids()),
                Err(e) => {
                    // A panic here would abort a whole index build over one
                    // text. Embed it as the zero vector instead (BM25 still
                    // covers it) and say so once.
                    static WARNED: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!(
                            "warning: tokenization failed ({e}); affected texts get \
                             keyword-only (BM25) matching"
                        );
                    }
                }
            }
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
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect(),
        Dtype::F64 => raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| f64::from_le_bytes(*b) as f32)
            .collect(),
        Dtype::F16 => raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| half::f16::from_le_bytes(*b).to_f32())
            .collect(),
        Dtype::I8 => raw.iter().map(|&b| f32::from(b as i8)).collect(),
        other => return Err(anyhow!("unsupported tensor dtype: {other:?}")),
    })
}

/// Decode the vocabulary-quantization row mapping (I64 or I32), rejecting
/// entries that do not index a row of the embedding table (negative values
/// would otherwise wrap to huge indexes in the `as u32` cast).
fn decode_mapping(tensor: &safetensors::tensor::TensorView<'_>, rows: usize) -> Result<Vec<u32>> {
    use safetensors::tensor::Dtype;
    let raw = tensor.data();
    let validated = |v: i64| -> Result<u32> {
        if v < 0 || v as usize >= rows {
            return Err(anyhow!(
                "mapping entry {v} outside the embedding table ({rows} rows)"
            ));
        }
        Ok(v as u32)
    };
    match tensor.dtype() {
        Dtype::I64 => raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| validated(i64::from_le_bytes(*b)))
            .collect(),
        Dtype::I32 => raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| validated(i64::from(i32::from_le_bytes(*b))))
            .collect(),
        other => Err(anyhow!("unsupported mapping dtype: {other:?}")),
    }
}

#[derive(Debug)]
struct InvalidModelArtifact(String);

impl std::fmt::Display for InvalidModelArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidModelArtifact {}

fn invalid_model_artifact(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(InvalidModelArtifact(message.into()))
}

fn is_invalid_model_artifact(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<InvalidModelArtifact>())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelOrigin {
    Local,
    Hub,
}

#[derive(Debug, Clone)]
struct ModelFiles {
    tokenizer: PathBuf,
    model: PathBuf,
    config: PathBuf,
    origin: ModelOrigin,
}

fn hub_model_lock_path(cache_root: &Path, id: &str) -> PathBuf {
    let repo = hf_hub::Repo::model(id.to_string());
    cache_root
        .join(repo.folder_name())
        .join(".trouve-model-load.lock")
}

fn parse_hub_model_lock_wait(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

fn hub_model_lock_wait() -> Duration {
    let Ok(value) = std::env::var(HUB_MODEL_LOCK_TIMEOUT_ENV) else {
        return DEFAULT_HUB_MODEL_LOCK_WAIT;
    };
    parse_hub_model_lock_wait(&value).unwrap_or_else(|| {
        eprintln!(
            "warning: {HUB_MODEL_LOCK_TIMEOUT_ENV} must be a positive integer number of seconds; \
             using the default of {} seconds",
            DEFAULT_HUB_MODEL_LOCK_WAIT.as_secs()
        );
        DEFAULT_HUB_MODEL_LOCK_WAIT
    })
}

fn lock_hub_model_at(cache_root: &Path, id: &str) -> Result<std::fs::File> {
    lock_hub_model_at_with_timeout(cache_root, id, hub_model_lock_wait())
}

fn lock_hub_model_at_with_timeout(
    cache_root: &Path,
    id: &str,
    timeout: Duration,
) -> Result<std::fs::File> {
    use fs4::fs_std::FileExt as _;

    let lock_path = hub_model_lock_path(cache_root, id);
    let parent = lock_path
        .parent()
        .with_context(|| format!("Hub model lock has no parent: {}", lock_path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating Hub model cache directory {}", parent.display()))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening Hub model lock {}", lock_path.display()))?;
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => return Ok(lock),
            Ok(false) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Err(anyhow!(
                        "timed out after {timeout:?} waiting for Hub model cache lock for \
                         {id:?} at {}; another process may be downloading or repairing this \
                         model; wait for it to finish or stop the stalled process, then retry",
                        lock_path.display()
                    ));
                }
                std::thread::sleep(HUB_MODEL_LOCK_RETRY.min(timeout - elapsed));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("locking Hub model cache for {id:?}"));
            }
        }
    }
}

fn with_hub_model_lock<T>(id: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let cache = hf_hub::Cache::from_env();
    let lock = lock_hub_model_at(cache.path(), id)?;
    let result = operation();
    let unlock = fs4::fs_std::FileExt::unlock(&lock)
        .with_context(|| format!("unlocking Hub model cache for {id:?}"));
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn load_model_with_cache_coordination(id: &str) -> Result<EmbeddingModel> {
    if Path::new(id).exists() {
        let files = resolve_model_files(id)?;
        return load_model_with_refresh(id, &files, || refresh_model_files(id, &files));
    }

    // The Hub cache is shared across processes. Hold one repository-level
    // filesystem lock from resolution through validation and any repair so a
    // concurrent loader cannot observe snapshot pointers being replaced. The
    // forced download remains inside the lock because hf-hub publishes each
    // artifact pointer separately; bounded acquisition keeps a stalled loader
    // from making every other process wait forever.
    with_hub_model_lock(id, || {
        let files = resolve_hub_model_files(id, false)?;
        load_model_with_refresh(id, &files, || refresh_model_files(id, &files))
    })
}

/// Load a resolved model and, when offered by the caller, retry once with a
/// freshly downloaded set of files. Local model directories deliberately pass
/// `Ok(None)` so a validation error never mutates user-owned files.
fn load_model_with_refresh<F>(id: &str, files: &ModelFiles, refresh: F) -> Result<EmbeddingModel>
where
    F: FnOnce() -> Result<Option<ModelFiles>>,
{
    match EmbeddingModel::from_files(files, id.to_string()) {
        Ok(model) => Ok(model),
        Err(initial_error) => {
            if !is_invalid_model_artifact(&initial_error) {
                return Err(initial_error);
            }
            let Some(refreshed) = refresh().with_context(|| {
                format!("cached model was invalid ({initial_error:#}); forced download also failed")
            })?
            else {
                return Err(initial_error);
            };
            EmbeddingModel::from_files(&refreshed, id.to_string()).with_context(|| {
                format!(
                    "model is still invalid after a forced download; initial error: {initial_error:#}"
                )
            })
        }
    }
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
        origin: ModelOrigin::Local,
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

    resolve_hub_model_files(id, false)
}

/// Resolve all Hub artifacts, optionally bypassing existing cache pointers.
fn resolve_hub_model_files(id: &str, force_download: bool) -> Result<ModelFiles> {
    let api = hf_hub::api::sync::ApiBuilder::from_env()
        .build()
        .context("hf-hub API init failed")?;
    let repo = api.model(id.to_string());
    let fetch = |name: &str| {
        if force_download {
            repo.download(name)
        } else {
            repo.get(name)
        }
    };
    let config = fetch("config.json")
        .or_else(|_| fetch("config_sentence_transformers.json"))
        .with_context(|| format!("could not load '{id}' from HuggingFace Hub"))?;
    let tokenizer = fetch("tokenizer.json").context("tokenizer.json not found")?;
    let model = fetch("model.safetensors").context("model.safetensors not found")?;
    Ok(ModelFiles {
        tokenizer,
        model,
        config,
        origin: ModelOrigin::Hub,
    })
}

/// Remove only the Hub snapshot pointers returned by the failed load. This is
/// necessary before `ApiRepo::download`: on Windows without symlink support,
/// hf-hub otherwise downloads a new blob but keeps returning the old regular
/// pointer file. Blob targets are deliberately left to hf-hub's own locking
/// and atomic replacement.
fn invalidate_hub_model_files(files: &ModelFiles) -> Result<()> {
    debug_assert_eq!(files.origin, ModelOrigin::Hub);
    for path in [&files.config, &files.tokenizer, &files.model] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to invalidate cached model file {path:?}"));
            }
        }
    }
    Ok(())
}

fn refresh_model_files(id: &str, files: &ModelFiles) -> Result<Option<ModelFiles>> {
    if files.origin == ModelOrigin::Local {
        return Ok(None);
    }
    invalidate_hub_model_files(files)?;
    resolve_hub_model_files(id, true).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_HUB_MODEL_LOCK_CACHE_ENV: &str = "TROUVE_TEST_HUB_MODEL_LOCK_CACHE";
    const TEST_HUB_MODEL_LOCK_ID_ENV: &str = "TROUVE_TEST_HUB_MODEL_LOCK_ID";
    const TEST_HUB_MODEL_LOCK_MARKER_ENV: &str = "TROUVE_TEST_HUB_MODEL_LOCK_MARKER";
    const TEST_HUB_MODEL_LOCK_RELEASE_ENV: &str = "TROUVE_TEST_HUB_MODEL_LOCK_RELEASE";

    #[test]
    fn parses_configurable_hub_model_lock_wait() {
        assert_eq!(
            parse_hub_model_lock_wait(" 600 "),
            Some(Duration::from_secs(600))
        );
        assert_eq!(parse_hub_model_lock_wait("0"), None);
        assert_eq!(parse_hub_model_lock_wait("not-a-duration"), None);
    }

    #[test]
    fn model_load_locks_serialize_per_id() {
        let first = model_load_lock("test-model-load-lock");
        let same = model_load_lock("test-model-load-lock");
        let other = model_load_lock("test-other-model-load-lock");

        let guard = first.lock().unwrap();
        assert!(same.try_lock().is_err());
        assert!(other.try_lock().is_ok());
        drop(guard);
        assert!(same.try_lock().is_ok());
    }

    #[test]
    fn model_load_locks_recover_from_poison() {
        let registry = MODEL_LOAD_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        assert!(
            std::panic::catch_unwind(|| {
                let _guard = registry.lock().unwrap();
                panic!("poison model-load registry");
            })
            .is_err()
        );
        let recovered = model_load_lock("test-poisoned-registry-lock");
        assert!(recovered.try_lock().is_ok());

        let load_lock = model_load_lock("test-poisoned-model-load-lock");
        let poison_target = load_lock.clone();
        assert!(
            std::panic::catch_unwind(move || {
                let _guard = poison_target.lock().unwrap();
                panic!("poison per-model load lock");
            })
            .is_err()
        );
        drop(lock_ignore_poison(&load_lock));
    }

    #[test]
    fn hub_model_lock_serializes_file_handles() {
        use fs4::fs_std::FileExt as _;

        let cache = tempfile::tempdir().unwrap();
        let id = "owner/test-hub-model-lock";
        let first = lock_hub_model_at(cache.path(), id).unwrap();
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(hub_model_lock_path(cache.path(), id))
            .unwrap();

        assert!(!second.try_lock_exclusive().unwrap());
        fs4::fs_std::FileExt::unlock(&first).unwrap();
        assert!(second.try_lock_exclusive().unwrap());
        second.unlock().unwrap();
    }

    #[test]
    fn hub_model_lock_process_helper() {
        let Some(cache_root) = std::env::var_os(TEST_HUB_MODEL_LOCK_CACHE_ENV) else {
            return;
        };
        let id = std::env::var(TEST_HUB_MODEL_LOCK_ID_ENV).unwrap();
        let marker = PathBuf::from(std::env::var_os(TEST_HUB_MODEL_LOCK_MARKER_ENV).unwrap());
        let release = PathBuf::from(std::env::var_os(TEST_HUB_MODEL_LOCK_RELEASE_ENV).unwrap());
        let lock =
            lock_hub_model_at_with_timeout(Path::new(&cache_root), &id, Duration::from_secs(2))
                .unwrap();
        std::fs::write(&marker, b"locked").unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        while !release.exists() {
            assert!(
                Instant::now() < deadline,
                "parent never released held Hub model lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        fs4::fs_std::FileExt::unlock(&lock).unwrap();
    }

    #[test]
    fn hub_model_lock_timeout_is_bounded_across_processes() {
        let cache = tempfile::tempdir().unwrap();
        let marker = cache.path().join("holder.locked");
        let release = cache.path().join("holder.release");
        let id = "owner/test-hub-model-lock-timeout";
        let test_binary = std::env::current_exe().unwrap();
        let mut command = std::process::Command::new(test_binary);
        command
            .arg("--exact")
            .arg("embed::tests::hub_model_lock_process_helper")
            .arg("--nocapture")
            .env(TEST_HUB_MODEL_LOCK_CACHE_ENV, cache.path())
            .env(TEST_HUB_MODEL_LOCK_ID_ENV, id)
            .env(TEST_HUB_MODEL_LOCK_MARKER_ENV, &marker)
            .env(TEST_HUB_MODEL_LOCK_RELEASE_ENV, &release)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut holder = trouve_process::spawn(&mut command).unwrap();

        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() {
            assert!(
                Instant::now() < marker_deadline,
                "helper process never acquired the Hub model lock"
            );
            assert!(
                holder.try_wait().unwrap().is_none(),
                "helper process exited before publishing its lock marker"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Attempt on the parent test thread so an oversubscribed runner cannot
        // delay a separate contender thread until after the holder is released.
        // The helper has a 30-second safety deadline, so a regression to a
        // blocking acquisition still terminates and fails rather than hanging.
        let attempt = lock_hub_model_at_with_timeout(cache.path(), id, Duration::from_millis(75));

        std::fs::write(&release, b"release").unwrap();
        let holder_status = holder.wait().unwrap();

        assert!(holder_status.success());
        let error = attempt.expect_err("contender unexpectedly acquired a held process lock");
        let message = format!("{error:#}");
        assert!(message.contains("75ms"), "{message}");
        assert!(message.contains(id), "{message}");
        assert!(
            message.contains(&hub_model_lock_path(cache.path(), id).display().to_string()),
            "{message}"
        );
        assert!(message.contains("stalled process, then retry"), "{message}");

        let retry = lock_hub_model_at_with_timeout(cache.path(), id, Duration::from_secs(1))
            .expect("lock should be available after the holder exits");
        fs4::fs_std::FileExt::unlock(&retry).unwrap();
    }

    /// WordLevel tokenizer with `words` in the vocabulary (plus [UNK] at 0).
    fn tokenizer_json(words: &[&str]) -> String {
        let mut vocab = serde_json::Map::new();
        vocab.insert("[UNK]".to_string(), serde_json::json!(0));
        for (i, w) in words.iter().enumerate() {
            vocab.insert((*w).to_string(), serde_json::json!(i + 1));
        }
        serde_json::json!({
            "version": "1.0",
            "added_tokens": [],
            "normalizer": {"type": "Lowercase"},
            "pre_tokenizer": {"type": "Whitespace"},
            "model": {"type": "WordLevel", "vocab": vocab, "unk_token": "[UNK]"}
        })
        .to_string()
    }

    /// A safetensors file with a `rows x 2` zero embedding table and optional
    /// I64 `mapping` and F32 `weights` tensors.
    fn safetensors_bytes(rows: usize, mapping: Option<&[i64]>, weights: Option<&[f32]>) -> Vec<u8> {
        let embed_len = rows * 2 * 4;
        let mut header = serde_json::json!({
            "embeddings": {"dtype": "F32", "shape": [rows, 2], "data_offsets": [0, embed_len]}
        });
        let mut data = vec![0u8; embed_len];
        if let Some(m) = mapping {
            let start = data.len();
            for v in m {
                data.extend_from_slice(&v.to_le_bytes());
            }
            header["mapping"] = serde_json::json!({
                "dtype": "I64", "shape": [m.len()], "data_offsets": [start, data.len()]
            });
        }
        if let Some(w) = weights {
            let start = data.len();
            for v in w {
                data.extend_from_slice(&v.to_le_bytes());
            }
            header["weights"] = serde_json::json!({
                "dtype": "F32", "shape": [w.len()], "data_offsets": [start, data.len()]
            });
        }
        let header = header.to_string();
        let mut out = Vec::new();
        out.extend_from_slice(&(header.len() as u64).to_le_bytes());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn write_model(dir: &Path, words: &[&str], rows: usize, mapping: Option<&[i64]>) -> ModelFiles {
        std::fs::write(dir.join("tokenizer.json"), tokenizer_json(words)).unwrap();
        std::fs::write(
            dir.join("model.safetensors"),
            safetensors_bytes(rows, mapping, None),
        )
        .unwrap();
        std::fs::write(dir.join("config.json"), r#"{"normalize": true}"#).unwrap();
        ModelFiles {
            tokenizer: dir.join("tokenizer.json"),
            model: dir.join("model.safetensors"),
            config: dir.join("config.json"),
            origin: ModelOrigin::Local,
        }
    }

    #[test]
    fn valid_model_loads_and_embeds() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_model(dir.path(), &["alpha", "beta"], 3, None);
        let model = EmbeddingModel::from_files(&files, "test".into()).unwrap();
        assert_eq!(model.encode_one("alpha beta").len(), 2);
    }

    #[test]
    fn retries_invalid_cached_model_with_refreshed_files() {
        let cached = tempfile::tempdir().unwrap();
        let refreshed = tempfile::tempdir().unwrap();
        let cached_files = write_model(cached.path(), &["alpha"], 2, None);
        std::fs::write(&cached_files.model, b"incomplete").unwrap();
        let refreshed_files = write_model(refreshed.path(), &["alpha"], 2, None);

        let mut attempted_refresh = false;
        let model = load_model_with_refresh("test", &cached_files, || {
            attempted_refresh = true;
            Ok(Some(refreshed_files))
        })
        .unwrap();

        assert!(attempted_refresh);
        assert_eq!(model.encode_one("alpha").len(), 2);
    }

    #[test]
    fn empty_hub_model_refreshes() {
        let cached = tempfile::tempdir().unwrap();
        let refreshed = tempfile::tempdir().unwrap();
        let mut cached_files = write_model(cached.path(), &["alpha"], 2, None);
        cached_files.origin = ModelOrigin::Hub;
        std::fs::write(&cached_files.model, []).unwrap();
        let refreshed_files = write_model(refreshed.path(), &["alpha"], 2, None);

        let mut attempted_refresh = false;
        let model = load_model_with_refresh("test", &cached_files, || {
            attempted_refresh = true;
            Ok(Some(refreshed_files))
        })
        .unwrap();

        assert!(attempted_refresh);
        assert_eq!(model.encode_one("alpha").len(), 2);
    }

    #[test]
    fn empty_local_model_does_not_refresh() {
        let cached = tempfile::tempdir().unwrap();
        let cached_files = write_model(cached.path(), &["alpha"], 2, None);
        std::fs::write(&cached_files.model, []).unwrap();

        EmbeddingModel::load(cached.path().to_str())
            .map(|_| ())
            .unwrap_err();

        assert_eq!(std::fs::metadata(&cached_files.model).unwrap().len(), 0);
    }

    #[test]
    fn io_failure_does_not_refresh_cached_model() {
        let cached = tempfile::tempdir().unwrap();
        let mut cached_files = write_model(cached.path(), &["alpha"], 2, None);
        cached_files.origin = ModelOrigin::Hub;
        let model_bytes = std::fs::read(&cached_files.model).unwrap();
        std::fs::remove_file(&cached_files.tokenizer).unwrap();

        let error = load_model_with_refresh("test", &cached_files, || {
            panic!("environmental I/O failures must not refresh the model")
        })
        .map(|_| ())
        .unwrap_err();

        assert!(
            format!("{error:#}").contains("failed to read tokenizer.json"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&cached_files.model).unwrap(), model_bytes);
    }

    #[test]
    fn valid_cached_model_does_not_refresh() {
        let cached = tempfile::tempdir().unwrap();
        let cached_files = write_model(cached.path(), &["alpha"], 2, None);

        load_model_with_refresh("test", &cached_files, || {
            panic!("valid cached model should not be refreshed")
        })
        .unwrap();
    }

    #[test]
    fn invalid_local_model_is_not_replaced() {
        let cached = tempfile::tempdir().unwrap();
        let cached_files = write_model(cached.path(), &["alpha"], 2, None);
        let incomplete = b"incomplete";
        std::fs::write(&cached_files.model, incomplete).unwrap();

        let error = EmbeddingModel::load(cached.path().to_str())
            .map(|_| ())
            .unwrap_err();

        assert!(format!("{error:#}").contains("safetensors"), "{error:#}");
        assert_eq!(std::fs::read(&cached_files.model).unwrap(), incomplete);
    }

    #[test]
    fn invalidating_hub_files_removes_snapshot_pointers() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = write_model(dir.path(), &["alpha"], 2, None);
        files.origin = ModelOrigin::Hub;

        invalidate_hub_model_files(&files).unwrap();

        assert!(!files.config.exists());
        assert!(!files.tokenizer.exists());
        assert!(!files.model.exists());
    }

    #[test]
    fn rejects_negative_mapping_entries() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_model(dir.path(), &["alpha", "beta"], 3, Some(&[0, 1, -1]));
        let err = EmbeddingModel::from_files(&files, "test".into())
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("mapping entry"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_mapping_entries() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_model(dir.path(), &["alpha", "beta"], 3, Some(&[0, 1, 3]));
        let err = EmbeddingModel::from_files(&files, "test".into())
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("mapping entry"), "{err}");
    }

    #[test]
    fn rejects_mapping_shorter_than_vocab() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_model(dir.path(), &["alpha", "beta"], 3, Some(&[0, 1]));
        let err = EmbeddingModel::from_files(&files, "test".into())
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("mapping tensor covers"), "{err}");
    }

    #[test]
    fn rejects_weights_shorter_than_vocab() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_model(dir.path(), &["alpha", "beta"], 3, None);
        std::fs::write(&files.model, safetensors_bytes(3, None, Some(&[1.0, 1.0]))).unwrap();
        let err = EmbeddingModel::from_files(&files, "test".into())
            .map(|_| ())
            .unwrap_err();
        assert!(is_invalid_model_artifact(&err));
        assert!(err.to_string().contains("weights tensor covers"), "{err}");
    }

    #[test]
    fn rejects_table_smaller_than_vocab() {
        let dir = tempfile::tempdir().unwrap();
        // 3 vocabulary tokens ([UNK] + 2 words) but only 2 table rows.
        let files = write_model(dir.path(), &["alpha", "beta"], 2, None);
        let err = EmbeddingModel::from_files(&files, "test".into())
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_string().contains("embedding table"), "{err}");
    }
}
