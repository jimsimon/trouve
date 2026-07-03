//! Assembled-index snapshots: the warm-query fast path.
//!
//! Assembling an index from the per-file store is incremental but still costs
//! one store read per manifest record plus a full BM25/dense rebuild. When
//! nothing changed since the last build that work is pure overhead, so after
//! every assembly the finished index is written to a single snapshot file
//! keyed by a hash of the manifest. On the next build with the same manifest
//! the snapshot is memory-mapped: embeddings and BM25 postings are used
//! zero-copy straight out of the mapping, and only the chunk table is
//! materialized.
//!
//! Layout (little-endian, every section 8-byte aligned):
//!
//! ```text
//! magic "SMBLSNP1" | manifest hash (32) | meta len u64 | meta (bincode)
//! chunk records | text blob | embeddings f32 | bm25 term blob
//! bm25 term offsets u32 | bm25 posting offsets u64 | bm25 postings
//! ```

use std::io::Write;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::bm25::Bm25Index;
use crate::dense::DenseIndex;
use crate::languages::detect_language;
use crate::types::Chunk;

const MAGIC: &[u8; 8] = b"SMBLSNP1";
/// Bump when the snapshot layout changes incompatibly.
pub const SNAPSHOT_VERSION: u32 = 1;
/// Snapshots kept per store before the oldest are pruned. Covers a handful of
/// branches/worktrees sharing one store without unbounded growth.
const KEEP_SNAPSHOTS: usize = 4;

/// A BM25 posting: precomputed `idf * tf` contribution of one document.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Posting {
    pub doc: u32,
    pub score: f32,
}

/// A typed view over either owned memory or a region of a shared mmap.
///
/// Lets [`DenseIndex`] and [`Bm25Index`] run identically whether they were
/// built in-process or loaded zero-copy from a snapshot.
pub enum Buf<T: Pod> {
    Owned(Vec<T>),
    Mapped {
        map: Arc<Mmap>,
        offset: usize,
        len: usize,
        _marker: PhantomData<T>,
    },
}

impl<T: Pod> Buf<T> {
    fn mapped(map: &Arc<Mmap>, offset: usize, len: usize) -> Buf<T> {
        Buf::Mapped {
            map: Arc::clone(map),
            offset,
            len,
            _marker: PhantomData,
        }
    }
}

impl<T: Pod> Deref for Buf<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match self {
            Buf::Owned(v) => v,
            Buf::Mapped {
                map, offset, len, ..
            } => bytemuck::cast_slice(&map[*offset..*offset + *len * std::mem::size_of::<T>()]),
        }
    }
}

impl<T: Pod> From<Vec<T>> for Buf<T> {
    fn from(v: Vec<T>) -> Buf<T> {
        Buf::Owned(v)
    }
}

/// Fixed-size per-chunk record; text and paths live in shared blobs/tables.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct ChunkRecord {
    text_off: u64,
    text_len: u32,
    file_id: u32,
    start_line: u32,
    end_line: u32,
}

#[derive(Serialize, Deserialize)]
struct SnapshotMeta {
    version: u32,
    model_id: String,
    /// Unique file paths; `ChunkRecord.file_id` indexes into this.
    files: Vec<String>,
    dim: u64,
    n_chunks: u64,
    texts_len: u64,
    term_blob_len: u64,
    n_terms: u64,
    n_postings: u64,
    num_docs: u64,
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

fn snapshot_path(dir: &Path, manifest_hash: &[u8; 32]) -> PathBuf {
    let hex: String = manifest_hash[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(format!("{hex}.snap"))
}

/// Writer that tracks position and pads each section to 8-byte alignment.
struct SectionWriter<W: Write> {
    inner: W,
    written: usize,
}

impl<W: Write> SectionWriter<W> {
    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.inner.write_all(bytes)?;
        self.written += bytes.len();
        Ok(())
    }

    fn pad(&mut self) -> Result<()> {
        static ZEROS: [u8; 8] = [0; 8];
        let target = align8(self.written);
        if target > self.written {
            self.inner.write_all(&ZEROS[..target - self.written])?;
            self.written = target;
        }
        Ok(())
    }

    fn section(&mut self, bytes: &[u8]) -> Result<()> {
        self.pad()?;
        self.put(bytes)
    }
}

/// Serialize an assembled index to `dir`, atomically. Returns the final path.
pub fn save(
    dir: &Path,
    manifest_hash: &[u8; 32],
    model_id: &str,
    chunks: &[Chunk],
    dense: &DenseIndex,
    bm25: &Bm25Index,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = snapshot_path(dir, manifest_hash);
    if path.exists() {
        return Ok(path);
    }

    // Dedupe file paths preserving first-seen (manifest) order.
    let mut files: Vec<String> = Vec::new();
    let mut file_ids: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    let mut records: Vec<ChunkRecord> = Vec::with_capacity(chunks.len());
    let mut texts_len = 0u64;
    for chunk in chunks {
        let file_id = *file_ids.entry(chunk.file_path.as_str()).or_insert_with(|| {
            files.push(chunk.file_path.clone());
            (files.len() - 1) as u32
        });
        records.push(ChunkRecord {
            text_off: texts_len,
            text_len: chunk.content.len() as u32,
            file_id,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
        });
        texts_len += chunk.content.len() as u64;
    }

    let (term_blob, term_offsets, posting_offsets, postings) = bm25.flat_parts();
    let meta = SnapshotMeta {
        version: SNAPSHOT_VERSION,
        model_id: model_id.to_string(),
        files,
        dim: dense.dim() as u64,
        n_chunks: chunks.len() as u64,
        texts_len,
        term_blob_len: term_blob.len() as u64,
        n_terms: term_offsets.len().saturating_sub(1) as u64,
        n_postings: postings.len() as u64,
        num_docs: bm25.num_docs() as u64,
    };
    let meta_bytes = bincode::serde::encode_to_vec(&meta, bincode::config::standard())?;

    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let file = std::fs::File::create(&tmp)?;
    let mut w = SectionWriter {
        inner: std::io::BufWriter::with_capacity(1 << 20, file),
        written: 0,
    };
    w.put(MAGIC)?;
    w.put(manifest_hash)?;
    w.put(&(meta_bytes.len() as u64).to_le_bytes())?;
    w.put(&meta_bytes)?;
    w.section(bytemuck::cast_slice(&records))?;
    w.pad()?;
    for chunk in chunks {
        w.put(chunk.content.as_bytes())?;
    }
    w.section(bytemuck::cast_slice(dense.vectors()))?;
    w.section(term_blob)?;
    w.section(bytemuck::cast_slice(term_offsets))?;
    w.section(bytemuck::cast_slice(posting_offsets))?;
    w.section(bytemuck::cast_slice(postings))?;
    w.inner
        .into_inner()
        .map_err(|e| anyhow::anyhow!("snapshot write failed: {e}"))?
        .sync_all()?;
    std::fs::rename(&tmp, &path)?;
    prune(dir);
    Ok(path)
}

/// The pieces of an index reconstructed from a snapshot.
pub struct LoadedSnapshot {
    pub chunks: Vec<Chunk>,
    pub dense: DenseIndex,
    pub bm25: Bm25Index,
}

/// Try to load the snapshot for `manifest_hash`. Returns `None` on any miss,
/// mismatch, or corruption (the caller falls back to normal assembly).
pub fn load(dir: &Path, manifest_hash: &[u8; 32], model_id: &str) -> Option<LoadedSnapshot> {
    let path = snapshot_path(dir, manifest_hash);
    let file = std::fs::File::open(&path).ok()?;
    // SAFETY: the snapshot is private to the semble cache and replaced only by
    // atomic renames; an existing mapping stays valid after replacement.
    let map = Arc::new(unsafe { Mmap::map(&file).ok()? });
    load_from(map, manifest_hash, model_id).ok()
}

fn load_from(map: Arc<Mmap>, manifest_hash: &[u8; 32], model_id: &str) -> Result<LoadedSnapshot> {
    let need = |end: usize| -> Result<()> {
        if map.len() < end {
            bail!("snapshot truncated");
        }
        Ok(())
    };
    need(48)?;
    if &map[..8] != MAGIC || &map[8..40] != manifest_hash {
        bail!("snapshot magic/hash mismatch");
    }
    let meta_len = u64::from_le_bytes(map[40..48].try_into().unwrap()) as usize;
    need(48 + meta_len)?;
    let (meta, _): (SnapshotMeta, usize) =
        bincode::serde::decode_from_slice(&map[48..48 + meta_len], bincode::config::standard())?;
    if meta.version != SNAPSHOT_VERSION || meta.model_id != model_id {
        bail!("snapshot version/model mismatch");
    }

    let mut cursor = align8(48 + meta_len);
    let mut section = |len_bytes: usize| -> Result<usize> {
        let offset = cursor;
        need(offset + len_bytes)?;
        cursor = align8(offset + len_bytes);
        Ok(offset)
    };

    let n_chunks = meta.n_chunks as usize;
    let dim = meta.dim as usize;
    let n_offsets = meta.n_terms as usize + 1;
    let records_len = n_chunks * std::mem::size_of::<ChunkRecord>();
    let records_off = section(records_len)?;
    let texts_off = section(meta.texts_len as usize)?;
    let embed_off = section(n_chunks * dim * 4)?;
    let term_blob_off = section(meta.term_blob_len as usize)?;
    let term_offsets_off = section(n_offsets * 4)?;
    let posting_offsets_off = section(n_offsets * 8)?;
    let postings_off = section(meta.n_postings as usize * std::mem::size_of::<Posting>())?;

    let records: &[ChunkRecord] =
        bytemuck::cast_slice(&map[records_off..records_off + records_len]);
    let texts = &map[texts_off..texts_off + meta.texts_len as usize];

    // Language is a pure function of the path: compute once per unique file.
    let languages: Vec<Option<String>> = meta
        .files
        .iter()
        .map(|f| detect_language(Path::new(f)).map(|l| l.to_string()))
        .collect();

    let mut chunks: Vec<Chunk> = Vec::with_capacity(n_chunks);
    for rec in records {
        let file_id = rec.file_id as usize;
        if file_id >= meta.files.len() {
            bail!("snapshot file_id out of range");
        }
        let start = rec.text_off as usize;
        let end = start + rec.text_len as usize;
        if end > texts.len() {
            bail!("snapshot text range out of bounds");
        }
        let content = std::str::from_utf8(&texts[start..end])
            .context("snapshot text not utf-8")?
            .to_string();
        chunks.push(Chunk {
            content,
            file_path: meta.files[file_id].clone(),
            start_line: rec.start_line,
            end_line: rec.end_line,
            language: languages[file_id].clone(),
        });
    }

    let dense = DenseIndex::from_parts(Buf::mapped(&map, embed_off, n_chunks * dim), dim, n_chunks);
    let bm25 = Bm25Index::from_parts(
        Buf::mapped(&map, term_blob_off, meta.term_blob_len as usize),
        Buf::mapped(&map, term_offsets_off, n_offsets),
        Buf::mapped(&map, posting_offsets_off, n_offsets),
        Buf::mapped(&map, postings_off, meta.n_postings as usize),
        meta.num_docs as usize,
    );

    Ok(LoadedSnapshot {
        chunks,
        dense,
        bm25,
    })
}

/// Keep only the newest [`KEEP_SNAPSHOTS`] snapshot files in `dir`.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut snaps: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "snap"))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    if snaps.len() <= KEEP_SNAPSHOTS {
        return;
    }
    snaps.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    for (_, path) in snaps.into_iter().skip(KEEP_SNAPSHOTS) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;

    fn sample() -> (Vec<Chunk>, DenseIndex, Bm25Index) {
        let chunks = vec![
            Chunk {
                content: "fn alpha() { save_model() }".into(),
                file_path: "src/a.rs".into(),
                start_line: 1,
                end_line: 3,
                language: Some("rust".into()),
            },
            Chunk {
                content: "fn beta() { load_model() }".into(),
                file_path: "src/a.rs".into(),
                start_line: 5,
                end_line: 7,
                language: Some("rust".into()),
            },
            Chunk {
                content: "def gamma(): pass".into(),
                file_path: "lib/b.py".into(),
                start_line: 1,
                end_line: 1,
                language: Some("python".into()),
            },
        ];
        let dense = DenseIndex::new(vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 1.0],
        ]);
        let docs: Vec<Vec<String>> = chunks.iter().map(|c| tokenize(&c.content)).collect();
        let bm25 = Bm25Index::build(&docs);
        (chunks, dense, bm25)
    }

    #[test]
    fn roundtrips_chunks_dense_and_bm25() {
        let dir = tempfile::tempdir().unwrap();
        let (chunks, dense, bm25) = sample();
        let hash = [7u8; 32];
        save(dir.path(), &hash, "model-x", &chunks, &dense, &bm25).unwrap();

        let snap = load(dir.path(), &hash, "model-x").expect("snapshot should load");
        assert_eq!(snap.chunks, chunks);

        let q = [1.0, 0.0, 0.0];
        assert_eq!(snap.dense.query(&q, 3, None), dense.query(&q, 3, None));

        let query = tokenize("save model");
        assert_eq!(
            snap.bm25.get_scores(&query, None),
            bm25.get_scores(&query, None)
        );
    }

    #[test]
    fn wrong_hash_or_model_misses() {
        let dir = tempfile::tempdir().unwrap();
        let (chunks, dense, bm25) = sample();
        let hash = [7u8; 32];
        save(dir.path(), &hash, "model-x", &chunks, &dense, &bm25).unwrap();

        assert!(load(dir.path(), &[8u8; 32], "model-x").is_none());
        assert!(load(dir.path(), &hash, "other-model").is_none());
    }

    #[test]
    fn corrupt_snapshot_misses() {
        let dir = tempfile::tempdir().unwrap();
        let (chunks, dense, bm25) = sample();
        let hash = [7u8; 32];
        let path = save(dir.path(), &hash, "model-x", &chunks, &dense, &bm25).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load(dir.path(), &hash, "model-x").is_none());
    }

    #[test]
    fn prunes_old_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let (chunks, dense, bm25) = sample();
        for i in 0..(KEEP_SNAPSHOTS as u8 + 3) {
            save(dir.path(), &[i; 32], "model-x", &chunks, &dense, &bm25).unwrap();
        }
        let count = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "snap"))
            .count();
        assert_eq!(count, KEEP_SNAPSHOTS);
    }

    #[test]
    fn empty_bm25_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let chunks = vec![Chunk {
            content: "x".into(),
            file_path: "a.rs".into(),
            start_line: 1,
            end_line: 1,
            language: None,
        }];
        let dense = DenseIndex::new(vec![vec![1.0]]);
        let bm25 = Bm25Index::build(&[Vec::new()]);
        let hash = [1u8; 32];
        save(dir.path(), &hash, "m", &chunks, &dense, &bm25).unwrap();
        let snap = load(dir.path(), &hash, "m").unwrap();
        assert_eq!(snap.bm25.num_docs(), 1);
        assert!(snap
            .bm25
            .get_scores(&["anything".into()], None)
            .iter()
            .all(|s| *s == 0.0));
    }
}
