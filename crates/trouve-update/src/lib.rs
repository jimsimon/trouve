//! Checksummed self-updates for binaries published by the trouve release
//! train (ADR 0042).
//!
//! A release is selected by its canonical `vX.Y.Z` tag and exact
//! component/target asset. The archive is downloaded to a temporary
//! directory, verified against the release's `SHA256SUMS`, and only then
//! passed to the platform-aware executable replacement primitive.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
const BINARY_COMPARE_CHUNK_BYTES: usize = 64 * 1024;
const MAX_INSTALLED_VERSION_BYTES: u64 = 128;
const UPDATE_STATE_DIRECTORY: &str = "updates";

/// Set this to a truthy value to disable startup/background updates. Manual
/// update commands and the desktop's explicit update button still work.
pub const DISABLE_AUTO_UPDATE_ENV: &str = "TROUVE_DISABLE_AUTO_UPDATE";

/// Verify that the running executable lives in an installation directory the
/// current user can update without elevation. Package-managed locations such
/// as /usr/bin and Program Files intentionally fail this probe.
pub fn ensure_self_update_supported() -> Result<()> {
    let executable = std::env::current_exe().context("locating the installed executable")?;
    ensure_update_directory_writable(&executable)
}

fn ensure_update_directory_writable(executable: &Path) -> Result<()> {
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow!("installed executable path has no parent"))?;
    let file_name = executable
        .file_name()
        .ok_or_else(|| anyhow!("installed executable path has no file name"))?
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe = parent.join(format!(
        ".{file_name}.update-probe-{}-{nonce}",
        std::process::id()
    ));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .with_context(|| {
            format!(
                "installation directory {} is not writable; this installation is package-managed",
                parent.display()
            )
        })?;
    drop(file);
    std::fs::remove_file(&probe)
        .with_context(|| format!("removing update probe {}", probe.display()))
}

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

const INSTALL_CANCELLABLE: u8 = 0;
const INSTALL_CANCELLED: u8 = 1;
const INSTALL_COMMITTED: u8 = 2;

/// Synchronizes a host cancellation request with the irreversible executable
/// replacement commit point.
#[derive(Debug, Default)]
pub struct InstallCancellation {
    state: AtomicU8,
}

impl InstallCancellation {
    /// Request cancellation. Returns false once executable replacement has
    /// committed, at which point the host must stay alive until installation
    /// reports its terminal result.
    pub fn request_cancel(&self) -> bool {
        match self.state.compare_exchange(
            INSTALL_CANCELLABLE,
            INSTALL_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(INSTALL_CANCELLED) => true,
            Err(INSTALL_COMMITTED) => false,
            Err(_) => false,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == INSTALL_CANCELLED
    }

    fn commit_install(&self) -> Result<()> {
        match self.state.compare_exchange(
            INSTALL_CANCELLABLE,
            INSTALL_COMMITTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(INSTALL_COMMITTED) => Ok(()),
            Err(INSTALL_CANCELLED) => bail!("update cancelled"),
            Err(_) => bail!("update cancellation state is invalid"),
        }
    }

    #[cfg(test)]
    fn is_committed(&self) -> bool {
        self.state.load(Ordering::Acquire) == INSTALL_COMMITTED
    }
}

const UPDATE_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

struct UpdateLock {
    _file: std::fs::File,
    executable: std::path::PathBuf,
}

async fn acquire_update_lock(cancellation: Option<Arc<InstallCancellation>>) -> Result<UpdateLock> {
    let executable = std::env::current_exe().context("locating executable for update lock")?;
    tokio::task::spawn_blocking(move || {
        acquire_update_lock_for(&executable, cancellation.as_deref())
    })
    .await
    .context("joining update-lock task")?
}

fn acquire_update_lock_for(
    executable: &Path,
    cancellation: Option<&InstallCancellation>,
) -> Result<UpdateLock> {
    use fs4::fs_std::FileExt as _;

    let file_name = executable
        .file_name()
        .ok_or_else(|| anyhow!("updated executable path has no file name"))?
        .to_string_lossy();
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow!("updated executable path has no parent"))?;
    let path = parent.join(format!(".{file_name}.update.lock"));
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("opening update lock {}", path.display()))?;
    loop {
        if let Some(cancellation) = cancellation {
            ensure_not_cancelled(cancellation)?;
        }
        match file.try_lock_exclusive() {
            Ok(true) => break,
            Ok(false) => std::thread::sleep(UPDATE_LOCK_POLL_INTERVAL),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("locking updated executable {}", executable.display())
                });
            }
        }
    }
    Ok(UpdateLock {
        _file: file,
        executable: executable.to_owned(),
    })
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

/// Install the latest eligible release, if one exists. One executable-scoped
/// interprocess lock covers the check, download, and replacement.
pub async fn install_latest(component: Component, current_version: &str) -> Result<UpdateStatus> {
    if cfg!(debug_assertions) {
        bail!("self-update is disabled for development builds");
    }
    let lock = acquire_update_lock(None).await?;
    let current = Version::parse(current_version)
        .with_context(|| format!("invalid current version {current_version:?}"))?;
    let observed = installed_binary_version(&lock.executable)
        .await
        .filter(|installed| installed > &current)
        .unwrap_or(current);
    let check = check(component, &observed.to_string()).await?;
    let Some(release) = check.update else {
        return Ok(UpdateStatus::UpToDate {
            version: check.current,
        });
    };
    install_release_locked(
        &release,
        &lock.executable,
        |_| {},
        Arc::new(InstallCancellation::default()),
    )
    .await?;
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
    install_release_with_progress_and_cancel(
        release,
        progress,
        Arc::new(InstallCancellation::default()),
    )
    .await
}

/// Install a release while allowing a host to cancel before executable
/// replacement. InstallCancellation atomically arbitrates cancellation
/// against the executable replacement commit point.
pub async fn install_release_with_progress_and_cancel(
    release: &Release,
    progress: impl Fn(InstallProgress),
    cancellation: Arc<InstallCancellation>,
) -> Result<()> {
    if cfg!(debug_assertions) {
        bail!("self-update is disabled for development builds");
    }
    ensure_not_cancelled(&cancellation)?;
    let lock = acquire_update_lock(Some(Arc::clone(&cancellation))).await?;
    if installed_binary_version(&lock.executable)
        .await
        .is_some_and(|installed| installed >= release.version)
    {
        return Ok(());
    }
    ensure_not_cancelled(&cancellation)?;
    install_release_locked(release, &lock.executable, progress, cancellation).await
}

async fn installed_binary_version(executable: &Path) -> Option<Version> {
    let executable = executable.to_owned();
    tokio::task::spawn_blocking(move || {
        let state_root = update_state_root()?;
        installed_binary_version_at(&state_root, &executable)
    })
    .await
    .ok()?
}

fn update_state_root() -> Option<std::path::PathBuf> {
    dirs::data_local_dir().map(|directory| directory.join("trouve").join(UPDATE_STATE_DIRECTORY))
}

fn installed_binary_version_at(state_root: &Path, executable: &Path) -> Option<Version> {
    verify_private_update_state_root(state_root).ok()?;
    let identity = executable_identity(executable).ok()?;
    read_installed_version(&update_state_path(state_root, executable, &identity))
}

fn record_installed_version(state_root: &Path, executable: &Path, version: &Version) -> Result<()> {
    let identity = executable_identity(executable)?;
    record_installed_version_for_identity(state_root, executable, &identity, version)
}

fn record_installed_version_for_identity(
    state_root: &Path,
    executable: &Path,
    identity: &str,
    version: &Version,
) -> Result<()> {
    prepare_private_update_state_root(state_root)?;
    let state = update_state_path(state_root, executable, identity);
    let text = format!("{version}\n");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(&state) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .context("writing installed-version state")?;
            file.sync_all().context("syncing installed-version state")?;
            sync_directory(state_root).context("syncing update-state directory")
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_installed_version(&state).as_ref() == Some(version) {
                Ok(())
            } else {
                bail!("installed-version state conflicts with the replacement executable")
            }
        }
        Err(error) => Err(error)
            .with_context(|| format!("creating installed-version state {}", state.display())),
    }
}

fn read_installed_version(path: &Path) -> Option<Version> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INSTALLED_VERSION_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_INSTALLED_VERSION_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_INSTALLED_VERSION_BYTES || text.lines().count() != 1 {
        return None;
    }
    Version::parse(text.trim()).ok()
}

fn prepare_private_update_state_root(state_root: &Path) -> Result<()> {
    std::fs::create_dir_all(state_root)
        .with_context(|| format!("creating update-state directory {}", state_root.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata =
            std::fs::symlink_metadata(state_root).context("inspecting update-state directory")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("update-state path is not a real directory");
        }
        std::fs::set_permissions(state_root, std::fs::Permissions::from_mode(0o700))
            .context("securing update-state directory")?;
    }
    verify_private_update_state_root(state_root)
}

fn verify_private_update_state_root(state_root: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(state_root)
        .with_context(|| format!("inspecting update-state directory {}", state_root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("update-state path is not a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            bail!("update-state directory is not private to the current user");
        }
    }
    Ok(())
}

fn update_state_path(state_root: &Path, executable: &Path, identity: &str) -> std::path::PathBuf {
    let mut digest = Sha256::new();
    digest.update(b"trouve-update-state-v1\0");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        digest.update(executable.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        for unit in executable.as_os_str().encode_wide() {
            digest.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    digest.update(executable.to_string_lossy().as_bytes());
    digest.update(b"\0");
    digest.update(identity.as_bytes());
    state_root.join(format!("{}.version", hex::encode(digest.finalize())))
}

fn executable_identity(executable: &Path) -> Result<String> {
    let file = std::fs::File::open(executable)
        .with_context(|| format!("opening executable identity {}", executable.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading executable identity {}", executable.display()))?;
    opened_executable_identity(&file, &metadata)
}

fn opened_executable_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<String> {
    if !metadata.is_file() {
        bail!("updated executable is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(format!(
            "unix:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec()
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        let succeeded =
            unsafe { GetFileInformationByHandle(_file.as_raw_handle().cast(), &mut information) };
        if succeeded == 0 {
            return Err(std::io::Error::last_os_error())
                .context("querying installed executable file identity");
        }
        Ok(format!(
            "windows:{}:{}:{}:{}",
            information.dwVolumeSerialNumber,
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
            metadata.file_size(),
            metadata.last_write_time()
        ))
    }
    #[cfg(not(any(unix, windows)))]
    bail!("installed-version state is unsupported on this platform")
}

#[cfg(target_os = "linux")]
fn opened_executable_is_executable(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd as _;

    // libc does not currently expose Linux's AT_EACCESS constant. Using
    // faccessat2 with AT_EMPTY_PATH binds the effective-credential and ACL
    // check to the same open file that is compared below.
    const AT_EACCESS: libc::c_int = 0x200;
    // SAFETY: `file` owns a valid descriptor and the pathname is a static,
    // NUL-terminated empty C string as required with AT_EMPTY_PATH.
    unsafe {
        libc::syscall(
            libc::SYS_faccessat2,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::X_OK,
            libc::AT_EMPTY_PATH | AT_EACCESS,
        ) == 0
    }
}

#[cfg(target_os = "macos")]
fn opened_executable_is_executable(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd as _;

    // `/dev/fd` resolves the already-open vnode, avoiding a second lookup of
    // the mutable installation pathname. AT_EACCESS delegates ownership,
    // groups, ACLs, and privilege semantics to the kernel.
    let Ok(path) = std::ffi::CString::new(format!("/dev/fd/{}", file.as_raw_fd())) else {
        return false;
    };
    // SAFETY: `path` is NUL-terminated and `faccessat` only reads it.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn opened_executable_is_executable(_file: &std::fs::File) -> bool {
    // Unknown Unix hosts cannot safely infer effective-user and ACL access
    // from mode bits alone. Conservatively perform the verified replacement.
    false
}

fn installed_matches_replacement(
    installed_executable: &Path,
    replacement: &Path,
    cancellation: &InstallCancellation,
) -> Result<Option<String>> {
    ensure_not_cancelled(cancellation)?;
    let mut installed = std::fs::File::open(installed_executable).with_context(|| {
        format!(
            "opening installed executable {} for comparison",
            installed_executable.display()
        )
    })?;
    let installed_metadata = installed
        .metadata()
        .context("reading installed executable metadata for comparison")?;
    #[cfg(unix)]
    if !opened_executable_is_executable(&installed) {
        return Ok(None);
    }
    let opened_identity = opened_executable_identity(&installed, &installed_metadata)?;
    let mut replacement =
        std::fs::File::open(replacement).context("opening verified replacement for comparison")?;
    let replacement_metadata = replacement
        .metadata()
        .context("reading verified replacement metadata for comparison")?;
    if !replacement_metadata.is_file()
        || installed_metadata.len() != replacement_metadata.len()
        || installed_metadata.len() > MAX_BINARY_BYTES
    {
        return Ok(None);
    }

    let mut installed_chunk = [0_u8; BINARY_COMPARE_CHUNK_BYTES];
    let mut replacement_chunk = [0_u8; BINARY_COMPARE_CHUNK_BYTES];
    let mut remaining = installed_metadata.len();
    while remaining > 0 {
        ensure_not_cancelled(cancellation)?;
        let chunk = usize::try_from(remaining.min(BINARY_COMPARE_CHUNK_BYTES as u64))
            .context("converting executable comparison chunk size")?;
        installed
            .read_exact(&mut installed_chunk[..chunk])
            .context("reading installed executable for comparison")?;
        replacement
            .read_exact(&mut replacement_chunk[..chunk])
            .context("reading verified replacement for comparison")?;
        if installed_chunk[..chunk] != replacement_chunk[..chunk] {
            return Ok(None);
        }
        remaining -= chunk as u64;
    }
    ensure_not_cancelled(cancellation)?;
    Ok(
        (executable_identity(installed_executable).ok().as_ref() == Some(&opened_identity))
            .then_some(opened_identity),
    )
}

async fn install_release_locked(
    release: &Release,
    installed_executable: &Path,
    progress: impl Fn(InstallProgress),
    cancellation: Arc<InstallCancellation>,
) -> Result<()> {
    ensure_not_cancelled(&cancellation)?;
    let client = client(env!("CARGO_PKG_VERSION"))?;
    progress(InstallProgress::FetchingChecksums);
    let checksum_text = download_text(
        &client,
        &release.checksum_url,
        MAX_CHECKSUM_BYTES,
        "release checksums",
    )
    .await?;
    ensure_not_cancelled(&cancellation)?;
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
        &cancellation,
    )
    .await?;
    ensure_not_cancelled(&cancellation)?;
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
    ensure_not_cancelled(&cancellation)?;
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

    ensure_not_cancelled(&cancellation)?;
    progress(InstallProgress::Installing);
    let installed_executable = installed_executable.to_owned();
    let state_root = update_state_root();
    let version = release.version.clone();
    let comparison_executable = installed_executable.clone();
    let comparison_replacement = replacement.clone();
    let comparison_state_root = state_root.clone();
    let comparison_version = version.clone();
    let comparison_cancellation = Arc::clone(&cancellation);
    let already_installed = tokio::task::spawn_blocking(move || {
        let matched_identity = installed_matches_replacement(
            &comparison_executable,
            &comparison_replacement,
            &comparison_cancellation,
        )
        .unwrap_or(None);
        ensure_not_cancelled(&comparison_cancellation)?;
        if let (Some(identity), Some(state_root)) =
            (matched_identity.as_deref(), comparison_state_root)
        {
            let _ = record_installed_version_for_identity(
                &state_root,
                &comparison_executable,
                identity,
                &comparison_version,
            );
        }
        Ok::<_, anyhow::Error>(matched_identity.is_some())
    })
    .await
    .context("joining installed executable comparison task")??;
    if already_installed {
        return Ok(());
    }

    let installed_parent = installed_executable
        .parent()
        .ok_or_else(|| anyhow!("updated executable path has no parent"))?
        .to_owned();
    tokio::task::spawn_blocking(move || {
        sync_directory(&installed_parent)
            .context("syncing installation directory before replacement")?;
        cancellation.commit_install()?;
        self_replace::self_replace(&replacement).context("replacing the running executable")?;
        // This is deduplication metadata, not part of the replacement commit.
        // Publish it only for the final path identity, and never turn a
        // successful executable replacement into an update failure.
        if let Some(state_root) = state_root {
            let _ = record_installed_version(&state_root, &installed_executable, &version);
        }
        sync_directory(&installed_parent).context("syncing installed executable directory")
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
    cancellation: &InstallCancellation,
) -> Result<String> {
    ensure_not_cancelled(cancellation)?;
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
        ensure_not_cancelled(cancellation)?;
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
    extract_binary_with_expanded_limit(
        archive_path,
        kind,
        binary_name,
        destination,
        MAX_ARCHIVE_EXPANDED_BYTES,
    )
}

fn extract_binary_with_expanded_limit(
    archive_path: &Path,
    kind: ArchiveKind,
    binary_name: &str,
    destination: &Path,
    expanded_limit: u64,
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
                    expanded_limit,
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
            if !zip_mode_is_regular(entry.unix_mode()) {
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
    output
        .sync_all()
        .context("syncing extracted update binary")?;
    drop(output);
    if let Some(parent) = destination.parent() {
        sync_directory(parent).context("syncing update staging directory")?;
    }
    Ok(())
}

fn zip_mode_is_regular(mode: Option<u32>) -> bool {
    const FILE_TYPE_MASK: u32 = 0o170_000;
    const REGULAR_FILE: u32 = 0o100_000;
    mode.is_none_or(|mode| mode & FILE_TYPE_MASK == REGULAR_FILE)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let directory = std::fs::File::open(path)
            .with_context(|| format!("opening directory {}", path.display()))?;
        match directory.sync_all() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(error) => {
                return Err(error).with_context(|| format!("syncing directory {}", path.display()));
            }
        }
    }
    let _ = path;
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

fn add_archive_entry_size(current: u64, entry_size: u64, limit: u64) -> Result<u64> {
    let expanded = current
        .checked_add(entry_size)
        .ok_or_else(|| anyhow!("update archive expanded byte count overflow"))?;
    if expanded > limit {
        bail!("update archive expands beyond the {limit}-byte limit");
    }
    Ok(expanded)
}

fn ensure_not_cancelled(cancellation: &InstallCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
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
    fn zip_entry_modes_reject_links_and_special_files() {
        assert!(zip_mode_is_regular(None));
        assert!(zip_mode_is_regular(Some(0o100_755)));
        assert!(!zip_mode_is_regular(Some(0o120_777)));
        assert!(!zip_mode_is_regular(Some(0o010_644)));
    }

    #[test]
    fn rejects_zip_symlink_at_the_expected_executable_path() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("update.zip");
        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(archive_file);
        archive
            .add_symlink(
                "trouve.exe",
                "elsewhere.exe",
                zip::write::SimpleFileOptions::default().unix_permissions(0o777),
            )
            .unwrap();
        archive.finish().unwrap();

        let error = extract_binary(
            &archive_path,
            ArchiveKind::Zip,
            "trouve.exe",
            &temp.path().join("new-trouve.exe"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
    }

    #[test]
    fn installed_version_state_is_bound_to_the_installed_file_identity() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let executable = temp.path().join("trouve-search");
        std::fs::write(&executable, b"trusted installed executable").unwrap();
        let version = Version::parse("4.1.0").unwrap();
        let compared_identity = executable_identity(&executable).unwrap();

        prepare_private_update_state_root(&state_root).unwrap();
        record_installed_version(&state_root, &executable, &version).unwrap();
        assert_eq!(
            installed_binary_version_at(&state_root, &executable),
            Some(version.clone())
        );

        let untrusted = temp.path().join("untrusted-trouve-search");
        std::fs::write(&untrusted, b"untrusted replacement").unwrap();
        std::fs::remove_file(&executable).unwrap();
        std::fs::rename(&untrusted, &executable).unwrap();
        record_installed_version_for_identity(
            &state_root,
            &executable,
            &compared_identity,
            &version,
        )
        .unwrap();
        assert_eq!(installed_binary_version_at(&state_root, &executable), None);
    }

    #[test]
    fn installed_version_state_rejects_noncanonical_contents() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let executable = temp.path().join("trouve");
        std::fs::write(&executable, b"executable").unwrap();
        prepare_private_update_state_root(&state_root).unwrap();
        let identity = executable_identity(&executable).unwrap();
        let state = update_state_path(&state_root, &executable, &identity);

        std::fs::write(&state, b"4.1.0\nforged\n").unwrap();
        assert_eq!(installed_binary_version_at(&state_root, &executable), None);
        std::fs::write(&state, vec![b'x'; MAX_INSTALLED_VERSION_BYTES as usize + 1]).unwrap();
        assert_eq!(installed_binary_version_at(&state_root, &executable), None);
    }

    #[test]
    fn verified_content_fallback_skips_only_an_exact_installed_binary() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("trouve");
        let replacement = temp.path().join("new-trouve");
        let cancellation = InstallCancellation::default();
        std::fs::write(&executable, b"verified release bytes").unwrap();
        std::fs::write(&replacement, b"verified release bytes").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert!(
                installed_matches_replacement(&executable, &replacement, &cancellation)
                    .unwrap()
                    .is_none()
            );
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o401)).unwrap();
            assert!(
                installed_matches_replacement(&executable, &replacement, &cancellation)
                    .unwrap()
                    .is_none()
            );
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(
            installed_matches_replacement(&executable, &replacement, &cancellation)
                .unwrap()
                .is_some()
        );

        std::fs::write(&replacement, b"different release data").unwrap();
        assert!(
            installed_matches_replacement(&executable, &replacement, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(cancellation.request_cancel());
        assert!(
            installed_matches_replacement(&executable, &replacement, &cancellation)
                .unwrap_err()
                .to_string()
                .contains("update cancelled")
        );
    }

    #[test]
    fn update_support_probe_rejects_a_non_directory_parent() {
        let temp = tempfile::tempdir().unwrap();
        let not_a_directory = temp.path().join("package");
        std::fs::write(&not_a_directory, b"file").unwrap();
        let error = ensure_update_directory_writable(&not_a_directory.join("trouve")).unwrap_err();
        assert!(error.to_string().contains("package-managed"));
    }

    #[test]
    fn expanded_archive_budget_counts_every_tar_entry() {
        let remaining = MAX_ARCHIVE_EXPANDED_BYTES - 1;
        assert_eq!(
            add_archive_entry_size(0, remaining, MAX_ARCHIVE_EXPANDED_BYTES).unwrap(),
            remaining
        );
        let error = add_archive_entry_size(remaining, 2, MAX_ARCHIVE_EXPANDED_BYTES).unwrap_err();
        assert!(error.to_string().contains("expands beyond"));
    }

    #[test]
    fn rejects_tar_with_oversized_unrelated_expansion() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("update.tar.gz");
        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (path, payload) in [
            ("trouve", b"binary".as_slice()),
            ("unrelated-zeroes", [0_u8; 64].as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, path, payload).unwrap();
        }
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();

        let error = extract_binary_with_expanded_limit(
            &archive_path,
            ArchiveKind::TarGz,
            "trouve",
            &temp.path().join("new-trouve"),
            32,
        )
        .unwrap_err();
        assert!(error.to_string().contains("expands beyond"));
    }

    #[test]
    fn cancellation_and_install_commit_are_atomic() {
        let cancelled = InstallCancellation::default();
        assert!(cancelled.request_cancel());
        assert_eq!(
            cancelled.commit_install().unwrap_err().to_string(),
            "update cancelled"
        );

        let committed = InstallCancellation::default();
        committed.commit_install().unwrap();
        assert!(committed.is_committed());
        assert!(!committed.request_cancel());
        assert!(!committed.is_cancelled());
    }

    #[test]
    fn executable_update_lock_serializes_installers() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("trouve-search");
        std::fs::write(&executable, b"binary").unwrap();
        let first = acquire_update_lock_for(&executable, None).unwrap();
        let second_executable = executable.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let second = acquire_update_lock_for(&second_executable, None).unwrap();
            acquired_tx.send(()).unwrap();
            second
        });
        assert!(matches!(
            acquired_rx.recv_timeout(Duration::from_millis(40)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(first);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(waiter.join().unwrap());
    }

    #[test]
    fn executable_update_lock_wait_is_cancellable() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("trouve");
        std::fs::write(&executable, b"binary").unwrap();
        let first = acquire_update_lock_for(&executable, None).unwrap();
        let cancellation = Arc::new(InstallCancellation::default());
        let waiter_cancellation = Arc::clone(&cancellation);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = acquire_update_lock_for(&executable, Some(&waiter_cancellation)).map(drop);
            finished_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cancellation.request_cancel());
        let error = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "update cancelled");
        drop(first);
        waiter.join().unwrap();
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
