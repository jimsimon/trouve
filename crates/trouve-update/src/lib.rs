//! Checksummed self-updates for binaries published by the trouve release
//! train (ADR 0042).
//!
//! A release is selected by its canonical `vX.Y.Z` tag and exact
//! component/target asset. The archive is downloaded to a temporary
//! directory, verified against the release's `SHA256SUMS`, and only then
//! passed to the platform-aware executable replacement primitive.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::StreamExt as _;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const RELEASE_API: &str = "https://api.github.com/repos/jimsimon/trouve/releases/latest";
const CHECKSUM_ASSET: &str = "SHA256SUMS";
const MAX_CHECKSUM_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_EXPANDED_BYTES: u64 = MAX_BINARY_BYTES + 64 * 1024 * 1024;

/// Set this to a truthy value to disable startup/background updates. Manual
/// update commands and the desktop's explicit update button still work.
pub const DISABLE_AUTO_UPDATE_ENV: &str = "TROUVE_DISABLE_AUTO_UPDATE";

/// One independently shipped executable in a trouve release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    /// The desktop app (`trouve`), which embeds the server and search library.
    Desktop,
    /// The standalone HTTP/SSE server.
    Server,
    /// The standalone search CLI / MCP server.
    Search,
}

impl Component {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Desktop => "trouve",
            Self::Server => "trouve-server",
            Self::Search => "trouve-search",
        }
    }

    fn binary_name(self, target: &str) -> String {
        let suffix = if target.contains("-windows-") {
            ".exe"
        } else {
            ""
        };
        format!("{}{suffix}", self.display_name())
    }
}

/// A newer release with every asset required to update one component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub artifact_name: String,
    artifact_url: String,
    checksum_url: String,
    binary_name: String,
    archive_kind: ArchiveKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    TarGz,
    Zip,
}

/// Result of querying the stable release channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheck {
    pub current: Version,
    pub latest: Version,
    pub update: Option<Release>,
}

/// Result of checking for and, when needed, installing an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate { version: Version },
    Updated { from: Version, to: Version },
}

/// Observable stages of a verified update installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallProgress {
    FetchingChecksums,
    Downloading {
        received_bytes: u64,
        total_bytes: Option<u64>,
    },
    Verifying,
    Extracting,
    Installing,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Whether automatic update behavior is enabled for this process. Development
/// builds are always disabled, even when no environment override is present.
pub fn auto_update_enabled() -> bool {
    !cfg!(debug_assertions)
        && std::env::var_os(DISABLE_AUTO_UPDATE_ENV)
            .and_then(|value| value.into_string().ok())
            .is_none_or(|value| !is_truthy(&value))
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Compile target used by the release workflow's artifact names.
pub fn current_target() -> Result<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") if cfg!(target_env = "musl") => Ok("x86_64-unknown-linux-musl"),
        ("aarch64", "linux") if cfg!(target_env = "musl") => Ok("aarch64-unknown-linux-musl"),
        ("x86_64", "linux") if cfg!(target_env = "gnu") => Ok("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") if cfg!(target_env = "gnu") => Ok("aarch64-unknown-linux-gnu"),
        ("x86_64", "macos") => Ok("x86_64-apple-darwin"),
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        ("x86_64", "windows") if cfg!(target_env = "msvc") => Ok("x86_64-pc-windows-msvc"),
        ("aarch64", "windows") if cfg!(target_env = "msvc") => Ok("aarch64-pc-windows-msvc"),
        (arch, os) => bail!("self-update is not supported on {arch}-{os}"),
    }
}

/// Query the latest stable release and resolve the exact artifact for this
/// component and compile target.
pub async fn check(component: Component, current_version: &str) -> Result<UpdateCheck> {
    let current = Version::parse(current_version)
        .with_context(|| format!("invalid current version {current_version:?}"))?;
    let target = current_target()?;
    let client = client(current_version)?;
    let release = client
        .get(RELEASE_API)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("checking the latest trouve release")?
        .error_for_status()
        .context("latest trouve release request failed")?
        .json::<GithubRelease>()
        .await
        .context("decoding the latest trouve release")?;
    select_release(release, component, current, target)
}

/// Install the latest eligible release, if one exists.
pub async fn install_latest(component: Component, current_version: &str) -> Result<UpdateStatus> {
    let check = check(component, current_version).await?;
    let Some(release) = check.update else {
        return Ok(UpdateStatus::UpToDate {
            version: check.current,
        });
    };
    install_release(&release).await?;
    Ok(UpdateStatus::Updated {
        from: check.current,
        to: release.version,
    })
}

/// Download, verify, extract, and atomically install a release returned by
/// [`check`]. The running process is not restarted.
pub async fn install_release(release: &Release) -> Result<()> {
    install_release_with_progress(release, |_| {}).await
}

/// Install a release while reporting coarse stages and archive byte progress.
/// The callback runs on the calling async task and should return quickly.
pub async fn install_release_with_progress(
    release: &Release,
    progress: impl Fn(InstallProgress),
) -> Result<()> {
    install_release_with_progress_and_cancel(release, progress, || false).await
}

/// Install a release while allowing a host to cancel before executable
/// replacement. Cancellation is checked throughout downloads and between
/// blocking extraction and installation.
pub async fn install_release_with_progress_and_cancel(
    release: &Release,
    progress: impl Fn(InstallProgress),
    cancelled: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<()> {
    if cfg!(debug_assertions) {
        bail!("self-update is disabled for development builds");
    }
    let cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync> = std::sync::Arc::new(cancelled);
    ensure_not_cancelled(cancelled.as_ref())?;
    let client = client(env!("CARGO_PKG_VERSION"))?;
    progress(InstallProgress::FetchingChecksums);
    let checksum_text = download_text(
        &client,
        &release.checksum_url,
        MAX_CHECKSUM_BYTES,
        "release checksums",
    )
    .await?;
    ensure_not_cancelled(cancelled.as_ref())?;
    let expected = checksum_for(&checksum_text, &release.artifact_name)?;

    let stage = tempfile::tempdir().context("creating update staging directory")?;
    let archive_path = stage.path().join(match release.archive_kind {
        ArchiveKind::TarGz => "update.tar.gz",
        ArchiveKind::Zip => "update.zip",
    });
    let actual = download_file(
        &client,
        &release.artifact_url,
        &archive_path,
        MAX_ARCHIVE_BYTES,
        &progress,
        cancelled.as_ref(),
    )
    .await?;
    ensure_not_cancelled(cancelled.as_ref())?;
    progress(InstallProgress::Verifying);
    if actual != expected {
        bail!(
            "checksum mismatch for {}: expected {expected}, got {actual}",
            release.artifact_name
        );
    }

    let replacement = stage.path().join(format!(".new-{}", release.binary_name));
    let archive = archive_path.clone();
    let binary_name = release.binary_name.clone();
    let archive_kind = release.archive_kind;
    let replacement_for_extract = replacement.clone();
    ensure_not_cancelled(cancelled.as_ref())?;
    progress(InstallProgress::Extracting);
    tokio::task::spawn_blocking(move || {
        extract_binary(
            &archive,
            archive_kind,
            &binary_name,
            &replacement_for_extract,
        )
    })
    .await
    .context("joining update extraction task")??;

    ensure_not_cancelled(cancelled.as_ref())?;
    progress(InstallProgress::Installing);
    let install_cancelled = std::sync::Arc::clone(&cancelled);
    tokio::task::spawn_blocking(move || {
        ensure_not_cancelled(install_cancelled.as_ref())?;
        self_replace::self_replace(&replacement).context("replacing the running executable")
    })
    .await
    .context("joining executable replacement task")??;
    Ok(())
}

fn select_release(
    release: GithubRelease,
    component: Component,
    current: Version,
    target: &str,
) -> Result<UpdateCheck> {
    if release.draft || release.prerelease {
        bail!("the latest release endpoint returned an unpublished release");
    }
    let raw_version = release
        .tag_name
        .strip_prefix('v')
        .ok_or_else(|| anyhow!("release tag {} is not canonical vX.Y.Z", release.tag_name))?;
    let latest = Version::parse(raw_version)
        .with_context(|| format!("invalid release tag {}", release.tag_name))?;
    if !latest.pre.is_empty()
        || !latest.build.is_empty()
        || release.tag_name != format!("v{latest}")
    {
        bail!("release tag {} is not canonical vX.Y.Z", release.tag_name);
    }
    if latest <= current {
        return Ok(UpdateCheck {
            current,
            latest,
            update: None,
        });
    }

    let (extension, archive_kind) = if target.contains("-windows-") {
        ("zip", ArchiveKind::Zip)
    } else {
        ("tar.gz", ArchiveKind::TarGz)
    };
    let artifact_name = format!(
        "{}-{}-{target}.{extension}",
        component.display_name(),
        release.tag_name
    );
    let artifact_url = unique_asset_url(&release.assets, &artifact_name)?;
    let checksum_url = unique_asset_url(&release.assets, CHECKSUM_ASSET)?;
    Ok(UpdateCheck {
        current,
        latest: latest.clone(),
        update: Some(Release {
            version: latest,
            tag: release.tag_name,
            artifact_name,
            artifact_url,
            checksum_url,
            binary_name: component.binary_name(target),
            archive_kind,
        }),
    })
}

fn unique_asset_url(assets: &[GithubAsset], name: &str) -> Result<String> {
    let mut matches = assets.iter().filter(|asset| asset.name == name);
    let first = matches
        .next()
        .ok_or_else(|| anyhow!("release is not ready: missing asset {name}"))?;
    if matches.next().is_some() {
        bail!("release has more than one asset named {name}");
    }
    Ok(first.browser_download_url.clone())
}

fn checksum_for(contents: &str, artifact_name: &str) -> Result<String> {
    let mut found = None;
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(digest) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        if fields.next().is_some() || name.trim_start_matches('*') != artifact_name {
            continue;
        }
        let digest = digest.to_ascii_lowercase();
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid SHA-256 entry for {artifact_name}");
        }
        if found.replace(digest).is_some() {
            bail!("duplicate SHA-256 entry for {artifact_name}");
        }
    }
    found.ok_or_else(|| anyhow!("SHA256SUMS has no entry for {artifact_name}"))
}

fn client(version: &str) -> Result<reqwest::Client> {
    // The final desktop binary can link more than one rustls provider
    // feature. Pick the workspace-standard provider before reqwest creates
    // its first TLS client; installing an already-selected provider is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(format!("trouve/{version}"))
        .build()
        .context("building update HTTP client")
}

async fn download_text(
    client: &reqwest::Client,
    url: &str,
    limit: u64,
    label: &str,
) -> Result<String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("downloading {label}"))?
        .error_for_status()
        .with_context(|| format!("{label} request failed"))?;
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        bail!("{label} exceeds the {limit}-byte limit");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading {label}"))?;
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("{label} byte count overflow"))?;
        if next_len as u64 > limit {
            bail!("{label} exceeds the {limit}-byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

async fn download_file(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    limit: u64,
    progress: &impl Fn(InstallProgress),
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> Result<String> {
    ensure_not_cancelled(cancelled)?;
    let response = client
        .get(url)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .context("downloading update archive")?
        .error_for_status()
        .context("update archive request failed")?;
    let total_bytes = response.content_length();
    if total_bytes.is_some_and(|length| length > limit) {
        bail!("update archive exceeds the {limit}-byte limit");
    }

    let mut file = tokio::fs::File::create(destination)
        .await
        .context("creating staged update archive")?;
    let mut digest = Sha256::new();
    let mut received = 0_u64;
    let mut last_reported = 0_u64;
    progress(InstallProgress::Downloading {
        received_bytes: 0,
        total_bytes,
    });
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        ensure_not_cancelled(cancelled)?;
        let chunk = chunk.context("reading update archive")?;
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow!("update archive byte count overflow"))?;
        if received > limit {
            bail!("update archive exceeds the {limit}-byte limit");
        }
        digest.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .context("writing staged update archive")?;
        if received.saturating_sub(last_reported) >= 256 * 1024
            || total_bytes.is_some_and(|total| received >= total)
        {
            progress(InstallProgress::Downloading {
                received_bytes: received,
                total_bytes,
            });
            last_reported = received;
        }
    }
    if received != last_reported {
        progress(InstallProgress::Downloading {
            received_bytes: received,
            total_bytes,
        });
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .context("flushing staged update archive")?;
    Ok(hex::encode(digest.finalize()))
}

fn extract_binary(
    archive_path: &Path,
    kind: ArchiveKind,
    binary_name: &str,
    destination: &Path,
) -> Result<()> {
    let packaged_binary = Path::new("bin").join(binary_name);
    let mut output = std::fs::File::create(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    let written = match kind {
        ArchiveKind::TarGz => {
            let archive_file = std::fs::File::open(archive_path)
                .with_context(|| format!("opening {}", archive_path.display()))?;
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive_file));
            let mut written = None;
            let mut expanded_bytes = 0_u64;
            for entry in archive.entries().context("reading update tar archive")? {
                let mut entry = entry.context("reading update tar entry")?;
                expanded_bytes = add_archive_entry_size(
                    expanded_bytes,
                    entry
                        .header()
                        .size()
                        .context("reading update tar entry size")?,
                )?;
                let path = entry.path().context("reading update tar entry path")?;
                if path != Path::new(binary_name) && path != packaged_binary {
                    continue;
                }
                if !entry.header().entry_type().is_file() {
                    bail!("update archive entry {binary_name} is not a regular file");
                }
                if written.is_some() {
                    bail!("update archive contains duplicate {binary_name} entries");
                }
                written = Some(copy_limited(&mut entry, &mut output)?);
            }
            written.ok_or_else(|| anyhow!("update archive has no {binary_name}"))?
        }
        ArchiveKind::Zip => {
            let archive_file = std::fs::File::open(archive_path)
                .with_context(|| format!("opening {}", archive_path.display()))?;
            let mut archive =
                zip::ZipArchive::new(archive_file).context("reading update zip archive")?;
            let packaged_binary = format!("bin/{binary_name}");
            let matches = archive
                .file_names()
                .filter(|name| *name == binary_name || *name == packaged_binary)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                bail!(
                    "update archive contains {} usable entries named {binary_name}; expected one",
                    matches.len()
                );
            }
            let mut entry = archive
                .by_name(&matches[0])
                .with_context(|| format!("reading {binary_name} from update zip archive"))?;
            if entry.is_dir() {
                bail!("update archive entry {binary_name} is not a regular file");
            }
            copy_limited(&mut entry, &mut output)?
        }
    };
    if written == 0 {
        bail!("update archive contains an empty {binary_name}");
    }
    output.flush().context("flushing extracted update binary")?;
    make_executable(destination)?;
    Ok(())
}

fn copy_limited(reader: &mut impl Read, writer: &mut impl Write) -> Result<u64> {
    let mut limited = reader.take(MAX_BINARY_BYTES + 1);
    let written = std::io::copy(&mut limited, writer).context("extracting update binary")?;
    if written > MAX_BINARY_BYTES {
        bail!("update binary exceeds the {MAX_BINARY_BYTES}-byte limit");
    }
    Ok(written)
}

fn add_archive_entry_size(current: u64, entry_size: u64) -> Result<u64> {
    let expanded = current
        .checked_add(entry_size)
        .ok_or_else(|| anyhow!("update archive expanded byte count overflow"))?;
    if expanded > MAX_ARCHIVE_EXPANDED_BYTES {
        bail!("update archive expands beyond the {MAX_ARCHIVE_EXPANDED_BYTES}-byte limit");
    }
    Ok(expanded)
}

fn ensure_not_cancelled(cancelled: &(impl Fn() -> bool + ?Sized)) -> Result<()> {
    if cancelled() {
        bail!("update cancelled");
    }
    Ok(())
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making {} executable", path.display()))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(debug_assertions)]
    #[test]
    fn development_builds_disable_automatic_updates() {
        assert!(!auto_update_enabled());
    }

    fn asset(name: &str) -> GithubAsset {
        GithubAsset {
            name: name.into(),
            browser_download_url: format!("https://example.invalid/{name}"),
        }
    }

    #[test]
    fn selects_exact_component_target_and_checksum_assets() {
        let release = GithubRelease {
            tag_name: "v3.7.0".into(),
            draft: false,
            prerelease: false,
            assets: vec![
                asset("trouve-server-v3.7.0-x86_64-unknown-linux-gnu.tar.gz"),
                asset(CHECKSUM_ASSET),
            ],
        };
        let check = select_release(
            release,
            Component::Server,
            Version::parse("3.6.0").unwrap(),
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();
        let update = check.update.unwrap();
        assert_eq!(update.version, Version::parse("3.7.0").unwrap());
        assert_eq!(
            update.artifact_name,
            "trouve-server-v3.7.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(update.binary_name, "trouve-server");
    }

    #[test]
    fn equal_or_older_release_does_not_require_assets() {
        for tag in ["v3.6.0", "v3.5.9"] {
            let release = GithubRelease {
                tag_name: tag.into(),
                draft: false,
                prerelease: false,
                assets: Vec::new(),
            };
            let check = select_release(
                release,
                Component::Search,
                Version::parse("3.6.0").unwrap(),
                "aarch64-apple-darwin",
            )
            .unwrap();
            assert!(check.update.is_none());
        }
    }

    #[test]
    fn rejects_noncanonical_and_prerelease_channels() {
        for (tag, prerelease) in [
            ("3.7.0", false),
            ("v3.7.0-beta.1", false),
            ("v3.7.0-beta.1", true),
        ] {
            let error = select_release(
                GithubRelease {
                    tag_name: tag.into(),
                    draft: false,
                    prerelease,
                    assets: Vec::new(),
                },
                Component::Desktop,
                Version::parse("3.6.0").unwrap(),
                "x86_64-pc-windows-msvc",
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("canonical")
                    || error.to_string().contains("unpublished")
            );
        }
    }

    #[test]
    fn checksum_parser_requires_one_exact_valid_entry() {
        let wanted = "trouve-v3.7.0-x86_64-unknown-linux-gnu.tar.gz";
        let digest = "ab".repeat(32);
        let contents = format!(
            "{}  other.tar.gz\n{}  {}\n",
            "cd".repeat(32),
            digest,
            wanted
        );
        assert_eq!(checksum_for(&contents, wanted).unwrap(), digest);
        assert!(checksum_for(&contents, "missing.tar.gz").is_err());
        assert!(checksum_for(&format!("not-a-hash  {wanted}"), wanted).is_err());
    }

    #[test]
    fn extracts_only_the_named_tar_entry() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("update.tar.gz");
        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let payload = b"new trouve binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "trouve", payload.as_slice())
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();

        let output = temp.path().join("new-trouve");
        extract_binary(&archive_path, ArchiveKind::TarGz, "trouve", &output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), payload);
    }

    #[test]
    fn extracts_linux_package_binary_from_bin_directory() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("update.tar.gz");
        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let payload = b"packaged trouve binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "bin/trouve", payload.as_slice())
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();

        let output = temp.path().join("new-trouve");
        extract_binary(&archive_path, ArchiveKind::TarGz, "trouve", &output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), payload);
    }

    #[test]
    fn expanded_archive_budget_counts_every_tar_entry() {
        let remaining = MAX_ARCHIVE_EXPANDED_BYTES - 1;
        assert_eq!(add_archive_entry_size(0, remaining).unwrap(), remaining);
        let error = add_archive_entry_size(remaining, 2).unwrap_err();
        assert!(error.to_string().contains("expands beyond"));
    }

    #[test]
    fn cancellation_is_reported_before_installation() {
        assert!(ensure_not_cancelled(&|| false).is_ok());
        assert_eq!(
            ensure_not_cancelled(&|| true).unwrap_err().to_string(),
            "update cancelled"
        );
    }

    #[test]
    fn missing_release_asset_is_not_treated_as_up_to_date() {
        let error = select_release(
            GithubRelease {
                tag_name: "v3.7.0".into(),
                draft: false,
                prerelease: false,
                assets: vec![asset(CHECKSUM_ASSET)],
            },
            Component::Search,
            Version::parse("3.6.0").unwrap(),
            "aarch64-unknown-linux-musl",
        )
        .unwrap_err();
        assert!(error.to_string().contains("release is not ready"));
    }

    #[test]
    fn truthy_auto_update_values_are_case_insensitive() {
        for value in ["1", " true ", "YES", "On"] {
            assert!(is_truthy(value));
        }
        for value in ["", "0", "false", "no", "off"] {
            assert!(!is_truthy(value));
        }
    }
}
