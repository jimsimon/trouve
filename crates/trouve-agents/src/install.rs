//! Managed vendor agent-runtime installs.
//!
//! Downloads official vendor builds into trouve's data directory, so users
//! don't depend on system packages that may lag behind. The legacy `/v1/clis`
//! API name covers both CLIs and Cursor's standalone Agent SDK Bridge.
//!
//! Layout under `<data_dir>/cli/`:
//! - `<id>/.generations/…`    — immutable runtime generations
//! - `<id>/installed.json`    — pointer to the active version + binary
//! - `.leases/<id>/…`         — filesystem-wide generation lifetime locks
//!
//! `installed.json` is the single source used by both discovery and process
//! launches. Activation atomically replaces it only after a complete immutable
//! generation has been published.
//!
//! Sources (no custom mirrors):
//! - cursor-sdk-bridge: one independently reviewed GitHub `cursor/sdk-bridge`
//!   release whose per-platform digests are pinned below; the release's
//!   `SHA256SUMS.txt` is checked as corroborating metadata, not trusted alone
//! - claude: `downloads.claude.ai/claude-code-releases` (`latest` + manifest
//!   with sha256 checksums; single static binary)
//! - codex: GitHub `openai/codex` latest release tarball (musl build on Linux)

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest;

const MAX_TEXT_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RUNTIME_DOWNLOAD_BYTES: usize = 512 * 1024 * 1024;
/// Failed root-directory syncs retain every generation that could still be
/// named by the last durable pointer. Refuse another publication once this
/// safety set reaches a fixed bound instead of allowing an outage to grow it
/// without limit.
const MAX_RETAINED_RUNTIME_GENERATIONS: usize = 8;
const CANCEL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
const CURSOR_SDK_BRIDGE_REVIEWED_VERSION: &str = "1.0.28";

fn cursor_sdk_bridge_reviewed_checksum(version: &str, asset: &str) -> Option<&'static str> {
    if version != CURSOR_SDK_BRIDGE_REVIEWED_VERSION {
        return None;
    }
    match asset {
        "cursor-sdk-bridge-standalone-darwin-arm64.tar.gz" => {
            Some("52ebfdab4e7806270122bea6c8f972646516297343c483e6700b37d444515af5")
        }
        "cursor-sdk-bridge-standalone-darwin-x64.tar.gz" => {
            Some("ba59c6eaad62338118e59ceb6d24006e06f7c75b28e32dbc13950c4027511c3c")
        }
        "cursor-sdk-bridge-standalone-linux-arm64.tar.gz" => {
            Some("0222f5c60c88b82063a0547bd938945c777c2a470def69de6464c04470ae0560")
        }
        "cursor-sdk-bridge-standalone-linux-x64.tar.gz" => {
            Some("5357a42d3faa668a3ef25c6669fe576544b032dd17fabbbfa515355cd8d33c19")
        }
        "cursor-sdk-bridge-standalone-win32-x64.tar.gz" => {
            Some("8af767f8b60f48ccf9147ce89085cd1956a5a1b8c66d26ff078cc1bd193f2ebb")
        }
        _ => None,
    }
}

fn verify_cursor_sdk_bridge_digests(
    asset: &str,
    reviewed: &str,
    manifest: &str,
    actual: &str,
) -> Result<(), InstallError> {
    if manifest != reviewed {
        return Err(InstallError::Checksum(format!(
            "{asset} release manifest did not match Trouve's reviewed digest"
        )));
    }
    if actual != reviewed {
        return Err(InstallError::Checksum(asset.into()));
    }
    Ok(())
}

/// A vendor agent runtime trouve knows how to install. `id` doubles as the
/// binary name and the legacy API path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliId {
    CursorSdkBridge,
    Claude,
    Codex,
    /// llama.cpp's `llama-server` — the local-inference runtime behind the
    /// built-in "local" provider, not an agent CLI. Kept out of `ALL_CLIS`
    /// so the CLI settings list doesn't show it; the Providers → Local tab
    /// drives its install through the same `/v1/clis` machinery.
    LlamaServer,
}

pub const ALL_CLIS: [CliId; 3] = [CliId::CursorSdkBridge, CliId::Claude, CliId::Codex];

impl CliId {
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            // Keep the pre-SDK route as an input alias so older clients can
            // manage the replacement runtime through the compatibility API.
            "cursor-agent" | "cursor-sdk-bridge" => Some(Self::CursorSdkBridge),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "llama-server" => Some(Self::LlamaServer),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CursorSdkBridge => "cursor-sdk-bridge",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::LlamaServer => "llama-server",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::CursorSdkBridge => "Cursor Agent SDK",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::LlamaServer => "llama.cpp",
        }
    }

    /// Provider kinds this runtime serves (for surfacing next to providers).
    pub fn provider_kinds(&self) -> &'static [&'static str] {
        match self {
            Self::CursorSdkBridge => &["cursor-sdk"],
            Self::Claude => &["claude-cli"],
            Self::Codex => &["codex-app-server"],
            Self::LlamaServer => &["local"],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("unsupported platform: {0}")]
    Unsupported(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("checksum mismatch for {0}")]
    Checksum(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Shared byte-level progress for one download, readable while the
/// transfer runs. `total` is 0 until (unless) the server reports a
/// Content-Length. Setting `cancel` makes the transfer stop at the next
/// chunk with [`InstallError::Cancelled`].
#[derive(Debug, Default)]
pub struct Progress {
    pub received: std::sync::atomic::AtomicU64,
    pub total: std::sync::atomic::AtomicU64,
    pub cancel: std::sync::atomic::AtomicBool,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The active managed install of one runtime, persisted as `installed.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledCli {
    pub version: String,
    /// Absolute path of the executable inside the runtime generation.
    pub bin: String,
}

/// Result of atomically publishing a managed runtime pointer.
///
/// Directory sync is necessarily after the pointer rename. A failure at that
/// boundary cannot be reported as an ordinary uncommitted install error:
/// readers already observe the new runtime, but crash durability is unknown.
#[derive(Debug)]
#[must_use = "activation durability must be surfaced to the caller"]
pub enum ActivationOutcome {
    Durable(InstalledCli),
    CommittedNotDurable {
        installed: InstalledCli,
        warning: String,
    },
}

impl ActivationOutcome {
    pub fn into_parts(self) -> (InstalledCli, Option<String>) {
        match self {
            Self::Durable(installed) => (installed, None),
            Self::CommittedNotDurable { installed, warning } => (installed, Some(warning)),
        }
    }
}

/// Keeps one resolved managed runtime generation alive for as long as a
/// backend can still use the executable it selected. The shared filesystem
/// lock is visible to every Trouve process using the same data directory.
#[derive(Debug)]
pub struct RuntimeLease {
    _runtime: PathBuf,
    _lock: std::fs::File,
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        // Closing a locked descriptor is not enough when a concurrent fork
        // inherited the same open-file description before exec. Explicitly
        // unlock so backend retirement cannot leave uninstall transiently
        // blocked by an otherwise unrelated child launch.
        if let Err(error) = fs4::fs_std::FileExt::unlock(&self._lock) {
            tracing::warn!(
                runtime = %self._runtime.display(),
                %error,
                "failed to release managed runtime lease"
            );
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RuntimeContainerKind {
    Generation,
    Legacy,
}

impl RuntimeContainerKind {
    fn lease_directory(self) -> &'static str {
        match self {
            Self::Generation => "generations",
            Self::Legacy => "legacy",
        }
    }
}

fn cli_root(data_dir: &Path, id: CliId) -> PathBuf {
    data_dir.join("cli").join(id.as_str())
}

fn legacy_managed_bin_path(data_dir: &Path, id: CliId) -> PathBuf {
    data_dir.join("cli").join("bin").join(id.as_str())
}

fn installed_unlocked(data_dir: &Path, id: CliId) -> Option<InstalledCli> {
    let raw = std::fs::read_to_string(cli_root(data_dir, id).join("installed.json")).ok()?;
    let info: InstalledCli = serde_json::from_str(&raw).ok()?;
    Path::new(&info.bin).exists().then_some(info)
}

/// The managed install of `id`, if one is active and its binary exists.
pub fn installed(data_dir: &Path, id: CliId) -> Option<InstalledCli> {
    installed_unlocked(data_dir, id)
}

/// Resolve the active managed install and lease its executable generation.
/// The activation lock makes pointer selection and shared-lease acquisition
/// atomic with publication, reclamation, and uninstall in every process.
pub fn installed_with_lease(data_dir: &Path, id: CliId) -> Option<(InstalledCli, RuntimeLease)> {
    let _activation_lock = lock_runtime_activation(data_dir, id).ok()?;
    let info = installed_unlocked(data_dir, id)?;
    let bin = PathBuf::from(&info.bin);
    let (kind, runtime) = managed_runtime_container(data_dir, id, &bin)?;
    let lock = open_runtime_lease_file(data_dir, id, kind, &runtime).ok()?;
    fs4::fs_std::FileExt::lock_shared(&lock).ok()?;
    Some((
        info,
        RuntimeLease {
            _runtime: runtime,
            _lock: lock,
        },
    ))
}

/// Legacy probe path. Launch generation-backed installs with
/// [`installed_with_lease`] so reclamation cannot invalidate the executable.
#[deprecated(note = "use installed_with_lease when launching a managed runtime")]
pub fn managed_bin(data_dir: &Path, id: CliId) -> PathBuf {
    legacy_managed_bin_path(data_dir, id)
}

fn http() -> Result<reqwest::Client, InstallError> {
    reqwest::Client::builder()
        .user_agent(concat!("trouve/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| InstallError::Download(e.to_string()))
}

async fn get_text_with_progress(url: &str, progress: &Progress) -> Result<String, InstallError> {
    get_text_controlled(url, Some(progress)).await
}

async fn wait_for_install_cancel(progress: Option<&Progress>) {
    let Some(progress) = progress else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        if progress.cancelled() {
            return;
        }
        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

async fn get_response(
    url: &str,
    progress: Option<&Progress>,
) -> Result<reqwest::Response, InstallError> {
    if progress.is_some_and(Progress::cancelled) {
        return Err(InstallError::Cancelled);
    }
    let request = http()?.get(url).send();
    let resp = tokio::select! {
        biased;
        _ = wait_for_install_cancel(progress) => return Err(InstallError::Cancelled),
        resp = request => resp.map_err(|e| InstallError::Download(format!("{url}: {e}")))?,
    };
    if !resp.status().is_success() {
        return Err(InstallError::Download(format!("{url}: {}", resp.status())));
    }
    Ok(resp)
}

async fn get_text_controlled(
    url: &str,
    progress: Option<&Progress>,
) -> Result<String, InstallError> {
    use futures::TryStreamExt as _;

    let resp = get_response(url, progress).await?;
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            biased;
            _ = wait_for_install_cancel(progress) => return Err(InstallError::Cancelled),
            next = stream.try_next() => next
                .map_err(|e| InstallError::Download(format!("{url}: {e}")))?,
        };
        let Some(chunk) = next else {
            break;
        };
        if out.len().saturating_add(chunk.len()) > MAX_TEXT_RESPONSE_BYTES {
            return Err(InstallError::Download(format!(
                "{url}: response exceeded {MAX_TEXT_RESPONSE_BYTES} bytes"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8(out)
        .map_err(|e| InstallError::Download(format!("{url}: response was not UTF-8: {e}")))
}

/// Download `url` fully into memory (runtime artifacts are tens of MB),
/// streaming chunks so `progress` stays live and cancellation can land
/// mid-transfer.
async fn get_bytes(url: &str, progress: &Progress, limit: usize) -> Result<Vec<u8>, InstallError> {
    use futures::TryStreamExt as _;
    use std::sync::atomic::Ordering::Relaxed;

    let resp = get_response(url, Some(progress)).await?;
    if let Some(len) = resp.content_length() {
        if len > limit as u64 {
            return Err(InstallError::Download(format!(
                "{url}: response exceeded {limit} bytes"
            )));
        }
        progress.total.store(len, Relaxed);
    }
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            biased;
            _ = wait_for_install_cancel(Some(progress)) => return Err(InstallError::Cancelled),
            next = stream.try_next() => next
                .map_err(|e| InstallError::Download(format!("{url}: {e}")))?,
        };
        let Some(chunk) = next else {
            break;
        };
        if out.len().saturating_add(chunk.len()) > limit {
            return Err(InstallError::Download(format!(
                "{url}: response exceeded {limit} bytes"
            )));
        }
        out.extend_from_slice(&chunk);
        progress.received.fetch_add(chunk.len() as u64, Relaxed);
    }
    Ok(out)
}

// --- version discovery -------------------------------------------------------

/// The newest version the vendor currently serves.
pub async fn latest_version(id: CliId) -> Result<String, InstallError> {
    latest_version_controlled(id, None).await
}

/// The newest version for an interactive managed-runtime install. Unlike the
/// background update check, release metadata lookup observes the same cancel
/// flag as the subsequent artifact download.
pub async fn latest_version_for_install(
    id: CliId,
    progress: &Progress,
) -> Result<String, InstallError> {
    latest_version_controlled(id, Some(progress)).await
}

async fn latest_version_controlled(
    id: CliId,
    progress: Option<&Progress>,
) -> Result<String, InstallError> {
    match id {
        // Managed execution stays on a release whose bytes were reviewed
        // independently of Cursor's mutable release assets. A newer Bridge is
        // promoted by updating this pin and its platform digests together.
        CliId::CursorSdkBridge => Ok(CURSOR_SDK_BRIDGE_REVIEWED_VERSION.into()),
        CliId::Claude => {
            let v = get_text_controlled(
                "https://downloads.claude.ai/claude-code-releases/latest",
                progress,
            )
            .await?;
            let v = v.trim().to_string();
            if v.chars().next().is_none_or(|c| !c.is_ascii_digit()) {
                return Err(InstallError::Download(format!(
                    "unexpected claude latest response: {v:.40}"
                )));
            }
            Ok(v)
        }
        CliId::Codex => {
            let tag = github_latest_tag("openai/codex", progress).await?;
            Ok(tag.trim_start_matches("rust-v").to_string())
        }
        // llama.cpp publishes binary builds as prereleases ("b9957"). Its
        // `/releases/latest` entry is now a metadata-only nightly marker, so
        // resolve the newest release that actually carries build artifacts.
        CliId::LlamaServer => {
            latest_llama_release_tag(|page| fetch_llama_release_page(page, progress)).await
        }
    }
}

async fn github_latest_tag(
    repo: &str,
    progress: Option<&Progress>,
) -> Result<String, InstallError> {
    github_latest_tag_url(
        &format!("https://api.github.com/repos/{repo}/releases/latest"),
        progress,
    )
    .await
}

async fn github_latest_tag_url(
    url: &str,
    progress: Option<&Progress>,
) -> Result<String, InstallError> {
    let body = get_text_controlled(url, progress).await?;
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| InstallError::Download(format!("github release json: {e}")))?;
    json["tag_name"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| InstallError::Download("github release had no tag_name".into()))
}

const LLAMA_RELEASE_PAGE_LIMIT: usize = 5;

async fn fetch_llama_release_page(
    page: usize,
    progress: Option<&Progress>,
) -> Result<String, InstallError> {
    get_text_controlled(
        &format!(
            "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=100&page={page}"
        ),
        progress,
    )
    .await
}

async fn latest_llama_release_tag<F, Fut>(mut fetch: F) -> Result<String, InstallError>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<String, InstallError>>,
{
    for page in 1..=LLAMA_RELEASE_PAGE_LIMIT {
        if let Some(tag) = parse_llama_release_tag(&fetch(page).await?)? {
            return Ok(tag);
        }
    }
    Err(InstallError::Download(format!(
        "the first {LLAMA_RELEASE_PAGE_LIMIT} github release pages had no llama.cpp build"
    )))
}

fn parse_llama_release_tag(body: &str) -> Result<Option<String>, InstallError> {
    let releases: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| InstallError::Download(format!("github release json: {e}")))?;
    Ok(releases
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|release| {
            let tag = release["tag_name"].as_str()?;
            let build = tag.strip_prefix('b')?;
            if build.is_empty()
                || !build.bytes().all(|byte| byte.is_ascii_digit())
                || release["draft"].as_bool().unwrap_or(false)
            {
                return None;
            }
            let artifact_prefix = format!("llama-{tag}-bin-");
            release["assets"]
                .as_array()?
                .iter()
                .any(|asset| {
                    asset["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with(&artifact_prefix))
                })
                .then(|| tag.to_string())
        }))
}

// --- platform mapping --------------------------------------------------------

fn cursor_platform() -> Result<(&'static str, &'static str), InstallError> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    Ok((os, arch))
}

fn cursor_sdk_bridge_platform() -> Result<(&'static str, &'static str), InstallError> {
    cursor_sdk_bridge_platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn cursor_sdk_bridge_platform_for(
    operating_system: &str,
    architecture: &str,
) -> Result<(&'static str, &'static str), InstallError> {
    if operating_system == "windows" && architecture == "aarch64" {
        return Err(InstallError::Unsupported("windows/aarch64".into()));
    }
    let os = match operating_system {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "win32",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    let arch = match architecture {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    Ok((os, arch))
}

fn claude_platform() -> Result<String, InstallError> {
    let (os, arch) = cursor_platform()?; // same os/arch token scheme
    Ok(format!("{os}-{arch}"))
}

/// Release-asset platform token for llama.cpp builds. On Linux, prefer the
/// Vulkan build when the Vulkan loader is present (works across NVIDIA/AMD/
/// Intel through the installed GPU driver); plain CPU builds otherwise.
/// macOS builds ship with Metal support built in.
fn llama_platform() -> Result<String, InstallError> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    match std::env::consts::OS {
        "macos" => Ok(format!("macos-{arch}")),
        "linux" if linux_has_vulkan_loader() => Ok(format!("ubuntu-vulkan-{arch}")),
        "linux" => Ok(format!("ubuntu-{arch}")),
        other => Err(InstallError::Unsupported(other.into())),
    }
}

/// Whether libvulkan is available on this Linux system (via ldconfig's
/// cache or the usual lib directories).
fn linux_has_vulkan_loader() -> bool {
    let mut command = std::process::Command::new("ldconfig");
    command
        .arg("-p")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Ok(out) =
        trouve_process::spawn(&mut command).and_then(std::process::Child::wait_with_output)
        && String::from_utf8_lossy(&out.stdout).contains("libvulkan.so.1")
    {
        return true;
    }
    [
        "/usr/lib/libvulkan.so.1",
        "/usr/lib64/libvulkan.so.1",
        "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

fn codex_triple() -> Result<String, InstallError> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    let sys = match std::env::consts::OS {
        // musl builds run on any distro regardless of glibc version.
        "linux" => "unknown-linux-musl",
        "macos" => "apple-darwin",
        other => return Err(InstallError::Unsupported(other.into())),
    };
    Ok(format!("{arch}-{sys}"))
}

// --- install -----------------------------------------------------------------

/// A verified runtime artifact that has been downloaded and unpacked without
/// changing the active managed runtime. Dropping it before activation removes
/// its staging directory.
#[derive(Debug)]
pub struct PreparedInstall {
    data_dir: PathBuf,
    id: CliId,
    version: String,
    stage: PathBuf,
    bin_rel: PathBuf,
}

impl PreparedInstall {
    /// Move the prepared runtime into an immutable generation, then atomically
    /// publish the metadata pointer used by both discovery and launches.
    pub fn activate(self) -> Result<ActivationOutcome, InstallError> {
        self.activate_with_checkpoint(|_| Ok(()))
    }

    /// Activate only if the originating install is still live at the pointer
    /// commit boundary. Preparation can finish concurrently with a late cancel,
    /// so checking inside the activation lock is what prevents a cancelled
    /// operation from publishing `installed.json`.
    pub fn activate_cancellable(
        self,
        progress: &Progress,
    ) -> Result<ActivationOutcome, InstallError> {
        self.activate_with_checkpoint(|_| {
            if progress.cancelled() {
                Err(InstallError::Cancelled)
            } else {
                Ok(())
            }
        })
    }

    fn activate_with_checkpoint(
        self,
        mut checkpoint: impl FnMut(ActivationCheckpoint) -> Result<(), InstallError>,
    ) -> Result<ActivationOutcome, InstallError> {
        self.activate_with_checkpoint_and_sync(&mut checkpoint, sync_runtime_path)
    }

    fn activate_with_checkpoint_and_sync(
        self,
        checkpoint: &mut impl FnMut(ActivationCheckpoint) -> Result<(), InstallError>,
        mut sync_path: impl FnMut(&Path) -> std::io::Result<()>,
    ) -> Result<ActivationOutcome, InstallError> {
        let root = cli_root(&self.data_dir, self.id);
        let _activation_lock = lock_runtime_activation(&self.data_dir, self.id)?;
        let staged_bin = self.stage.join(&self.bin_rel);
        if !self.stage.is_dir() || !staged_bin.is_file() {
            return Err(InstallError::Download(format!(
                "prepared {} {} artifact is no longer available",
                self.id.as_str(),
                self.version
            )));
        }

        // A generation is never replaced in place. Therefore an interrupted
        // activation always leaves the old pointer's runtime intact, and the
        // final pointer rename can serve as the single commit point.
        let generations = root.join(".generations");
        std::fs::create_dir_all(&generations)?;
        ensure_runtime_generation_capacity(
            &self.data_dir,
            self.id,
            &root,
            &generations,
            &mut sync_path,
        )?;
        let generation = unique_runtime_path(&generations, &format!("runtime-{}", self.version))?;
        let bin = generation.join(&self.bin_rel);
        let info = InstalledCli {
            version: self.version.clone(),
            bin: bin.to_string_lossy().into_owned(),
        };
        // Establish the stable external lease inode before publication. A
        // committed pointer must never name a generation that another process
        // cannot protect from reclamation.
        drop(open_runtime_lease_file(
            &self.data_dir,
            self.id,
            RuntimeContainerKind::Generation,
            &generation,
        )?);
        let pointer = root.join("installed.json");

        // Build and flush the sole publication candidate before changing live
        // state. Until its atomic replacement, both discovery and launches
        // continue resolving the complete previous generation.
        let pointer_candidate = unique_runtime_path(&root, "installed.json.candidate")?;
        let _pointer_candidate_cleanup = PathCleanup::new(pointer_candidate.clone());
        let mut pointer_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pointer_candidate)?;
        std::io::Write::write_all(
            &mut pointer_file,
            serde_json::to_string_pretty(&info).unwrap().as_bytes(),
        )?;
        pointer_file.sync_all()?;
        drop(pointer_file);

        let mut generation_cleanup = PathCleanup::new(generation.clone());
        replace_runtime_file(&self.stage, &generation, false)?;
        // The pointer must never be reported durable while the runtime it
        // names exists only in volatile cache. Flush files before directories,
        // then flush the rename into the generations directory.
        sync_runtime_tree(&generation, &mut sync_path)?;
        sync_path(&generations)?;

        // The filesystem-wide activation lock couples the last pointer read,
        // publication, and reclamation to lease acquisition. A backend in any
        // process can therefore never select a generation in the gap before
        // an activation removes it.
        let previous = installed_unlocked(&self.data_dir, self.id);
        let previous_generation = previous
            .as_ref()
            .and_then(|install| runtime_container(&generations, Path::new(&install.bin)));
        let previous_legacy = previous.as_ref().and_then(|install| {
            runtime_container(&root, Path::new(&install.bin)).filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !name.starts_with('.'))
            })
        });
        let replacing_pointer = path_exists(&pointer)?;
        checkpoint(ActivationCheckpoint::BeforePointer)?;
        replace_runtime_file(&pointer_candidate, &pointer, replacing_pointer)?;

        // installed.json is the one atomically replaced commit marker. Disarm
        // generation cleanup immediately: a later directory-sync error cannot
        // roll the commit back or remove the runtime now named by the pointer.
        generation_cleanup.disarm();
        let durability_error = sync_path(&root).err();
        if durability_error.is_none() {
            // Reclamation is safe only after the new pointer is known durable.
            // Across one or more failed root syncs, the last durable pointer may
            // lag behind the visible pointer by multiple generations.
            prune_runtime_generations(
                &self.data_dir,
                self.id,
                &generations,
                &generation,
                previous_generation.as_deref(),
            );
            prune_old_versions(&self.data_dir, self.id, &root, previous_legacy.as_deref());
            remove_legacy_managed_bin_best_effort(&self.data_dir, self.id);
        }
        match durability_error {
            None => Ok(ActivationOutcome::Durable(info)),
            Some(error) => {
                let warning = format!(
                    "{} {} is active, but crash durability could not be confirmed: {error}; previous runtime generations were retained",
                    self.id.display_name(),
                    self.version
                );
                tracing::warn!(
                    runtime = self.id.as_str(),
                    path = %root.display(),
                    %error,
                    "managed runtime committed but its root directory could not be synced"
                );
                Ok(ActivationOutcome::CommittedNotDurable {
                    installed: info,
                    warning,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationCheckpoint {
    BeforePointer,
}

struct PathCleanup(Option<PathBuf>);

impl PathCleanup {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for PathCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.as_ref() {
            let _ = remove_path(path);
        }
    }
}

fn path_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Return the immediate child of `root` containing `bin`, if the recorded
/// executable is actually beneath that managed root.
fn runtime_container(root: &Path, bin: &Path) -> Option<PathBuf> {
    let relative = bin.strip_prefix(root).ok()?;
    let Component::Normal(name) = relative.components().next()? else {
        return None;
    };
    Some(root.join(name))
}

fn managed_runtime_container(
    data_dir: &Path,
    id: CliId,
    bin: &Path,
) -> Option<(RuntimeContainerKind, PathBuf)> {
    let root = cli_root(data_dir, id);
    let generations = root.join(".generations");
    if let Some(generation) = runtime_container(&generations, bin) {
        return Some((RuntimeContainerKind::Generation, generation));
    }
    runtime_container(&root, bin)
        .filter(|runtime| {
            runtime
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with('.'))
        })
        .map(|runtime| (RuntimeContainerKind::Legacy, runtime))
}

#[cfg(not(windows))]
fn replace_runtime_file(
    replacement: &Path,
    destination: &Path,
    _replacing_existing: bool,
) -> std::io::Result<()> {
    std::fs::rename(replacement, destination)
}

#[cfg(windows)]
fn replace_runtime_file(
    replacement: &Path,
    destination: &Path,
    replacing_existing: bool,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_WRITE_THROUGH, ReplaceFileW,
    };

    let replacement = replacement
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        if replacing_existing {
            ReplaceFileW(
                destination.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                replacement.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_runtime_path(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_runtime_path(path: &Path) -> std::io::Result<()> {
    if path.is_file() {
        std::fs::File::open(path)?.sync_all()
    } else {
        // Windows directory publication uses MOVEFILE_WRITE_THROUGH /
        // REPLACEFILE_WRITE_THROUGH; regular-file contents are still flushed
        // explicitly above.
        Ok(())
    }
}

fn sync_runtime_tree(
    path: &Path,
    sync_path: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        // Never follow archive-created links while traversing the managed
        // generation. Their directory entries are covered by the containing
        // directory sync.
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            sync_runtime_tree(&entry?.path(), sync_path)?;
        }
        sync_path(path)
    } else if metadata.is_file() {
        sync_path(path)
    } else {
        Ok(())
    }
}

fn runtime_generation_count(generations: &Path) -> std::io::Result<usize> {
    let mut retained = 0usize;
    for entry in std::fs::read_dir(generations)? {
        if entry?.file_type()?.is_dir() {
            retained += 1;
        }
    }
    Ok(retained)
}

fn ensure_runtime_generation_capacity(
    data_dir: &Path,
    id: CliId,
    root: &Path,
    generations: &Path,
    sync_path: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let retained = runtime_generation_count(generations)?;
    if retained < MAX_RETAINED_RUNTIME_GENERATIONS {
        return Ok(());
    }

    // Every generation published by this transaction has already had its
    // contents and `.generations` entry flushed. If the final root sync later
    // failed, retry durability for the currently visible pointer before
    // reclaiming anything. Once that succeeds, no crash can select an older
    // generation, although live backends may still retain one through leases.
    let active = installed_unlocked(data_dir, id)
        .and_then(|install| runtime_container(generations, Path::new(&install.bin)))
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "managed runtime retains {retained} recovery generations without a recoverable active pointer"
            ))
        })?;
    sync_runtime_tree(&active, sync_path)?;
    sync_path(generations)?;
    sync_path(root)?;
    prune_runtime_generations(data_dir, id, generations, &active, None);
    sync_path(generations)?;

    let retained = runtime_generation_count(generations)?;
    if retained >= MAX_RETAINED_RUNTIME_GENERATIONS {
        Err(std::io::Error::other(format!(
            "managed runtime retains {retained} leased recovery generations; refusing another activation until their users stop"
        )))
    } else {
        Ok(())
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn unique_runtime_path(parent: &Path, label: &str) -> std::io::Result<PathBuf> {
    for _ in 0..8 {
        let path = parent.join(format!(".{label}-{}", uuid::Uuid::new_v4().simple()));
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(path),
            Ok(_) => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("could not reserve a unique runtime path for {label}"),
    ))
}

fn runtime_activation_lock_path(data_dir: &Path, id: CliId) -> PathBuf {
    data_dir
        .join("cli")
        .join(".locks")
        .join(format!("{}.lock", id.as_str()))
}

fn runtime_lease_directory(data_dir: &Path, id: CliId, kind: RuntimeContainerKind) -> PathBuf {
    data_dir
        .join("cli")
        .join(".leases")
        .join(id.as_str())
        .join(kind.lease_directory())
}

fn open_runtime_lease_file(
    data_dir: &Path,
    id: CliId,
    kind: RuntimeContainerKind,
    runtime: &Path,
) -> std::io::Result<std::fs::File> {
    let path = runtime_lease_path(data_dir, id, kind, runtime)?;
    std::fs::create_dir_all(path.parent().expect("runtime lease has a parent"))?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn runtime_lease_path(
    data_dir: &Path,
    id: CliId,
    kind: RuntimeContainerKind,
    runtime: &Path,
) -> std::io::Result<PathBuf> {
    let runtime_name = runtime.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "managed runtime has no generation name: {}",
                runtime.display()
            ),
        )
    })?;
    let directory = runtime_lease_directory(data_dir, id, kind);
    let mut lease_name = runtime_name.to_os_string();
    lease_name.push(".lock");
    Ok(directory.join(lease_name))
}

fn lock_runtime_activation(data_dir: &Path, id: CliId) -> std::io::Result<std::fs::File> {
    use fs4::fs_std::FileExt as _;

    let lock_path = runtime_activation_lock_path(data_dir, id);
    std::fs::create_dir_all(lock_path.parent().expect("runtime lock has a parent"))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    Ok(lock)
}

impl Drop for PreparedInstall {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.stage);
    }
}

fn create_install_stage(root: &Path, version: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let stage = root.join(format!(
        ".stage-{version}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    // A UUID collision or externally created path is an error, never a path
    // this attempt is allowed to delete and claim.
    std::fs::create_dir(&stage)?;
    Ok(stage)
}

/// Download and verify `version` of `id` into a staging directory without
/// changing the active managed runtime. Call [`PreparedInstall::activate`]
/// only after any runtime-specific teardown is complete.
pub async fn prepare_install(
    data_dir: &Path,
    id: CliId,
    version: &str,
    progress: &Progress,
) -> Result<PreparedInstall, InstallError> {
    // `version` is scraped from vendor endpoints and also joined into
    // filesystem paths (version dir, staging dir, download URLs). A crafted
    // or compromised endpoint returning `1/../../../etc` would otherwise let
    // `remove_dir_all`/`rename` touch an arbitrary directory. Constrain it to
    // a strict, path-safe allowlist before it reaches the filesystem.
    let version = normalized_version(id, version).to_string();
    validate_version(&version)?;
    let root = cli_root(data_dir, id);
    // Stage into a unique sibling so failed or overlapping installs never
    // half-replace the active version or clean up another attempt's files.
    let stage = create_install_stage(&root, &version)?;

    let result = install_into(&stage, id, &version, progress).await;
    let bin_rel = match result {
        Ok(rel) => rel,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
    };

    Ok(PreparedInstall {
        data_dir: data_dir.to_path_buf(),
        id,
        version,
        stage,
        bin_rel,
    })
}

/// Download and activate `version` of `id` under `data_dir`. Returns the
/// activated install. Idempotent: re-installing the active version just
/// re-downloads and atomically replaces the active pointer. Byte progress lands in
/// `progress`, which also carries the cancel flag.
pub async fn install(
    data_dir: &Path,
    id: CliId,
    version: &str,
    progress: &Progress,
) -> Result<ActivationOutcome, InstallError> {
    prepare_install(data_dir, id, version, progress)
        .await?
        .activate_cancellable(progress)
}

/// Remove the managed install of `id` entirely, including legacy stable-path
/// artifacts. Binaries found on PATH are untouched — trouve only manages its
/// own copies.
pub fn uninstall(data_dir: &Path, id: CliId) -> std::io::Result<()> {
    let _activation_lock = lock_runtime_activation(data_dir, id)?;
    // Lease files live outside the runtime root, so they remain lockable while
    // that root is removed (including on Windows). Refuse the whole operation
    // before changing any path when another process can still use a runtime.
    let _runtime_leases = lock_all_runtime_leases_exclusive(data_dir, id)?;
    let link = legacy_managed_bin_path(data_dir, id);
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)?;
    }
    let root = cli_root(data_dir, id);
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    drop(_runtime_leases);
    remove_path(&data_dir.join("cli").join(".leases").join(id.as_str()))?;
    Ok(())
}

fn lock_all_runtime_leases_exclusive(
    data_dir: &Path,
    id: CliId,
) -> std::io::Result<Vec<std::fs::File>> {
    let mut leases = Vec::new();
    for kind in [
        RuntimeContainerKind::Generation,
        RuntimeContainerKind::Legacy,
    ] {
        let directory = runtime_lease_directory(data_dir, id, kind);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let lease = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())?;
            if !fs4::fs_std::FileExt::try_lock_exclusive(&lease)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!("managed {} runtime is still in use", id.as_str()),
                ));
            }
            leases.push(lease);
        }
    }
    Ok(leases)
}

fn remove_legacy_managed_bin_best_effort(data_dir: &Path, id: CliId) {
    let path = legacy_managed_bin_path(data_dir, id);
    if let Err(error) = remove_path(&path) {
        tracing::warn!(
            "managed runtime activation completed, but obsolete stable executable {} could not be removed: {error}",
            path.display()
        );
    }
}

/// Fetch and unpack one runtime into `dir`; returns the executable's path
/// relative to `dir`.
async fn install_into(
    dir: &Path,
    id: CliId,
    version: &str,
    progress: &Progress,
) -> Result<PathBuf, InstallError> {
    match id {
        CliId::CursorSdkBridge => {
            let (os, arch) = cursor_sdk_bridge_platform()?;
            let asset = format!("cursor-sdk-bridge-standalone-{os}-{arch}.tar.gz");
            let reviewed =
                cursor_sdk_bridge_reviewed_checksum(version, &asset).ok_or_else(|| {
                    InstallError::Unsupported(format!(
                        "Cursor SDK Bridge {version} ({asset}) has no independently reviewed digest"
                    ))
                })?;
            let base = format!("https://github.com/cursor/sdk-bridge/releases/download/v{version}");
            let sums = get_text_with_progress(&format!("{base}/SHA256SUMS.txt"), progress).await?;
            let url = format!("{base}/{asset}");
            let bytes = get_bytes(&url, progress, MAX_RUNTIME_DOWNLOAD_BYTES).await?;
            let manifest = checksum_for_asset(&sums, &asset).ok_or_else(|| {
                InstallError::Download(format!("SHA256SUMS.txt had no entry for {asset}"))
            })?;
            let actual = sha2::Sha256::digest(&bytes).iter().fold(
                String::with_capacity(64),
                |mut output, byte| {
                    write!(output, "{byte:02x}").expect("writing to String cannot fail");
                    output
                },
            );
            verify_cursor_sdk_bridge_digests(&asset, reviewed, &manifest, &actual)?;
            untar_gz(bytes, dir).await?;
            let executable = if cfg!(windows) {
                "cursor-sdk-bridge.exe"
            } else {
                "cursor-sdk-bridge"
            };
            let rel = PathBuf::from("bin").join(executable);
            if !dir.join(&rel).exists() {
                return Err(InstallError::Download(format!(
                    "{asset} had no {}",
                    rel.display()
                )));
            }
            make_executable(&dir.join(&rel))?;
            Ok(rel)
        }
        CliId::Claude => {
            let platform = claude_platform()?;
            let base = "https://downloads.claude.ai/claude-code-releases";
            let manifest =
                get_text_with_progress(&format!("{base}/{version}/manifest.json"), progress)
                    .await?;
            let manifest: serde_json::Value = serde_json::from_str(&manifest)
                .map_err(|e| InstallError::Download(format!("claude manifest: {e}")))?;
            let expected = manifest["platforms"][&platform]["checksum"]
                .as_str()
                .ok_or_else(|| InstallError::Unsupported(platform.clone()))?
                .to_string();
            let bytes = get_bytes(
                &format!("{base}/{version}/{platform}/claude"),
                progress,
                MAX_RUNTIME_DOWNLOAD_BYTES,
            )
            .await?;
            let actual = sha2::Sha256::digest(&bytes).iter().fold(
                String::with_capacity(64),
                |mut output, byte| {
                    write!(output, "{byte:02x}").expect("writing to String cannot fail");
                    output
                },
            );
            if actual != expected {
                return Err(InstallError::Checksum("claude".into()));
            }
            let rel = PathBuf::from("claude");
            std::fs::write(dir.join(&rel), bytes)?;
            make_executable(&dir.join(&rel))?;
            Ok(rel)
        }
        CliId::Codex => {
            let triple = codex_triple()?;
            let url = format!(
                "https://github.com/openai/codex/releases/download/rust-v{version}/codex-{triple}.tar.gz"
            );
            let bytes = get_bytes(&url, progress, MAX_RUNTIME_DOWNLOAD_BYTES).await?;
            untar_gz(bytes, dir).await?;
            let rel = PathBuf::from("codex");
            std::fs::rename(dir.join(format!("codex-{triple}")), dir.join(&rel))?;
            make_executable(&dir.join(&rel))?;
            Ok(rel)
        }
        CliId::LlamaServer => {
            let platform = llama_platform()?;
            let url = format!(
                "https://github.com/ggml-org/llama.cpp/releases/download/{version}/llama-{version}-bin-{platform}.tar.gz"
            );
            let bytes = get_bytes(&url, progress, MAX_RUNTIME_DOWNLOAD_BYTES).await?;
            untar_gz(bytes, dir).await?;
            // The tarball unpacks to `llama-<version>/` with llama-server and
            // its shared libraries side by side (rpath $ORIGIN).
            let rel = PathBuf::from(format!("llama-{version}")).join("llama-server");
            if !dir.join(&rel).exists() {
                return Err(InstallError::Download(
                    "llama.cpp archive had no llama-server binary".into(),
                ));
            }
            make_executable(&dir.join(&rel))?;
            Ok(rel)
        }
    }
}

fn checksum_for_asset(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let checksum = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name.rsplit('/').next() == Some(asset)
            && checksum.len() == 64
            && checksum
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
        .then(|| checksum.to_ascii_lowercase())
    })
}

fn normalized_version(id: CliId, version: &str) -> &str {
    if id == CliId::CursorSdkBridge {
        version.strip_prefix('v').unwrap_or(version)
    } else {
        version
    }
}

/// A version string safe to use as a path component and in a download URL.
fn validate_version(version: &str) -> Result<(), InstallError> {
    let ok = !version.is_empty()
        && version.len() <= 100
        && version != "."
        && version != ".."
        && !version.contains("..")
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(InstallError::Download(format!(
            "refusing unsafe version string: {version:.40}"
        )))
    }
}

/// Whether a tar entry path (or link target) stays within the extraction
/// root: relative, with no `..`, root, or drive-prefix components.
fn path_is_contained(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Unpack a gzipped tarball (already in memory) into `dir` off the async
/// runtime. Every entry is validated before extraction: paths that escape
/// `dir` (absolute or `..`) are rejected, and symlink/hardlink entries whose
/// target escapes are refused — otherwise a crafted archive could plant a
/// symlink and then write through it to an arbitrary location (tar-slip).
async fn untar_gz(bytes: Vec<u8>, dir: &Path) -> Result<(), InstallError> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), InstallError> {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        let mut archive = tar::Archive::new(decoder);
        for entry in archive
            .entries()
            .map_err(|e| InstallError::Download(format!("reading archive: {e}")))?
        {
            let mut entry =
                entry.map_err(|e| InstallError::Download(format!("archive entry: {e}")))?;
            let path = entry
                .path()
                .map_err(|e| InstallError::Download(format!("archive entry path: {e}")))?
                .into_owned();
            if !path_is_contained(&path) {
                return Err(InstallError::Download(format!(
                    "archive entry escapes the extraction directory: {}",
                    path.display()
                )));
            }
            if matches!(
                entry.header().entry_type(),
                tar::EntryType::Symlink | tar::EntryType::Link
            ) {
                match entry.link_name() {
                    Ok(Some(target)) if path_is_contained(&target) => {}
                    Ok(Some(target)) => {
                        return Err(InstallError::Download(format!(
                            "archive link {} points outside the extraction directory: {}",
                            path.display(),
                            target.display()
                        )));
                    }
                    Ok(None) => {
                        return Err(InstallError::Download(format!(
                            "archive link {} has no target",
                            path.display()
                        )));
                    }
                    Err(e) => {
                        return Err(InstallError::Download(format!("archive link name: {e}")));
                    }
                }
            }
            // unpack_in re-checks containment as a second layer and returns
            // false if it still refuses the path.
            if !entry
                .unpack_in(&dir)
                .map_err(|e| InstallError::Download(format!("unpacking entry: {e}")))?
            {
                return Err(InstallError::Download(format!(
                    "archive entry refused: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| InstallError::Download(format!("unpack task: {e}")))??;
    Ok(())
}

fn make_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = path;
    Ok(())
}

fn try_lock_runtime_exclusive(
    data_dir: &Path,
    id: CliId,
    kind: RuntimeContainerKind,
    runtime: &Path,
) -> std::io::Result<Option<std::fs::File>> {
    let lock = open_runtime_lease_file(data_dir, id, kind, runtime)?;
    if fs4::fs_std::FileExt::try_lock_exclusive(&lock)? {
        Ok(Some(lock))
    } else {
        Ok(None)
    }
}

/// Keep the active immutable generation, the exact previously active
/// generation, and every retired generation still leased by a backend. Orphans
/// are collected on a later activation after their last consumer drains.
fn prune_runtime_generations(
    data_dir: &Path,
    id: CliId,
    generations: &Path,
    active: &Path,
    previous: Option<&Path>,
) {
    let Ok(entries) = std::fs::read_dir(generations) else {
        return;
    };
    for generation in entries.flatten().map(|entry| entry.path()) {
        if !generation.is_dir() || generation == active || previous == Some(generation.as_path()) {
            continue;
        }
        // The activation lock prevents new shared leases while this exclusive
        // guard is held. A busy or unreadable lease is conservatively kept.
        let Ok(Some(lease)) =
            try_lock_runtime_exclusive(data_dir, id, RuntimeContainerKind::Generation, &generation)
        else {
            continue;
        };
        let lease_path =
            runtime_lease_path(data_dir, id, RuntimeContainerKind::Generation, &generation);
        if std::fs::remove_dir_all(&generation).is_ok() {
            drop(lease);
            if let Ok(lease_path) = lease_path {
                let _ = std::fs::remove_file(lease_path);
            }
        }
    }
}

/// During migration, keep only the exact legacy directory selected by the
/// previous pointer. Once both current and previous installs are generations,
/// all legacy version directories can be removed.
fn prune_old_versions(data_dir: &Path, id: CliId, root: &Path, previous: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for directory in entries.flatten().map(|entry| entry.path()) {
        if !directory.is_dir()
            || directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| name.starts_with('.'))
            || previous == Some(directory.as_path())
        {
            continue;
        }
        let Ok(Some(lease)) =
            try_lock_runtime_exclusive(data_dir, id, RuntimeContainerKind::Legacy, &directory)
        else {
            continue;
        };
        let lease_path = runtime_lease_path(data_dir, id, RuntimeContainerKind::Legacy, &directory);
        if std::fs::remove_dir_all(&directory).is_ok() {
            drop(lease);
            if let Ok(lease_path) = lease_path {
                let _ = std::fs::remove_file(lease_path);
            }
        }
    }
}

/// Resolve a bare command name to its full path via `$PATH` (absolute and
/// relative paths pass through when they exist).
pub fn find_on_path(command: &str) -> Option<PathBuf> {
    crate::process_env::find_executable(command)
}

/// Best-effort `<bin> --version` (first line, trimmed), for reporting the
/// version of runtimes found on PATH.
pub async fn binary_version(command: &str) -> Option<String> {
    let mut command = crate::process_env::tokio_command(command);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let child = trouve_process::with_spawn_lock(|| command.spawn())?;
        child.wait_with_output().await
    })
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cursor_sdk_release_checksum() {
        let sums = r#"
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  cursor-sdk-bridge-standalone-linux-x64.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *cursor-sdk-bridge-standalone-darwin-arm64.tar.gz
"#;
        assert_eq!(
            checksum_for_asset(sums, "cursor-sdk-bridge-standalone-linux-x64.tar.gz").as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(checksum_for_asset(sums, "missing.tar.gz"), None);
    }

    #[tokio::test]
    async fn release_metadata_fetch_observes_install_cancellation_before_headers() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let request_received = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let handler_received = request_received.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/release",
                    axum::routing::get(move || {
                        let handler_received = handler_received.clone();
                        async move {
                            handler_received.add_permits(1);
                            std::future::pending::<&'static str>().await
                        }
                    }),
                ),
            )
            .await
        });
        let progress = std::sync::Arc::new(Progress::default());
        let request_progress = progress.clone();
        let request = tokio::spawn(async move {
            github_latest_tag_url(
                &format!("http://{address}/release"),
                Some(&request_progress),
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            request_received.acquire(),
        )
        .await
        .expect("release-metadata request never reached the fixture")
        .unwrap()
        .forget();
        progress
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), request)
            .await
            .expect("cancelled release-metadata request stayed pending")
            .unwrap();
        assert!(matches!(result, Err(InstallError::Cancelled)));
        server.abort();
    }

    #[tokio::test]
    async fn runtime_download_enforces_the_streaming_byte_budget() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/runtime",
                    axum::routing::get(|| async {
                        axum::body::Body::from_stream(futures::stream::iter([
                            Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(b"123")),
                            Ok(bytes::Bytes::from_static(b"45")),
                        ]))
                    }),
                ),
            )
            .await
        });
        let progress = Progress::default();
        let error = get_bytes(&format!("http://{address}/runtime"), &progress, 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeded 4 bytes"));
        server.abort();
    }

    #[test]
    fn selects_llama_build_release_instead_of_latest_marker() {
        let releases = serde_json::json!([
            {
                "tag_name": "v0.3.0",
                "draft": false,
                "assets": [{ "name": "nightly-tag.txt" }]
            },
            {
                "tag_name": "b10665",
                "draft": false,
                "prerelease": true,
                "assets": [
                    { "name": "llama-b10665-bin-ubuntu-vulkan-x64.tar.gz" }
                ]
            }
        ]);

        assert_eq!(
            parse_llama_release_tag(&releases.to_string())
                .unwrap()
                .as_deref(),
            Some("b10665")
        );
    }

    #[tokio::test]
    async fn searches_later_llama_release_pages_for_builds() {
        let requested = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let pages = std::sync::Arc::new([
            serde_json::json!([{
                "tag_name": "v0.3.0",
                "draft": false,
                "assets": [{ "name": "nightly-tag.txt" }]
            }])
            .to_string(),
            serde_json::json!([{
                "tag_name": "b10665",
                "draft": false,
                "prerelease": true,
                "assets": [
                    { "name": "llama-b10665-bin-ubuntu-vulkan-x64.tar.gz" }
                ]
            }])
            .to_string(),
        ]);

        let tag = latest_llama_release_tag({
            let requested = requested.clone();
            move |page| {
                let requested = requested.clone();
                let pages = pages.clone();
                async move {
                    requested.lock().unwrap().push(page);
                    Ok(pages[page - 1].clone())
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(tag, "b10665");
        assert_eq!(*requested.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn version_validation_rejects_path_tricks() {
        for good in ["1.2.3", "2026.07.01-41b2de7", "b9957", "rust-v0.5.0"] {
            assert!(validate_version(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            ".",
            "..",
            "1/../../etc",
            "../evil",
            "a/b",
            "a\\b",
            "1 2",
            "x/..",
        ] {
            assert!(validate_version(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn cursor_sdk_versions_accept_one_release_tag_prefix() {
        assert_eq!(
            normalized_version(CliId::CursorSdkBridge, "v1.0.28"),
            "1.0.28"
        );
        assert_eq!(
            normalized_version(CliId::Codex, "rust-v0.5.0"),
            "rust-v0.5.0"
        );
    }

    #[test]
    fn tar_entry_containment_checks() {
        assert!(path_is_contained(Path::new("bin/cursor-sdk-bridge")));
        assert!(path_is_contained(Path::new("libllama.so.1")));
        assert!(!path_is_contained(Path::new("../escape")));
        assert!(!path_is_contained(Path::new("/etc/passwd")));
        assert!(!path_is_contained(Path::new("a/../../b")));
    }

    #[tokio::test]
    async fn untar_rejects_symlink_escape() {
        // Build a tarball whose first entry is a symlink pointing outside the
        // extraction dir, then a file "through" it — the classic tar-slip.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        let mut buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut buf);
            builder.append_link(&mut header, "link", "/tmp").unwrap();
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            enc.write_all(&buf).unwrap();
            enc.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let err = untar_gz(gz, dir.path()).await.unwrap_err();
        assert!(
            matches!(err, InstallError::Download(m) if m.contains("outside")),
            "expected a containment error"
        );
    }

    #[test]
    fn cli_ids_round_trip() {
        for id in ALL_CLIS {
            assert_eq!(CliId::parse(id.as_str()), Some(id));
        }
        assert_eq!(
            CliId::parse(CliId::LlamaServer.as_str()),
            Some(CliId::LlamaServer)
        );
        assert_eq!(CliId::parse("unknown"), None);
        assert_eq!(CliId::parse("cursor-agent"), Some(CliId::CursorSdkBridge));
    }

    #[test]
    fn cursor_sdk_rejects_windows_arm64_without_constructing_an_asset() {
        assert!(matches!(
            cursor_sdk_bridge_platform_for("windows", "aarch64"),
            Err(InstallError::Unsupported(platform)) if platform == "windows/aarch64"
        ));
        assert_eq!(
            cursor_sdk_bridge_platform_for("windows", "x86_64").unwrap(),
            ("win32", "x64")
        );
    }

    #[test]
    fn cursor_sdk_execution_trusts_only_reviewed_release_digests() {
        let asset = "cursor-sdk-bridge-standalone-linux-x64.tar.gz";
        let reviewed =
            cursor_sdk_bridge_reviewed_checksum(CURSOR_SDK_BRIDGE_REVIEWED_VERSION, asset);
        assert_eq!(
            reviewed,
            Some("5357a42d3faa668a3ef25c6669fe576544b032dd17fabbbfa515355cd8d33c19")
        );
        assert!(matches!(
            verify_cursor_sdk_bridge_digests(asset, reviewed.unwrap(), "00", "00"),
            Err(InstallError::Checksum(_))
        ));
        assert_eq!(cursor_sdk_bridge_reviewed_checksum("1.0.29", asset), None);
        assert_eq!(
            cursor_sdk_bridge_reviewed_checksum(
                CURSOR_SDK_BRIDGE_REVIEWED_VERSION,
                "cursor-sdk-bridge-standalone-plan9-x64.tar.gz"
            ),
            None
        );
    }

    #[test]
    #[allow(deprecated)]
    fn installed_reads_pointer_when_binary_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        std::fs::create_dir_all(root.join("1.0.0")).unwrap();
        let bin = root.join("1.0.0").join("codex");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::to_string(&InstalledCli {
                version: "1.0.0".into(),
                bin: bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let retired = legacy_managed_bin_path(tmp.path(), CliId::Codex);
        std::fs::create_dir_all(retired.parent().unwrap()).unwrap();
        std::fs::write(&retired, "obsolete runtime").unwrap();

        let info = installed(tmp.path(), CliId::Codex).unwrap();
        assert_eq!(info.version, "1.0.0");
        assert_eq!(managed_bin(tmp.path(), CliId::Codex), retired);

        // Pointer with a missing binary reports not installed.
        std::fs::remove_file(&bin).unwrap();
        assert!(installed(tmp.path(), CliId::Codex).is_none());
        assert_eq!(managed_bin(tmp.path(), CliId::Codex), retired);
    }

    #[test]
    fn prepared_install_does_not_publish_until_activation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let stage = root.join(".stage-2.0.0");
        let bin_rel = PathBuf::from("codex");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join(&bin_rel), "new runtime").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage,
            bin_rel,
        };

        assert!(installed(tmp.path(), CliId::Codex).is_none());
        let legacy_stable = legacy_managed_bin_path(tmp.path(), CliId::Codex);
        std::fs::create_dir_all(legacy_stable.parent().unwrap()).unwrap();
        std::fs::write(&legacy_stable, "obsolete runtime").unwrap();

        let ActivationOutcome::Durable(activated) = prepared.activate().unwrap() else {
            panic!("ordinary activation unexpectedly lacked durability");
        };

        assert_eq!(activated.version, "2.0.0");
        assert!(Path::new(&activated.bin).starts_with(root.join(".generations")));
        assert_eq!(
            std::fs::read_to_string(&activated.bin).unwrap(),
            "new runtime"
        );
        assert!(legacy_stable.symlink_metadata().is_err());
        assert_eq!(
            installed(tmp.path(), CliId::Codex).unwrap().version,
            "2.0.0"
        );
    }

    #[test]
    fn dropping_prepared_install_removes_staged_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let stage_a = create_install_stage(&root, "2.0.0").unwrap();
        let stage_b = create_install_stage(&root, "2.0.0").unwrap();
        assert_ne!(stage_a, stage_b);
        let prepared_a = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage: stage_a.clone(),
            bin_rel: PathBuf::from("codex"),
        };
        let prepared_b = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage: stage_b.clone(),
            bin_rel: PathBuf::from("codex"),
        };

        drop(prepared_b);

        assert!(!stage_b.exists());
        assert!(stage_a.exists());
        drop(prepared_a);
        assert!(!stage_a.exists());
    }

    #[test]
    fn missing_prepared_artifact_preserves_same_version_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let version_dir = root.join("2.0.0");
        let active_bin = version_dir.join("codex");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(&active_bin, "active runtime").unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::to_string(&InstalledCli {
                version: "2.0.0".into(),
                bin: active_bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();

        let stage = create_install_stage(&root, "2.0.0").unwrap();
        std::fs::write(stage.join("codex"), "prepared runtime").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage: stage.clone(),
            bin_rel: PathBuf::from("codex"),
        };
        std::fs::remove_dir_all(stage).unwrap();

        assert!(prepared.activate().is_err());
        assert_eq!(
            std::fs::read_to_string(&active_bin).unwrap(),
            "active runtime"
        );
        assert_eq!(
            installed(tmp.path(), CliId::Codex).unwrap().version,
            "2.0.0"
        );
    }

    #[test]
    fn failure_before_pointer_keeps_previous_runtime_authoritative() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let version_dir = root.join("2.0.0");
        let active_bin = version_dir.join("codex");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(&active_bin, "active runtime").unwrap();
        let previous_pointer = serde_json::to_string_pretty(&InstalledCli {
            version: "2.0.0".into(),
            bin: active_bin.to_string_lossy().into_owned(),
        })
        .unwrap();
        std::fs::write(root.join("installed.json"), &previous_pointer).unwrap();

        let stage = create_install_stage(&root, "2.0.0").unwrap();
        std::fs::write(stage.join("codex"), "prepared runtime").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage,
            bin_rel: PathBuf::from("codex"),
        };

        let result = prepared.activate_with_checkpoint(|checkpoint| {
            assert_eq!(checkpoint, ActivationCheckpoint::BeforePointer);
            let visible = installed(tmp.path(), CliId::Codex).unwrap();
            assert_eq!(visible.bin, active_bin.to_string_lossy());
            assert_eq!(
                std::fs::read_to_string(&visible.bin).unwrap(),
                "active runtime"
            );
            Err(InstallError::Io(std::io::Error::other(
                "injected publication failure",
            )))
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&active_bin).unwrap(),
            "active runtime"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("installed.json")).unwrap(),
            previous_pointer
        );
        assert_eq!(
            installed(tmp.path(), CliId::Codex).unwrap().version,
            "2.0.0"
        );
        assert_eq!(
            std::fs::read_dir(root.join(".generations"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn runtime_contents_are_synced_before_pointer_publication() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let generations = root.join(".generations");
        let stage = create_install_stage(&root, "2.0.0").unwrap();
        std::fs::create_dir_all(stage.join("lib")).unwrap();
        std::fs::write(stage.join("codex"), "runtime").unwrap();
        std::fs::write(stage.join("lib").join("helper"), "helper").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage,
            bin_rel: PathBuf::from("codex"),
        };
        let synced = std::cell::RefCell::new(Vec::<PathBuf>::new());
        let mut checkpoint = |checkpoint| {
            assert_eq!(checkpoint, ActivationCheckpoint::BeforePointer);
            let synced = synced.borrow();
            let generation = synced
                .iter()
                .find(|path| {
                    path.parent() == Some(generations.as_path())
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(".runtime-2.0.0-"))
                })
                .expect("generation directory was not synced before publication");
            for path in [
                generation.join("codex"),
                generation.join("lib").join("helper"),
                generation.join("lib"),
                generation.clone(),
                generations.clone(),
            ] {
                assert!(
                    synced.contains(&path),
                    "{} was not synced before publication",
                    path.display()
                );
            }
            assert!(!synced.contains(&root));
            Ok(())
        };

        assert!(matches!(
            prepared.activate_with_checkpoint_and_sync(&mut checkpoint, |path| {
                synced.borrow_mut().push(path.to_path_buf());
                Ok(())
            }),
            Ok(ActivationOutcome::Durable(_))
        ));
    }

    #[test]
    fn directory_sync_failure_after_pointer_reports_degraded_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let stage = create_install_stage(&root, "2.0.0").unwrap();
        std::fs::write(stage.join("codex"), "committed runtime").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage,
            bin_rel: PathBuf::from("codex"),
        };

        let mut checkpoint = |_| Ok(());
        let outcome = prepared
            .activate_with_checkpoint_and_sync(&mut checkpoint, |path| {
                if path == root {
                    Err(std::io::Error::other(
                        "injected post-commit directory sync failure",
                    ))
                } else {
                    Ok(())
                }
            })
            .expect("the installed pointer already committed");
        let ActivationOutcome::CommittedNotDurable {
            installed: activated,
            warning,
        } = outcome
        else {
            panic!("the injected root sync failure was not surfaced");
        };

        assert_eq!(activated.version, "2.0.0");
        assert!(warning.contains("crash durability could not be confirmed"));
        let visible = installed(tmp.path(), CliId::Codex).unwrap();
        assert_eq!(visible.bin, activated.bin);
        assert_eq!(
            std::fs::read_to_string(visible.bin).unwrap(),
            "committed runtime"
        );
    }

    #[test]
    fn recovery_generation_bound_blocks_growth_and_recovers_after_sync_returns() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let generations = root.join(".generations");

        for index in 0..MAX_RETAINED_RUNTIME_GENERATIONS {
            let version = format!("1.0.{index}");
            let stage = create_install_stage(&root, &version).unwrap();
            std::fs::write(stage.join("codex"), &version).unwrap();
            let prepared = PreparedInstall {
                data_dir: tmp.path().to_path_buf(),
                id: CliId::Codex,
                version,
                stage,
                bin_rel: PathBuf::from("codex"),
            };
            let mut checkpoint = |_| Ok(());
            assert!(matches!(
                prepared.activate_with_checkpoint_and_sync(&mut checkpoint, |path| {
                    if path == root {
                        Err(std::io::Error::other("injected root sync failure"))
                    } else {
                        Ok(())
                    }
                }),
                Ok(ActivationOutcome::CommittedNotDurable { .. })
            ));
        }

        let retained = std::fs::read_dir(&generations)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), MAX_RETAINED_RUNTIME_GENERATIONS);
        assert!(
            retained
                .iter()
                .all(|generation| generation.join("codex").is_file())
        );

        let stage = create_install_stage(&root, "blocked").unwrap();
        std::fs::write(stage.join("codex"), "blocked").unwrap();
        let staged_path = stage.clone();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "blocked".into(),
            stage,
            bin_rel: PathBuf::from("codex"),
        };
        let mut checkpoint = |_| Ok(());
        let error = prepared
            .activate_with_checkpoint_and_sync(&mut checkpoint, |path| {
                if path == root {
                    Err(std::io::Error::other("root sync is still unavailable"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("root sync is still unavailable"));
        assert!(!staged_path.exists());
        assert_eq!(
            std::fs::read_dir(&generations).unwrap().count(),
            MAX_RETAINED_RUNTIME_GENERATIONS
        );

        let stage = create_install_stage(&root, "recovered").unwrap();
        std::fs::write(stage.join("codex"), "recovered").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "recovered".into(),
            stage,
            bin_rel: PathBuf::from("codex"),
        };
        let mut checkpoint = |_| Ok(());
        let outcome = prepared
            .activate_with_checkpoint_and_sync(&mut checkpoint, |_| Ok(()))
            .unwrap();
        assert!(matches!(outcome, ActivationOutcome::Durable(_)));
        assert!(std::fs::read_dir(&generations).unwrap().count() <= 2);
        assert_eq!(
            installed(tmp.path(), CliId::Codex).unwrap().version,
            "recovered"
        );
    }

    #[test]
    fn cancellation_at_activation_commit_preserves_previous_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let previous = root.join("1.0.0");
        let previous_bin = previous.join("codex");
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(&previous_bin, "previous runtime").unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::to_string_pretty(&InstalledCli {
                version: "1.0.0".into(),
                bin: previous_bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();

        let stage = create_install_stage(&root, "2.0.0").unwrap();
        std::fs::write(stage.join("codex"), "prepared runtime").unwrap();
        let prepared = PreparedInstall {
            data_dir: tmp.path().to_path_buf(),
            id: CliId::Codex,
            version: "2.0.0".into(),
            stage,
            bin_rel: PathBuf::from("codex"),
        };
        let progress = Progress::default();
        progress
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);

        assert!(matches!(
            prepared.activate_cancellable(&progress),
            Err(InstallError::Cancelled)
        ));
        let active = installed(tmp.path(), CliId::Codex).unwrap();
        assert_eq!(active.version, "1.0.0");
        assert_eq!(
            std::fs::read_to_string(active.bin).unwrap(),
            "previous runtime"
        );
        assert_eq!(
            std::fs::read_dir(root.join(".generations"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn runtime_activation_lock_serializes_independent_openers() {
        use fs4::fs_std::FileExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let first = lock_runtime_activation(tmp.path(), CliId::Codex).unwrap();
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(runtime_activation_lock_path(tmp.path(), CliId::Codex))
            .unwrap();

        assert!(!second.try_lock_exclusive().unwrap());
        fs4::fs_std::FileExt::unlock(&first).unwrap();
        assert!(second.try_lock_exclusive().unwrap());
        fs4::fs_std::FileExt::unlock(&second).unwrap();
    }

    #[test]
    fn uninstall_removes_managed_install() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        std::fs::create_dir_all(root.join("1.0.0")).unwrap();
        let bin = root.join("1.0.0").join("codex");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::to_string(&InstalledCli {
                version: "1.0.0".into(),
                bin: bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let link = legacy_managed_bin_path(tmp.path(), CliId::Codex);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&bin, &link).unwrap();
        drop(
            open_runtime_lease_file(
                tmp.path(),
                CliId::Codex,
                RuntimeContainerKind::Legacy,
                &root.join("1.0.0"),
            )
            .unwrap(),
        );

        uninstall(tmp.path(), CliId::Codex).unwrap();
        assert!(installed(tmp.path(), CliId::Codex).is_none());
        assert!(!root.exists());
        assert!(link.symlink_metadata().is_err());
        assert!(!tmp.path().join("cli/.leases/codex").exists());

        // Uninstalling again is a no-op, not an error.
        uninstall(tmp.path(), CliId::Codex).unwrap();
    }

    #[test]
    fn uninstall_refuses_to_remove_a_leased_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let version_dir = root.join("1.0.0");
        let bin = version_dir.join("codex");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(&bin, "runtime").unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::to_string(&InstalledCli {
                version: "1.0.0".into(),
                bin: bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let (_, lease) = installed_with_lease(tmp.path(), CliId::Codex).unwrap();

        let error = uninstall(tmp.path(), CliId::Codex).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(bin.is_file());
        assert!(root.join("installed.json").is_file());

        drop(lease);
        uninstall(tmp.path(), CliId::Codex).unwrap();
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_runtime_lease_unlocks_an_inherited_descriptor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let runtime = root.join(".generations").join("runtime-test");
        let bin = runtime.join("codex");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(&bin, "codex").unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::to_string(&InstalledCli {
                version: "test".into(),
                bin: bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();

        let (_, lease) = installed_with_lease(tmp.path(), CliId::Codex).unwrap();
        let inherited = lease._lock.try_clone().unwrap();
        drop(lease);

        let contender = open_runtime_lease_file(
            tmp.path(),
            CliId::Codex,
            RuntimeContainerKind::Generation,
            &runtime,
        )
        .unwrap();
        assert!(fs4::fs_std::FileExt::try_lock_exclusive(&contender).unwrap());
        fs4::fs_std::FileExt::unlock(&contender).unwrap();
        drop(inherited);
    }

    #[test]
    fn cursor_sdk_uninstall_preserves_legacy_runtime_for_older_processes() {
        let tmp = tempfile::tempdir().unwrap();
        let sdk_root = cli_root(tmp.path(), CliId::CursorSdkBridge);
        let sdk_bin = sdk_root
            .join("1.0.28")
            .join("bin")
            .join("cursor-sdk-bridge");
        std::fs::create_dir_all(sdk_bin.parent().unwrap()).unwrap();
        std::fs::write(&sdk_bin, "bridge").unwrap();
        std::fs::write(
            sdk_root.join("installed.json"),
            serde_json::to_string(&InstalledCli {
                version: "1.0.28".into(),
                bin: sdk_bin.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let sdk_link = legacy_managed_bin_path(tmp.path(), CliId::CursorSdkBridge);
        std::fs::create_dir_all(sdk_link.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&sdk_bin, &sdk_link).unwrap();

        let legacy_root = tmp.path().join("cli").join("cursor-agent");
        std::fs::create_dir_all(&legacy_root).unwrap();
        std::fs::write(legacy_root.join("installed.json"), "legacy").unwrap();
        let legacy_link = tmp.path().join("cli").join("bin").join("cursor-agent");
        std::fs::write(&legacy_link, "legacy").unwrap();
        let legacy_windows_link = tmp.path().join("cli").join("bin").join("cursor-agent.exe");
        std::fs::write(&legacy_windows_link, "legacy windows").unwrap();
        let external = tmp.path().join("system-cursor-agent");
        std::fs::write(&external, "outside trouve's managed layout").unwrap();

        uninstall(tmp.path(), CliId::CursorSdkBridge).unwrap();

        assert!(!sdk_root.exists());
        assert!(sdk_link.symlink_metadata().is_err());
        assert!(legacy_root.exists());
        assert!(legacy_link.exists());
        assert!(legacy_windows_link.exists());
        assert!(external.exists());
    }

    #[test]
    fn cursor_sdk_uninstall_does_not_touch_a_legacy_runtime_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sdk_root = cli_root(tmp.path(), CliId::CursorSdkBridge);
        std::fs::create_dir_all(&sdk_root).unwrap();
        let legacy_root = tmp.path().join("cli").join("cursor-agent");
        std::fs::write(&legacy_root, "not a directory").unwrap();

        uninstall(tmp.path(), CliId::CursorSdkBridge).unwrap();

        assert!(!sdk_root.exists());
        assert!(legacy_root.is_file());
    }

    #[test]
    fn legacy_prune_keeps_the_exact_previous_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        for v in ["1.0.0", "1.1.0", "1.2.0"] {
            std::fs::create_dir_all(root.join(v)).unwrap();
        }
        let previous = root.join("1.1.0");
        prune_old_versions(tmp.path(), CliId::Codex, &root, Some(&previous));
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["1.1.0"]);
    }

    #[test]
    fn generation_prune_keeps_active_and_one_previous() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let generations = root.join(".generations");
        std::fs::create_dir_all(&generations).unwrap();
        let active = generations.join("runtime-active");
        let previous = generations.join("runtime-previous");
        for generation in ["runtime-oldest", "runtime-previous", "runtime-active"] {
            std::fs::create_dir(generations.join(generation)).unwrap();
        }

        prune_runtime_generations(
            tmp.path(),
            CliId::Codex,
            &generations,
            &active,
            Some(&previous),
        );

        let remaining = std::fs::read_dir(&generations)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(remaining.len(), 2);
        assert!(active.is_dir());
        assert!(previous.is_dir());
        assert!(!generations.join("runtime-oldest").exists());
        assert!(
            !runtime_lease_path(
                tmp.path(),
                CliId::Codex,
                RuntimeContainerKind::Generation,
                &generations.join("runtime-oldest"),
            )
            .unwrap()
            .exists()
        );
    }

    #[test]
    fn leased_generation_survives_two_later_activations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);

        let activate = |version: &str| {
            let stage = create_install_stage(&root, version).unwrap();
            std::fs::write(stage.join("codex"), version).unwrap();
            let outcome = PreparedInstall {
                data_dir: tmp.path().to_path_buf(),
                id: CliId::Codex,
                version: version.into(),
                stage,
                bin_rel: PathBuf::from("codex"),
            }
            .activate()
            .unwrap();
            let ActivationOutcome::Durable(installed) = outcome else {
                panic!("ordinary activation unexpectedly lacked durability");
            };
            installed
        };

        let first = activate("1.0.0");
        let (_, lease) = installed_with_lease(tmp.path(), CliId::Codex).unwrap();
        activate("2.0.0");
        activate("3.0.0");

        assert!(Path::new(&first.bin).is_file());
        drop(lease);
        activate("4.0.0");
        assert!(!Path::new(&first.bin).exists());
    }

    const TEST_RUNTIME_LEASE_DATA_DIR_ENV: &str = "TROUVE_TEST_RUNTIME_LEASE_DATA_DIR";
    const TEST_RUNTIME_LEASE_MARKER_ENV: &str = "TROUVE_TEST_RUNTIME_LEASE_MARKER";
    const TEST_RUNTIME_LEASE_RELEASE_ENV: &str = "TROUVE_TEST_RUNTIME_LEASE_RELEASE";

    #[test]
    fn runtime_lease_process_helper() {
        let Some(data_dir) = std::env::var_os(TEST_RUNTIME_LEASE_DATA_DIR_ENV) else {
            return;
        };
        let marker = PathBuf::from(std::env::var_os(TEST_RUNTIME_LEASE_MARKER_ENV).unwrap());
        let release = PathBuf::from(std::env::var_os(TEST_RUNTIME_LEASE_RELEASE_ENV).unwrap());
        let data_dir = PathBuf::from(data_dir);
        let (runtime, lease) = installed_with_lease(&data_dir, CliId::Codex).unwrap();
        std::fs::write(&marker, "leased").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !release.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            release.exists(),
            "parent never released runtime lease helper"
        );
        assert_eq!(std::fs::read_to_string(&runtime.bin).unwrap(), "1.0.0");
        drop(lease);
    }

    #[test]
    fn cross_process_lease_survives_two_later_activations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = cli_root(tmp.path(), CliId::Codex);
        let activate = |version: &str| {
            let stage = create_install_stage(&root, version).unwrap();
            std::fs::write(stage.join("codex"), version).unwrap();
            let outcome = PreparedInstall {
                data_dir: tmp.path().to_path_buf(),
                id: CliId::Codex,
                version: version.into(),
                stage,
                bin_rel: PathBuf::from("codex"),
            }
            .activate()
            .unwrap();
            let ActivationOutcome::Durable(installed) = outcome else {
                panic!("ordinary activation unexpectedly lacked durability");
            };
            installed
        };

        let first = activate("1.0.0");
        let marker = tmp.path().join("lease-held");
        let release = tmp.path().join("lease-release");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("install::tests::runtime_lease_process_helper")
            .arg("--nocapture")
            .env(TEST_RUNTIME_LEASE_DATA_DIR_ENV, tmp.path())
            .env(TEST_RUNTIME_LEASE_MARKER_ENV, &marker)
            .env(TEST_RUNTIME_LEASE_RELEASE_ENV, &release)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut holder = trouve_process::spawn(&mut command).unwrap();

        let marker_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < marker_deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !marker.exists() {
            let _ = std::fs::write(&release, "release");
            let _ = holder.wait();
            panic!("runtime lease helper did not acquire its shared lease");
        }

        activate("2.0.0");
        activate("3.0.0");
        let survived = Path::new(&first.bin).is_file();
        std::fs::write(&release, "release").unwrap();
        let status = holder.wait().unwrap();

        assert!(status.success());
        assert!(survived, "another process pruned the leased generation");
        activate("4.0.0");
        assert!(!Path::new(&first.bin).exists());
    }
}
