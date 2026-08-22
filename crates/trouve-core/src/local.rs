//! Local ("offline / integrated") model support.
//!
//! trouve manages the whole local-inference stack itself so it works out of
//! the box with zero configuration:
//!
//! - the **runtime** is llama.cpp's `llama-server`, installed through the
//!   same managed-CLI machinery as the vendor agent CLIs (`install.rs` in
//!   trouve-agents; Vulkan build on Linux when the loader is present, Metal
//!   on macOS, CPU otherwise);
//! - **models** are single-file GGUFs from HuggingFace: a curated catalog
//!   of known-good, tool-calling-capable coding models at Q4_K_M-class
//!   quants (beginners never see the word "quant"), plus user-added repo/
//!   file pairs for power users;
//! - a **hardware probe** (RAM + VRAM) classifies each model as fitting on
//!   the GPU, fitting in RAM (CPU, slower), or too large — the same
//!   conservative "will it fit" heuristic Ollama uses;
//! - the **sidecar** llama-server process is spawned lazily on the first
//!   turn that uses a `local/<model>` id, health-checked, reused across
//!   turns, and restarted when the user switches models;
//! - the **provider** is a thin [`Provider`] wrapper that ensures the
//!   sidecar is up and then delegates to the existing OpenAI-compatible
//!   client (llama-server speaks that protocol natively).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use trouve_protocol::LocalGpu;
use trouve_providers::Provider;

// --- curated catalog ---------------------------------------------------------

/// One model trouve knows how to download and run. Sizes were read from the
/// HuggingFace file metadata when the entry was curated; they only gate the
/// hardware-fit label, not the download itself.
pub struct CatalogEntry {
    pub id: &'static str,
    pub display_name: &'static str,
    pub repo: &'static str,
    pub file: &'static str,
    pub size_bytes: u64,
    pub params: &'static str,
    pub notes: &'static str,
}

/// How a local model's reasoning is steered through its chat template.
/// There is no universal knob in llama.cpp — it's per model family, applied
/// via `chat_template_kwargs` on the request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Thinking {
    /// Plain instruct model: no thinking controls.
    #[default]
    None,
    /// On/off via the `enable_thinking` template kwarg (Qwen3-style).
    Toggle,
    /// low/medium/high via the `reasoning_effort` template kwarg (GPT-OSS).
    Effort,
}

/// Known-good coding models with working llama.cpp tool calling, smallest
/// first. Curation rules: official or well-established GGUF repos only,
/// single-file quants only (no split GGUFs), Q4_K_M-class quality.
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "qwen2.5-coder-3b",
        display_name: "Qwen2.5 Coder 3B",
        repo: "Qwen/Qwen2.5-Coder-3B-Instruct-GGUF",
        file: "qwen2.5-coder-3b-instruct-q4_k_m.gguf",
        size_bytes: 2_104_932_800,
        params: "3B",
        notes: "Smallest option; quick answers and light edits on any machine.",
    },
    CatalogEntry {
        id: "qwen2.5-coder-7b",
        display_name: "Qwen2.5 Coder 7B",
        repo: "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF",
        file: "qwen2.5-coder-7b-instruct-q4_k_m.gguf",
        size_bytes: 4_683_073_536,
        params: "7B",
        notes: "Best pick for 8 GB GPUs; solid completions and small tasks.",
    },
    CatalogEntry {
        id: "gpt-oss-20b",
        display_name: "GPT-OSS 20B",
        repo: "ggml-org/gpt-oss-20b-GGUF",
        file: "gpt-oss-20b-mxfp4.gguf",
        size_bytes: 12_109_566_560,
        params: "21B MoE",
        notes: "OpenAI's open-weight model; strong reasoning and tool use.",
    },
    CatalogEntry {
        id: "devstral-small-2507",
        display_name: "Devstral Small",
        repo: "mistralai/Devstral-Small-2507_gguf",
        file: "Devstral-Small-2507-Q4_K_M.gguf",
        size_bytes: 14_333_915_904,
        params: "24B",
        notes: "Mistral's coding-agent specialist; good at multi-file edits.",
    },
    CatalogEntry {
        id: "qwen3.6-27b",
        display_name: "Qwen3.6 27B",
        repo: "unsloth/Qwen3.6-27B-GGUF",
        file: "Qwen3.6-27B-Q4_K_M.gguf",
        size_bytes: 16_817_244_384,
        params: "27B",
        notes: "Best all-round coding model for a single 24 GB GPU.",
    },
    CatalogEntry {
        id: "qwen3-coder-30b",
        display_name: "Qwen3 Coder 30B",
        repo: "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF",
        file: "Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf",
        size_bytes: 18_556_689_568,
        params: "30B MoE",
        notes: "Only 3B active parameters — usable even on CPU with enough RAM.",
    },
];

/// Dedicated session-title model. It is intentionally absent from the local
/// coding-model catalog: the title sidecar has its own lifecycle and never
/// appears in thread model pickers.
pub const TITLE_MODEL_ID: &str = "qwen3-title-1.7b-q4-k-m";
pub const TITLE_MODEL_CONTEXT: u64 = 2_048;
pub const TITLE_MODEL_SHA256: &str =
    "d2387ca2dbfee2ffabce7120d3770dadca0b293052bc2f0e138fdc940d9bc7b5";
pub const TITLE_MODEL_LICENSE: &str = "Apache-2.0";
pub(crate) const LEGACY_TITLE_MODEL_FILES: &[&str] =
    &["qwen2.5-0.5b-instruct-q4_k_m.gguf", "Qwen3-0.6B-Q8_0.gguf"];

pub fn title_model_entry() -> ModelEntry {
    ModelEntry {
        id: TITLE_MODEL_ID.into(),
        display_name: "Session naming model".into(),
        repo: "ggml-org/Qwen3-1.7B-GGUF".into(),
        file: "Qwen3-1.7B-Q4_K_M.gguf".into(),
        size_bytes: 1_282_439_264,
        params: "1.7B".into(),
        notes: format!("Balanced-quality dedicated session-title model ({TITLE_MODEL_LICENSE})"),
        custom: false,
    }
}

// --- user-added models -------------------------------------------------------

/// A user-added GGUF (settings → Local Models → custom). Persisted in
/// `<config>/local-models.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomModel {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    pub file: String,
    /// Read from HuggingFace when the entry was added.
    #[serde(default)]
    pub size_bytes: u64,
}

pub fn custom_models_path(config_dir: &Path) -> PathBuf {
    config_dir.join("local-models.json")
}

pub fn read_custom_models(path: &Path) -> Vec<CustomModel> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<CustomModel>>(&raw).unwrap_or_default()
}

pub fn write_custom_models(path: &Path, models: &[CustomModel]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(models).unwrap())
}

/// Stable id for a custom entry, slugged from the GGUF filename.
pub fn slug_from_file(file: &str) -> String {
    let stem = file
        .rsplit('/')
        .next()
        .unwrap_or(file)
        .trim_end_matches(".gguf");
    let mut slug: String = stem
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').to_string()
}

// --- resolved entries --------------------------------------------------------

/// A catalog or custom entry, resolved to one shape.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    pub file: String,
    pub size_bytes: u64,
    pub params: String,
    pub notes: String,
    pub custom: bool,
}

/// Every model trouve can offer locally: the curated catalog plus the
/// user's custom entries (custom wins on id collision).
pub fn all_entries(config_dir: Option<&Path>) -> Vec<ModelEntry> {
    let mut entries: Vec<ModelEntry> = CATALOG
        .iter()
        .map(|c| ModelEntry {
            id: c.id.into(),
            display_name: c.display_name.into(),
            repo: c.repo.into(),
            file: c.file.into(),
            size_bytes: c.size_bytes,
            params: c.params.into(),
            notes: c.notes.into(),
            custom: false,
        })
        .collect();
    if let Some(dir) = config_dir {
        for custom in read_custom_models(&custom_models_path(dir)) {
            entries.retain(|e| e.id != custom.id);
            entries.push(ModelEntry {
                id: custom.id,
                display_name: custom.display_name,
                repo: custom.repo,
                file: custom.file,
                size_bytes: custom.size_bytes,
                params: String::new(),
                notes: String::new(),
                custom: true,
            });
        }
    }
    entries
}

/// Where downloaded GGUFs live.
pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// On-disk path for one entry's GGUF (flat: just the filename portion).
pub fn gguf_path(data_dir: &Path, entry: &ModelEntry) -> PathBuf {
    let name = entry.file.rsplit('/').next().unwrap_or(&entry.file);
    models_dir(data_dir).join(name)
}

/// Runtime-relevant metadata embedded in a downloaded GGUF. This is the
/// model itself describing its identity, native context, and chat-template
/// controls; repo and filename conventions are not consulted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelMetadata {
    pub display_name: Option<String>,
    pub context_window: u64,
    pub thinking: Thinking,
}

type MetadataStamp = (u64, Option<std::time::SystemTime>);
type MetadataCache = std::collections::HashMap<PathBuf, (MetadataStamp, ModelMetadata)>;

/// Read-once process cache: GGUF metadata is immutable after the atomic
/// download rename, and tokenizer arrays can make a full header scan costly.
static GGUF_METADATA: std::sync::OnceLock<std::sync::Mutex<MetadataCache>> =
    std::sync::OnceLock::new();

fn metadata_cache_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

pub fn model_metadata(path: &Path) -> ModelMetadata {
    let cache_path = metadata_cache_path(path);
    let Ok(fs) = std::fs::metadata(&cache_path) else {
        return ModelMetadata::default();
    };
    let stamp = (fs.len(), fs.modified().ok());
    let cache = GGUF_METADATA.get_or_init(|| std::sync::Mutex::new(MetadataCache::new()));
    if let Some((cached_stamp, metadata)) = cache.lock().unwrap().get(&cache_path)
        && *cached_stamp == stamp
    {
        return metadata.clone();
    }
    let metadata = read_gguf_metadata(&cache_path).unwrap_or_else(|e| {
        tracing::warn!(path = %cache_path.display(), "could not read GGUF metadata: {e:#}");
        ModelMetadata::default()
    });
    cache
        .lock()
        .unwrap()
        .insert(cache_path, (stamp, metadata.clone()));
    metadata
}

fn read_gguf_metadata(path: &Path) -> Result<ModelMetadata> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    type Reader = std::io::BufReader<std::fs::File>;

    fn bytes<const N: usize>(r: &mut Reader) -> std::io::Result<[u8; N]> {
        let mut bytes = [0; N];
        r.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn u32_le(r: &mut Reader) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(bytes(r)?))
    }
    fn u64_le(r: &mut Reader) -> std::io::Result<u64> {
        Ok(u64::from_le_bytes(bytes(r)?))
    }
    fn string(r: &mut Reader, max: u64) -> Result<String> {
        let len = u64_le(r)?;
        if len > max {
            bail!("GGUF string length {len} exceeds metadata limit {max}");
        }
        let mut value = vec![0; usize::try_from(len)?];
        r.read_exact(&mut value)?;
        String::from_utf8(value).context("GGUF metadata string is not UTF-8")
    }
    fn scalar_width(value_type: u32) -> Option<u64> {
        match value_type {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        }
    }
    fn skip(r: &mut Reader, value_type: u32, depth: u8) -> Result<()> {
        if depth > 4 {
            bail!("nested GGUF metadata arrays are too deep");
        }
        if let Some(width) = scalar_width(value_type) {
            r.seek(SeekFrom::Current(i64::try_from(width)?))?;
            return Ok(());
        }
        match value_type {
            8 => {
                let len = u64_le(r)?;
                r.seek(SeekFrom::Current(i64::try_from(len)?))?;
                Ok(())
            }
            9 => {
                let element_type = u32_le(r)?;
                let count = u64_le(r)?;
                if count > 100_000_000 {
                    bail!("implausible GGUF metadata array length {count}");
                }
                if let Some(width) = scalar_width(element_type) {
                    let bytes = width
                        .checked_mul(count)
                        .ok_or_else(|| anyhow::anyhow!("GGUF metadata array size overflow"))?;
                    r.seek(SeekFrom::Current(i64::try_from(bytes)?))?;
                    return Ok(());
                }
                for _ in 0..count {
                    skip(r, element_type, depth + 1)?;
                }
                Ok(())
            }
            other => bail!("unknown GGUF metadata value type {other}"),
        }
    }
    fn positive_integer(r: &mut Reader, value_type: u32) -> Result<Option<u64>> {
        let value = match value_type {
            0 => u64::from(bytes::<1>(r)?[0]),
            1 => i8::from_le_bytes(bytes(r)?).try_into().unwrap_or(0),
            2 => u64::from(u16::from_le_bytes(bytes(r)?)),
            3 => i16::from_le_bytes(bytes(r)?).try_into().unwrap_or(0),
            4 => u64::from(u32_le(r)?),
            5 => i32::from_le_bytes(bytes(r)?).try_into().unwrap_or(0),
            10 => u64_le(r)?,
            11 => i64::from_le_bytes(bytes(r)?).try_into().unwrap_or(0),
            _ => {
                skip(r, value_type, 0)?;
                return Ok(None);
            }
        };
        Ok((value > 0).then_some(value))
    }

    let mut file = std::io::BufReader::new(std::fs::File::open(path)?);
    if &bytes::<4>(&mut file)? != b"GGUF" {
        bail!("not a GGUF file");
    }
    let version = u32_le(&mut file)?;
    if !matches!(version, 2 | 3) {
        bail!("unsupported GGUF version {version}");
    }
    let _tensor_count = u64_le(&mut file)?;
    let metadata_count = u64_le(&mut file)?;
    if metadata_count > 100_000 {
        bail!("implausible GGUF metadata count {metadata_count}");
    }

    let mut architecture = None;
    let mut display_name = None;
    let mut chat_template = None;
    let mut contexts = std::collections::HashMap::<String, u64>::new();
    for _ in 0..metadata_count {
        let key = string(&mut file, 64 * 1024)?;
        let value_type = u32_le(&mut file)?;
        match key.as_str() {
            "general.architecture" if value_type == 8 => {
                architecture = Some(string(&mut file, 1024)?);
            }
            "general.name" if value_type == 8 => {
                display_name = Some(string(&mut file, 1024 * 1024)?);
            }
            "tokenizer.chat_template" if value_type == 8 => {
                chat_template = Some(string(&mut file, 16 * 1024 * 1024)?);
            }
            _ if key.ends_with(".context_length") => {
                if let Some(value) = positive_integer(&mut file, value_type)? {
                    contexts.insert(key, value);
                }
            }
            _ => skip(&mut file, value_type, 0)?,
        }
    }

    let context_window = architecture
        .as_deref()
        .and_then(|arch| contexts.get(&format!("{arch}.context_length")))
        .copied()
        .or_else(|| contexts.values().copied().max())
        .unwrap_or(0);
    let template = chat_template.unwrap_or_default().to_ascii_lowercase();
    let thinking = if template.contains("reasoning_effort") {
        Thinking::Effort
    } else if template.contains("enable_thinking") {
        Thinking::Toggle
    } else {
        Thinking::None
    };
    Ok(ModelMetadata {
        display_name,
        context_window,
        thinking,
    })
}

/// Direct download URL for a HuggingFace repo file.
pub fn download_url(repo: &str, file: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve/main/{file}?download=true")
}

// --- HuggingFace search ------------------------------------------------------

/// One repo from the HF model-search API.
#[derive(Debug, Clone, Deserialize)]
pub struct HfRepo {
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
}

/// Search HuggingFace for GGUF repos matching `query`, most-downloaded
/// first.
pub async fn search_hf_repos(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<HfRepo>> {
    let url = format!(
        "https://huggingface.co/api/models?search={}&filter=gguf&sort=downloads&limit={limit}",
        urlencoding_encode(query)
    );
    let resp = client.get(&url).send().await.context("HF search failed")?;
    if !resp.status().is_success() {
        bail!("HF search returned {}", resp.status());
    }
    resp.json().await.context("HF search response")
}

/// List a repo's GGUF files (path, size), excluding split multi-part
/// GGUFs (llama.cpp needs the single-file variants we download).
pub async fn list_gguf_files(client: &reqwest::Client, repo: &str) -> Result<Vec<(String, u64)>> {
    #[derive(Deserialize)]
    struct TreeEntry {
        #[serde(rename = "type")]
        kind: String,
        path: String,
        #[serde(default)]
        size: u64,
    }
    let url = format!("https://huggingface.co/api/models/{repo}/tree/main?recursive=true");
    let resp = client.get(&url).send().await.context("HF tree failed")?;
    if !resp.status().is_success() {
        bail!("HF tree returned {}", resp.status());
    }
    let entries: Vec<TreeEntry> = resp.json().await.context("HF tree response")?;
    Ok(entries
        .into_iter()
        .filter(|e| {
            e.kind == "file"
                && e.path.to_ascii_lowercase().ends_with(".gguf")
                && !is_split_gguf(&e.path)
                && e.size > 0
        })
        .map(|e| (e.path, e.size))
        .collect())
}

/// Multi-part GGUFs follow the `…-00001-of-00004.gguf` convention.
fn is_split_gguf(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".gguf") else {
        return false;
    };
    let mut parts = stem.rsplitn(3, '-');
    match (parts.next(), parts.next()) {
        (Some(last), Some(mid)) => {
            mid == "of" && !last.is_empty() && last.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// The quantization tag from a GGUF filename ("Q4_K_M", "IQ2_XS", "F16");
/// empty when none is recognizable.
pub fn quant_of(file: &str) -> String {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name
        .strip_suffix(".gguf")
        .or_else(|| name.strip_suffix(".GGUF"))
        .unwrap_or(name);
    for token in stem.rsplit(['-', '.']) {
        let upper = token.to_ascii_uppercase();
        let bytes = upper.as_bytes();
        let quantish = (bytes.first() == Some(&b'Q') || upper.starts_with("IQ"))
            && bytes.iter().any(|b| b.is_ascii_digit())
            && upper.len() <= 8;
        if quantish || matches!(upper.as_str(), "F16" | "F32" | "BF16" | "FP16") {
            return upper;
        }
    }
    String::new()
}

/// Minimal query-string escaping for the HF search parameter.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else if c == ' ' {
            out.push('+');
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).as_bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// The active trouve-managed llama-server binary.
///
/// Local inference deliberately does not fall back to `PATH`: trouve relies
/// on the version and platform package it installed when selecting supported
/// flags and backends.
pub fn runtime_bin(data_dir: &Path) -> Option<PathBuf> {
    trouve_agents::install::installed(data_dir, trouve_agents::install::CliId::LlamaServer)
        .map(|install| PathBuf::from(install.bin))
}

// --- hardware probe ----------------------------------------------------------

/// Detected memory resources. Conservative and best-effort: a machine
/// where GPU detection fails just gets CPU-tier recommendations.
#[derive(Debug, Clone, Default)]
pub struct Hardware {
    pub ram_bytes: u64,
    pub gpus: Vec<LocalGpu>,
}

/// Probe RAM and GPU VRAM. Sync and cheap (procfs/sysfs reads plus at most
/// one `nvidia-smi` invocation); call it from a blocking-ok context once
/// and cache.
pub fn probe_hardware() -> Hardware {
    let ram_bytes = probe_ram().unwrap_or(0);
    let mut gpus = Vec::new();

    // Apple Silicon: unified memory — the GPU can use system RAM.
    if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
        gpus.push(LocalGpu {
            name: "Apple Silicon (unified memory)".into(),
            vram_bytes: ram_bytes,
        });
        return Hardware { ram_bytes, gpus };
    }

    // NVIDIA via nvidia-smi (present wherever the proprietary driver is).
    let mut nvidia_smi = std::process::Command::new("nvidia-smi");
    nvidia_smi.args([
        "--query-gpu=name,memory.total",
        "--format=csv,noheader,nounits",
    ]);
    if let Ok(out) = trouve_process::output(&mut nvidia_smi)
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((name, mib)) = line.rsplit_once(',')
                && let Ok(mib) = mib.trim().parse::<u64>()
            {
                gpus.push(LocalGpu {
                    name: name.trim().to_string(),
                    vram_bytes: mib * 1024 * 1024,
                });
            }
        }
    }

    // AMD/Intel discrete GPUs via DRM sysfs (Linux). NVIDIA cards covered
    // above are skipped by vendor id.
    if std::env::consts::OS == "linux" {
        gpus.extend(probe_drm_gpus(
            Path::new("/sys/class/drm"),
            !gpus.is_empty(),
        ));
    }

    Hardware { ram_bytes, gpus }
}

fn probe_ram() -> Option<u64> {
    match std::env::consts::OS {
        "linux" => {
            let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
            let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
            let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            Some(kb * 1024)
        }
        "macos" => {
            let mut command = std::process::Command::new("sysctl");
            command.args(["-n", "hw.memsize"]);
            let out = trouve_process::output(&mut command).ok()?;
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()
        }
        _ => None,
    }
}

/// Memory the OS currently considers available without swapping. Used by
/// adaptive title-model preloading; total RAM is the conservative fallback
/// on platforms where an available-memory probe is unavailable.
pub fn available_ram_bytes() -> u64 {
    if std::env::consts::OS == "linux"
        && let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo")
        && let Some(kb) = meminfo
            .lines()
            .find(|line| line.starts_with("MemAvailable:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok())
    {
        return kb * 1024;
    }
    probe_ram().unwrap_or(0)
}

/// VRAM of non-NVIDIA cards from `/sys/class/drm/card*/device/`.
fn probe_drm_gpus(drm: &Path, skip_nvidia: bool) -> Vec<LocalGpu> {
    let mut gpus = Vec::new();
    let Ok(entries) = std::fs::read_dir(drm) else {
        return gpus;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Cards only ("card0"), not connectors ("card0-DP-1").
        if !name.starts_with("card") || name[4..].parse::<u32>().is_err() {
            continue;
        }
        let device = entry.path().join("device");
        let vendor = std::fs::read_to_string(device.join("vendor"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let vendor_name = match vendor.as_str() {
            "0x1002" => "AMD GPU",
            "0x8086" => "Intel GPU",
            "0x10de" => {
                if skip_nvidia {
                    continue; // already reported by nvidia-smi
                }
                "NVIDIA GPU"
            }
            _ => continue,
        };
        let Ok(vram) = std::fs::read_to_string(device.join("mem_info_vram_total")) else {
            continue;
        };
        let Ok(vram_bytes) = vram.trim().parse::<u64>() else {
            continue;
        };
        // Skip tiny integrated framebuffers; they can't host a model.
        if vram_bytes >= 2 * 1024 * 1024 * 1024 {
            gpus.push(LocalGpu {
                name: vendor_name.into(),
                vram_bytes,
            });
        }
    }
    gpus
}

/// Hardware-fit tier for a model of `size_bytes`, Ollama-style: weights ×
/// 1.15 plus a KV-cache/overhead allowance must fit in VRAM (GPU tier) or
/// in most of system RAM (CPU tier).
pub fn fit(size_bytes: u64, hw: &Hardware) -> &'static str {
    const OVERHEAD: u64 = 2 * 1024 * 1024 * 1024;
    let need = size_bytes + size_bytes / 7 + OVERHEAD; // ~ ×1.15 + 2 GiB
    if hw.gpus.iter().any(|g| g.vram_bytes >= need) {
        "gpu"
    } else if hw.ram_bytes * 85 / 100 >= need {
        "cpu"
    } else {
        "too-large"
    }
}

// --- llama-server lifecycle ---------------------------------------------------

#[cfg(test)]
fn pids_path(data_dir: &Path) -> PathBuf {
    data_dir.join("llama-server.pids")
}

fn read_pids(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn write_pids(path: &Path, pids: &[u32]) {
    let body = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    if pids.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if let Err(e) = std::fs::write(path, body) {
        tracing::warn!("cannot write {}: {e}", path.display());
    }
}

/// A process's command line, for identity checks before killing.
fn process_cmdline(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.replace('\0', " "))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut command = std::process::Command::new("ps");
        command.args(["-p", &pid.to_string(), "-o", "command="]);
        let out = trouve_process::output(&mut command).ok()?;
        let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !cmd.is_empty()).then_some(cmd)
    }
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        let mut command = std::process::Command::new("kill");
        command.args(["-9", &pid.to_string()]);
        let _ = trouve_process::status(&mut command);
    }
    #[cfg(not(unix))]
    {
        let mut command = std::process::Command::new("taskkill");
        command.args(["/F", "/PID", &pid.to_string()]);
        let _ = trouve_process::status(&mut command);
    }
}

struct Running {
    model_id: String,
    port: u16,
    child: tokio::process::Child,
}

/// Sidecar lifecycle as seen by status polling; a mirror kept outside the
/// spawn lock so reads never wait behind a multi-minute model load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    Stopped,
    /// Process spawned, model loading (waiting for /health).
    Starting(String),
    Running(String),
}

fn effective_title_resources(
    configured: trouve_protocol::TitleModelResourcePolicy,
    local_model_active: bool,
) -> trouve_protocol::TitleModelResourcePolicy {
    match configured {
        trouve_protocol::TitleModelResourcePolicy::Adaptive if local_model_active => {
            trouve_protocol::TitleModelResourcePolicy::CpuRamOnly
        }
        trouve_protocol::TitleModelResourcePolicy::Adaptive => {
            trouve_protocol::TitleModelResourcePolicy::GpuCpuRam
        }
        policy => policy,
    }
}

fn title_resource_args(
    resources: trouve_protocol::TitleModelResourcePolicy,
) -> &'static [&'static str] {
    match resources {
        trouve_protocol::TitleModelResourcePolicy::CpuRamOnly => &["-ngl", "0", "--device", "none"],
        trouve_protocol::TitleModelResourcePolicy::GpuOnly => &["-ngl", "all", "--fit", "off"],
        trouve_protocol::TitleModelResourcePolicy::GpuCpuRam => &[],
        trouve_protocol::TitleModelResourcePolicy::Adaptive => {
            unreachable!("adaptive title resources must be resolved before launch")
        }
    }
}

/// Owns the single llama-server sidecar. One model is loaded at a time;
/// asking for a different model stops the old server and starts a new one.
pub struct LlamaManager {
    inner: tokio::sync::Mutex<Option<Running>>,
    state: std::sync::Mutex<ServerState>,
    /// Pidfile tracking spawned servers across app runs (crash recovery).
    pids: PathBuf,
    /// Effective context reported by `/props` for models loaded this run.
    effective_contexts: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Hardware probe shared by local model launches.
    hardware: std::sync::OnceLock<Hardware>,
    /// A fixed context for special-purpose sidecars; coding models use an
    /// adaptive context derived from model metadata and available hardware.
    context: Option<u64>,
    /// Present only for the dedicated title sidecar.
    title_resources: Option<std::sync::RwLock<trouve_protocol::TitleModelResourcePolicy>>,
    /// Adaptive title placement avoids the GPU while the local coding-model
    /// sidecar is loading or running.
    adaptive_peer: Option<std::sync::Weak<LlamaManager>>,
    /// The coding-model manager uses this to evict an adaptive title sidecar
    /// from the GPU before beginning a local-model load.
    adaptive_title:
        std::sync::Mutex<Option<std::sync::Weak<crate::title_model::TitleModelManager>>>,
}

/// Restores an honest stopped state if a caller cancels `ensure` while the
/// child is loading (the title path intentionally has a short time budget).
struct StartingGuard<'a> {
    manager: &'a LlamaManager,
    armed: bool,
}

impl Drop for StartingGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.manager.set_state(ServerState::Stopped);
        }
    }
}

impl LlamaManager {
    /// Build the manager and reap any llama-server left over from a previous
    /// run that ended without cleanup (crash/SIGKILL) — leaked servers keep
    /// multi-GB VRAM allocations alive and starve the next load.
    pub fn new(data_dir: &Path) -> Self {
        Self::configured(data_dir, "llama-server.pids", None, None, None)
    }

    /// Independent short-context sidecar used only for session title
    /// generation.
    pub fn title(
        data_dir: &Path,
        resources: trouve_protocol::TitleModelResourcePolicy,
        local_model: std::sync::Weak<LlamaManager>,
    ) -> Self {
        Self::configured(
            data_dir,
            "title-llama-server.pids",
            Some(TITLE_MODEL_CONTEXT),
            Some(resources),
            Some(local_model),
        )
    }

    fn configured(
        data_dir: &Path,
        pidfile: &str,
        context: Option<u64>,
        title_resources: Option<trouve_protocol::TitleModelResourcePolicy>,
        adaptive_peer: Option<std::sync::Weak<LlamaManager>>,
    ) -> Self {
        let pids = data_dir.join(pidfile);
        Self::reap_stale(&pids, data_dir);
        Self {
            inner: tokio::sync::Mutex::new(None),
            state: std::sync::Mutex::new(ServerState::Stopped),
            pids,
            effective_contexts: std::sync::Mutex::new(std::collections::HashMap::new()),
            hardware: std::sync::OnceLock::new(),
            context,
            title_resources: title_resources.map(std::sync::RwLock::new),
            adaptive_peer,
            adaptive_title: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn set_adaptive_title(
        &self,
        title_model: std::sync::Weak<crate::title_model::TitleModelManager>,
    ) {
        *self.adaptive_title.lock().unwrap() = Some(title_model);
    }

    pub fn set_title_resources(&self, resources: trouve_protocol::TitleModelResourcePolicy) {
        if let Some(current) = &self.title_resources {
            *current.write().unwrap() = resources;
        }
    }

    /// Kill every pid the pidfile lists, provided it still looks like one of
    /// ours (its command line names llama-server under this data dir — a
    /// recycled pid must never take down an innocent process).
    fn reap_stale(pids_file: &Path, data_dir: &Path) {
        let stale = read_pids(pids_file);
        if stale.is_empty() {
            return;
        }
        let data_dir = data_dir.to_string_lossy();
        for pid in &stale {
            let Some(cmd) = process_cmdline(*pid) else {
                continue; // Already gone.
            };
            if cmd.contains("llama-server") && cmd.contains(data_dir.as_ref()) {
                tracing::info!("reaping stale llama-server (pid {pid}) from a previous run");
                kill_pid(*pid);
            }
        }
        write_pids(pids_file, &[]);
    }

    fn pids_add(&self, pid: Option<u32>) {
        if let Some(pid) = pid {
            let mut pids = read_pids(&self.pids);
            if !pids.contains(&pid) {
                pids.push(pid);
                write_pids(&self.pids, &pids);
            }
        }
    }

    fn pids_remove(&self, pid: Option<u32>) {
        if let Some(pid) = pid {
            let mut pids = read_pids(&self.pids);
            pids.retain(|p| *p != pid);
            write_pids(&self.pids, &pids);
        }
    }
    /// Sidecar state (non-blocking; safe to poll during a model load).
    pub fn state(&self) -> ServerState {
        self.state.lock().unwrap().clone()
    }

    /// Model id currently being served or loaded, if any.
    pub fn running_model(&self) -> Option<String> {
        match self.state() {
            ServerState::Stopped => None,
            ServerState::Starting(m) | ServerState::Running(m) => Some(m),
        }
    }

    pub fn context_window(&self, model_id: &str) -> Option<u64> {
        self.effective_contexts
            .lock()
            .unwrap()
            .get(model_id)
            .copied()
    }

    fn set_state(&self, state: ServerState) {
        *self.state.lock().unwrap() = state;
    }

    pub async fn stop(&self) {
        if let Some(mut running) = self.inner.lock().await.take() {
            let pid = running.child.id();
            let _ = running.child.kill().await;
            self.pids_remove(pid);
        }
        self.set_state(ServerState::Stopped);
    }

    /// Make sure llama-server is up and serving `model_id`; returns the
    /// OpenAI-compatible base URL. Blocks while the model loads (large
    /// GGUFs take a while on first start).
    pub async fn ensure(
        &self,
        bin: &Path,
        model_id: &str,
        gguf: &Path,
        log_path: &Path,
    ) -> Result<String> {
        let mut inner = self.inner.lock().await;
        let activating_local_model =
            self.title_resources.is_none() && self.state() == ServerState::Stopped;
        if let Some(running) = inner.as_mut() {
            // try_wait: a crashed server should be restarted, not reused.
            if running.model_id == model_id && running.child.try_wait()?.is_none() {
                return Ok(format!("http://127.0.0.1:{}/v1", running.port));
            }
            let pid = running.child.id();
            let _ = running.child.kill().await;
            self.pids_remove(pid);
            *inner = None;
        }
        self.set_state(ServerState::Starting(model_id.to_string()));
        let mut starting = StartingGuard {
            manager: self,
            armed: true,
        };
        if activating_local_model {
            let adaptive_title = self.adaptive_title.lock().unwrap().clone();
            if let Some(title_model) = adaptive_title.and_then(|manager| manager.upgrade()) {
                title_model.yield_to_local_model().await;
            }
        }
        match self.spawn_and_wait(bin, gguf, log_path).await {
            Ok((port, child, context_window)) => {
                if context_window > 0 {
                    self.effective_contexts
                        .lock()
                        .unwrap()
                        .insert(model_id.to_string(), context_window);
                }
                self.set_state(ServerState::Running(model_id.to_string()));
                starting.armed = false;
                *inner = Some(Running {
                    model_id: model_id.to_string(),
                    port,
                    child,
                });
                Ok(format!("http://127.0.0.1:{port}/v1"))
            }
            Err(error) => {
                if activating_local_model {
                    let adaptive_title = self.adaptive_title.lock().unwrap().clone();
                    if let Some(title_model) = adaptive_title.and_then(|manager| manager.upgrade())
                    {
                        title_model.local_model_stopped().await;
                    }
                }
                Err(error)
            }
        }
    }

    /// Spawn llama-server and wait for /health; returns the bound port and
    /// child on success.
    async fn spawn_and_wait(
        &self,
        bin: &Path,
        gguf: &Path,
        log_path: &Path,
    ) -> Result<(u16, tokio::process::Child, u64)> {
        let requested_context = self.context.unwrap_or_else(|| {
            let native_context = model_metadata(gguf).context_window;
            let model_size = std::fs::metadata(gguf)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let hardware = self.hardware.get_or_init(probe_hardware);
            launch_context(native_context, model_size, hardware)
        });
        let port = free_port()?;
        let log = std::fs::File::create(log_path)
            .with_context(|| format!("creating {}", log_path.display()))?;
        let bin = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.arg("-m")
            .arg(gguf)
            .args(["--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            // Loading a model's full native 128k–1M context can exhaust the
            // machine's KV-cache budget. Clamp it to a conservative estimate
            // derived from the memory left after loading the weights.
            .arg("-c")
            .arg(requested_context.to_string())
            // No -ngl: llama.cpp then auto-fits n_gpu_layers to *free* VRAM,
            // spilling gracefully to CPU instead of into GTT/system memory
            // over PCIe (forcing 999 disables the fit and runs at ~5 tok/s
            // when another process holds VRAM).
            // Jinja chat templating enables OpenAI-style tool calling.
            .arg("--jinja")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log))
            .kill_on_drop(true);
        if let Some(resources) = &self.title_resources {
            // Title generation is serialized, so a single slot keeps the
            // shared prompt prefix hot without provisioning unused parallel
            // slots.
            cmd.args(["-np", "1", "--cache-prompt", "--no-ui"]);

            let configured = *resources.read().unwrap();
            let local_model_active = self
                .adaptive_peer
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .is_some_and(|manager| manager.state() != ServerState::Stopped);
            let effective = effective_title_resources(configured, local_model_active);
            match effective {
                trouve_protocol::TitleModelResourcePolicy::CpuRamOnly => {
                    // `-ngl 0` prevents layer offload; `--device none` also
                    // disables backend operations that can otherwise still
                    // touch Vulkan, Metal, or another accelerator.
                }
                trouve_protocol::TitleModelResourcePolicy::GpuOnly => {
                    let hardware = self.hardware.get_or_init(probe_hardware);
                    if hardware.gpus.is_empty() {
                        bail!("GPU-only session naming requires a detected GPU");
                    }
                    // Disable llama.cpp's fit adjustment so an undersized GPU
                    // fails instead of silently spilling model layers to RAM.
                }
                trouve_protocol::TitleModelResourcePolicy::GpuCpuRam => {
                    // llama.cpp's defaults auto-fit layers to currently free
                    // VRAM and spill the remainder to CPU/system RAM.
                }
                trouve_protocol::TitleModelResourcePolicy::Adaptive => {
                    unreachable!("adaptive title resources are resolved above")
                }
            }
            cmd.args(title_resource_args(effective));
        }
        // The release tarballs carry their shared libraries next to the
        // binary; rpath usually covers it, but belt and braces.
        if let Some(dir) = bin.parent() {
            let key = if std::env::consts::OS == "macos" {
                "DYLD_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            };
            let mut val = dir.as_os_str().to_os_string();
            if let Some(existing) = std::env::var_os(key) {
                val.push(":");
                val.push(existing);
            }
            cmd.env(key, val);
        }
        let mut child = trouve_process::with_spawn_lock(|| cmd.spawn())
            .with_context(|| format!("spawning {}", bin.display()))?;
        // Into the pidfile before anything can go wrong: a crash during the
        // multi-minute model load must still leave a trail to reap. (Capture
        // the pid now — Child::id() is None once the process is reaped.)
        let pid = child.id();
        self.pids_add(pid);

        // Wait for /health to go 200 (503 while the model loads).
        let url = format!("http://127.0.0.1:{port}/health");
        let http = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            if let Some(status) = child.try_wait()? {
                self.pids_remove(pid);
                bail!(
                    "llama-server exited during startup ({status}); log tail:\n{}",
                    log_tail(log_path)
                );
            }
            if let Ok(resp) = http.get(&url).send().await
                && resp.status().is_success()
            {
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill().await;
                self.pids_remove(pid);
                bail!(
                    "llama-server did not become healthy within 5 minutes; log tail:\n{}",
                    log_tail(log_path)
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        let context_window = http
            .get(format!("http://127.0.0.1:{port}/props"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .ok()
            .and_then(|resp| resp.error_for_status().ok());
        let props = match context_window {
            Some(resp) => resp.json::<serde_json::Value>().await.ok(),
            None => None,
        };
        let context_window = effective_context_window(props.as_ref(), requested_context);

        Ok((port, child, context_window))
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Conservative context ceiling from the memory left after model weights.
/// A 128 KiB/token KV estimate covers common 7B–70B GQA architectures; the
/// native GGUF value remains the hard upper bound.
fn launch_context(native_context: u64, model_size: u64, hardware: &Hardware) -> u64 {
    const KV_BYTES_PER_TOKEN: u64 = 128 * 1024;
    const FALLBACK_CONTEXT: u64 = 8 * 1024;
    const MIN_CONTEXT: u64 = 512;

    let weights = model_size.saturating_add(model_size / 7);
    let gpu_budget = hardware
        .gpus
        .iter()
        .map(|gpu| gpu.vram_bytes)
        .filter(|budget| *budget > weights)
        .max();
    let cpu_budget =
        (hardware.ram_bytes * 85 / 100 > weights).then_some(hardware.ram_bytes * 85 / 100);
    let ceiling = gpu_budget
        .or(cpu_budget)
        .map(|budget| budget.saturating_sub(weights) / KV_BYTES_PER_TOKEN)
        .filter(|tokens| *tokens > 0)
        .unwrap_or(FALLBACK_CONTEXT)
        .max(MIN_CONTEXT);

    if native_context > 0 {
        native_context.min(ceiling)
    } else {
        ceiling
    }
}

fn effective_context_window(props: Option<&serde_json::Value>, launched_context: u64) -> u64 {
    props
        .and_then(|props| {
            props
                .pointer("/default_generation_settings/n_ctx")
                .and_then(serde_json::Value::as_u64)
        })
        .filter(|context| *context > 0)
        .unwrap_or(launched_context)
}

fn log_tail(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

// --- provider ----------------------------------------------------------------

/// The built-in "local" provider: downloaded GGUFs served by the managed
/// llama-server. Registered unconditionally; it just lists no models until
/// something is downloaded.
pub struct LocalProvider {
    data_dir: PathBuf,
    config_dir: Option<PathBuf>,
    manager: Arc<LlamaManager>,
}

impl LocalProvider {
    pub fn new(data_dir: PathBuf, config_dir: Option<PathBuf>, manager: Arc<LlamaManager>) -> Self {
        Self {
            data_dir,
            config_dir,
            manager,
        }
    }

    fn runtime_bin(&self) -> Option<PathBuf> {
        runtime_bin(&self.data_dir)
    }

    fn downloaded_entries(&self) -> Vec<ModelEntry> {
        all_entries(self.config_dir.as_deref())
            .into_iter()
            .filter(|e| gguf_path(&self.data_dir, e).exists())
            .collect()
    }
}

/// Options schema for one local model — the composer's thinking dropdown
/// renders from this (`thinking_level` / `reasoning_effort` are the keys
/// clients look for).
pub fn options_schema(thinking: Thinking) -> serde_json::Value {
    match thinking {
        Thinking::None => serde_json::json!({}),
        Thinking::Toggle => serde_json::json!({
            "type": "object",
            "properties": {
                "thinking_level": {
                    "type": "string",
                    "enum": ["off", "on"],
                    "default": "on",
                    "description": "Whether the model thinks before answering"
                }
            }
        }),
        Thinking::Effort => serde_json::json!({
            "type": "object",
            "properties": {
                "reasoning_effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high"],
                    "default": "medium",
                    "description": "How much thinking the model does before answering"
                }
            }
        }),
    }
}

/// Fold the thread's thinking option into llama.cpp `chat_template_kwargs`.
/// The thinking keys are always stripped (model swaps can leave a stale key
/// from the previous model's schema); only the supported kwarg is re-added.
fn apply_thinking_options(
    thinking: Thinking,
    options: &mut serde_json::Map<String, serde_json::Value>,
) {
    let effort = options.remove("reasoning_effort");
    let level = options.remove("thinking_level");
    let kwargs = match thinking {
        Thinking::None => None,
        Thinking::Effort => effort.map(|v| serde_json::json!({"reasoning_effort": v})),
        Thinking::Toggle => level
            .as_ref()
            .and_then(|v| v.as_str())
            .map(|v| serde_json::json!({"enable_thinking": v != "off"})),
    };
    if let Some(kwargs) = kwargs {
        options.insert("chat_template_kwargs".into(), kwargs);
    }
}

#[async_trait::async_trait]
impl Provider for LocalProvider {
    fn id(&self) -> &str {
        "local"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.downloaded_entries()
            .into_iter()
            .map(|e| {
                let path = gguf_path(&self.data_dir, &e);
                let metadata = model_metadata(&path);
                let context_window = self
                    .manager
                    .context_window(&e.id)
                    .unwrap_or(metadata.context_window);
                trouve_protocol::ModelInfo {
                    id: format!("local/{}", e.id),
                    display_name: format!(
                        "{} (local)",
                        metadata
                            .display_name
                            .as_deref()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or(&e.display_name)
                    ),
                    context_window,
                    // llama.cpp's --jinja path provides native or generic
                    // OpenAI-style function calling for chat models.
                    supports_tools: true,
                    input_price_per_mtok: Some(0.0),
                    output_price_per_mtok: Some(0.0),
                    options_schema: options_schema(metadata.thinking),
                }
            })
            .collect()
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[trouve_providers::Message],
        tools: &[trouve_providers::ToolSpec],
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<trouve_providers::EventStream, trouve_providers::ProviderError> {
        use trouve_providers::ProviderError;
        let entry = all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == model)
            .ok_or_else(|| ProviderError::Request(format!("unknown local model {model}")))?;
        let gguf = gguf_path(&self.data_dir, &entry);
        if !gguf.exists() {
            return Err(ProviderError::Request(format!(
                "model {model} is not downloaded — download it in Settings → Providers → Local"
            )));
        }
        let metadata = model_metadata(&gguf);
        let bin = self.runtime_bin().ok_or_else(|| {
            ProviderError::Request(
                "the llama.cpp runtime is not installed — install it in \
                 Settings → Providers → Local"
                    .into(),
            )
        })?;
        let log_path = self.data_dir.join("llama-server.log");
        let base_url = self
            .manager
            .ensure(&bin, &entry.id, &gguf, &log_path)
            .await
            .map_err(|e| ProviderError::Request(format!("starting llama-server: {e:#}")))?;

        let inner = trouve_providers::openai_compat::OpenAiCompatProvider::with_token(
            "local".to_string(),
            base_url,
            Arc::new(trouve_providers::auth::StaticToken(String::new())),
        );
        // Thinking knobs travel as template kwargs, not top-level fields.
        let mut options = options.clone();
        apply_thinking_options(metadata.thinking, &mut options);
        inner.stream_chat(model, messages, tools, &options).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_sane() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|c| c.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        for entry in CATALOG {
            assert!(entry.file.ends_with(".gguf"), "{}", entry.id);
            assert!(!entry.file.contains('/'), "split GGUFs unsupported");
            assert!(entry.size_bytes > 1_000_000_000, "{}", entry.id);
        }
    }

    #[test]
    fn fit_tiers() {
        let hw = Hardware {
            ram_bytes: 32 * 1024 * 1024 * 1024,
            gpus: vec![LocalGpu {
                name: "test".into(),
                vram_bytes: 10 * 1024 * 1024 * 1024,
            }],
        };
        // 4.7 GB model: ~7.4 GB needed, fits the 10 GB GPU.
        assert_eq!(fit(4_700_000_000, &hw), "gpu");
        // 16.8 GB model: ~21 GB needed; too big for VRAM, fits 85% of RAM.
        assert_eq!(fit(16_800_000_000, &hw), "cpu");
        // 40 GB model: needs ~48 GB, over both.
        assert_eq!(fit(40_000_000_000, &hw), "too-large");
        // No GPU at all: everything is CPU-or-nothing.
        let cpu_only = Hardware {
            ram_bytes: 8 * 1024 * 1024 * 1024,
            gpus: Vec::new(),
        };
        assert_eq!(fit(2_100_000_000, &cpu_only), "cpu");
        assert_eq!(fit(12_000_000_000, &cpu_only), "too-large");
    }

    #[test]
    fn launch_context_respects_native_and_memory_limits() {
        let hw = Hardware {
            ram_bytes: 16 * 1024 * 1024 * 1024,
            gpus: Vec::new(),
        };
        let model_size = 8 * 1024 * 1024 * 1024;
        let ceiling = launch_context(1_000_000, model_size, &hw);
        assert!(ceiling < 1_000_000);
        assert_eq!(launch_context(4_096, model_size, &hw), 4_096);
    }

    #[test]
    fn adaptive_title_resources_avoid_an_active_local_model() {
        use trouve_protocol::TitleModelResourcePolicy::{Adaptive, CpuRamOnly, GpuCpuRam};

        assert_eq!(effective_title_resources(Adaptive, false), GpuCpuRam);
        assert_eq!(effective_title_resources(Adaptive, true), CpuRamOnly);
        assert_eq!(effective_title_resources(GpuCpuRam, true), GpuCpuRam);
        assert_eq!(effective_title_resources(CpuRamOnly, false), CpuRamOnly);
    }

    #[test]
    fn title_resource_arguments_enforce_strict_modes() {
        use trouve_protocol::TitleModelResourcePolicy::{CpuRamOnly, GpuCpuRam, GpuOnly};

        assert_eq!(
            title_resource_args(CpuRamOnly),
            ["-ngl", "0", "--device", "none"]
        );
        assert_eq!(
            title_resource_args(GpuOnly),
            ["-ngl", "all", "--fit", "off"]
        );
        assert!(title_resource_args(GpuCpuRam).is_empty());
    }

    #[test]
    fn launched_context_is_the_fallback_for_unusable_props() {
        const LAUNCHED: u64 = 32_768;
        for props in [
            None,
            Some(serde_json::json!({})),
            Some(serde_json::json!({
                "default_generation_settings": {"n_ctx": "invalid"}
            })),
            Some(serde_json::json!({
                "default_generation_settings": {"n_ctx": 0}
            })),
        ] {
            assert_eq!(effective_context_window(props.as_ref(), LAUNCHED), LAUNCHED);
        }
        assert_eq!(
            effective_context_window(
                Some(&serde_json::json!({
                    "default_generation_settings": {"n_ctx": 16_384}
                })),
                LAUNCHED
            ),
            16_384
        );
    }

    #[test]
    fn slugs_and_custom_models_round_trip() {
        assert_eq!(
            slug_from_file("Devstral-Small-2507-Q4_K_M.gguf"),
            "devstral-small-2507-q4-k-m"
        );
        assert_eq!(slug_from_file("sub/dir/My__Model.gguf"), "my-model");

        let tmp = tempfile::tempdir().unwrap();
        let path = custom_models_path(tmp.path());
        assert!(read_custom_models(&path).is_empty());
        let models = vec![CustomModel {
            id: "my-model".into(),
            display_name: "My Model".into(),
            repo: "me/My-GGUF".into(),
            file: "My__Model.gguf".into(),
            size_bytes: 123,
        }];
        write_custom_models(&path, &models).unwrap();
        assert_eq!(read_custom_models(&path), models);

        // Custom entries appear in all_entries and shadow by id.
        let entries = all_entries(Some(tmp.path()));
        let custom = entries.iter().find(|e| e.id == "my-model").unwrap();
        assert!(custom.custom);
        assert_eq!(entries.len(), CATALOG.len() + 1);
    }

    #[test]
    fn runtime_bin_requires_an_active_managed_install() {
        let tmp = tempfile::tempdir().unwrap();
        let stable = trouve_agents::install::managed_bin(
            tmp.path(),
            trouve_agents::install::CliId::LlamaServer,
        );
        std::fs::create_dir_all(stable.parent().unwrap()).unwrap();
        std::fs::write(&stable, b"unregistered llama-server").unwrap();

        // A binary at the conventional location is not enough: only the
        // managed install record is authoritative.
        assert_eq!(runtime_bin(tmp.path()), None);

        let version_dir = tmp.path().join("cli/llama-server/b123");
        std::fs::create_dir_all(&version_dir).unwrap();
        let binary = version_dir.join("llama-server");
        std::fs::write(&binary, b"managed llama-server").unwrap();
        let record = trouve_agents::install::InstalledCli {
            version: "b123".into(),
            bin: binary.to_string_lossy().into_owned(),
        };
        std::fs::create_dir_all(tmp.path().join("cli/llama-server")).unwrap();
        std::fs::write(
            tmp.path().join("cli/llama-server/installed.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        assert_eq!(runtime_bin(tmp.path()), Some(binary));
    }

    #[test]
    fn pidfile_round_trips_and_clears_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = pids_path(tmp.path());
        assert!(read_pids(&path).is_empty());
        write_pids(&path, &[123, 456]);
        assert_eq!(read_pids(&path), vec![123, 456]);
        write_pids(&path, &[]);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn reap_only_kills_processes_that_look_like_ours() {
        // A live process that is *not* a llama-server under our data dir
        // must survive a reap even when the pidfile (wrongly) lists it —
        // pids get recycled, and killing an innocent process is the one
        // unforgivable failure mode here.
        let tmp = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        let mut bystander = trouve_process::spawn(&mut command).unwrap();
        let path = pids_path(tmp.path());
        write_pids(&path, &[bystander.id()]);

        LlamaManager::reap_stale(&path, tmp.path());

        // Still alive (no exit status), and the pidfile is cleared.
        assert!(bystander.try_wait().unwrap().is_none());
        assert!(!path.exists());
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    #[test]
    fn context_and_thinking_come_from_gguf_metadata() {
        fn string(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        fn string_value(bytes: &mut Vec<u8>, key: &str, value: &str) {
            string(bytes, key);
            bytes.extend_from_slice(&8u32.to_le_bytes());
            string(bytes, value);
        }
        fn u32_value(bytes: &mut Vec<u8>, key: &str, value: u32) {
            string(bytes, key);
            bytes.extend_from_slice(&4u32.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes()); // tensors
        bytes.extend_from_slice(&4u64.to_le_bytes()); // metadata pairs
        string_value(&mut bytes, "general.architecture", "qwen3");
        string_value(&mut bytes, "general.name", "Metadata Test Model");
        u32_value(&mut bytes, "qwen3.context_length", 262_144);
        string_value(
            &mut bytes,
            "tokenizer.chat_template",
            "{% if enable_thinking %}<think>{% endif %}",
        );

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("model.gguf");
        std::fs::write(&path, bytes).unwrap();
        let metadata = model_metadata(&path);
        assert_eq!(
            metadata.display_name.as_deref(),
            Some("Metadata Test Model")
        );
        assert_eq!(metadata.context_window, 262_144);
        assert_eq!(metadata.thinking, Thinking::Toggle);
        let nested = tmp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let alias = nested.join("..").join("model.gguf");
        assert_eq!(model_metadata(&alias), metadata);
        let cache = GGUF_METADATA.get().unwrap().lock().unwrap();
        assert!(cache.contains_key(&std::fs::canonicalize(&path).unwrap()));
        assert!(!cache.contains_key(&alias));
    }

    #[test]
    fn options_schema_matches_derived_thinking() {
        assert_eq!(options_schema(Thinking::None), serde_json::json!({}));
        assert_eq!(
            options_schema(Thinking::Toggle)
                .pointer("/properties/thinking_level/enum")
                .unwrap(),
            &serde_json::json!(["off", "on"])
        );
        assert_eq!(
            options_schema(Thinking::Effort)
                .pointer("/properties/reasoning_effort/default")
                .unwrap(),
            &serde_json::json!("medium")
        );
    }

    #[test]
    fn thinking_options_become_template_kwargs() {
        // Effort rides through as reasoning_effort.
        let mut opts = serde_json::json!({"reasoning_effort": "high"})
            .as_object()
            .unwrap()
            .clone();
        apply_thinking_options(Thinking::Effort, &mut opts);
        assert_eq!(
            opts.get("chat_template_kwargs"),
            Some(&serde_json::json!({"reasoning_effort": "high"}))
        );
        assert!(!opts.contains_key("reasoning_effort"));

        // The toggle maps to enable_thinking.
        let mut opts = serde_json::json!({"thinking_level": "off"})
            .as_object()
            .unwrap()
            .clone();
        apply_thinking_options(Thinking::Toggle, &mut opts);
        assert_eq!(
            opts.get("chat_template_kwargs"),
            Some(&serde_json::json!({"enable_thinking": false}))
        );

        // Non-thinking models strip stale keys left by a model swap and
        // send no kwargs at all.
        let mut opts = serde_json::json!({"reasoning_effort": "high", "thinking_level": "on"})
            .as_object()
            .unwrap()
            .clone();
        apply_thinking_options(Thinking::None, &mut opts);
        assert!(opts.is_empty());
    }

    #[test]
    fn split_ggufs_are_detected() {
        assert!(is_split_gguf("model-q4_0-00001-of-00002.gguf"));
        assert!(is_split_gguf(
            "sub/dir/M-00003-of-00004.GGUF".to_lowercase().as_str()
        ));
        assert!(!is_split_gguf("model-q4_k_m.gguf"));
        assert!(!is_split_gguf("model-of-legends.gguf"));
        assert!(!is_split_gguf("readme.md"));
    }

    #[test]
    fn quants_parse_from_filenames() {
        assert_eq!(quant_of("qwen2.5-coder-7b-instruct-q4_k_m.gguf"), "Q4_K_M");
        assert_eq!(quant_of("Devstral-Small-2507-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_of("model.IQ2_XS.gguf"), "IQ2_XS");
        assert_eq!(quant_of("model-fp16.gguf"), "FP16");
        assert_eq!(quant_of("gpt-oss-20b-F16.gguf"), "F16");
        assert_eq!(quant_of("some-model.gguf"), "");
    }

    #[test]
    fn drm_probe_parses_sysfs_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let device = tmp.path().join("card0/device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("vendor"), "0x1002\n").unwrap();
        std::fs::write(device.join("mem_info_vram_total"), "17163091968\n").unwrap();
        // A connector dir that must be ignored.
        std::fs::create_dir_all(tmp.path().join("card0-DP-1")).unwrap();

        let gpus = probe_drm_gpus(tmp.path(), false);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "AMD GPU");
        assert_eq!(gpus[0].vram_bytes, 17163091968);
    }
}
