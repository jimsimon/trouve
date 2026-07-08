//! Managed vendor CLI installs.
//!
//! Downloads the official vendor CLI builds (the same artifacts their
//! install scripts fetch) into trouve's data directory, so users don't
//! depend on system packages that may lag behind — e.g. the ACP mode of
//! `cursor-agent` needs a newer build than most distro packages ship.
//!
//! Layout under `<data_dir>/cli/`:
//! - `<id>/<version>/…`       — one directory per installed version
//! - `<id>/installed.json`    — pointer to the active version + binary
//! - `bin/<id>`               — stable symlink backends resolve at spawn
//!
//! Sources (no custom mirrors, no version pinning by us):
//! - cursor-agent: `downloads.cursor.com/lab/<ver>/<os>/<arch>/agent-cli-package.tar.gz`
//!   (version discovered from the official install script)
//! - claude: `downloads.claude.ai/claude-code-releases` (`latest` + manifest
//!   with sha256 checksums; single static binary)
//! - codex: GitHub `openai/codex` latest release tarball (musl build on Linux)

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest;

/// A vendor CLI trouve knows how to install. `id` doubles as the binary
/// name and the API path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliId {
    CursorAgent,
    Claude,
    Codex,
}

pub const ALL_CLIS: [CliId; 3] = [CliId::CursorAgent, CliId::Claude, CliId::Codex];

impl CliId {
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "cursor-agent" => Some(Self::CursorAgent),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CursorAgent => "cursor-agent",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::CursorAgent => "Cursor CLI",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex CLI",
        }
    }

    /// Provider kinds this CLI serves (for surfacing next to providers).
    pub fn provider_kinds(&self) -> &'static [&'static str] {
        match self {
            Self::CursorAgent => &["cursor-cli"],
            Self::Claude => &["claude-cli"],
            Self::Codex => &["codex-app-server", "codex-responses"],
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
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The active managed install of one CLI, persisted as `installed.json`.
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

async fn get_text(url: &str) -> Result<String, InstallError> {
    let resp = http()?
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::Download(format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(InstallError::Download(format!("{url}: {}", resp.status())));
    }
    resp.text()
        .await
        .map_err(|e| InstallError::Download(format!("{url}: {e}")))
}

async fn get_bytes(url: &str) -> Result<Vec<u8>, InstallError> {
    let resp = http()?
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::Download(format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(InstallError::Download(format!("{url}: {}", resp.status())));
    }
    Ok(resp
        .bytes()
        .await
        .map_err(|e| InstallError::Download(format!("{url}: {e}")))?
        .to_vec())
}

// --- version discovery -------------------------------------------------------

/// The newest version the vendor currently serves.
pub async fn latest_version(id: CliId) -> Result<String, InstallError> {
    match id {
        CliId::CursorAgent => {
            let script = get_text("https://cursor.com/install").await?;
            parse_cursor_install_version(&script).ok_or_else(|| {
                InstallError::Download("cursor install script had no version".into())
            })
        }
        CliId::Claude => {
            let v = get_text("https://downloads.claude.ai/claude-code-releases/latest").await?;
            let v = v.trim().to_string();
            if v.chars().next().is_none_or(|c| !c.is_ascii_digit()) {
                return Err(InstallError::Download(format!(
                    "unexpected claude latest response: {v:.40}"
                )));
            }
            Ok(v)
        }
        CliId::Codex => {
            let body =
                get_text("https://api.github.com/repos/openai/codex/releases/latest").await?;
            let json: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| InstallError::Download(format!("github release json: {e}")))?;
            let tag = json["tag_name"]
                .as_str()
                .ok_or_else(|| InstallError::Download("github release had no tag_name".into()))?;
            Ok(tag.trim_start_matches("rust-v").to_string())
        }
    }
}

/// Pull the pinned version out of the official cursor install script
/// (`…downloads.cursor.com/lab/<version>/<os>/<arch>/…`).
fn parse_cursor_install_version(script: &str) -> Option<String> {
    let idx = script.find("downloads.cursor.com/lab/")?;
    let rest = &script[idx + "downloads.cursor.com/lab/".len()..];
    let version: String = rest.chars().take_while(|c| *c != '/').collect();
    (!version.is_empty()).then_some(version)
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

fn claude_platform() -> Result<String, InstallError> {
    let (os, arch) = cursor_platform()?; // same os/arch token scheme
    Ok(format!("{os}-{arch}"))
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
/// re-downloads and re-points the symlink.
pub async fn install(data_dir: &Path, id: CliId, version: &str) -> Result<InstalledCli, InstallError> {
    let root = cli_root(data_dir, id);
    let version_dir = root.join(version);
    // Stage into a temp sibling so a failed install never half-replaces an
    // existing version directory.
    let stage = root.join(format!(".stage-{version}"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;

    let result = install_into(&stage, id, version).await;
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
    let mut f = std::fs::File::create(root.join("installed.json"))?;
    f.write_all(serde_json::to_string_pretty(&info).unwrap().as_bytes())?;

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
    Ok(info)
}

/// Fetch and unpack one CLI into `dir`; returns the executable's path
/// relative to `dir`.
async fn install_into(dir: &Path, id: CliId, version: &str) -> Result<PathBuf, InstallError> {
    match id {
        CliId::CursorAgent => {
            let (os, arch) = cursor_platform()?;
            let url = format!(
                "https://downloads.cursor.com/lab/{version}/{os}/{arch}/agent-cli-package.tar.gz"
            );
            let bytes = get_bytes(&url).await?;
            untar_gz(bytes, dir).await?;
            let rel = PathBuf::from("dist-package").join("cursor-agent");
            make_executable(&dir.join(&rel))?;
            Ok(rel)
        }
        CliId::Claude => {
            let platform = claude_platform()?;
            let base = "https://downloads.claude.ai/claude-code-releases";
            let manifest = get_text(&format!("{base}/{version}/manifest.json")).await?;
            let manifest: serde_json::Value = serde_json::from_str(&manifest)
                .map_err(|e| InstallError::Download(format!("claude manifest: {e}")))?;
            let expected = manifest["platforms"][&platform]["checksum"]
                .as_str()
                .ok_or_else(|| InstallError::Unsupported(platform.clone()))?
                .to_string();
            let bytes = get_bytes(&format!("{base}/{version}/{platform}/claude")).await?;
            let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
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
            let bytes = get_bytes(&url).await?;
            untar_gz(bytes, dir).await?;
            let rel = PathBuf::from("codex");
            std::fs::rename(dir.join(format!("codex-{triple}")), dir.join(&rel))?;
            make_executable(&dir.join(&rel))?;
            Ok(rel)
        }
    }
}

/// Unpack a gzipped tarball (already in memory) into `dir` off the async
/// runtime.
async fn untar_gz(bytes: Vec<u8>, dir: &Path) -> Result<(), InstallError> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        tar::Archive::new(decoder).unpack(&dir)
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
    if command.contains('/') {
        let p = PathBuf::from(command);
        return p.exists().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(command))
        .find(|p| p.is_file())
}

/// Best-effort `<bin> --version` (first line, trimmed), for reporting the
/// version of CLIs found on PATH.
pub async fn binary_version(command: &str) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(command)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output(),
    )
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
    fn parses_cursor_version_from_install_script() {
        let script = r#"
DOWNLOAD_URL="https://downloads.cursor.com/lab/2026.07.01-41b2de7/${OS}/${ARCH}/agent-cli-package.tar.gz"
"#;
        assert_eq!(
            parse_cursor_install_version(script).as_deref(),
            Some("2026.07.01-41b2de7")
        );
        assert_eq!(parse_cursor_install_version("nothing here"), None);
    }

    #[test]
    fn cli_ids_round_trip() {
        for id in ALL_CLIS {
            assert_eq!(CliId::parse(id.as_str()), Some(id));
        }
        assert_eq!(CliId::parse("unknown"), None);
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
