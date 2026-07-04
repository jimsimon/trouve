//! Repository manifest: the list of files to index and a content key per file.
//!
//! For git repositories, blob OIDs from `git ls-files -s` identify clean file
//! content with no file reads at all; dirty and untracked files (from
//! `git status --porcelain`) are hashed directly. All worktrees and branches
//! of one repository share the same store identity (the git common dir).
//! Non-git roots fall back to walking the tree and hashing content, with an
//! mtime+size fast path so unchanged files are not re-read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::store::{ChunkStore, FsManifestRecord};
use crate::walker::{walk_files, TrouveIgnore, DEFAULT_IGNORED_DIRS};

/// One file to index.
#[derive(Debug, Clone)]
pub struct FileRecord {
    /// Absolute path on disk.
    pub abs_path: PathBuf,
    /// Repo-relative path with forward slashes (as stored in chunks).
    pub rel_path: String,
    /// Content key: `git:<blob-oid>` or `b3:<blake3-of-content>`.
    pub content_key: String,
}

/// How the repository identity for the shared store was determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoIdentity {
    /// Git repository: identity is the canonicalized git common dir, shared
    /// across all worktrees and branches.
    Git(String),
    /// Plain directory: identity is the canonicalized path.
    Path(String),
}

impl RepoIdentity {
    pub fn as_str(&self) -> &str {
        match self {
            RepoIdentity::Git(s) | RepoIdentity::Path(s) => s,
        }
    }
}

fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Detect the repository identity for a root directory.
pub fn detect_repo_identity(root: &Path) -> RepoIdentity {
    if let Some(common) = run_git(root, &["rev-parse", "--git-common-dir"]) {
        let common = common.trim();
        if !common.is_empty() {
            let common_path = if Path::new(common).is_absolute() {
                PathBuf::from(common)
            } else {
                root.join(common)
            };
            let canonical = common_path
                .canonicalize()
                .unwrap_or(common_path)
                .to_string_lossy()
                .into_owned();
            return RepoIdentity::Git(canonical);
        }
    }
    let canonical = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    RepoIdentity::Path(canonical)
}

fn matches_extension(rel_path: &str, extensions: &HashSet<String>) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    match name.rfind('.') {
        Some(idx) if idx > 0 => extensions.contains(&name[idx..].to_lowercase()),
        _ => false,
    }
}

fn in_default_ignored_dir(rel_path: &str) -> bool {
    rel_path.split('/').any(|component| {
        DEFAULT_IGNORED_DIRS
            .iter()
            .any(|d| d.trim_end_matches('/') == component)
    })
}

fn hash_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(format!("b3:{}", blake3::hash(&bytes).to_hex()))
}

/// Build the manifest for a git repository root.
fn git_manifest(root: &Path, extensions: &HashSet<String>) -> Result<Vec<FileRecord>> {
    let ls = run_git(root, &["ls-files", "-s", "-z", "--"]).context("git ls-files failed")?;

    // Working-tree state: modified/deleted/untracked paths.
    let status = run_git(
        root,
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )
    .unwrap_or_default();
    let mut dirty: HashSet<String> = HashSet::new();
    let mut deleted: HashSet<String> = HashSet::new();
    let mut untracked: Vec<String> = Vec::new();
    for record in status.split('\0').filter(|s| !s.is_empty()) {
        if record.len() < 4 {
            continue;
        }
        let (code, path) = record.split_at(3);
        let code = &code[..2];
        let path = path.to_string();
        if code == "??" {
            untracked.push(path);
        } else if code.contains('D') {
            deleted.insert(path);
        } else {
            dirty.insert(path);
        }
    }

    let mut records: Vec<FileRecord> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // `.trouveignore` excludes files from indexing without git-ignoring
    // them, so it must be applied on top of the git file listing (git only
    // honours `.gitignore` for untracked files). Checked before hashing so
    // excluded files (e.g. a large generated tree) are never read.
    let mut trouve_ignore = TrouveIgnore::new(root);

    for line in ls.split('\0').filter(|s| !s.is_empty()) {
        // Format: "<mode> <oid> <stage>\t<path>"
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split_whitespace();
        let _mode = parts.next();
        let Some(oid) = parts.next() else { continue };
        let rel = path.to_string();
        if deleted.contains(&rel)
            || !matches_extension(&rel, extensions)
            || in_default_ignored_dir(&rel)
            || trouve_ignore.is_ignored(&rel)
        {
            continue;
        }
        if !seen.insert(rel.clone()) {
            continue;
        }
        let abs = root.join(&rel);
        if dirty.contains(&rel) {
            // Working tree differs from the index: hash actual content.
            if let Some(key) = hash_file(&abs) {
                records.push(FileRecord {
                    abs_path: abs,
                    rel_path: rel,
                    content_key: key,
                });
            }
        } else {
            records.push(FileRecord {
                abs_path: abs,
                rel_path: rel,
                content_key: format!("git:{oid}"),
            });
        }
    }

    // Untracked (but not gitignored) files, hashed directly. The ignore
    // check runs sequentially first (`TrouveIgnore` caches per-directory
    // specs behind `&mut self`), so excluded files skip the parallel hash.
    let untracked: Vec<String> = untracked
        .into_iter()
        .filter(|rel| {
            matches_extension(rel, extensions)
                && !in_default_ignored_dir(rel)
                && !trouve_ignore.is_ignored(rel)
        })
        .collect();
    let hashed: Vec<Option<FileRecord>> = untracked
        .par_iter()
        .map(|rel| {
            let abs = root.join(rel);
            if abs.symlink_metadata().ok()?.is_symlink() {
                return None;
            }
            let key = hash_file(&abs)?;
            Some(FileRecord {
                abs_path: abs,
                rel_path: rel.clone(),
                content_key: key,
            })
        })
        .collect();
    for record in hashed.into_iter().flatten() {
        if seen.insert(record.rel_path.clone()) {
            records.push(record);
        }
    }

    records.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(records)
}

/// Build the manifest for a non-git root by walking and hashing, with an
/// mtime+size fast path stored in the repo's chunk store.
fn fs_manifest(root: &Path, extensions: &[String], store: &ChunkStore) -> Result<Vec<FileRecord>> {
    let files = walk_files(root, extensions, &[]);
    let previous = store.load_fs_manifest();

    let records: Vec<Option<(FileRecord, FsManifestRecord)>> = files
        .par_iter()
        .map(|abs| {
            let rel = abs
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let meta = abs.metadata().ok()?;
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(-1);
            let size = meta.len();
            let content_key = match previous.get(&rel) {
                Some(prev) if prev.mtime_ns == mtime_ns && prev.size == size => {
                    prev.content_key.clone()
                }
                _ => hash_file(abs)?,
            };
            Some((
                FileRecord {
                    abs_path: abs.clone(),
                    rel_path: rel.clone(),
                    content_key: content_key.clone(),
                },
                FsManifestRecord {
                    mtime_ns,
                    size,
                    content_key,
                },
            ))
        })
        .collect();

    let mut out = Vec::new();
    let mut new_manifest = HashMap::new();
    for pair in records.into_iter().flatten() {
        new_manifest.insert(pair.0.rel_path.clone(), pair.1);
        out.push(pair.0);
    }
    let _ = store.save_fs_manifest(&new_manifest);
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

/// Build the file manifest for a root directory.
pub fn build_manifest(
    root: &Path,
    identity: &RepoIdentity,
    extensions: &[String],
    store: &ChunkStore,
) -> Result<Vec<FileRecord>> {
    let extension_set: HashSet<String> = extensions.iter().cloned().collect();
    match identity {
        RepoIdentity::Git(_) => git_manifest(root, &extension_set),
        RepoIdentity::Path(_) => fs_manifest(root, extensions, store),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?} failed");
    }

    fn exts() -> Vec<String> {
        vec![".py".into(), ".rs".into()]
    }

    #[test]
    fn non_git_manifest_hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.py"), "print(1)\n").unwrap();
        fs::write(root.join("b.txt"), "nope").unwrap();
        let store = ChunkStore::open_at(root.join(".teststore")).unwrap();
        let identity = RepoIdentity::Path(root.to_string_lossy().into_owned());
        let manifest = build_manifest(root, &identity, &exts(), &store).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].rel_path, "a.py");
        assert!(manifest[0].content_key.starts_with("b3:"));

        // Fast path: same mtime/size returns the same key without rehashing.
        let manifest2 = build_manifest(root, &identity, &exts(), &store).unwrap();
        assert_eq!(manifest2[0].content_key, manifest[0].content_key);
    }

    #[test]
    fn git_manifest_uses_blob_oids() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        fs::write(root.join("clean.py"), "clean = 1\n").unwrap();
        fs::write(root.join("dirty.py"), "dirty = 1\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "init"]);
        fs::write(root.join("dirty.py"), "dirty = 2\n").unwrap();
        fs::write(root.join("untracked.py"), "new = 1\n").unwrap();

        let identity = detect_repo_identity(root);
        assert!(matches!(identity, RepoIdentity::Git(_)));
        let store = ChunkStore::open_at(root.join(".teststore")).unwrap();
        let manifest = build_manifest(root, &identity, &exts(), &store).unwrap();
        let by_path: HashMap<&str, &FileRecord> =
            manifest.iter().map(|r| (r.rel_path.as_str(), r)).collect();
        assert!(by_path["clean.py"].content_key.starts_with("git:"));
        assert!(by_path["dirty.py"].content_key.starts_with("b3:"));
        assert!(by_path["untracked.py"].content_key.starts_with("b3:"));
    }

    #[test]
    fn git_manifest_skips_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        fs::write(root.join("gone.py"), "x = 1\n").unwrap();
        fs::write(root.join("kept.py"), "y = 1\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "init"]);
        fs::remove_file(root.join("gone.py")).unwrap();

        let identity = detect_repo_identity(root);
        let store = ChunkStore::open_at(root.join(".teststore")).unwrap();
        let manifest = build_manifest(root, &identity, &exts(), &store).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].rel_path, "kept.py");
    }

    #[test]
    fn git_manifest_honours_trouveignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        fs::write(root.join("tracked_secret.py"), "s = 1\n").unwrap();
        fs::write(root.join("kept.py"), "k = 1\n").unwrap();
        fs::create_dir(root.join("generated")).unwrap();
        fs::write(root.join("generated/out.py"), "g = 1\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "init"]);
        // Written after the commit: excludes both tracked and untracked
        // files from indexing without git-ignoring them.
        fs::write(root.join("untracked_secret.py"), "u = 1\n").unwrap();
        fs::write(root.join(".trouveignore"), "*_secret.py\ngenerated/\n").unwrap();

        let identity = detect_repo_identity(root);
        let store = ChunkStore::open_at(root.join(".teststore")).unwrap();
        let manifest = build_manifest(root, &identity, &exts(), &store).unwrap();
        let paths: Vec<&str> = manifest.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["kept.py"]);
    }

    #[test]
    fn git_manifest_honours_nested_trouveignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/.trouveignore"), "local.py\n").unwrap();
        fs::write(root.join("sub/local.py"), "l = 1\n").unwrap();
        fs::write(root.join("sub/other.py"), "o = 1\n").unwrap();
        fs::write(root.join("local.py"), "top = 1\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "init"]);

        let identity = detect_repo_identity(root);
        let store = ChunkStore::open_at(root.join(".teststore")).unwrap();
        let manifest = build_manifest(root, &identity, &exts(), &store).unwrap();
        let paths: Vec<&str> = manifest.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["local.py", "sub/other.py"]);
    }

    #[test]
    fn worktrees_share_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("main");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        fs::write(root.join("a.py"), "x = 1\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "init"]);
        let wt = dir.path().join("wt");
        git(&root, &["worktree", "add", wt.to_str().unwrap()]);

        let id_main = detect_repo_identity(&root);
        let id_wt = detect_repo_identity(&wt);
        assert_eq!(id_main.as_str(), id_wt.as_str());
    }
}
