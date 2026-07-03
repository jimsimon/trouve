//! The Semble index: incremental, content-addressed, multithreaded.
//!
//! Replaces upstream's all-or-nothing cached index (`semble/index/index.py` +
//! `semble/cache.py`). Assembly works from a manifest of `(path, content key)`
//! pairs: cached files load their chunks/embeddings/tokens from the shared
//! store, missing files are parsed/chunked/tokenized in parallel with rayon
//! and embedded in batches, and the BM25 corpus statistics are recomputed on
//! every assembly (cheap relative to embedding).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

use crate::bm25::Bm25Index;
use crate::chunk::chunk_source;
use crate::dense::DenseIndex;
use crate::embed::EmbeddingModel;
use crate::languages::{detect_language, file_status_for_bytes, get_extensions, FileStatus};
use crate::manifest::{build_manifest, detect_repo_identity, FileRecord, RepoIdentity};
use crate::search;
use crate::snapshot;
use crate::stats::save_search_stats;
use crate::store::{ChunkStore, FileEntry, StoredChunk};
use crate::tokens::tokenize;
use crate::types::{CallType, Chunk, ContentType, IndexStats, SearchResult};

/// How the index build went: total files and how many needed fresh computation.
#[derive(Debug, Clone, Default)]
pub struct BuildStats {
    pub files_total: usize,
    pub files_from_store: usize,
    pub files_computed: usize,
    pub chunks_total: usize,
}

pub struct SembleIndex {
    model: Arc<EmbeddingModel>,
    pub chunks: Vec<Chunk>,
    dense: DenseIndex,
    bm25: Bm25Index,
    file_mapping: HashMap<String, Vec<usize>>,
    language_mapping: HashMap<String, Vec<usize>>,
    file_sizes: HashMap<String, usize>,
    content: Vec<ContentType>,
    pub build_stats: BuildStats,
}

fn git_clone_timeout() -> u64 {
    std::env::var("SEMBLE_CLONE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
}

impl SembleIndex {
    /// Create and index a SembleIndex from a directory.
    pub fn from_path(
        path: &Path,
        content: &[ContentType],
        model_id: Option<&str>,
    ) -> Result<SembleIndex> {
        if !path.exists() {
            bail!("Path does not exist: {}", path.display());
        }
        if !path.is_dir() {
            bail!("Path is not a directory: {}", path.display());
        }
        let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let identity = detect_repo_identity(&root);
        Self::build(
            &root,
            identity.as_str().to_string(),
            &identity,
            content,
            model_id,
        )
    }

    /// Clone a git repository (shallow) into a temp directory and index it.
    ///
    /// The store identity is derived from the URL (and optional ref), so
    /// repeated calls share cached chunks even though the clone directory
    /// changes every time.
    pub fn from_git(
        url: &str,
        git_ref: Option<&str>,
        content: &[ContentType],
        model_id: Option<&str>,
    ) -> Result<SembleIndex> {
        let tmp = tempdir_for_clone()?;
        let tmp_path = tmp.path().to_path_buf();
        let mut cmd = Command::new("git");
        cmd.arg("clone").arg("--depth").arg("1");
        if let Some(r) = git_ref {
            cmd.arg("--branch").arg(r);
        }
        // `--` prevents `url` from being interpreted as a git option.
        cmd.arg("--").arg(url).arg(&tmp_path);
        cmd.env("GIT_HTTP_LOW_SPEED_TIME", git_clone_timeout().to_string())
            .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000");
        let output = cmd
            .stdin(std::process::Stdio::null())
            .output()
            .context("git is not installed or not on PATH")?;
        if !output.status.success() {
            bail!(
                "git clone failed for {url:?}:\n{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let identity_key = match git_ref {
            Some(r) => format!("git-url:{url}@{r}"),
            None => format!("git-url:{url}"),
        };
        let identity = detect_repo_identity(&tmp_path);
        Self::build(&tmp_path, identity_key, &identity, content, model_id)
    }

    fn build(
        root: &Path,
        store_identity: String,
        repo_identity: &RepoIdentity,
        content: &[ContentType],
        model_id: Option<&str>,
    ) -> Result<SembleIndex> {
        let content: Vec<ContentType> = if content.is_empty() {
            vec![ContentType::Code]
        } else {
            content.to_vec()
        };
        let model = crate::utils::timed("model load", || EmbeddingModel::load(model_id))?;
        let store = ChunkStore::open(&store_identity)?;
        let extensions = get_extensions(&content);
        let manifest = crate::utils::timed("manifest", || {
            build_manifest(root, repo_identity, &extensions, &store)
        })?;

        // All paths assemble in rel_path order; sort the manifest up front.
        let mut manifest = manifest;
        manifest.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

        // Fast path 1: identical manifest -> mmap the assembled snapshot and
        // skip per-file store reads and index reconstruction entirely.
        let manifest_hash = manifest_digest(&manifest, &content, &model.model_id);
        let snapshot_dir = store.root().join("snapshots");
        let content_strs: Vec<String> = content.iter().map(|c| c.as_str().to_string()).collect();
        if let Some(snap) = snapshot::load(&snapshot_dir, &manifest_hash, &model.model_id) {
            let stats = BuildStats {
                files_total: manifest.len(),
                files_from_store: manifest.len(),
                files_computed: 0,
                chunks_total: snap.chunks.len(),
            };
            return Ok(Self::from_loaded(snap, model, content, stats));
        }

        // Fast path 2: patch the newest compatible snapshot. Only changed
        // files pay for store reads or recomputation; everything else is
        // spliced out of the old mapping.
        if let Some(old) = snapshot::open_latest(&snapshot_dir, &model.model_id, &content_strs) {
            // Incompatible or failed patches fall through to full assembly.
            if let Ok(Some((snap, stats))) = Self::build_by_patching(
                &old,
                &manifest,
                &model,
                &store,
                &snapshot_dir,
                &manifest_hash,
                &content_strs,
            ) {
                return Ok(Self::from_loaded(snap, model, content, stats));
            }
        }

        // Phase 1: look up every manifest record in the store (parallel).
        let keyed: Vec<(FileRecord, String, Option<FileEntry>)> =
            crate::utils::timed("store lookups", || {
                std::mem::take(&mut manifest)
                    .into_par_iter()
                    .map(|record| {
                        let language = detect_language(Path::new(&record.rel_path));
                        let key =
                            ChunkStore::entry_key(&record.content_key, language, &model.model_id);
                        let cached = store.get(&key);
                        (record, key, cached)
                    })
                    .collect()
            });

        let files_total = keyed.len();
        let mut hits: Vec<(FileRecord, FileEntry)> = Vec::new();
        let mut misses: Vec<(FileRecord, String)> = Vec::new();
        for (record, key, cached) in keyed {
            match cached {
                Some(entry) => hits.push((record, entry)),
                None => misses.push((record, key)),
            }
        }
        let files_from_store = hits.len();
        let files_computed = misses.len();

        // Phases 2+3: chunk, tokenize, embed, and persist missing files.
        let computed = compute_file_entries(misses, &model, &store);

        // Phase 4: assemble in manifest order (hits and computed interleaved
        // back into a single sorted-by-path sequence). Per-file work runs in
        // parallel and moves data out of the entries instead of cloning.
        let assemble_start = std::time::Instant::now();
        let mut per_file: Vec<(FileRecord, FileEntry)> = hits;
        per_file.extend(
            computed
                .into_iter()
                .map(|(record, _, entry)| (record, entry)),
        );
        per_file.sort_by(|a, b| a.0.rel_path.cmp(&b.0.rel_path));

        let dim = per_file
            .iter()
            .map(|(_, e)| e.dim as usize)
            .find(|d| *d > 0)
            .unwrap_or(0);

        struct FilePart {
            rel_path: String,
            content_key: String,
            chunks: Vec<Chunk>,
            vectors: Vec<f32>,
            docs: Vec<Vec<String>>,
        }
        let parts: Vec<FilePart> = per_file
            .into_par_iter()
            .map(|(record, entry)| {
                let language = detect_language(Path::new(&record.rel_path));
                let path_tokens = path_enrichment_tokens(&record.rel_path);
                let entry_dim = entry.dim as usize;
                let n = entry.chunks.len();
                let mut chunks = Vec::with_capacity(n);
                let mut vectors = Vec::with_capacity(n * dim);
                let mut docs = Vec::with_capacity(n);
                let mut token_lists = entry.token_lists;
                for (i, stored) in entry.chunks.into_iter().enumerate() {
                    let mut doc = if i < token_lists.len() {
                        std::mem::take(&mut token_lists[i])
                    } else {
                        tokenize(&stored.content)
                    };
                    doc.extend(path_tokens.iter().cloned());
                    docs.push(doc);
                    if entry_dim == dim && entry.embeddings.len() >= (i + 1) * dim {
                        vectors.extend_from_slice(&entry.embeddings[i * dim..(i + 1) * dim]);
                    } else {
                        vectors.extend(std::iter::repeat_n(0.0, dim));
                    }
                    chunks.push(Chunk {
                        content: stored.content,
                        file_path: record.rel_path.clone(),
                        start_line: stored.start_line,
                        end_line: stored.end_line,
                        language: language.map(|l| l.to_string()),
                    });
                }
                FilePart {
                    rel_path: record.rel_path,
                    content_key: record.content_key,
                    chunks,
                    vectors,
                    docs,
                }
            })
            .collect();

        let total_chunks: usize = parts.iter().map(|p| p.chunks.len()).sum();
        let mut chunks: Vec<Chunk> = Vec::with_capacity(total_chunks);
        let mut vectors: Vec<f32> = Vec::with_capacity(total_chunks * dim);
        let mut docs: Vec<Vec<String>> = Vec::with_capacity(total_chunks);
        let mut manifest_entries: Vec<snapshot::ManifestEntry> = Vec::with_capacity(parts.len());
        for part in parts {
            manifest_entries.push(snapshot::ManifestEntry {
                rel_path: part.rel_path,
                content_key: part.content_key,
                first_row: chunks.len() as u32,
                n_rows: part.chunks.len() as u32,
            });
            chunks.extend(part.chunks);
            vectors.extend_from_slice(&part.vectors);
            docs.extend(part.docs);
        }

        if chunks.is_empty() {
            bail!("No supported files found under {}.", root.display());
        }

        let chunks_total = chunks.len();
        if std::env::var_os("SEMBLE_TIMING").is_some() {
            eprintln!(
                "[timing] assemble chunks: {:.1} ms",
                assemble_start.elapsed().as_secs_f64() * 1e3
            );
        }
        let bm25 = crate::utils::timed("bm25 build", || Bm25Index::build(&docs));
        let dense = crate::utils::timed("dense build", || {
            DenseIndex::from_unnormalized_flat(vectors, dim, chunks_total)
        });

        // Persist the assembled index so the next identical-manifest build is
        // a single mmap. Best effort: a failed write only costs speed.
        crate::utils::timed("snapshot write", || {
            let _ = snapshot::save(
                &snapshot_dir,
                &manifest_hash,
                &model.model_id,
                &content_strs,
                manifest_entries,
                &chunks,
                &dense,
                &bm25,
            );
        });

        let stats = BuildStats {
            files_total,
            files_from_store,
            files_computed,
            chunks_total,
        };
        Ok(Self::from_loaded(
            snapshot::LoadedSnapshot {
                chunks,
                dense,
                bm25,
            },
            model,
            content,
            stats,
        ))
    }

    /// Wrap loaded/assembled index pieces into a `SembleIndex`.
    fn from_loaded(
        snap: snapshot::LoadedSnapshot,
        model: Arc<EmbeddingModel>,
        content: Vec<ContentType>,
        build_stats: BuildStats,
    ) -> SembleIndex {
        let (file_mapping, language_mapping) = populate_mappings(&snap.chunks);
        let mut file_sizes: HashMap<String, usize> = HashMap::new();
        for chunk in &snap.chunks {
            *file_sizes.entry(chunk.file_path.clone()).or_insert(0) += chunk.content.len();
        }
        SembleIndex {
            model,
            chunks: snap.chunks,
            dense: snap.dense,
            bm25: snap.bm25,
            file_mapping,
            language_mapping,
            file_sizes,
            content,
            build_stats,
        }
    }

    /// Build by patching the newest compatible snapshot: unchanged files are
    /// spliced from the old mapping, changed files come from the store or are
    /// recomputed. Returns `Ok(None)` when the snapshot is unusable (e.g.
    /// embedding dimension mismatch with fresh entries).
    #[allow(clippy::too_many_arguments)]
    fn build_by_patching(
        old: &snapshot::RawSnapshot,
        manifest: &[FileRecord],
        model: &Arc<EmbeddingModel>,
        store: &ChunkStore,
        snapshot_dir: &Path,
        manifest_hash: &[u8; 32],
        content_strs: &[String],
    ) -> Result<Option<(snapshot::LoadedSnapshot, BuildStats)>> {
        let old_by_path: HashMap<&str, &snapshot::ManifestEntry> = old
            .manifest()
            .iter()
            .map(|e| (e.rel_path.as_str(), e))
            .collect();

        // Classify every file: unchanged (copy rows), or changed/new (needs
        // a store entry, possibly computed).
        enum Plan<'m> {
            Copy(&'m FileRecord, u32, u32),
            Need(&'m FileRecord, String),
        }
        let plans: Vec<Plan> = manifest
            .iter()
            .map(|record| {
                if let Some(entry) = old_by_path.get(record.rel_path.as_str()) {
                    if entry.content_key == record.content_key {
                        return Plan::Copy(record, entry.first_row, entry.n_rows);
                    }
                }
                let language = detect_language(Path::new(&record.rel_path));
                let key = ChunkStore::entry_key(&record.content_key, language, &model.model_id);
                Plan::Need(record, key)
            })
            .collect();

        // Fetch store entries for changed files (parallel), compute the rest.
        let fetched: Vec<(&FileRecord, String, Option<FileEntry>)> =
            crate::utils::timed("patch store lookups", || {
                plans
                    .par_iter()
                    .filter_map(|plan| match plan {
                        Plan::Copy(..) => None,
                        Plan::Need(record, key) => Some((*record, key.clone(), store.get(key))),
                    })
                    .collect()
            });
        let mut entries: HashMap<&str, FileEntry> = HashMap::new();
        let mut misses: Vec<(FileRecord, String)> = Vec::new();
        for (record, key, cached) in fetched {
            match cached {
                Some(entry) => {
                    entries.insert(record.rel_path.as_str(), entry);
                }
                None => misses.push(((*record).clone(), key)),
            }
        }
        let files_computed = misses.len();
        for (record, _, entry) in compute_file_entries(misses, model, store) {
            // Keys the borrowed map by the manifest's copy of the path.
            let rel: &str = &manifest
                .iter()
                .find(|r| r.rel_path == record.rel_path)
                .expect("computed record comes from the manifest")
                .rel_path;
            entries.insert(rel, entry);
        }

        // Fresh entries must match the snapshot's embedding dimension.
        let dim = old.dim();
        if entries
            .values()
            .any(|e| !e.chunks.is_empty() && e.dim as usize != dim)
        {
            return Ok(None);
        }

        let files: Vec<snapshot::PatchFile> = plans
            .iter()
            .map(|plan| match plan {
                Plan::Copy(record, first_row, n_rows) => snapshot::PatchFile {
                    rel_path: &record.rel_path,
                    source: snapshot::PatchSource::Copy {
                        first_row: *first_row,
                        n_rows: *n_rows,
                    },
                },
                Plan::Need(record, _) => snapshot::PatchFile {
                    rel_path: &record.rel_path,
                    source: snapshot::PatchSource::Fresh(&entries[record.rel_path.as_str()]),
                },
            })
            .collect();

        let snap = crate::utils::timed("patch splice", || snapshot::patch(old, &files))?;
        if snap.chunks.is_empty() {
            bail!("no chunks after patch");
        }

        // Persist the patched index for the next exact-match load.
        let mut manifest_entries: Vec<snapshot::ManifestEntry> = Vec::with_capacity(manifest.len());
        let mut row = 0u32;
        for (plan, record) in plans.iter().zip(manifest) {
            let n_rows = match plan {
                Plan::Copy(_, _, n) => *n,
                Plan::Need(..) => entries[record.rel_path.as_str()].chunks.len() as u32,
            };
            manifest_entries.push(snapshot::ManifestEntry {
                rel_path: record.rel_path.clone(),
                content_key: record.content_key.clone(),
                first_row: row,
                n_rows,
            });
            row += n_rows;
        }
        crate::utils::timed("snapshot write", || {
            let _ = snapshot::save(
                snapshot_dir,
                manifest_hash,
                &model.model_id,
                content_strs,
                manifest_entries,
                &snap.chunks,
                &snap.dense,
                &snap.bm25,
            );
        });

        let stats = BuildStats {
            files_total: manifest.len(),
            files_from_store: manifest.len() - files_computed,
            files_computed,
            chunks_total: snap.chunks.len(),
        };
        Ok(Some((snap, stats)))
    }

    /// Stats of the index.
    pub fn stats(&self) -> IndexStats {
        let mut languages: HashMap<String, usize> = HashMap::new();
        for chunk in &self.chunks {
            if let Some(lang) = &chunk.language {
                *languages.entry(lang.clone()).or_insert(0) += 1;
            }
        }
        IndexStats {
            indexed_files: self.file_mapping.len(),
            total_chunks: self.chunks.len(),
            languages,
        }
    }

    fn selector(
        &self,
        filter_languages: Option<&[String]>,
        filter_paths: Option<&[String]>,
    ) -> Option<Vec<usize>> {
        let mut selector: Vec<usize> = Vec::new();
        let mut any_filter = false;
        if let Some(languages) = filter_languages {
            any_filter = !languages.is_empty() || any_filter;
            for lang in languages {
                if let Some(ids) = self.language_mapping.get(lang) {
                    selector.extend(ids.iter().copied());
                }
            }
        }
        if let Some(paths) = filter_paths {
            any_filter = !paths.is_empty() || any_filter;
            for p in paths {
                if let Some(ids) = self.file_mapping.get(p) {
                    selector.extend(ids.iter().copied());
                }
            }
        }
        if !any_filter || selector.is_empty() {
            if any_filter {
                // Filters were requested but matched nothing: empty selector.
                return Some(Vec::new());
            }
            return None;
        }
        selector.sort_unstable();
        selector.dedup();
        Some(selector)
    }

    /// Search the index and return the top-k most relevant chunks.
    #[allow(clippy::too_many_arguments)]
    pub fn search(
        &self,
        query: &str,
        top_k: usize,
        alpha: Option<f64>,
        filter_languages: Option<&[String]>,
        filter_paths: Option<&[String]>,
        rerank: Option<bool>,
        max_snippet_lines: Option<usize>,
    ) -> Vec<SearchResult> {
        if self.chunks.is_empty() || query.trim().is_empty() {
            return Vec::new();
        }
        let resolved_rerank = rerank.unwrap_or_else(|| self.content.contains(&ContentType::Code));
        let selector = self.selector(filter_languages, filter_paths);
        let results = search::search(
            query,
            &self.model,
            &self.dense,
            &self.bm25,
            &self.chunks,
            top_k,
            alpha,
            selector.as_deref(),
            resolved_rerank,
        );
        save_search_stats(
            &results,
            CallType::Search,
            &self.file_sizes,
            max_snippet_lines,
        );
        results
    }

    /// Return chunks semantically similar to the given chunk.
    pub fn find_related(
        &self,
        source: &Chunk,
        top_k: usize,
        max_snippet_lines: Option<usize>,
    ) -> Vec<SearchResult> {
        let selector: Option<Vec<usize>> = source
            .language
            .as_ref()
            .and_then(|lang| self.language_mapping.get(lang).cloned());
        let results = search::find_related(
            source,
            &self.model,
            &self.dense,
            &self.chunks,
            top_k,
            selector.as_deref(),
        );
        save_search_stats(
            &results,
            CallType::FindRelated,
            &self.file_sizes,
            max_snippet_lines,
        );
        results
    }
}

/// Chunk, tokenize, embed, and persist a batch of files that missed the
/// store. Returns `(record, key, entry)` per readable file.
fn compute_file_entries(
    misses: Vec<(FileRecord, String)>,
    model: &Arc<EmbeddingModel>,
    store: &ChunkStore,
) -> Vec<(FileRecord, String, FileEntry)> {
    // Parse + chunk + tokenize in parallel.
    let mut computed: Vec<(FileRecord, String, FileEntry, Vec<String>)> =
        crate::utils::timed("chunk+tokenize", || {
            misses
                .into_par_iter()
                .filter_map(|(record, key)| {
                    let bytes = std::fs::read(&record.abs_path).ok()?;
                    let entry = match file_status_for_bytes(&bytes) {
                        FileStatus::Valid => {
                            let source = String::from_utf8_lossy(&bytes);
                            let language = detect_language(Path::new(&record.rel_path));
                            let chunks = chunk_source(&source, &record.rel_path, language);
                            let token_lists: Vec<Vec<String>> =
                                chunks.iter().map(|c| tokenize(&c.content)).collect();
                            FileEntry {
                                chunks: chunks
                                    .iter()
                                    .map(|c| StoredChunk {
                                        content: c.content.clone(),
                                        start_line: c.start_line,
                                        end_line: c.end_line,
                                    })
                                    .collect(),
                                embeddings: Vec::new(),
                                dim: 0,
                                token_lists,
                            }
                        }
                        // Too large or empty: store an empty entry so the
                        // file is never re-read on subsequent builds.
                        _ => FileEntry::default(),
                    };
                    let chunk_texts: Vec<String> =
                        entry.chunks.iter().map(|c| c.content.clone()).collect();
                    Some((record, key, entry, chunk_texts))
                })
                .collect()
        });

    // Embed all fresh chunks in large parallel batches.
    let all_texts: Vec<String> = computed
        .iter()
        .flat_map(|(_, _, _, texts)| texts.iter().cloned())
        .collect();
    let embeddings = crate::utils::timed("embed", || model.embed_texts(&all_texts));
    let dim = embeddings.first().map(|r| r.len()).unwrap_or(0);

    // Scatter embedding rows back to entries and persist, in parallel.
    let mut offsets: Vec<usize> = Vec::with_capacity(computed.len());
    let mut cursor = 0usize;
    for (_, _, _, texts) in &computed {
        offsets.push(cursor);
        cursor += texts.len();
    }
    crate::utils::timed("store writes", || {
        computed
            .par_iter_mut()
            .zip(offsets)
            .for_each(|((_, key, entry, texts), start)| {
                let n = texts.len();
                let mut flat = Vec::with_capacity(n * dim);
                for row in &embeddings[start..start + n] {
                    flat.extend_from_slice(row);
                }
                entry.embeddings = flat;
                entry.dim = dim as u32;
                let _ = store.put(key, entry);
            });
    });

    computed
        .into_iter()
        .map(|(record, key, entry, _)| (record, key, entry))
        .collect()
}

/// Digest of everything that determines the assembled index: the set of
/// `(path, content key)` pairs plus every parameter baked into store entries.
fn manifest_digest(manifest: &[FileRecord], content: &[ContentType], model_id: &str) -> [u8; 32] {
    let mut pairs: Vec<(&str, &str)> = manifest
        .iter()
        .map(|r| (r.rel_path.as_str(), r.content_key.as_str()))
        .collect();
    pairs.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    for (path, key) in pairs {
        hasher.update(path.as_bytes());
        hasher.update(b"\x00");
        hasher.update(key.as_bytes());
        hasher.update(b"\x00");
    }
    let mut sorted_content = content.to_vec();
    sorted_content.sort();
    for ct in sorted_content {
        hasher.update(ct.as_str().as_bytes());
        hasher.update(b"\x00");
    }
    hasher.update(model_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(&crate::chunk::DESIRED_CHUNK_LENGTH.to_le_bytes());
    hasher.update(&crate::store::STORE_VERSION.to_le_bytes());
    hasher.update(&snapshot::SNAPSHOT_VERSION.to_le_bytes());
    *hasher.finalize().as_bytes()
}

pub(crate) fn path_enrichment_tokens(rel_path: &str) -> Vec<String> {
    let path = Path::new(rel_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let dir_parts: Vec<&str> = path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => s.to_str(),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let keep = dir_parts.len().saturating_sub(3);
    let enrichment = format!("{stem} {stem} {}", dir_parts[keep..].join(" "));
    tokenize(&enrichment)
}

fn populate_mappings(
    chunks: &[Chunk],
) -> (HashMap<String, Vec<usize>>, HashMap<String, Vec<usize>>) {
    let mut file_mapping: HashMap<String, Vec<usize>> = HashMap::new();
    let mut language_mapping: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(lang) = &chunk.language {
            language_mapping.entry(lang.clone()).or_default().push(i);
        }
        file_mapping
            .entry(chunk.file_path.clone())
            .or_default()
            .push(i);
    }
    (file_mapping, language_mapping)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn tempdir_for_clone() -> Result<TempDir> {
    let base = std::env::temp_dir();
    let path = base.join(format!(
        "semble-clone-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&path)?;
    Ok(TempDir { path })
}
