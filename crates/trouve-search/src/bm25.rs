//! BM25 sparse retrieval.
//!
//! Reimplementation of the parts of `bm25s` used by upstream Semble: the
//! "lucene" scoring variant with k1=1.5, b=0.75, plus `enrich_for_bm25`
//! from `semble/index/sparse.py`.
//!
//! Note that production indexing does not call [`enrich_for_bm25`]: it
//! tokenizes chunk content and path enrichment separately (see
//! `crate::index::path_enrichment_tokens`, which yields the same token
//! multiset without building the concatenated string). The helper is kept
//! as the reference form of the upstream enrichment and for tests.

use std::path::Path;

use rayon::prelude::*;

type FastMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
type FastSet<K> = std::collections::HashSet<K, ahash::RandomState>;

use crate::snapshot::{Buf, Posting};
use crate::tokens::TokenDocs;
use crate::types::Chunk;

const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Append file path components to BM25 content to boost path-based queries.
///
/// Assumes `chunk.file_path` is already repo-relative so machine-specific
/// directory components are never indexed.
pub fn enrich_for_bm25(chunk: &Chunk) -> String {
    let path = Path::new(&chunk.file_path);
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
    let dir_text = dir_parts[keep..].join(" ");
    format!("{} {stem} {stem} {dir_text}", chunk.content)
}

/// An immutable BM25 index over tokenized documents (lucene scoring variant).
///
/// Stored flat so it can live either in owned memory (fresh build) or
/// zero-copy in a memory-mapped snapshot: terms are sorted lexicographically
/// in one contiguous blob, and each term's postings are a contiguous slice of
/// the flattened posting array. Query tokens are resolved by binary search.
///
/// Postings hold raw term frequencies; `idf` and the document-length norm are
/// applied at query time from `doc_lengths` and per-term document frequencies
/// (the posting-list lengths). This keeps every stored value per-document
/// stable, which is what makes snapshot patching possible.
pub struct Bm25Index {
    /// Concatenated UTF-8 bytes of all terms, sorted lexicographically.
    term_blob: Buf<u8>,
    /// `n_terms + 1` byte offsets into `term_blob`.
    term_offsets: Buf<u32>,
    /// `n_terms + 1` offsets into `postings`.
    posting_offsets: Buf<u64>,
    /// Flattened postings, grouped by term, docs ascending within a term.
    postings: Buf<Posting>,
    /// Token count of every document.
    doc_lengths: Buf<u32>,
}

impl Bm25Index {
    /// Build an index from nested tokenized documents (test/debug helper).
    pub fn build(docs: &[Vec<String>]) -> Bm25Index {
        Self::build_flat(&TokenDocs::from_nested(docs))
    }

    /// Build an index from flat tokenized documents (parallel).
    pub fn build_flat(docs: &TokenDocs) -> Bm25Index {
        let n_docs = docs.n_docs();
        let doc_lengths: Vec<u32> = (0..n_docs)
            .into_par_iter()
            .map(|d| docs.doc_len(d) as u32)
            .collect();

        // Per-document term frequencies, in parallel.
        let doc_tfs: Vec<Vec<(&[u8], u32)>> = (0..n_docs)
            .into_par_iter()
            .map(|d| {
                let mut tf: FastMap<&[u8], u32> =
                    FastMap::with_capacity_and_hasher(docs.doc_len(d), Default::default());
                for tok in docs.doc_tokens(d) {
                    *tf.entry(tok).or_insert(0) += 1;
                }
                tf.into_iter().collect()
            })
            .collect();

        // Global sorted vocabulary: parallel per-shard uniquing, then merge.
        let mut terms: Vec<&[u8]> = doc_tfs
            .par_iter()
            .fold(FastSet::default, |mut set: FastSet<&[u8]>, tf| {
                set.extend(tf.iter().map(|(t, _)| *t));
                set
            })
            .reduce(FastSet::default, |mut a, b| {
                if a.len() < b.len() {
                    let mut b = b;
                    b.extend(a);
                    return b;
                }
                a.extend(b);
                a
            })
            .into_iter()
            .collect();
        terms.par_sort_unstable();

        let term_ids: FastMap<&[u8], u32> = terms
            .iter()
            .enumerate()
            .map(|(i, t)| (*t, i as u32))
            .collect();
        let mut term_blob: Vec<u8> = Vec::new();
        let mut term_offsets: Vec<u32> = Vec::with_capacity(terms.len() + 1);
        term_offsets.push(0);
        for term in &terms {
            term_blob.extend_from_slice(term);
            term_offsets.push(term_blob.len() as u32);
        }

        // All (term, doc, tf) triples, sorted by (term, doc): sorting scales
        // across cores where per-term pushes cannot.
        let term_ids = &term_ids;
        let mut triples: Vec<(u32, u32, u32)> = doc_tfs
            .par_iter()
            .enumerate()
            .flat_map_iter(|(doc_id, tf)| {
                tf.iter()
                    .map(move |(term, count)| (term_ids[term], doc_id as u32, *count))
            })
            .collect();
        triples.par_sort_unstable();

        let postings: Vec<Posting> = triples
            .par_iter()
            .map(|&(_, doc, tf)| Posting { doc, tf })
            .collect();
        // Each term's postings range is found by binary search over the
        // sorted triples; terms with no postings get an empty range.
        let posting_offsets: Vec<u64> = (0..=terms.len())
            .into_par_iter()
            .map(|t| triples.partition_point(|&(id, _, _)| (id as usize) < t) as u64)
            .collect();

        Bm25Index {
            term_blob: term_blob.into(),
            term_offsets: term_offsets.into(),
            posting_offsets: posting_offsets.into(),
            postings: postings.into(),
            doc_lengths: doc_lengths.into(),
        }
    }

    /// Reassemble an index from flat parts (snapshot load path).
    pub fn from_parts(
        term_blob: Buf<u8>,
        term_offsets: Buf<u32>,
        posting_offsets: Buf<u64>,
        postings: Buf<Posting>,
        doc_lengths: Buf<u32>,
    ) -> Bm25Index {
        Bm25Index {
            term_blob,
            term_offsets,
            posting_offsets,
            postings,
            doc_lengths,
        }
    }

    /// Borrow the flat parts for serialization (snapshot save path).
    #[allow(clippy::type_complexity)]
    pub fn flat_parts(&self) -> (&[u8], &[u32], &[u64], &[Posting], &[u32]) {
        (
            &self.term_blob,
            &self.term_offsets,
            &self.posting_offsets,
            &self.postings,
            &self.doc_lengths,
        )
    }

    fn term_bytes(&self, i: usize) -> &[u8] {
        &self.term_blob[self.term_offsets[i] as usize..self.term_offsets[i + 1] as usize]
    }

    /// Binary search for a token in the sorted term blob.
    fn find_term(&self, token: &str) -> Option<usize> {
        let n_terms = self.term_offsets.len().saturating_sub(1);
        let (mut lo, mut hi) = (0usize, n_terms);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.term_bytes(mid).cmp(token.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    /// Return BM25 scores for all documents given query tokens; documents
    /// excluded by `mask` (when provided) score 0. Duplicate query tokens are
    /// counted multiple times, matching `bm25s.get_scores`.
    pub fn get_scores(&self, query_tokens: &[String], mask: Option<&[bool]>) -> Vec<f32> {
        let num_docs = self.num_docs();
        let mut scores = vec![0.0f32; num_docs];
        let n = num_docs as f64;
        let avgdl = if num_docs == 0 {
            0.0
        } else {
            self.doc_lengths.iter().map(|l| *l as f64).sum::<f64>() / n
        };
        let norm_base = K1 * (1.0 - B);
        let norm_scale = K1 * B / avgdl.max(f64::MIN_POSITIVE);
        for tok in query_tokens {
            if let Some(id) = self.find_term(tok) {
                let range =
                    self.posting_offsets[id] as usize..self.posting_offsets[id + 1] as usize;
                let plist = &self.postings[range];
                let dfi = plist.len() as f64;
                let idf = (1.0 + (n - dfi + 0.5) / (dfi + 0.5)).ln();
                for p in plist {
                    let tf = p.tf as f64;
                    let norm = norm_base + norm_scale * self.doc_lengths[p.doc as usize] as f64;
                    scores[p.doc as usize] += (idf * (tf / (tf + norm))) as f32;
                }
            }
        }
        if let Some(mask) = mask {
            for (i, keep) in mask.iter().enumerate() {
                if !keep {
                    scores[i] = 0.0;
                }
            }
        }
        scores
    }

    pub fn num_docs(&self) -> usize {
        self.doc_lengths.len()
    }
}

/// Convert a selector of chunk indices into a boolean mask of length `size`.
pub fn selector_to_mask(selector: Option<&[usize]>, size: usize) -> Option<Vec<bool>> {
    selector.map(|sel| {
        let mut mask = vec![false; size];
        for i in sel {
            if *i < size {
                mask[*i] = true;
            }
        }
        mask
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;

    fn docs() -> Vec<Vec<String>> {
        vec![
            tokenize("fn save_model(path: &str) { write_to_disk(path) }"),
            tokenize("fn load_model(path: &str) { read_from_disk(path) }"),
            tokenize("struct HttpClient { retries: u32 }"),
        ]
    }

    #[test]
    fn scores_relevant_doc_highest() {
        let index = Bm25Index::build(&docs());
        let scores = index.get_scores(&tokenize("save model"), None);
        assert_eq!(scores.len(), 3);
        assert!(scores[0] > scores[1]);
        assert!(scores[0] > scores[2]);
    }

    #[test]
    fn unknown_tokens_score_zero() {
        let index = Bm25Index::build(&docs());
        let scores = index.get_scores(&tokenize("zzz qqq"), None);
        assert!(scores.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn mask_zeroes_excluded_docs() {
        let index = Bm25Index::build(&docs());
        let mask = vec![false, true, true];
        let scores = index.get_scores(&tokenize("model path"), Some(&mask));
        assert_eq!(scores[0], 0.0);
        assert!(scores[1] > 0.0);
    }

    #[test]
    fn enrich_appends_stem_twice_and_dirs() {
        let chunk = Chunk {
            content: "code".into(),
            file_path: "a/b/c/d/handler.py".into(),
            start_line: 1,
            end_line: 1,
            language: Some("python".into()),
        };
        let enriched = enrich_for_bm25(&chunk);
        assert_eq!(enriched, "code handler handler b c d");
    }

    #[test]
    fn duplicate_query_tokens_count_twice() {
        let index = Bm25Index::build(&docs());
        let once = index.get_scores(&["model".to_string()], None);
        let twice = index.get_scores(&["model".to_string(), "model".to_string()], None);
        assert!((twice[0] - 2.0 * once[0]).abs() < 1e-6);
    }
}
