//! Persistent cache of shallow clones for remote git URLs.
//!
//! `TrouveIndex::from_git` used to clone into a throwaway temp directory on
//! every call, making the (network-bound) clone the dominant repeated cost of
//! querying a remote repository — chunks and embeddings were already cached
//! by the content-addressed store. Clones now persist under
//! `<cache>/clones/<digest>` keyed by URL (and optional ref) and are
//! refreshed with a cheap `git fetch` once a freshness window has elapsed.
//!
//! Concurrency: each clone directory is guarded by an advisory file lock
//! held (exclusively) for the whole index build, so concurrent processes
//! never observe a half-fetched working tree. Idle clones are evicted after
//! [`IDLE_EVICTION`]; eviction skips anything currently locked.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::store::resolve_cache_folder;

/// Don't refresh a cached clone more often than this (override with
/// `TROUVE_CLONE_TTL`, in seconds; `0` fetches on every call).
const DEFAULT_TTL: Duration = Duration::from_secs(300);

/// Clones not used for this long are deleted during cache access.
const IDLE_EVICTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A locked, up-to-date clone. The advisory lock is held until drop, so the
/// working tree cannot be mutated by another trouve process while an index
/// is being built from it.
pub struct CloneGuard {
    path: PathBuf,
    _lock: File,
}

impl CloneGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn clone_ttl() -> Duration {
    std::env::var("TROUVE_CLONE_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TTL)
}

fn git_timeout() -> u64 {
    std::env::var("TROUVE_CLONE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
}

/// Return a locked clone of `url` (at `git_ref` if given), reusing and
/// refreshing the cached copy when one exists.
pub fn cached_clone(url: &str, git_ref: Option<&str>) -> Result<CloneGuard> {
    cached_clone_at(
        &resolve_cache_folder().join("clones"),
        url,
        git_ref,
        clone_ttl(),
    )
}

/// Remove the entire clone cache. Returns the removed path, if any.
pub fn clear_clones() -> Option<PathBuf> {
    let root = resolve_cache_folder().join("clones");
    if root.exists() && fs::remove_dir_all(&root).is_ok() {
        Some(root)
    } else {
        None
    }
}

fn cache_key(url: &str, git_ref: Option<&str>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(url.as_bytes());
    hasher.update(b"\x00");
    hasher.update(git_ref.unwrap_or("").as_bytes());
    hasher.finalize().to_hex()[..16].to_string()
}

pub(crate) fn cached_clone_at(
    root: &Path,
    url: &str,
    git_ref: Option<&str>,
    ttl: Duration,
) -> Result<CloneGuard> {
    fs::create_dir_all(root).with_context(|| format!("creating clone cache dir {root:?}"))?;
    let key = cache_key(url, git_ref);
    let dir = root.join(&key);
    let lock_path = root.join(format!("{key}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening clone lock {lock_path:?}"))?;
    lock.lock()
        .with_context(|| format!("locking clone lock {lock_path:?}"))?;

    let fetched_stamp = root.join(format!("{key}.fetched"));
    if !dir.join(".git").exists() {
        // Missing or half-created (crashed clone without a rename): rebuild.
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        let partial = root.join(format!("{key}.partial.{}", std::process::id()));
        if partial.exists() {
            fs::remove_dir_all(&partial)?;
        }
        clone(url, git_ref, &partial).inspect_err(|_| {
            let _ = fs::remove_dir_all(&partial);
        })?;
        fs::rename(&partial, &dir)?;
        let _ = fs::write(&fetched_stamp, b"");
    } else if stamp_age(&fetched_stamp)
        .map(|age| age >= ttl)
        .unwrap_or(true)
    {
        // A stale clone is still correct enough to search; failing to reach
        // the remote only costs freshness, so warn instead of erroring.
        match refresh(&dir, git_ref) {
            Ok(()) => {
                let _ = fs::write(&fetched_stamp, b"");
            }
            Err(e) => eprintln!("warning: could not refresh cached clone of {url}: {e:#}"),
        }
    }

    let _ = fs::write(root.join(format!("{key}.used")), b"");
    evict_idle(root, &key, IDLE_EVICTION);
    Ok(CloneGuard {
        path: dir,
        _lock: lock,
    })
}

fn git_command(args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.env("GIT_HTTP_LOW_SPEED_TIME", git_timeout().to_string())
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .context("git is not installed or not on PATH")?;
    if !output.status.success() {
        bail!(
            "git {} failed:\n{}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn clone(url: &str, git_ref: Option<&str>, dest: &Path) -> Result<()> {
    let dest_str = dest.to_string_lossy();
    let mut args = vec!["clone", "--depth", "1"];
    if let Some(r) = git_ref {
        args.extend(["--branch", r]);
    }
    // `--` prevents `url` from being interpreted as a git option.
    args.extend(["--", url, &dest_str]);
    git_command(&args, None).map_err(|e| anyhow::anyhow!("git clone failed for {url:?}: {e:#}"))
}

/// Fetch the tip of `git_ref` (or the remote HEAD) and hard-reset the
/// working tree to it, mirroring what a fresh shallow clone would contain.
fn refresh(dir: &Path, git_ref: Option<&str>) -> Result<()> {
    git_command(
        &["fetch", "--depth", "1", "origin", git_ref.unwrap_or("HEAD")],
        Some(dir),
    )?;
    git_command(&["reset", "--hard", "FETCH_HEAD"], Some(dir))
}

fn stamp_age(path: &Path) -> Option<Duration> {
    fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()
}

/// Best-effort removal of clones unused for longer than `idle`. Skips the
/// clone in use (`keep_key`) and anything another process holds locked.
fn evict_idle(root: &Path, keep_key: &str, idle: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_clone_dir = name.len() == 16
            && name.bytes().all(|b| b.is_ascii_hexdigit())
            && entry.path().is_dir();
        if !is_clone_dir || name == keep_key {
            continue;
        }
        let used = stamp_age(&root.join(format!("{name}.used")))
            .or_else(|| stamp_age(&entry.path()))
            .unwrap_or(Duration::ZERO);
        if used < idle {
            continue;
        }
        let Ok(lock) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(root.join(format!("{name}.lock")))
        else {
            continue;
        };
        if lock.try_lock().is_ok() {
            let _ = fs::remove_dir_all(entry.path());
            let _ = fs::remove_file(root.join(format!("{name}.fetched")));
            let _ = fs::remove_file(root.join(format!("{name}.used")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(args: &[&str], cwd: &Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn init_source_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        run_git(&["init", "-q", "-b", "main"], dir);
        run_git(&["config", "user.email", "t@example.com"], dir);
        run_git(&["config", "user.name", "t"], dir);
        fs::write(dir.join("a.py"), "def one():\n    return 1\n").unwrap();
        run_git(&["add", "."], dir);
        run_git(&["commit", "-q", "-m", "initial"], dir);
    }

    fn file_url(dir: &Path) -> String {
        format!("file://{}", dir.canonicalize().unwrap().display())
    }

    #[test]
    fn clones_once_and_reuses_within_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        init_source_repo(&source);
        let root = tmp.path().join("clones");
        let url = file_url(&source);

        let guard = cached_clone_at(&root, &url, None, Duration::from_secs(3600)).unwrap();
        let clone_path = guard.path().to_path_buf();
        assert!(clone_path.join("a.py").exists());
        drop(guard);

        // Change the source; within the TTL the cached copy is served as-is.
        fs::write(source.join("a.py"), "def two():\n    return 2\n").unwrap();
        run_git(&["commit", "-qam", "update"], &source);
        let guard = cached_clone_at(&root, &url, None, Duration::from_secs(3600)).unwrap();
        assert_eq!(guard.path(), clone_path);
        let content = fs::read_to_string(guard.path().join("a.py")).unwrap();
        assert!(content.contains("one"), "stale content expected within TTL");
    }

    #[test]
    fn refreshes_once_ttl_expires() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        init_source_repo(&source);
        let root = tmp.path().join("clones");
        let url = file_url(&source);

        drop(cached_clone_at(&root, &url, None, Duration::ZERO).unwrap());
        fs::write(source.join("a.py"), "def two():\n    return 2\n").unwrap();
        run_git(&["commit", "-qam", "update"], &source);

        let guard = cached_clone_at(&root, &url, None, Duration::ZERO).unwrap();
        let content = fs::read_to_string(guard.path().join("a.py")).unwrap();
        assert!(content.contains("two"), "TTL=0 must fetch fresh content");
    }

    #[test]
    fn recovers_from_corrupt_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        init_source_repo(&source);
        let root = tmp.path().join("clones");
        let url = file_url(&source);

        let guard = cached_clone_at(&root, &url, None, Duration::from_secs(3600)).unwrap();
        let clone_path = guard.path().to_path_buf();
        drop(guard);
        fs::remove_dir_all(clone_path.join(".git")).unwrap();

        let guard = cached_clone_at(&root, &url, None, Duration::from_secs(3600)).unwrap();
        assert!(guard.path().join(".git").exists());
        assert!(guard.path().join("a.py").exists());
    }

    #[test]
    fn stale_clone_survives_unreachable_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        init_source_repo(&source);
        let root = tmp.path().join("clones");
        let url = file_url(&source);

        drop(cached_clone_at(&root, &url, None, Duration::from_secs(3600)).unwrap());
        fs::remove_dir_all(&source).unwrap();

        // Refresh fails (remote is gone) but the cached clone still serves.
        let guard = cached_clone_at(&root, &url, None, Duration::ZERO).unwrap();
        assert!(guard.path().join("a.py").exists());
    }

    #[test]
    fn keys_differ_by_url_and_ref() {
        let a = cache_key("https://example.com/a.git", None);
        let b = cache_key("https://example.com/b.git", None);
        let c = cache_key("https://example.com/a.git", Some("dev"));
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn evicts_idle_clones_but_keeps_recent_and_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("clones");
        fs::create_dir_all(&root).unwrap();

        // Fake three cached clones: idle, recent, and idle-but-locked.
        for key in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"] {
            fs::create_dir_all(root.join(key)).unwrap();
            fs::write(root.join(format!("{key}.used")), b"").unwrap();
        }
        // Make a and c look idle by backdating their stamps.
        for key in ["aaaaaaaaaaaaaaaa", "cccccccccccccccc"] {
            let stamp = root.join(format!("{key}.used"));
            let old = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
            let file = File::options().write(true).open(&stamp).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let held = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(root.join("cccccccccccccccc.lock"))
            .unwrap();
        held.lock().unwrap();

        evict_idle(&root, "dddddddddddddddd", IDLE_EVICTION);
        assert!(
            !root.join("aaaaaaaaaaaaaaaa").exists(),
            "idle clone evicted"
        );
        assert!(root.join("bbbbbbbbbbbbbbbb").exists(), "recent clone kept");
        assert!(root.join("cccccccccccccccc").exists(), "locked clone kept");
    }
}
