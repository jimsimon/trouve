//! BM25 sparse retrieval.
//!
//! Reimplementation of the parts of `bm25s` used by upstream Semble: the
//! "lucene" scoring variant with k1=1.5, b=0.75, plus `enrich_for_bm25`
//! from `semble/index/sparse.py`.

use std::collections::HashMap;
use std::path::Path;

use crate::snapshot::{Buf, Posting};
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
pub struct Bm25Index {
    /// Concatenated UTF-8 bytes of all terms, sorted lexicographically.
    term_blob: Buf<u8>,
    /// `n_terms + 1` byte offsets into `term_blob`.
    term_offsets: Buf<u32>,
    /// `n_terms + 1` offsets into `postings`.
    posting_offsets: Buf<u64>,
    /// Flattened postings, grouped by term: precomputed `idf * tf` scores.
    postings: Buf<Posting>,
    num_docs: usize,
}

impl Bm25Index {
    /// Build an index from tokenized documents.
    pub fn build(docs: &[Vec<String>]) -> Bm25Index {
        let num_docs = docs.len();
        let avgdl = if num_docs == 0 {
            0.0
        } else {
            docs.iter().map(|d| d.len() as f64).sum::<f64>() / num_docs as f64
        };

        let mut vocab: HashMap<String, u32> = HashMap::new();
        // Term frequencies per document.
        let mut doc_tfs: Vec<HashMap<u32, u32>> = Vec::with_capacity(num_docs);
        for doc in docs {
            let mut tf: HashMap<u32, u32> = HashMap::new();
            for tok in doc {
                let next_id = vocab.len() as u32;
                let id = *vocab.entry(tok.clone()).or_insert(next_id);
                *tf.entry(id).or_insert(0) += 1;
            }
            doc_tfs.push(tf);
        }

        // Document frequency per token.
        let mut df = vec![0u32; vocab.len()];
        for tf in &doc_tfs {
            for id in tf.keys() {
                df[*id as usize] += 1;
            }
        }

        let n = num_docs as f64;
        let mut term_postings: Vec<Vec<Posting>> = vec![Vec::new(); vocab.len()];
        for (doc_id, tf) in doc_tfs.iter().enumerate() {
            let dl = docs[doc_id].len() as f64;
            let norm = K1 * (1.0 - B + B * dl / avgdl.max(f64::MIN_POSITIVE));
            for (id, count) in tf {
                let dfi = df[*id as usize] as f64;
                let idf = (1.0 + (n - dfi + 0.5) / (dfi + 0.5)).ln();
                let tfc = *count as f64 / (*count as f64 + norm);
                term_postings[*id as usize].push(Posting {
                    doc: doc_id as u32,
                    score: (idf * tfc) as f32,
                });
            }
        }

        // Flatten into the sorted-term representation.
        let mut terms: Vec<(&String, u32)> = vocab.iter().map(|(t, id)| (t, *id)).collect();
        terms.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

        let mut term_blob: Vec<u8> = Vec::new();
        let mut term_offsets: Vec<u32> = Vec::with_capacity(terms.len() + 1);
        let mut posting_offsets: Vec<u64> = Vec::with_capacity(terms.len() + 1);
        let mut postings: Vec<Posting> = Vec::new();
        term_offsets.push(0);
        posting_offsets.push(0);
        for (term, id) in terms {
            term_blob.extend_from_slice(term.as_bytes());
            term_offsets.push(term_blob.len() as u32);
            postings.extend_from_slice(&term_postings[id as usize]);
            posting_offsets.push(postings.len() as u64);
        }

        Bm25Index {
            term_blob: term_blob.into(),
            term_offsets: term_offsets.into(),
            posting_offsets: posting_offsets.into(),
            postings: postings.into(),
            num_docs,
        }
    }

    /// Reassemble an index from flat parts (snapshot load path).
    pub fn from_parts(
        term_blob: Buf<u8>,
        term_offsets: Buf<u32>,
        posting_offsets: Buf<u64>,
        postings: Buf<Posting>,
        num_docs: usize,
    ) -> Bm25Index {
        Bm25Index {
            term_blob,
            term_offsets,
            posting_offsets,
            postings,
            num_docs,
        }
    }

    /// Borrow the flat parts for serialization (snapshot save path).
    pub fn flat_parts(&self) -> (&[u8], &[u32], &[u64], &[Posting]) {
        (
            &self.term_blob,
            &self.term_offsets,
            &self.posting_offsets,
            &self.postings,
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
        let mut scores = vec![0.0f32; self.num_docs];
        for tok in query_tokens {
            if let Some(id) = self.find_term(tok) {
                let range =
                    self.posting_offsets[id] as usize..self.posting_offsets[id + 1] as usize;
                for p in &self.postings[range] {
                    scores[p.doc as usize] += p.score;
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
        self.num_docs
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
