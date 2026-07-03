//! Assembled-index snapshots: the warm-query and incremental fast paths.
//!
//! Assembling an index from the per-file store is incremental but still costs
//! one store read per manifest record plus a full BM25/dense rebuild. After
//! every assembly the finished index is written to a single snapshot file
//! keyed by a hash of the manifest, enabling two fast paths:
//!
//! - **Exact match** (nothing changed): the snapshot is memory-mapped and
//!   embeddings/postings are used zero-copy straight out of the mapping.
//! - **Patch** (a few files changed): the newest snapshot is diffed against
//!   the new manifest; unchanged rows are spliced out of the old mapping and
//!   only changed files pay for store reads or recomputation. BM25 postings
//!   store raw term frequencies (scoring stats are applied at query time), so
//!   the patched index is exactly what a full rebuild would produce.
//!
//! Layout (little-endian, every section 8-byte aligned):
//!
//! ```text
//! magic "SMBLSNP2" | manifest hash (32) | meta len u64 | meta (bincode)
//! chunk records | text blob | embeddings f32 | bm25 term blob
//! bm25 term offsets u32 | bm25 posting offsets u64 | bm25 postings
//! bm25 doc lengths u32
//! ```

use std::collections::HashMap;
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
use crate::store::FileEntry;
use crate::types::Chunk;

const MAGIC: &[u8; 8] = b"SMBLSNP3";
/// Bump when the snapshot layout or embedding semantics change incompatibly.
/// v3: padding-free (batch-independent) embeddings.
pub const SNAPSHOT_VERSION: u32 = 3;
/// Snapshots kept per store before the oldest are pruned. Covers a handful of
/// branches/worktrees sharing one store without unbounded growth.
const KEEP_SNAPSHOTS: usize = 4;

/// A BM25 posting: raw term frequency of one term in one document.
///
/// Stores `tf` rather than a precomputed score so postings are per-document
/// stable: `idf` and the length norm (which depend on global corpus stats)
/// are computed at query time, letting snapshots be patched incrementally.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct Posting {
    pub doc: u32,
    pub tf: u32,
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
    pub(crate) fn mapped(map: &Arc<Mmap>, offset: usize, len: usize) -> Buf<T> {
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

/// One manifest file baked into the snapshot: what content produced which
/// contiguous run of chunk rows. Sorted by `rel_path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub rel_path: String,
    pub content_key: String,
    pub first_row: u32,
    pub n_rows: u32,
}

#[derive(Serialize, Deserialize)]
struct SnapshotMeta {
    version: u32,
    model_id: String,
    /// Content types this index was built with (must match to reuse).
    content: Vec<String>,
    /// The manifest this snapshot was assembled from.
    manifest: Vec<ManifestEntry>,
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
#[allow(clippy::too_many_arguments)]
pub fn save(
    dir: &Path,
    manifest_hash: &[u8; 32],
    model_id: &str,
    content: &[String],
    manifest: Vec<ManifestEntry>,
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
    let mut file_ids: HashMap<&str, u32> = HashMap::new();
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

    let (term_blob, term_offsets, posting_offsets, postings, doc_lengths) = bm25.flat_parts();
    let meta = SnapshotMeta {
        version: SNAPSHOT_VERSION,
        model_id: model_id.to_string(),
        content: content.to_vec(),
        manifest,
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
    w.section(bytemuck::cast_slice(doc_lengths))?;
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

/// An open, validated snapshot file with resolved section offsets.
pub struct RawSnapshot {
    map: Arc<Mmap>,
    meta: SnapshotMeta,
    records_off: usize,
    texts_off: usize,
    embed_off: usize,
    term_blob_off: usize,
    term_offsets_off: usize,
    posting_offsets_off: usize,
    postings_off: usize,
    doc_lengths_off: usize,
}

/// Try to load the snapshot for `manifest_hash`. Returns `None` on any miss,
/// mismatch, or corruption (the caller falls back to normal assembly).
pub fn load(dir: &Path, manifest_hash: &[u8; 32], model_id: &str) -> Option<LoadedSnapshot> {
    let raw = RawSnapshot::open(&snapshot_path(dir, manifest_hash), Some(manifest_hash)).ok()?;
    if raw.meta.model_id != model_id {
        return None;
    }
    raw.materialize().ok()
}

/// Open the newest snapshot in `dir` compatible with the given parameters,
/// regardless of manifest hash (patch base).
pub fn open_latest(dir: &Path, model_id: &str, content: &[String]) -> Option<RawSnapshot> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut snaps: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "snap"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    snaps.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    for (_, path) in snaps {
        if let Ok(raw) = RawSnapshot::open(&path, None) {
            if raw.meta.model_id == model_id && raw.meta.content == content {
                return Some(raw);
            }
        }
    }
    None
}

impl RawSnapshot {
    fn open(path: &Path, expect_hash: Option<&[u8; 32]>) -> Result<RawSnapshot> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the snapshot is private to the trouve cache and replaced
        // only by atomic renames; an existing mapping stays valid after
        // replacement.
        let map = Arc::new(unsafe { Mmap::map(&file)? });
        let need = |end: usize| -> Result<()> {
            if map.len() < end {
                bail!("snapshot truncated");
            }
            Ok(())
        };
        need(48)?;
        if &map[..8] != MAGIC {
            bail!("snapshot magic mismatch");
        }
        if let Some(hash) = expect_hash {
            if &map[8..40] != hash {
                bail!("snapshot hash mismatch");
            }
        }
        let meta_len = u64::from_le_bytes(map[40..48].try_into().unwrap()) as usize;
        need(48 + meta_len)?;
        let (meta, _): (SnapshotMeta, usize) = bincode::serde::decode_from_slice(
            &map[48..48 + meta_len],
            bincode::config::standard(),
        )?;
        if meta.version != SNAPSHOT_VERSION {
            bail!("snapshot version mismatch");
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
        let records_off = section(n_chunks * std::mem::size_of::<ChunkRecord>())?;
        let texts_off = section(meta.texts_len as usize)?;
        let embed_off = section(n_chunks * dim * 4)?;
        let term_blob_off = section(meta.term_blob_len as usize)?;
        let term_offsets_off = section(n_offsets * 4)?;
        let posting_offsets_off = section(n_offsets * 8)?;
        let postings_off = section(meta.n_postings as usize * std::mem::size_of::<Posting>())?;
        let doc_lengths_off = section(meta.num_docs as usize * 4)?;

        Ok(RawSnapshot {
            map,
            meta,
            records_off,
            texts_off,
            embed_off,
            term_blob_off,
            term_offsets_off,
            posting_offsets_off,
            postings_off,
            doc_lengths_off,
        })
    }

    pub fn manifest(&self) -> &[ManifestEntry] {
        &self.meta.manifest
    }

    pub fn dim(&self) -> usize {
        self.meta.dim as usize
    }

    fn records(&self) -> &[ChunkRecord] {
        bytemuck::cast_slice(
            &self.map[self.records_off
                ..self.records_off
                    + self.meta.n_chunks as usize * std::mem::size_of::<ChunkRecord>()],
        )
    }

    fn texts(&self) -> &[u8] {
        &self.map[self.texts_off..self.texts_off + self.meta.texts_len as usize]
    }

    fn embeddings(&self) -> &[f32] {
        bytemuck::cast_slice(
            &self.map[self.embed_off
                ..self.embed_off + self.meta.n_chunks as usize * self.meta.dim as usize * 4],
        )
    }

    fn doc_lengths(&self) -> &[u32] {
        bytemuck::cast_slice(
            &self.map[self.doc_lengths_off..self.doc_lengths_off + self.meta.num_docs as usize * 4],
        )
    }

    fn n_terms(&self) -> usize {
        self.meta.n_terms as usize
    }

    fn term_bytes(&self, i: usize) -> &[u8] {
        let offsets: &[u32] = bytemuck::cast_slice(
            &self.map[self.term_offsets_off..self.term_offsets_off + (self.n_terms() + 1) * 4],
        );
        &self.map
            [self.term_blob_off + offsets[i] as usize..self.term_blob_off + offsets[i + 1] as usize]
    }

    fn term_postings(&self, i: usize) -> &[Posting] {
        let offsets: &[u64] = bytemuck::cast_slice(
            &self.map
                [self.posting_offsets_off..self.posting_offsets_off + (self.n_terms() + 1) * 8],
        );
        let all: &[Posting] = bytemuck::cast_slice(
            &self.map[self.postings_off
                ..self.postings_off
                    + self.meta.n_postings as usize * std::mem::size_of::<Posting>()],
        );
        &all[offsets[i] as usize..offsets[i + 1] as usize]
    }

    /// Materialize a chunk from its row index.
    fn chunk_at(&self, row: usize, languages: &[Option<String>]) -> Result<Chunk> {
        let rec = &self.records()[row];
        let file_id = rec.file_id as usize;
        if file_id >= self.meta.files.len() {
            bail!("snapshot file_id out of range");
        }
        let texts = self.texts();
        let start = rec.text_off as usize;
        let end = start + rec.text_len as usize;
        if end > texts.len() {
            bail!("snapshot text range out of bounds");
        }
        let content = std::str::from_utf8(&texts[start..end])
            .context("snapshot text not utf-8")?
            .to_string();
        Ok(Chunk {
            content,
            file_path: self.meta.files[file_id].clone(),
            start_line: rec.start_line,
            end_line: rec.end_line,
            language: languages[file_id].clone(),
        })
    }

    fn file_languages(&self) -> Vec<Option<String>> {
        self.meta
            .files
            .iter()
            .map(|f| detect_language(Path::new(f)).map(|l| l.to_string()))
            .collect()
    }

    /// Reconstruct the full index zero-copy (exact manifest match).
    pub fn materialize(&self) -> Result<LoadedSnapshot> {
        let languages = self.file_languages();
        let n_chunks = self.meta.n_chunks as usize;
        let mut chunks: Vec<Chunk> = Vec::with_capacity(n_chunks);
        for row in 0..n_chunks {
            chunks.push(self.chunk_at(row, &languages)?);
        }
        let n_offsets = self.n_terms() + 1;
        let dense = DenseIndex::from_parts(
            Buf::mapped(&self.map, self.embed_off, n_chunks * self.meta.dim as usize),
            self.meta.dim as usize,
            n_chunks,
        );
        let bm25 = Bm25Index::from_parts(
            Buf::mapped(
                &self.map,
                self.term_blob_off,
                self.meta.term_blob_len as usize,
            ),
            Buf::mapped(&self.map, self.term_offsets_off, n_offsets),
            Buf::mapped(&self.map, self.posting_offsets_off, n_offsets),
            Buf::mapped(&self.map, self.postings_off, self.meta.n_postings as usize),
            Buf::mapped(&self.map, self.doc_lengths_off, self.meta.num_docs as usize),
        );
        Ok(LoadedSnapshot {
            chunks,
            dense,
            bm25,
        })
    }
}

/// Where one file's rows come from when patching.
pub enum PatchSource<'a> {
    /// Unchanged file: splice `n_rows` rows starting at `first_row` out of
    /// the old snapshot.
    Copy { first_row: u32, n_rows: u32 },
    /// Changed or new file: fresh store entry (with raw embeddings).
    Fresh(&'a FileEntry),
}

/// One file in the new manifest order (sorted by `rel_path`).
pub struct PatchFile<'a> {
    pub rel_path: &'a str,
    pub source: PatchSource<'a>,
}

/// Splice a new index out of an old snapshot plus fresh entries for changed
/// files. Produces exactly what a full rebuild from the store would produce.
pub fn patch(old: &RawSnapshot, files: &[PatchFile<'_>]) -> Result<LoadedSnapshot> {
    let dim = old.dim();
    let old_records = old.records();
    let old_embeddings = old.embeddings();
    let old_doc_lengths = old.doc_lengths();
    let old_languages = old.file_languages();

    // Pass 1: chunks, embeddings, doc lengths, old-row -> new-row map, and
    // fresh docs' term frequencies.
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut vectors: Vec<f32> = Vec::new();
    let mut doc_lengths: Vec<u32> = Vec::new();
    let mut old_to_new: Vec<u32> = vec![u32::MAX; old_records.len()];
    // Flat token storage for fresh rows (one doc per fresh chunk) plus the
    // new-index doc id of each fresh doc.
    let mut fresh_tokens = crate::tokens::TokenDocs::default();
    let mut fresh_doc_ids: Vec<u32> = Vec::new();

    for file in files {
        match &file.source {
            PatchSource::Copy { first_row, n_rows } => {
                let (start, n) = (*first_row as usize, *n_rows as usize);
                if start + n > old_records.len() {
                    bail!("patch copy range out of bounds");
                }
                for row in start..start + n {
                    old_to_new[row] = chunks.len() as u32;
                    chunks.push(old.chunk_at(row, &old_languages)?);
                    vectors.extend_from_slice(&old_embeddings[row * dim..(row + 1) * dim]);
                    doc_lengths.push(old_doc_lengths[row]);
                }
            }
            PatchSource::Fresh(entry) => {
                if !entry.chunks.is_empty() && entry.dim as usize != dim {
                    bail!("patch dim mismatch");
                }
                let language = detect_language(Path::new(file.rel_path));
                let path_tokens = crate::index::path_enrichment_tokens(file.rel_path);
                for (i, stored) in entry.chunks.iter().enumerate() {
                    let doc_id = chunks.len() as u32;
                    chunks.push(Chunk {
                        content: stored.content.clone(),
                        file_path: file.rel_path.to_string(),
                        start_line: stored.start_line,
                        end_line: stored.end_line,
                        language: language.map(|l| l.to_string()),
                    });
                    // Fresh embeddings are raw: normalize like DenseIndex::new.
                    let row_start = vectors.len();
                    if dim > 0 && entry.embeddings.len() >= (i + 1) * dim {
                        vectors.extend_from_slice(&entry.embeddings[i * dim..(i + 1) * dim]);
                    } else {
                        vectors.extend(std::iter::repeat_n(0.0, dim.max(1)));
                    }
                    crate::dense::normalize(&mut vectors[row_start..]);

                    if i < entry.tokens.n_docs() {
                        for tok in entry.tokens.doc_tokens(i) {
                            fresh_tokens.push_token_bytes(tok);
                        }
                    } else {
                        fresh_tokens.push_text(&stored.content);
                    }
                    for tok in &path_tokens {
                        fresh_tokens.push_token_bytes(tok.as_bytes());
                    }
                    fresh_tokens.finish_doc();
                    doc_lengths.push(fresh_tokens.doc_len(fresh_tokens.n_docs() - 1) as u32);
                    fresh_doc_ids.push(doc_id);
                }
            }
        }
    }

    // (doc, term, tf) triples for fresh rows.
    let mut fresh_tfs: Vec<(u32, &[u8], u32)> = Vec::new();
    for (d, &doc_id) in fresh_doc_ids.iter().enumerate() {
        let mut tf: HashMap<&[u8], u32> = HashMap::with_capacity(fresh_tokens.doc_len(d));
        for tok in fresh_tokens.doc_tokens(d) {
            *tf.entry(tok).or_insert(0) += 1;
        }
        for (term, count) in tf {
            fresh_tfs.push((doc_id, term, count));
        }
    }

    // Pass 2: merge BM25 postings. Old terms are already sorted; fresh terms
    // are sorted and merged in. Old postings survive with remapped doc ids
    // (the remap is monotonic, so per-term doc order is preserved).
    fresh_tfs.sort_unstable_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)));

    let n_old_terms = old.n_terms();
    let mut term_blob: Vec<u8> = Vec::new();
    let mut term_offsets: Vec<u32> = vec![0];
    let mut posting_offsets: Vec<u64> = vec![0];
    let mut postings: Vec<Posting> = Vec::new();

    let mut old_i = 0usize;
    let mut fresh_i = 0usize;
    while old_i < n_old_terms || fresh_i < fresh_tfs.len() {
        // Next term is the smaller of the old term and the fresh term.
        let old_term = (old_i < n_old_terms).then(|| old.term_bytes(old_i));
        let fresh_term = (fresh_i < fresh_tfs.len()).then(|| fresh_tfs[fresh_i].1);
        let term: &[u8] = match (old_term, fresh_term) {
            (Some(o), Some(f)) => {
                if o <= f {
                    o
                } else {
                    f
                }
            }
            (Some(o), None) => o,
            (None, Some(f)) => f,
            (None, None) => unreachable!(),
        };

        let start = postings.len();
        // Merge old (remapped) and fresh postings for this term, docs
        // ascending. Both inputs are ascending and disjoint (fresh docs are
        // new rows; old surviving docs are copies).
        let old_slice: &[Posting] = if old_term == Some(term) {
            let s = old.term_postings(old_i);
            old_i += 1;
            s
        } else {
            &[]
        };
        let fresh_start = fresh_i;
        if fresh_term == Some(term) {
            while fresh_i < fresh_tfs.len() && fresh_tfs[fresh_i].1 == term {
                fresh_i += 1;
            }
        }
        let fresh_slice = &fresh_tfs[fresh_start..fresh_i];

        let mut a = 0usize;
        let mut b = 0usize;
        loop {
            let old_next = loop {
                if a >= old_slice.len() {
                    break None;
                }
                let p = old_slice[a];
                let new_doc = old_to_new[p.doc as usize];
                if new_doc == u32::MAX {
                    a += 1;
                    continue;
                }
                break Some(Posting {
                    doc: new_doc,
                    tf: p.tf,
                });
            };
            let fresh_next = (b < fresh_slice.len()).then(|| Posting {
                doc: fresh_slice[b].0,
                tf: fresh_slice[b].2,
            });
            match (old_next, fresh_next) {
                (Some(o), Some(f)) => {
                    if o.doc <= f.doc {
                        postings.push(o);
                        a += 1;
                    } else {
                        postings.push(f);
                        b += 1;
                    }
                }
                (Some(o), None) => {
                    postings.push(o);
                    a += 1;
                }
                (None, Some(f)) => {
                    postings.push(f);
                    b += 1;
                }
                (None, None) => break,
            }
        }

        // Terms whose every posting was removed are dropped, exactly like a
        // full rebuild would.
        if postings.len() > start {
            term_blob.extend_from_slice(term);
            term_offsets.push(term_blob.len() as u32);
            posting_offsets.push(postings.len() as u64);
        }
    }

    let rows = chunks.len();
    let dense = DenseIndex::from_parts(vectors.into(), dim, rows);
    let bm25 = Bm25Index::from_parts(
        term_blob.into(),
        term_offsets.into(),
        posting_offsets.into(),
        postings.into(),
        doc_lengths.into(),
    );
    Ok(LoadedSnapshot {
        chunks,
        dense,
        bm25,
    })
}

/// Union of store entry keys referenced by every readable snapshot in `dir`:
/// the mark phase of the store's mark-and-sweep GC. Snapshots that fail to
/// open (older versions, corruption) contribute nothing, which is correct:
/// their entries were written under a different store version and can never
/// be hit by this binary anyway.
pub fn live_entry_keys(dir: &Path) -> std::collections::HashSet<String> {
    let mut live = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return live;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "snap") {
            continue;
        }
        let Ok(raw) = RawSnapshot::open(&path, None) else {
            continue;
        };
        for m in raw.manifest() {
            let language = detect_language(Path::new(&m.rel_path));
            live.insert(crate::store::ChunkStore::entry_key(
                &m.content_key,
                language,
                &raw.meta.model_id,
            ));
        }
    }
    live
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

    fn manifest_for(chunks: &[Chunk]) -> Vec<ManifestEntry> {
        let mut entries: Vec<ManifestEntry> = Vec::new();
        for (row, chunk) in chunks.iter().enumerate() {
            match entries.last_mut() {
                Some(last) if last.rel_path == chunk.file_path => last.n_rows += 1,
                _ => entries.push(ManifestEntry {
                    rel_path: chunk.file_path.clone(),
                    content_key: format!("b3:{}", chunk.file_path),
                    first_row: row as u32,
                    n_rows: 1,
                }),
            }
        }
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        entries
    }

    fn save_sample(dir: &Path, hash: &[u8; 32]) -> (Vec<Chunk>, DenseIndex, Bm25Index) {
        let (chunks, dense, bm25) = sample();
        save(
            dir,
            hash,
            "model-x",
            &["code".to_string()],
            manifest_for(&chunks),
            &chunks,
            &dense,
            &bm25,
        )
        .unwrap();
        (chunks, dense, bm25)
    }

    #[test]
    fn roundtrips_chunks_dense_and_bm25() {
        let dir = tempfile::tempdir().unwrap();
        let hash = [7u8; 32];
        let (chunks, dense, bm25) = save_sample(dir.path(), &hash);

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
        let hash = [7u8; 32];
        save_sample(dir.path(), &hash);

        assert!(load(dir.path(), &[8u8; 32], "model-x").is_none());
        assert!(load(dir.path(), &hash, "other-model").is_none());
    }

    #[test]
    fn corrupt_snapshot_misses() {
        let dir = tempfile::tempdir().unwrap();
        let hash = [7u8; 32];
        let (chunks, dense, bm25) = sample();
        let path = save(
            dir.path(),
            &hash,
            "model-x",
            &["code".to_string()],
            manifest_for(&chunks),
            &chunks,
            &dense,
            &bm25,
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load(dir.path(), &hash, "model-x").is_none());
    }

    #[test]
    fn prunes_old_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(KEEP_SNAPSHOTS as u8 + 3) {
            save_sample(dir.path(), &[i; 32]);
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
        save(
            dir.path(),
            &hash,
            "m",
            &["code".to_string()],
            manifest_for(&chunks),
            &chunks,
            &dense,
            &bm25,
        )
        .unwrap();
        let snap = load(dir.path(), &hash, "m").unwrap();
        assert_eq!(snap.bm25.num_docs(), 1);
        assert!(snap
            .bm25
            .get_scores(&["anything".into()], None)
            .iter()
            .all(|s| *s == 0.0));
    }

    #[test]
    fn live_entry_keys_covers_all_kept_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let (chunks, _, _) = save_sample(dir.path(), &[1u8; 32]);

        let live = live_entry_keys(dir.path());
        for entry in manifest_for(&chunks) {
            let language = detect_language(Path::new(&entry.rel_path));
            let key = crate::store::ChunkStore::entry_key(&entry.content_key, language, "model-x");
            assert!(live.contains(&key), "missing key for {}", entry.rel_path);
        }
        // Exactly one key per manifest file, no extras.
        assert_eq!(live.len(), manifest_for(&chunks).len());

        // Unreadable snapshots contribute nothing instead of failing.
        std::fs::write(dir.path().join("junk.snap"), b"not a snapshot").unwrap();
        assert_eq!(live_entry_keys(dir.path()), live);
    }

    #[test]
    fn open_latest_respects_model_and_content() {
        let dir = tempfile::tempdir().unwrap();
        save_sample(dir.path(), &[3u8; 32]);
        assert!(open_latest(dir.path(), "model-x", &["code".to_string()]).is_some());
        assert!(open_latest(dir.path(), "model-y", &["code".to_string()]).is_none());
        assert!(open_latest(dir.path(), "model-x", &["docs".to_string()]).is_none());
    }
}
