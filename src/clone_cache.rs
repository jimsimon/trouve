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
//! `IDLE_EVICTION` (one week); eviction skips anything currently locked.

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
    crate::utils::env_var_compat("TROUVE_CLONE_TIMEOUT", "SEMBLE_CLONE_TIMEOUT")
        .and_then(|(_, v)| v.parse().ok())
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

/// What clearing the clone cache accomplished.
#[derive(Debug, Default, PartialEq)]
pub struct ClearReport {
    pub root: PathBuf,
    pub removed: usize,
    /// Clones another process held locked (its index build keeps its tree).
    pub skipped_locked: usize,
}

/// Remove cached clones, honouring per-key advisory locks so a concurrent
/// index build never loses its working tree. Returns `None` when no clone
/// cache exists.
pub fn clear_clones() -> Option<ClearReport> {
    let root = resolve_cache_folder().join("clones");
    if !root.exists() {
        return None;
    }
    // Reclaim everything reclaimable, regardless of age.
    let report = evict_all(&root, Duration::ZERO);
    // Best effort: drop the (now mostly empty) root when nothing holds it.
    if report.skipped_locked == 0 {
        let _ = fs::remove_dir_all(&root);
    }
    Some(report)
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
        // the remote only costs freshness, so warn instead of erroring. The
        // stamp advances either way — retrying an unreachable remote on
        // every call would block each query on network timeouts.
        if let Err(e) = refresh(&dir, git_ref) {
            eprintln!("warning: could not refresh cached clone of {url}: {e:#}");
        }
        let _ = fs::write(&fetched_stamp, b"");
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
    // `--end-of-options` prevents a ref beginning with `-` from being
    // interpreted as a git option.
    git_command(
        &[
            "fetch",
            "--depth",
            "1",
            "origin",
            "--end-of-options",
            git_ref.unwrap_or("HEAD"),
        ],
        Some(dir),
    )?;
    git_command(&["reset", "--hard", "FETCH_HEAD"], Some(dir))
}

fn stamp_age(path: &Path) -> Option<Duration> {
    fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()
}

fn is_cache_key(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Best-effort removal of clones unused for longer than `idle`, skipping
/// the clone in use (`keep_key`).
fn evict_idle(root: &Path, keep_key: &str, idle: Duration) {
    evict(root, Some(keep_key), idle);
}

/// Remove every reclaimable clone regardless of age.
fn evict_all(root: &Path, idle: Duration) -> ClearReport {
    evict(root, None, idle)
}

/// Remove clones unused for longer than `idle` — plus orphaned
/// `<key>.partial.<pid>` directories left by crashed clones — honouring the
/// per-key advisory locks: the clone in use (`keep_key`) and anything
/// another process holds locked are skipped. Lock files themselves are
/// kept: they are empty, and deleting one that another process is about to
/// open would let two processes hold "the" lock for one key.
fn evict(root: &Path, keep_key: Option<&str>, idle: Duration) -> ClearReport {
    let mut report = ClearReport {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let Ok(entries) = fs::read_dir(root) else {
        return report;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !entry.path().is_dir() {
            continue;
        }
        let (key, is_partial) = match name.split_once(".partial.") {
            Some((key, _)) => (key, true),
            None => (name, false),
        };
        if !is_cache_key(key) || Some(key) == keep_key {
            continue;
        }
        // A partial's freshness is its own mtime: judging it by the key's
        // `.used` stamp would let an active completed clone shield an
        // orphaned partial from reclamation indefinitely.
        let used = if is_partial {
            stamp_age(&entry.path())
        } else {
            stamp_age(&root.join(format!("{key}.used"))).or_else(|| stamp_age(&entry.path()))
        }
        .unwrap_or(Duration::ZERO);
        if used < idle {
            continue;
        }
        let Ok(lock) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(root.join(format!("{key}.lock")))
        else {
            continue;
        };
        if lock.try_lock().is_ok() {
            if fs::remove_dir_all(entry.path()).is_ok() {
                report.removed += 1;
            }
            // Stamps belong to the completed clone; keep them when only an
            // orphaned partial was reclaimed.
            if !is_partial {
                let _ = fs::remove_file(root.join(format!("{key}.fetched")));
                let _ = fs::remove_file(root.join(format!("{key}.used")));
            }
        } else {
            report.skipped_locked += 1;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Backdate a file or directory's mtime by a month. `filetime` works on
    /// directories on every platform (std `File::open` cannot open a
    /// directory on Windows).
    fn backdate(path: &Path) {
        let old = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(old)).unwrap();
    }

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
            backdate(&root.join(format!("{key}.used")));
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

    #[test]
    fn evict_all_reclaims_everything_except_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("clones");
        for key in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"] {
            fs::create_dir_all(root.join(key)).unwrap();
            fs::write(root.join(format!("{key}.used")), b"").unwrap();
        }
        let held = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(root.join("bbbbbbbbbbbbbbbb.lock"))
            .unwrap();
        held.lock().unwrap();

        // Even brand-new clones are reclaimed — except the locked one,
        // whose in-progress index build keeps its working tree.
        let report = evict_all(&root, Duration::ZERO);
        assert_eq!(report.removed, 1);
        assert_eq!(report.skipped_locked, 1);
        assert!(!root.join("aaaaaaaaaaaaaaaa").exists());
        assert!(root.join("bbbbbbbbbbbbbbbb").exists(), "locked clone kept");
    }

    #[test]
    fn evicts_orphaned_partial_clones() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("clones");
        // A crashed clone left an old partial dir behind, while the same
        // key's completed clone is alive and recently used. The partial is
        // judged by its own mtime, so it must be reclaimed anyway — and the
        // completed clone and its stamps must survive.
        let key = "aaaaaaaaaaaaaaaa";
        let partial = root.join(format!("{key}.partial.12345"));
        fs::create_dir_all(&partial).unwrap();
        backdate(&partial);
        fs::create_dir_all(root.join(key)).unwrap();
        fs::write(root.join(format!("{key}.used")), b"").unwrap();

        evict_idle(&root, "dddddddddddddddd", IDLE_EVICTION);
        assert!(!partial.exists(), "orphaned partial clone evicted");
        assert!(root.join(key).exists(), "completed clone kept");
        assert!(
            root.join(format!("{key}.used")).exists(),
            "stamps of the completed clone kept"
        );
    }
}
