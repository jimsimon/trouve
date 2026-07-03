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
        let model = EmbeddingModel::load(model_id)?;
        let store = ChunkStore::open(&store_identity)?;
        let extensions = get_extensions(&content);
        let manifest = build_manifest(root, repo_identity, &extensions, &store)?;

        // Phase 1: look up every manifest record in the store (parallel).
        let keyed: Vec<(FileRecord, String, Option<FileEntry>)> = manifest
            .into_par_iter()
            .map(|record| {
                let language = detect_language(Path::new(&record.rel_path));
                let key = ChunkStore::entry_key(&record.content_key, language, &model.model_id);
                let cached = store.get(&key);
                (record, key, cached)
            })
            .collect();

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

        // Phase 2: parse + chunk + tokenize missing files in parallel.
        struct Computed {
            record: FileRecord,
            key: String,
            entry: FileEntry,
            chunk_texts: Vec<String>,
        }
        let mut computed: Vec<Computed> = misses
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
                    // Too large or empty: store an empty entry so the file is
                    // never re-read on subsequent builds.
                    _ => FileEntry::default(),
                };
                let chunk_texts = entry.chunks.iter().map(|c| c.content.clone()).collect();
                Some(Computed {
                    record,
                    key,
                    entry,
                    chunk_texts,
                })
            })
            .collect();

        // Phase 3: embed all fresh chunks in large parallel batches.
        let all_texts: Vec<String> = computed
            .iter()
            .flat_map(|c| c.chunk_texts.iter().cloned())
            .collect();
        let embeddings = model.embed_texts(&all_texts);
        let dim = embeddings.first().map(|r| r.len()).unwrap_or(0);
        let mut cursor = 0usize;
        for item in computed.iter_mut() {
            let n = item.chunk_texts.len();
            let mut flat = Vec::with_capacity(n * dim);
            for row in &embeddings[cursor..cursor + n] {
                flat.extend_from_slice(row);
            }
            cursor += n;
            item.entry.embeddings = flat;
            item.entry.dim = dim as u32;
            let _ = store.put(&item.key, &item.entry);
        }

        // Phase 4: assemble in manifest order (hits and computed interleaved
        // back into a single sorted-by-path sequence).
        let mut per_file: Vec<(FileRecord, FileEntry)> = hits;
        per_file.extend(computed.into_iter().map(|c| (c.record, c.entry)));
        per_file.sort_by(|a, b| a.0.rel_path.cmp(&b.0.rel_path));

        let mut chunks: Vec<Chunk> = Vec::new();
        let mut rows: Vec<Vec<f32>> = Vec::new();
        let mut docs: Vec<Vec<String>> = Vec::new();
        let mut file_sizes: HashMap<String, usize> = HashMap::new();
        for (record, entry) in &per_file {
            let language = detect_language(Path::new(&record.rel_path));
            let dim = entry.dim as usize;
            let path_tokens = path_enrichment_tokens(&record.rel_path);
            let file_chars: usize = entry.chunks.iter().map(|c| c.content.len()).sum();
            if !entry.chunks.is_empty() {
                file_sizes.insert(record.rel_path.clone(), file_chars);
            }
            for (i, stored) in entry.chunks.iter().enumerate() {
                chunks.push(Chunk {
                    content: stored.content.clone(),
                    file_path: record.rel_path.clone(),
                    start_line: stored.start_line,
                    end_line: stored.end_line,
                    language: language.map(|l| l.to_string()),
                });
                if dim > 0 && entry.embeddings.len() >= (i + 1) * dim {
                    rows.push(entry.embeddings[i * dim..(i + 1) * dim].to_vec());
                } else {
                    rows.push(vec![0.0; dim.max(1)]);
                }
                let mut doc = entry
                    .token_lists
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| tokenize(&stored.content));
                doc.extend(path_tokens.iter().cloned());
                docs.push(doc);
            }
        }

        if chunks.is_empty() {
            bail!("No supported files found under {}.", root.display());
        }

        let chunks_total = chunks.len();
        let bm25 = Bm25Index::build(&docs);
        let dense = DenseIndex::new(rows);
        let (file_mapping, language_mapping) = populate_mappings(&chunks);

        Ok(SembleIndex {
            model,
            chunks,
            dense,
            bm25,
            file_mapping,
            language_mapping,
            file_sizes,
            content,
            build_stats: BuildStats {
                files_total,
                files_from_store,
                files_computed,
                chunks_total,
            },
        })
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

fn path_enrichment_tokens(rel_path: &str) -> Vec<String> {
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
