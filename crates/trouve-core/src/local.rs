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
    pub thinking: Thinking,
}

/// How a local model's reasoning is steered through its chat template.
/// There is no universal knob in llama.cpp — it's per model family, applied
/// via `chat_template_kwargs` on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    /// Plain instruct model: no thinking controls.
    None,
    /// On/off via the `enable_thinking` template kwarg (Qwen3-style).
    Toggle,
    /// low/medium/high via the `reasoning_effort` template kwarg (GPT-OSS).
    Effort,
}

/// Guess the thinking support of a user-added GGUF from its repo/filename
/// (catalog entries declare it explicitly). Conservative: only families
/// whose templates are known to take the kwargs.
pub fn thinking_support(repo: &str, file: &str) -> Thinking {
    let s = format!("{repo} {file}").to_ascii_lowercase();
    if s.contains("gpt-oss") {
        return Thinking::Effort;
    }
    // Qwen3-family hybrids honor enable_thinking; the Coder and Instruct
    // variants are non-thinking and their templates reject the kwarg.
    if s.contains("qwen3") && !s.contains("coder") && !s.contains("instruct") {
        return Thinking::Toggle;
    }
    Thinking::None
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
        thinking: Thinking::None,
    },
    CatalogEntry {
        id: "qwen2.5-coder-7b",
        display_name: "Qwen2.5 Coder 7B",
        repo: "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF",
        file: "qwen2.5-coder-7b-instruct-q4_k_m.gguf",
        size_bytes: 4_683_073_536,
        params: "7B",
        notes: "Best pick for 8 GB GPUs; solid completions and small tasks.",
        thinking: Thinking::None,
    },
    CatalogEntry {
        id: "gpt-oss-20b",
        display_name: "GPT-OSS 20B",
        repo: "ggml-org/gpt-oss-20b-GGUF",
        file: "gpt-oss-20b-mxfp4.gguf",
        size_bytes: 12_109_566_560,
        params: "21B MoE",
        notes: "OpenAI's open-weight model; strong reasoning and tool use.",
        thinking: Thinking::Effort,
    },
    CatalogEntry {
        id: "devstral-small-2507",
        display_name: "Devstral Small",
        repo: "mistralai/Devstral-Small-2507_gguf",
        file: "Devstral-Small-2507-Q4_K_M.gguf",
        size_bytes: 14_333_915_904,
        params: "24B",
        notes: "Mistral's coding-agent specialist; good at multi-file edits.",
        thinking: Thinking::None,
    },
    CatalogEntry {
        id: "qwen3.6-27b",
        display_name: "Qwen3.6 27B",
        repo: "unsloth/Qwen3.6-27B-GGUF",
        file: "Qwen3.6-27B-Q4_K_M.gguf",
        size_bytes: 16_817_244_384,
        params: "27B",
        notes: "Best all-round coding model for a single 24 GB GPU.",
        thinking: Thinking::Toggle,
    },
    CatalogEntry {
        id: "qwen3-coder-30b",
        display_name: "Qwen3 Coder 30B",
        repo: "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF",
        file: "Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf",
        size_bytes: 18_556_689_568,
        params: "30B MoE",
        notes: "Only 3B active parameters — usable even on CPU with enough RAM.",
        thinking: Thinking::None,
    },
];

/// Context window trouve serves every local model with. A fixed, honest
/// value: it is what `-c` is set to, what ModelInfo reports, and what
/// compaction budgets against. 32k balances capability against KV-cache
/// memory; models with smaller native windows are clamped by llama.cpp.
pub const SERVE_CONTEXT: u64 = 32_768;

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
    pub thinking: Thinking,
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
            thinking: c.thinking,
        })
        .collect();
    if let Some(dir) = config_dir {
        for custom in read_custom_models(&custom_models_path(dir)) {
            entries.retain(|e| e.id != custom.id);
            let thinking = thinking_support(&custom.repo, &custom.file);
            entries.push(ModelEntry {
                id: custom.id,
                display_name: custom.display_name,
                repo: custom.repo,
                file: custom.file,
                size_bytes: custom.size_bytes,
                params: String::new(),
                notes: String::new(),
                custom: true,
                thinking,
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

/// The llama-server binary to run: trouve-managed install first, PATH as
/// a fallback.
pub fn runtime_bin(data_dir: &Path) -> Option<PathBuf> {
    let managed =
        trouve_agents::install::managed_bin(data_dir, trouve_agents::install::CliId::LlamaServer);
    if managed.exists() {
        return Some(managed);
    }
    trouve_agents::install::find_on_path("llama-server")
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
    if let Ok(out) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
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
            let out = std::process::Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()
        }
        _ => None,
    }
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

/// Pidfile listing every llama-server this data dir has spawned and not yet
/// cleanly killed. `kill_on_drop` only covers graceful exits — an app crash
/// or SIGKILL leaves sidecars running (and holding VRAM), so the next start
/// reaps whatever the file still lists.
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
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()?;
        let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !cmd.is_empty()).then_some(cmd)
    }
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    #[cfg(not(unix))]
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status();
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

/// Owns the single llama-server sidecar. One model is loaded at a time;
/// asking for a different model stops the old server and starts a new one.
pub struct LlamaManager {
    inner: tokio::sync::Mutex<Option<Running>>,
    state: std::sync::Mutex<ServerState>,
    /// Pidfile tracking spawned servers across app runs (crash recovery).
    pids: PathBuf,
}

impl LlamaManager {
    /// Build the manager and reap any llama-server left over from a previous
    /// run that ended without cleanup (crash/SIGKILL) — leaked servers keep
    /// multi-GB VRAM allocations alive and starve the next load.
    pub fn new(data_dir: &Path) -> Self {
        let pids = pids_path(data_dir);
        Self::reap_stale(&pids, data_dir);
        Self {
            inner: tokio::sync::Mutex::new(None),
            state: std::sync::Mutex::new(ServerState::Stopped),
            pids,
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
        match self.spawn_and_wait(bin, gguf, log_path).await {
            Ok((port, child)) => {
                self.set_state(ServerState::Running(model_id.to_string()));
                *inner = Some(Running {
                    model_id: model_id.to_string(),
                    port,
                    child,
                });
                Ok(format!("http://127.0.0.1:{port}/v1"))
            }
            Err(e) => {
                self.set_state(ServerState::Stopped);
                Err(e)
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
    ) -> Result<(u16, tokio::process::Child)> {
        let port = free_port()?;
        let log = std::fs::File::create(log_path)
            .with_context(|| format!("creating {}", log_path.display()))?;
        let bin = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.arg("-m")
            .arg(gguf)
            .args(["--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .arg("-c")
            .arg(SERVE_CONTEXT.to_string())
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
        let mut child = cmd
            .spawn()
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

        Ok((port, child))
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
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
            .map(|e| trouve_protocol::ModelInfo {
                id: format!("local/{}", e.id),
                display_name: format!("{} (local)", e.display_name),
                context_window: SERVE_CONTEXT,
                supports_tools: true,
                input_price_per_mtok: Some(0.0),
                output_price_per_mtok: Some(0.0),
                options_schema: options_schema(e.thinking),
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
                "model {model} is not downloaded — download it in Settings → Local Models"
            )));
        }
        let bin = self.runtime_bin().ok_or_else(|| {
            ProviderError::Request(
                "the llama.cpp runtime is not installed — install it in \
                 Settings → Local Models"
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
        apply_thinking_options(entry.thinking, &mut options);
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
        let mut bystander = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
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
    fn thinking_support_is_guessed_from_repo_names() {
        // GPT-OSS takes effort levels; Qwen3 hybrids take the on/off kwarg.
        assert_eq!(
            thinking_support("ggml-org/gpt-oss-120b-GGUF", "gpt-oss-120b-mxfp4.gguf"),
            Thinking::Effort
        );
        assert_eq!(
            thinking_support("unsloth/Qwen3-14B-GGUF", "Qwen3-14B-Q4_K_M.gguf"),
            Thinking::Toggle
        );
        // Coder/Instruct Qwen3 variants don't think; unknown families get
        // no knobs at all.
        assert_eq!(
            thinking_support(
                "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF",
                "Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf"
            ),
            Thinking::None
        );
        assert_eq!(
            thinking_support("mistralai/Devstral-Small-2507_gguf", "d.gguf"),
            Thinking::None
        );
    }

    #[test]
    fn options_schema_matches_thinking_support() {
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
