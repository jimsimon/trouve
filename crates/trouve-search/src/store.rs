//! Content-addressed chunk store.
//!
//! Every per-file artifact (chunks, embedding rows, BM25 token lists) is stored
//! by *content hash* in a per-repository store. Unlike Semble's checkout-local
//! partial reuse, all branches and worktrees of one git repository share the
//! store, so branch switches and incremental edits only pay for content that
//! has never been embedded before.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// Bump when the chunking algorithm, tokenizer, embedding semantics, or
/// entry layout change incompatibly. v2: padding-free (batch-independent)
/// embeddings. v3: flat token storage in entries. v4: Vue and Clojure
/// tree-sitter chunking.
pub const STORE_VERSION: u32 = 4;

const STORE_KEY_LENGTH: usize = 16;
const STORE_IDENTITY_FILE: &str = "identity.json";
const STORE_IDENTITY_VERSION: u32 = 1;
const STORE_LEASES_DIRECTORY: &str = ".leases";
const MAX_STORE_IDENTITY_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoreIdentityMetadata {
    version: u32,
    repo_identity: String,
    /// Full BLAKE3 digest. The store directory uses only its first 16 hex
    /// characters, so retaining the full digest lets cleanup reject a
    /// truncated-key collision rather than deleting the wrong store.
    digest: String,
}

fn repo_identity_digest(repo_identity: &str) -> String {
    blake3::hash(repo_identity.as_bytes()).to_hex().to_string()
}

fn identity_metadata_path(store_root: &Path) -> PathBuf {
    store_root.join(STORE_IDENTITY_FILE)
}

fn open_store_lease_file(store_root: &Path, digest: &str) -> Result<File> {
    let prefix = digest
        .get(..STORE_KEY_LENGTH)
        .context("store identity digest is too short")?;
    let lease_root = store_root.join(STORE_LEASES_DIRECTORY);
    fs::create_dir_all(&lease_root)
        .with_context(|| format!("creating store lease directory {lease_root:?}"))?;
    let metadata = fs::symlink_metadata(&lease_root)
        .with_context(|| format!("reading store lease directory {lease_root:?}"))?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "store lease path is not a regular directory: {lease_root:?}"
    );

    let lease_path = lease_root.join(format!("{prefix}.lock"));
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lease_path)
        .with_context(|| format!("opening store lease {lease_path:?}"))
}

fn acquire_shared_store_lease(store_root: &Path, digest: &str) -> Result<File> {
    let lease = open_store_lease_file(store_root, digest)?;
    fs4::fs_std::FileExt::lock_shared(&lease)
        .with_context(|| format!("locking shared store lease for {digest}"))?;
    Ok(lease)
}

fn load_identity_metadata(store_root: &Path) -> Result<Option<StoreIdentityMetadata>> {
    let path = identity_metadata_path(store_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading metadata for {path:?}")),
    };
    ensure!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "store identity metadata is not a regular file: {path:?}"
    );
    ensure!(
        metadata.len() <= MAX_STORE_IDENTITY_BYTES,
        "store identity metadata is unexpectedly large: {path:?}"
    );
    let bytes = fs::read(&path).with_context(|| format!("reading store identity {path:?}"))?;
    let identity = serde_json::from_slice(&bytes)
        .with_context(|| format!("decoding store identity {path:?}"))?;
    Ok(Some(identity))
}

fn identity_matches_store(store_root: &Path, identity: &StoreIdentityMetadata) -> bool {
    if identity.version != STORE_IDENTITY_VERSION {
        return false;
    }
    let digest = repo_identity_digest(&identity.repo_identity);
    identity.digest == digest
        && store_root.file_name().and_then(|name| name.to_str()) == digest.get(..STORE_KEY_LENGTH)
}

fn validate_identity_metadata(store_root: &Path, expected: &StoreIdentityMetadata) -> Result<()> {
    let actual = load_identity_metadata(store_root)?
        .with_context(|| format!("missing identity metadata in {store_root:?}"))?;
    ensure!(
        actual == *expected && identity_matches_store(store_root, &actual),
        "store identity metadata does not match repository: {store_root:?}"
    );
    Ok(())
}

fn persist_identity_metadata(store_root: &Path, repo_identity: &str, digest: &str) -> Result<()> {
    let expected = StoreIdentityMetadata {
        version: STORE_IDENTITY_VERSION,
        repo_identity: repo_identity.to_string(),
        digest: digest.to_string(),
    };
    if load_identity_metadata(store_root)?.is_some() {
        return validate_identity_metadata(store_root, &expected);
    }

    // A unique same-directory temporary file keeps the final rename atomic
    // and avoids concurrent first opens sharing a predictable temp path.
    let mut temporary = tempfile::NamedTempFile::new_in(store_root)
        .with_context(|| format!("creating identity temp file in {store_root:?}"))?;
    serde_json::to_writer(temporary.as_file_mut(), &expected)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;

    let path = identity_metadata_path(store_root);
    match temporary.persist_noclobber(&path) {
        Ok(file) => {
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            // Another process won the first-open race. Trust it only if it
            // wrote exactly the identity we were about to persist.
            drop(error.file);
            validate_identity_metadata(store_root, &expected)
        }
        Err(error) => {
            Err(error.error).with_context(|| format!("persisting store identity metadata {path:?}"))
        }
    }
}

/// Resolve the trouve cache folder, respecting `TROUVE_CACHE_LOCATION`
/// (highest precedence, with a deprecated `SEMBLE_CACHE_LOCATION` fallback)
/// and platform conventions (XDG on Linux).
pub fn resolve_cache_folder() -> PathBuf {
    let dir = user_cache_override().unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("trouve")
    });
    let _ = fs::create_dir_all(&dir);
    dir
}

fn user_cache_override() -> Option<PathBuf> {
    let (name, loc) =
        crate::utils::env_var_compat("TROUVE_CACHE_LOCATION", "SEMBLE_CACHE_LOCATION")?;
    let p = PathBuf::from(loc);
    if p.is_absolute() {
        Some(p)
    } else {
        eprintln!("warning: {name} is not an absolute path; ignoring");
        None
    }
}

/// A stored chunk: everything needed to reconstruct a [`crate::types::Chunk`]
/// except the repo-relative path (which the manifest supplies at assembly).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChunk {
    pub content: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// Per-file cache record: chunks, embedding rows, and BM25 token lists for the
/// chunk *content* (path-derived enrichment tokens are appended at assembly).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileEntry {
    pub chunks: Vec<StoredChunk>,
    /// Flattened row-major embeddings, `chunks.len() * dim` values.
    pub embeddings: Vec<f32>,
    pub dim: u32,
    /// One token document per chunk (flat storage).
    pub tokens: crate::tokens::TokenDocs,
}

impl FileEntry {
    /// Whether every flattened component has exactly one row/document per
    /// chunk and all nested offsets are safe to dereference.
    fn is_well_formed(&self) -> bool {
        let n_chunks = self.chunks.len();
        let dim = self.dim as usize;
        if (n_chunks > 0 && dim == 0)
            || n_chunks
                .checked_mul(dim)
                .is_none_or(|len| len != self.embeddings.len())
            || self.embeddings.iter().any(|value| !value.is_finite())
            || self.tokens.doc_ends.len() != n_chunks
        {
            return false;
        }

        let Ok(blob_text) = std::str::from_utf8(&self.tokens.blob) else {
            return false;
        };
        let Ok(blob_len) = u32::try_from(self.tokens.blob.len()) else {
            return false;
        };
        let mut previous = 0u32;
        for &end in &self.tokens.token_ends {
            // Empty UTF-8 tokens are supported by TokenDocs' public builder.
            if end < previous || end > blob_len || !blob_text.is_char_boundary(end as usize) {
                return false;
            }
            previous = end;
        }
        if previous != blob_len {
            return false;
        }

        let Ok(token_count) = u32::try_from(self.tokens.token_ends.len()) else {
            return false;
        };
        previous = 0;
        for &end in &self.tokens.doc_ends {
            // Empty token documents are valid, hence non-decreasing rather
            // than strictly increasing offsets here.
            if end < previous || end > token_count {
                return false;
            }
            previous = end;
        }
        previous == token_count
            && self
                .chunks
                .iter()
                .all(|chunk| chunk.start_line > 0 && chunk.end_line >= chunk.start_line)
    }
}

/// A content-addressed store rooted in the trouve cache folder, one per
/// repository identity (git common dir or plain path).
pub struct ChunkStore {
    root: PathBuf,
    _lease: File,
}

impl Drop for ChunkStore {
    fn drop(&mut self) {
        let _ = fs4::fs_std::FileExt::unlock(&self._lease);
    }
}

impl ChunkStore {
    /// Open (creating if needed) the store for a repository identity string.
    pub fn open(repo_identity: &str) -> Result<ChunkStore> {
        Self::open_for_identity_at(&resolve_cache_folder().join("store"), repo_identity)
    }

    fn open_for_identity_at(store_root: &Path, repo_identity: &str) -> Result<ChunkStore> {
        let digest = repo_identity_digest(repo_identity);
        // Acquire the lifetime lease before touching the deletable store
        // directory. Orphan cleanup takes the corresponding exclusive lease,
        // so it must either finish first or observe this store as active.
        let lease = acquire_shared_store_lease(store_root, &digest)?;
        let root = store_root.join(&digest[..STORE_KEY_LENGTH]);
        fs::create_dir_all(&root).with_context(|| format!("creating store dir {root:?}"))?;
        let metadata = fs::symlink_metadata(&root)
            .with_context(|| format!("reading store dir metadata {root:?}"))?;
        ensure!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "store path is not a regular directory: {root:?}"
        );
        persist_identity_metadata(&root, repo_identity, &digest)?;
        Ok(ChunkStore {
            root,
            _lease: lease,
        })
    }

    /// Open a store at an explicit directory (used by tests).
    pub fn open_at(root: PathBuf) -> Result<ChunkStore> {
        let store_root = root
            .parent()
            .context("explicit store path has no parent directory")?;
        let digest = repo_identity_digest(root.to_string_lossy().as_ref());
        let lease = acquire_shared_store_lease(store_root, &digest)?;
        fs::create_dir_all(&root)?;
        Ok(ChunkStore {
            root,
            _lease: lease,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compute the entry key for a file's content + indexing parameters.
    ///
    /// `content_key` is either a git blob OID (`git:<sha1>`) or a working-tree
    /// content hash (`b3:<blake3>`). Language matters because it selects the
    /// grammar; the model id because it determines the embedding rows.
    pub fn entry_key(content_key: &str, language: Option<&str>, model_id: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(content_key.as_bytes());
        hasher.update(b"\x00");
        hasher.update(language.unwrap_or("").as_bytes());
        hasher.update(b"\x00");
        hasher.update(model_id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(&crate::chunk::DESIRED_CHUNK_LENGTH.to_le_bytes());
        hasher.update(&STORE_VERSION.to_le_bytes());
        hasher.finalize().to_hex().to_string()
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.root.join(&key[..2]).join(format!("{key}.bin"))
    }

    /// Load an entry, returning None on miss or corruption.
    pub fn get(&self, key: &str) -> Option<FileEntry> {
        let path = self.entry_path(key);
        let bytes = fs::read(path).ok()?;
        let (entry, consumed): (FileEntry, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
        (consumed == bytes.len() && entry.is_well_formed()).then_some(entry)
    }

    /// Persist an entry atomically (write to temp file, then rename).
    pub fn put(&self, key: &str, entry: &FileEntry) -> Result<()> {
        anyhow::ensure!(entry.is_well_formed(), "refusing malformed store entry");
        let path = self.entry_path(key);
        fs::create_dir_all(path.parent().unwrap())?;
        let bytes = bincode::serde::encode_to_vec(entry, bincode::config::standard())?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entry_path(key).exists()
    }

    /// Load the auxiliary filesystem manifest (mtime/size fast path for
    /// non-git roots). Missing or corrupt manifests return an empty map.
    pub fn load_fs_manifest(&self) -> std::collections::HashMap<String, FsManifestRecord> {
        let path = self.root.join("fs_manifest.bin");
        let Ok(bytes) = fs::read(path) else {
            return Default::default();
        };
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map(|(m, _)| m)
            .unwrap_or_default()
    }

    pub fn save_fs_manifest(
        &self,
        manifest: &std::collections::HashMap<String, FsManifestRecord>,
    ) -> Result<()> {
        let bytes = bincode::serde::encode_to_vec(manifest, bincode::config::standard())?;
        let path = self.root.join("fs_manifest.bin");
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// Entries younger than this are never swept, protecting concurrent builds
/// that have written entries but not yet saved their snapshot.
const GC_GRACE: Duration = Duration::from_secs(60 * 60);

/// Minimum interval between sweeps of one store (tracked via a stamp file),
/// so the entry tree is not rescanned on every build.
const GC_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What a sweep removed.
#[derive(Debug, Default, PartialEq)]
pub struct SweepReport {
    pub entries_removed: usize,
    pub bytes_removed: u64,
}

impl ChunkStore {
    /// Mark-and-sweep GC: delete entries not referenced by any kept snapshot.
    ///
    /// Runs at most once per `GC_INTERVAL` per store; call after a snapshot
    /// save so the current manifest is always in the mark set. Returns `None`
    /// when throttled.
    pub fn maybe_gc(&self) -> Option<SweepReport> {
        let stamp = self.root.join("gc_stamp");
        if let Ok(meta) = fs::metadata(&stamp) {
            let recent = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < GC_INTERVAL);
            if recent {
                return None;
            }
        }
        // Touch the stamp before sweeping so concurrent builds skip out early.
        let _ = fs::write(&stamp, b"");
        let live = crate::snapshot::live_entry_keys(&self.root.join("snapshots"));
        Some(self.sweep(&live, GC_GRACE))
    }

    /// Delete every entry whose key is not in `live` and whose file is older
    /// than `grace`. Also removes stale `*.tmp.*` files from crashed writes.
    ///
    /// Deleting an entry is always safe: the store is a cache, and a miss
    /// just recomputes the file on the next build.
    pub fn sweep(&self, live: &HashSet<String>, grace: Duration) -> SweepReport {
        let mut report = SweepReport::default();
        let Ok(shards) = fs::read_dir(&self.root) else {
            return report;
        };
        for shard in shards.flatten() {
            // Entry shards are two-hex-char directories; skip snapshots,
            // fs_manifest.bin, gc_stamp, and anything else.
            let name = shard.file_name();
            let is_shard = name
                .to_str()
                .is_some_and(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()));
            if !is_shard || !shard.path().is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(shard.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                let Ok(meta) = file.metadata() else { continue };
                let old_enough = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age >= grace);
                if !old_enough {
                    continue;
                }
                let file_name = file.file_name();
                let Some(file_name) = file_name.to_str() else {
                    continue;
                };
                let dead = match file_name.strip_suffix(".bin") {
                    Some(key) => !live.contains(key),
                    // Leftover temp file from a crashed atomic write.
                    None => file_name.contains(".tmp."),
                };
                if dead && fs::remove_file(&path).is_ok() {
                    report.entries_removed += 1;
                    report.bytes_removed += meta.len();
                }
            }
        }
        report
    }
}

/// mtime/size fast-path record for one file in a non-git root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FsManifestRecord {
    pub mtime_ns: i128,
    pub size: u64,
    pub content_key: String,
}

/// Remove all cached indexes and stores. Returns the paths that were removed.
pub fn clear_all_stores() -> Vec<PathBuf> {
    let store_root = resolve_cache_folder().join("store");
    let mut removed = Vec::new();
    if store_root.exists()
        && let Ok(entries) = fs::read_dir(&store_root)
    {
        for entry in entries.flatten() {
            if entry.file_name() == STORE_LEASES_DIRECTORY {
                continue;
            }
            if fs::remove_dir_all(entry.path()).is_ok() {
                removed.push(entry.path());
            }
        }
    }
    removed
}

/// Remove stores whose recorded repository identity no longer exists.
///
/// Cleanup is deliberately conservative: legacy stores without identity
/// metadata, corrupt or unknown metadata, digest/name mismatches, symlinks,
/// and identities whose existence cannot be determined are all left alone.
pub fn clear_orphan_stores() -> Vec<PathBuf> {
    clear_orphan_stores_at(&resolve_cache_folder().join("store"))
}

fn clear_orphan_stores_at(store_root: &Path) -> Vec<PathBuf> {
    clear_orphan_stores_at_with(store_root, |_| {})
}

fn clear_orphan_stores_at_with(
    store_root: &Path,
    mut before_exclusive_lease: impl FnMut(&Path),
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(root_metadata) = fs::symlink_metadata(store_root) else {
        return removed;
    };
    if !root_metadata.file_type().is_dir() || root_metadata.file_type().is_symlink() {
        return removed;
    }
    let Ok(entries) = fs::read_dir(store_root) else {
        return removed;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let Ok(Some(identity)) = load_identity_metadata(&path) else {
            continue;
        };
        if !identity_matches_store(&path, &identity) {
            continue;
        }
        // Repository identities currently cross the store boundary as UTF-8
        // strings. A replacement character may have come from lossy
        // conversion of a non-UTF-8 path, which cannot be reconstructed
        // safely for an existence check.
        if identity.repo_identity.contains('�') {
            continue;
        }
        let identity_path = Path::new(&identity.repo_identity);
        if !identity_path.is_absolute() || !matches!(identity_path.try_exists(), Ok(false)) {
            continue;
        }
        before_exclusive_lease(&path);

        // Lifetime leases live outside the directory being deleted. An open
        // ChunkStore holds a shared lease; cleanup never waits for it or
        // disrupts active indexing, and stale lease files are intentionally
        // retained so future openers always coordinate on the same inode.
        let Ok(lease) = open_store_lease_file(store_root, &identity.digest) else {
            continue;
        };
        if !matches!(fs4::fs_std::FileExt::try_lock_exclusive(&lease), Ok(true)) {
            continue;
        }

        // Every deletion condition is reloaded while the exclusive lease is
        // held. Checks made before the lease are only a cheap candidate filter.
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let Ok(Some(current_identity)) = load_identity_metadata(&path) else {
            continue;
        };
        if current_identity != identity || !identity_matches_store(&path, &current_identity) {
            continue;
        }
        if current_identity.repo_identity.contains('�') {
            continue;
        }
        let current_identity_path = Path::new(&current_identity.repo_identity);
        if !current_identity_path.is_absolute()
            || !matches!(current_identity_path.try_exists(), Ok(false))
        {
            continue;
        }

        // Recheck the entry without following symlinks immediately before
        // removal. remove_dir_all itself does not follow directory symlinks,
        // but this also makes the intended boundary explicit.
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        if fs::remove_dir_all(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_identity(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn concurrent_first_opens_persist_one_full_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let repository = dir.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let identity = std::sync::Arc::new(path_identity(&repository));
        let store_root = std::sync::Arc::new(store_root);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let identity = std::sync::Arc::clone(&identity);
                let store_root = std::sync::Arc::clone(&store_root);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let store = ChunkStore::open_for_identity_at(&store_root, &identity).unwrap();
                    store.root.clone()
                })
            })
            .collect();
        let roots: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert!(roots.windows(2).all(|pair| pair[0] == pair[1]));
        let metadata = load_identity_metadata(&roots[0]).unwrap().unwrap();
        let expected_digest = repo_identity_digest(&identity);
        assert_eq!(metadata.version, STORE_IDENTITY_VERSION);
        assert_eq!(metadata.repo_identity, *identity);
        assert_eq!(metadata.digest, expected_digest);
        assert!(identity_matches_store(&roots[0], &metadata));

        let files: Vec<_> = fs::read_dir(&roots[0])
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(files, [std::ffi::OsString::from(STORE_IDENTITY_FILE)]);
    }

    #[test]
    fn orphan_cleanup_deletes_only_a_verified_missing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let live_repository = dir.path().join("live");
        let deleted_repository = dir.path().join("deleted");
        fs::create_dir(&live_repository).unwrap();
        fs::create_dir(&deleted_repository).unwrap();

        let live_store =
            ChunkStore::open_for_identity_at(&store_root, &path_identity(&live_repository))
                .unwrap();
        let deleted_store =
            ChunkStore::open_for_identity_at(&store_root, &path_identity(&deleted_repository))
                .unwrap();
        let live_store_root = live_store.root.clone();
        let deleted_store_root = deleted_store.root.clone();
        drop(live_store);
        drop(deleted_store);
        fs::remove_dir(&deleted_repository).unwrap();

        let removed = clear_orphan_stores_at(&store_root);

        assert_eq!(removed, std::slice::from_ref(&deleted_store_root));
        assert!(live_store_root.try_exists().unwrap());
        assert!(!deleted_store_root.try_exists().unwrap());
    }

    #[test]
    fn orphan_cleanup_defers_active_store_until_lease_release() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let repository = dir.path().join("repository");
        fs::create_dir(&repository).unwrap();

        let first_store =
            ChunkStore::open_for_identity_at(&store_root, &path_identity(&repository)).unwrap();
        let second_store =
            ChunkStore::open_for_identity_at(&store_root, &path_identity(&repository)).unwrap();
        let active_store_root = first_store.root.clone();
        fs::remove_dir(&repository).unwrap();

        assert!(clear_orphan_stores_at(&store_root).is_empty());
        assert!(active_store_root.try_exists().unwrap());

        drop(first_store);
        assert!(clear_orphan_stores_at(&store_root).is_empty());
        assert!(active_store_root.try_exists().unwrap());

        drop(second_store);
        assert_eq!(
            clear_orphan_stores_at(&store_root),
            std::slice::from_ref(&active_store_root)
        );
        assert!(!active_store_root.try_exists().unwrap());
    }

    #[test]
    fn orphan_cleanup_revalidates_identity_after_candidate_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let repository = dir.path().join("repository");
        fs::create_dir(&repository).unwrap();

        let store =
            ChunkStore::open_for_identity_at(&store_root, &path_identity(&repository)).unwrap();
        let store_path = store.root.clone();
        drop(store);
        fs::remove_dir(&repository).unwrap();

        let removed = clear_orphan_stores_at_with(&store_root, |_| {
            fs::create_dir(&repository).unwrap();
        });

        assert!(removed.is_empty());
        assert!(store_path.try_exists().unwrap());
    }

    #[test]
    fn orphan_cleanup_keeps_git_stores_by_the_live_common_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let git_common_dir = dir.path().join("main").join(".git");
        let disposable_worktree = dir.path().join("worktree");
        fs::create_dir_all(&git_common_dir).unwrap();
        fs::create_dir(&disposable_worktree).unwrap();
        let store =
            ChunkStore::open_for_identity_at(&store_root, &path_identity(&git_common_dir)).unwrap();
        let store_path = store.root.clone();
        drop(store);

        // Removing one checkout does not orphan the repository-wide store:
        // git identities point at the shared common .git directory.
        fs::remove_dir(&disposable_worktree).unwrap();
        assert!(clear_orphan_stores_at(&store_root).is_empty());
        assert!(store_path.try_exists().unwrap());
    }

    #[test]
    fn orphan_cleanup_skips_legacy_corrupt_unknown_and_mismatched_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        fs::create_dir(&store_root).unwrap();

        let legacy = store_root.join("aaaaaaaaaaaaaaaa");
        fs::create_dir(&legacy).unwrap();

        let corrupt = store_root.join("bbbbbbbbbbbbbbbb");
        fs::create_dir(&corrupt).unwrap();
        fs::write(identity_metadata_path(&corrupt), b"{not-json").unwrap();

        let unknown_identity = path_identity(&dir.path().join("missing-unknown"));
        let unknown = ChunkStore::open_for_identity_at(&store_root, &unknown_identity).unwrap();
        let mut unknown_metadata = load_identity_metadata(&unknown.root).unwrap().unwrap();
        unknown_metadata.version += 1;
        fs::write(
            identity_metadata_path(&unknown.root),
            serde_json::to_vec(&unknown_metadata).unwrap(),
        )
        .unwrap();

        let digest_identity = path_identity(&dir.path().join("missing-digest"));
        let digest_store = ChunkStore::open_for_identity_at(&store_root, &digest_identity).unwrap();
        let mut digest_metadata = load_identity_metadata(&digest_store.root).unwrap().unwrap();
        let replacement = if digest_metadata.digest.ends_with('0') {
            "1"
        } else {
            "0"
        };
        digest_metadata.digest.replace_range(63..64, replacement);
        fs::write(
            identity_metadata_path(&digest_store.root),
            serde_json::to_vec(&digest_metadata).unwrap(),
        )
        .unwrap();
        let unknown_root = unknown.root.clone();
        let digest_root = digest_store.root.clone();
        drop(unknown);
        drop(digest_store);

        let prefix_identity = path_identity(&dir.path().join("missing-prefix"));
        let prefix_digest = repo_identity_digest(&prefix_identity);
        let wrong_name = if prefix_digest.starts_with("cccccccccccccccc") {
            "dddddddddddddddd"
        } else {
            "cccccccccccccccc"
        };
        let prefix_mismatch = store_root.join(wrong_name);
        fs::create_dir(&prefix_mismatch).unwrap();
        let prefix_metadata = StoreIdentityMetadata {
            version: STORE_IDENTITY_VERSION,
            repo_identity: prefix_identity,
            digest: prefix_digest,
        };
        fs::write(
            identity_metadata_path(&prefix_mismatch),
            serde_json::to_vec(&prefix_metadata).unwrap(),
        )
        .unwrap();

        assert!(clear_orphan_stores_at(&store_root).is_empty());
        for path in [legacy, corrupt, unknown_root, digest_root, prefix_mismatch] {
            assert!(path.try_exists().unwrap(), "unexpectedly removed {path:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn orphan_cleanup_skips_symlinks_and_identity_existence_errors() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        fs::create_dir(&store_root).unwrap();

        // A symlinked store entry must not be followed even when the target
        // contains otherwise valid metadata for a missing identity.
        let external_root = dir.path().join("external");
        let missing_store_identity = path_identity(&dir.path().join("missing-store-target"));
        let external_store =
            ChunkStore::open_for_identity_at(&external_root, &missing_store_identity).unwrap();
        let store_link = store_root.join(external_store.root.file_name().unwrap());
        symlink(&external_store.root, &store_link).unwrap();

        // Nor may a regular store directory borrow identity metadata through
        // a symlink.
        let missing_metadata_identity = path_identity(&dir.path().join("missing-metadata-target"));
        let metadata_source =
            ChunkStore::open_for_identity_at(&external_root, &missing_metadata_identity).unwrap();
        let metadata_link_store = store_root.join(metadata_source.root.file_name().unwrap());
        fs::create_dir(&metadata_link_store).unwrap();
        symlink(
            identity_metadata_path(&metadata_source.root),
            identity_metadata_path(&metadata_link_store),
        )
        .unwrap();

        // This is absolute and has valid metadata, but statting it fails with
        // InvalidInput. Cleanup must delete only on Ok(false), not on errors.
        let error_identity = format!("/missing-identity-{}\0", std::process::id());
        let error_store = ChunkStore::open_for_identity_at(&store_root, &error_identity).unwrap();
        let error_store_root = error_store.root.clone();
        drop(error_store);
        assert!(Path::new(&error_identity).try_exists().is_err());

        assert!(clear_orphan_stores_at(&store_root).is_empty());
        assert!(
            fs::symlink_metadata(&store_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(identity_metadata_path(&metadata_link_store))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(error_store_root.try_exists().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn orphan_cleanup_preserves_lossy_non_utf8_identity() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        fs::create_dir(&store_root).unwrap();

        // RepoIdentity currently uses a lossy UTF-8 string at the store
        // boundary. The reconstructed string does not name this live path,
        // so cleanup must treat its replacement character as unverifiable.
        let non_utf8_repository = dir
            .path()
            .join(OsString::from_vec(b"live-non-utf8-\xff".to_vec()));
        fs::create_dir(&non_utf8_repository).unwrap();
        let non_utf8_identity = non_utf8_repository.to_string_lossy().into_owned();
        assert!(non_utf8_identity.contains('�'));
        let non_utf8_store =
            ChunkStore::open_for_identity_at(&store_root, &non_utf8_identity).unwrap();
        let non_utf8_store_root = non_utf8_store.root.clone();
        drop(non_utf8_store);

        assert!(clear_orphan_stores_at(&store_root).is_empty());
        assert!(non_utf8_store_root.try_exists().unwrap());
    }

    #[test]
    fn roundtrips_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let entry = FileEntry {
            chunks: vec![StoredChunk {
                content: "fn main() {}".into(),
                start_line: 1,
                end_line: 1,
            }],
            embeddings: vec![0.1, 0.2, 0.3],
            dim: 3,
            tokens: crate::tokens::TokenDocs::from_nested(&[vec!["fn".into(), "main".into()]]),
        };
        let key = ChunkStore::entry_key("b3:abc", Some("rust"), "model-x");
        assert!(store.get(&key).is_none());
        store.put(&key, &entry).unwrap();
        assert!(store.contains(&key));
        let loaded = store.get(&key).unwrap();
        assert_eq!(loaded.chunks.len(), 1);
        assert_eq!(loaded.chunks[0].content, "fn main() {}");
        assert_eq!(loaded.embeddings, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn malformed_entries_are_cache_misses() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let valid = FileEntry {
            chunks: vec![StoredChunk {
                content: "fn main() {}".into(),
                start_line: 1,
                end_line: 1,
            }],
            embeddings: vec![0.1, 0.2],
            dim: 2,
            tokens: crate::tokens::TokenDocs::from_nested(&[vec!["fn".into(), "main".into()]]),
        };

        let mut wrong_embedding_count = valid.clone();
        wrong_embedding_count.embeddings.pop();
        let mut missing_token_doc = valid.clone();
        missing_token_doc.tokens.doc_ends.clear();
        let mut bad_token_offset = valid.clone();
        bad_token_offset.tokens.token_ends[0] = u32::MAX;
        let mut bad_line_range = valid;
        bad_line_range.chunks[0].end_line = 0;

        for (i, malformed) in [
            wrong_embedding_count,
            missing_token_doc,
            bad_token_offset,
            bad_line_range,
        ]
        .into_iter()
        .enumerate()
        {
            let key = ChunkStore::entry_key(&format!("b3:bad-{i}"), Some("rust"), "model-x");
            let path = store.entry_path(&key);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes =
                bincode::serde::encode_to_vec(&malformed, bincode::config::standard()).unwrap();
            fs::write(path, bytes).unwrap();

            assert!(store.get(&key).is_none(), "malformed case {i} was accepted");
            assert!(store.put(&key, &malformed).is_err());
        }
    }

    #[test]
    fn empty_entry_may_retain_batch_dimension() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let entry = FileEntry {
            dim: 256,
            ..FileEntry::default()
        };
        let key = ChunkStore::entry_key("b3:empty", Some("rust"), "model-x");
        store.put(&key, &entry).unwrap();
        assert!(store.get(&key).is_some());
    }

    #[test]
    fn utf8_and_empty_tokens_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let entry = FileEntry {
            chunks: vec![StoredChunk {
                content: "fn café() {}".into(),
                start_line: 1,
                end_line: 1,
            }],
            embeddings: vec![1.0],
            dim: 1,
            tokens: crate::tokens::TokenDocs::from_nested(&[vec!["".into(), "café".into()]]),
        };
        let key = ChunkStore::entry_key("b3:utf8", Some("rust"), "model-x");

        store.put(&key, &entry).unwrap();
        let loaded = store.get(&key).unwrap();

        assert_eq!(loaded.tokens.token(0), b"");
        assert_eq!(loaded.tokens.token(1), "café".as_bytes());
    }

    #[test]
    fn keys_differ_by_language_and_model() {
        let a = ChunkStore::entry_key("b3:abc", Some("rust"), "m1");
        let b = ChunkStore::entry_key("b3:abc", Some("python"), "m1");
        let c = ChunkStore::entry_key("b3:abc", Some("rust"), "m2");
        let d = ChunkStore::entry_key("b3:abd", Some("rust"), "m1");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    fn entry(content: &str) -> FileEntry {
        FileEntry {
            chunks: vec![StoredChunk {
                content: content.into(),
                start_line: 1,
                end_line: 1,
            }],
            embeddings: vec![0.5],
            dim: 1,
            tokens: crate::tokens::TokenDocs::from_nested(&[Vec::new()]),
        }
    }

    #[test]
    fn sweep_removes_dead_entries_and_keeps_live() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let live_key = ChunkStore::entry_key("b3:live", Some("rust"), "m");
        let dead_key = ChunkStore::entry_key("b3:dead", Some("rust"), "m");
        store.put(&live_key, &entry("live")).unwrap();
        store.put(&dead_key, &entry("dead")).unwrap();

        let live: HashSet<String> = [live_key.clone()].into();
        let report = store.sweep(&live, Duration::ZERO);
        assert_eq!(report.entries_removed, 1);
        assert!(report.bytes_removed > 0);
        assert!(store.contains(&live_key));
        assert!(!store.contains(&dead_key));
    }

    #[test]
    fn sweep_respects_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let key = ChunkStore::entry_key("b3:young", Some("rust"), "m");
        store.put(&key, &entry("young")).unwrap();

        // The entry is unreferenced but was written just now.
        let report = store.sweep(&HashSet::new(), Duration::from_secs(3600));
        assert_eq!(report, SweepReport::default());
        assert!(store.contains(&key));
    }

    #[test]
    fn sweep_removes_stale_tmp_files_only_in_shards() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let shard = store.root().join("ab");
        fs::create_dir_all(&shard).unwrap();
        fs::write(shard.join("abcd.tmp.123"), b"crashed write").unwrap();
        // Non-shard files and directories are never touched.
        store.save_fs_manifest(&Default::default()).unwrap();
        let snapdir = store.root().join("snapshots");
        fs::create_dir_all(&snapdir).unwrap();
        fs::write(snapdir.join("x.snap"), b"snapshot").unwrap();

        let report = store.sweep(&HashSet::new(), Duration::ZERO);
        assert_eq!(report.entries_removed, 1);
        assert!(!shard.join("abcd.tmp.123").exists());
        assert!(store.root().join("fs_manifest.bin").exists());
        assert!(snapdir.join("x.snap").exists());
    }

    #[test]
    fn maybe_gc_is_throttled_by_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        assert!(store.maybe_gc().is_some());
        assert!(store.maybe_gc().is_none(), "second run within interval");
    }

    #[test]
    fn fs_manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open_at(dir.path().join("s")).unwrap();
        let mut m = std::collections::HashMap::new();
        m.insert(
            "src/a.py".to_string(),
            FsManifestRecord {
                mtime_ns: 123,
                size: 42,
                content_key: "b3:xyz".into(),
            },
        );
        store.save_fs_manifest(&m).unwrap();
        assert_eq!(store.load_fs_manifest(), m);
    }
}
