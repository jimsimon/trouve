//! Managed vendor agent-runtime installs.
//!
//! Downloads official vendor builds into trouve's data directory, so users
//! don't depend on system packages that may lag behind. The legacy `/v1/clis`
//! API name covers both CLIs and Cursor's standalone Agent SDK Bridge.
//!
//! Layout under `<data_dir>/cli/`:
//! - `<id>/<version>/…`       — one directory per installed version
//! - `<id>/installed.json`    — pointer to the active version + binary
//! - `bin/<id>`               — stable symlink backends resolve at spawn
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
    /// Absolute path of the executable inside the version directory.
    pub bin: String,
}

fn cli_root(data_dir: &Path, id: CliId) -> PathBuf {
    data_dir.join("cli").join(id.as_str())
}

/// Stable path of the managed binary (a symlink), whether or not it exists.
pub fn managed_bin(data_dir: &Path, id: CliId) -> PathBuf {
    data_dir.join("cli").join("bin").join(id.as_str())
}

/// The managed install of `id`, if one is active and its binary exists.
pub fn installed(data_dir: &Path, id: CliId) -> Option<InstalledCli> {
    let raw = std::fs::read_to_string(cli_root(data_dir, id).join("installed.json")).ok()?;
    let info: InstalledCli = serde_json::from_str(&raw).ok()?;
    Path::new(&info.bin).exists().then_some(info)
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

/// Download and activate `version` of `id` under `data_dir`. Returns the
/// activated install. Idempotent: re-installing the active version just
/// re-downloads and re-points the symlink. Byte progress lands in
/// `progress`, which also carries the cancel flag.
pub async fn install(
    data_dir: &Path,
    id: CliId,
    version: &str,
    progress: &Progress,
) -> Result<InstalledCli, InstallError> {
    // `version` is scraped from vendor endpoints and also joined into
    // filesystem paths (version dir, staging dir, download URLs). A crafted
    // or compromised endpoint returning `1/../../../etc` would otherwise let
    // `remove_dir_all`/`rename` touch an arbitrary directory. Constrain it to
    // a strict, path-safe allowlist before it reaches the filesystem.
    let version = normalized_version(id, version);
    validate_version(version)?;
    let root = cli_root(data_dir, id);
    let version_dir = root.join(version);
    // Stage into a temp sibling so a failed install never half-replaces an
    // existing version directory.
    let stage = root.join(format!(".stage-{version}"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;

    let result = install_into(&stage, id, version, progress).await;
    let bin_rel = match result {
        Ok(rel) => rel,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
    };

    let _ = std::fs::remove_dir_all(&version_dir);
    std::fs::rename(&stage, &version_dir)?;
    let bin = version_dir.join(&bin_rel);

    let info = InstalledCli {
        version: version.to_string(),
        bin: bin.to_string_lossy().into_owned(),
    };
    // Write the pointer atomically: a crash mid-write would otherwise leave
    // a truncated installed.json that parses as "not installed" even though
    // the binary is present.
    let pointer = root.join("installed.json");
    let tmp = root.join(".installed.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&info).unwrap().as_bytes(),
    )?;
    std::fs::rename(&tmp, &pointer)?;

    let link = managed_bin(data_dir, id);
    std::fs::create_dir_all(link.parent().unwrap())?;
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&bin, &link)?;
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::remove_file(&link);
        std::fs::copy(&bin, &link)?;
    }

    // Keep at most one older version around for rollback; drop the rest.
    prune_old_versions(&root, version);
    if id == CliId::CursorSdkBridge {
        // The SDK is the Cursor transport now. Remove Trouve's obsolete
        // managed ACP binary once the replacement is active; system installs
        // remain outside our ownership and are never touched.
        remove_legacy_cursor_agent_best_effort(data_dir);
    }
    Ok(info)
}

/// Remove the managed install of `id` entirely: every version directory,
/// the pointer, and the stable symlink. Binaries found on PATH are
/// untouched — trouve only manages its own copies.
pub fn uninstall(data_dir: &Path, id: CliId) -> std::io::Result<()> {
    let link = managed_bin(data_dir, id);
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)?;
    }
    let root = cli_root(data_dir, id);
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    if id == CliId::CursorSdkBridge {
        remove_legacy_cursor_agent_best_effort(data_dir);
    }
    Ok(())
}

fn remove_legacy_cursor_agent(data_dir: &Path) -> std::io::Result<()> {
    for executable in ["cursor-agent", "cursor-agent.exe"] {
        let legacy_link = data_dir.join("cli").join("bin").join(executable);
        if legacy_link.symlink_metadata().is_ok() {
            std::fs::remove_file(legacy_link)?;
        }
    }
    let legacy_root = data_dir.join("cli").join("cursor-agent");
    if legacy_root.exists() {
        std::fs::remove_dir_all(legacy_root)?;
    }
    Ok(())
}

fn remove_legacy_cursor_agent_best_effort(data_dir: &Path) {
    if let Err(error) = remove_legacy_cursor_agent(data_dir) {
        tracing::warn!(
            "Cursor Agent SDK operation completed, but obsolete managed cursor-agent cleanup failed: {error}"
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

/// Remove all version directories except the active one and the
/// lexicographically greatest other (a cheap "previous version" heuristic).
fn prune_old_versions(root: &Path, active: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut others: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n != active && !n.starts_with('.'))
                    .unwrap_or(false)
        })
        .collect();
    others.sort();
    for dir in others.iter().rev().skip(1) {
        let _ = std::fs::remove_dir_all(dir);
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

        let info = installed(tmp.path(), CliId::Codex).unwrap();
        assert_eq!(info.version, "1.0.0");

        // Pointer with a missing binary reports not installed.
        std::fs::remove_file(&bin).unwrap();
        assert!(installed(tmp.path(), CliId::Codex).is_none());
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
        let link = managed_bin(tmp.path(), CliId::Codex);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&bin, &link).unwrap();

        uninstall(tmp.path(), CliId::Codex).unwrap();
        assert!(installed(tmp.path(), CliId::Codex).is_none());
        assert!(!root.exists());
        assert!(link.symlink_metadata().is_err());

        // Uninstalling again is a no-op, not an error.
        uninstall(tmp.path(), CliId::Codex).unwrap();
    }

    #[test]
    fn cursor_sdk_uninstall_removes_only_trouve_managed_cursor_runtimes() {
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
        let sdk_link = managed_bin(tmp.path(), CliId::CursorSdkBridge);
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
        assert!(!legacy_root.exists());
        assert!(legacy_link.symlink_metadata().is_err());
        assert!(legacy_windows_link.symlink_metadata().is_err());
        assert!(external.exists());
    }

    #[test]
    fn cursor_sdk_uninstall_succeeds_when_legacy_cleanup_fails() {
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
    fn prune_keeps_active_and_one_previous() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for v in ["1.0.0", "1.1.0", "1.2.0", "2.0.0"] {
            std::fs::create_dir_all(root.join(v)).unwrap();
        }
        prune_old_versions(&root, "2.0.0");
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["1.2.0", "2.0.0"]);
    }
}
