//! Content-addressed chunk store.
//!
//! The central design change from upstream Semble: instead of an all-or-nothing
//! cached index per path, every per-file artifact (chunks, embedding rows, BM25
//! token lists) is stored keyed by *content hash* in a per-repository store.
//! All branches and worktrees of one git repository share a store, so branch
//! switches and incremental edits only pay for content that has never been
//! embedded before.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Bump when the chunking algorithm, tokenizer, embedding semantics, or
/// entry layout change incompatibly. v2: padding-free (batch-independent)
/// embeddings. v3: flat token storage in entries.
pub const STORE_VERSION: u32 = 3;

/// Resolve the semble cache folder, respecting `SEMBLE_CACHE_LOCATION`
/// (highest precedence) and platform conventions (XDG on Linux).
pub fn resolve_cache_folder() -> PathBuf {
    let dir = user_cache_override().unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("semble")
    });
    let _ = fs::create_dir_all(&dir);
    dir
}

fn user_cache_override() -> Option<PathBuf> {
    let loc = std::env::var("SEMBLE_CACHE_LOCATION").ok()?;
    let p = PathBuf::from(loc);
    if p.is_absolute() {
        Some(p)
    } else {
        eprintln!("warning: SEMBLE_CACHE_LOCATION is not an absolute path; ignoring");
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

/// A content-addressed store rooted in the semble cache folder, one per
/// repository identity (git common dir, remote URL, or plain path).
pub struct ChunkStore {
    root: PathBuf,
}

impl ChunkStore {
    /// Open (creating if needed) the store for a repository identity string.
    pub fn open(repo_identity: &str) -> Result<ChunkStore> {
        let digest = blake3::hash(repo_identity.as_bytes()).to_hex().to_string();
        let root = resolve_cache_folder().join("store").join(&digest[..16]);
        fs::create_dir_all(&root).with_context(|| format!("creating store dir {root:?}"))?;
        Ok(ChunkStore { root })
    }

    /// Open a store at an explicit directory (used by tests).
    pub fn open_at(root: PathBuf) -> Result<ChunkStore> {
        fs::create_dir_all(&root)?;
        Ok(ChunkStore { root })
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
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .ok()
            .map(|(entry, _)| entry)
    }

    /// Persist an entry atomically (write to temp file, then rename).
    pub fn put(&self, key: &str, entry: &FileEntry) -> Result<()> {
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
    if store_root.exists() {
        if let Ok(entries) = fs::read_dir(&store_root) {
            for entry in entries.flatten() {
                if fs::remove_dir_all(entry.path()).is_ok() {
                    removed.push(entry.path());
                }
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn keys_differ_by_language_and_model() {
        let a = ChunkStore::entry_key("b3:abc", Some("rust"), "m1");
        let b = ChunkStore::entry_key("b3:abc", Some("python"), "m1");
        let c = ChunkStore::entry_key("b3:abc", Some("rust"), "m2");
        let d = ChunkStore::entry_key("b3:abd", Some("rust"), "m1");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
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
