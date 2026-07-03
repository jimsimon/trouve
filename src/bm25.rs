//! BM25 sparse retrieval.
//!
//! Reimplementation of the parts of `bm25s` used by upstream Semble: the
//! "lucene" scoring variant with k1=1.5, b=0.75, plus `enrich_for_bm25`
//! from `semble/index/sparse.py`.

use std::collections::HashMap;
use std::path::Path;

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
pub struct Bm25Index {
    vocab: HashMap<String, u32>,
    /// Precomputed per-token postings: (doc_id, idf * tf_component).
    postings: Vec<Vec<(u32, f32)>>,
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
        let mut postings: Vec<Vec<(u32, f32)>> = vec![Vec::new(); vocab.len()];
        for (doc_id, tf) in doc_tfs.iter().enumerate() {
            let dl = docs[doc_id].len() as f64;
            let norm = K1 * (1.0 - B + B * dl / avgdl.max(f64::MIN_POSITIVE));
            for (id, count) in tf {
                let dfi = df[*id as usize] as f64;
                let idf = (1.0 + (n - dfi + 0.5) / (dfi + 0.5)).ln();
                let tfc = *count as f64 / (*count as f64 + norm);
                postings[*id as usize].push((doc_id as u32, (idf * tfc) as f32));
            }
        }

        Bm25Index {
            vocab,
            postings,
            num_docs,
        }
    }

    /// Return BM25 scores for all documents given query tokens; documents
    /// excluded by `mask` (when provided) score 0. Duplicate query tokens are
    /// counted multiple times, matching `bm25s.get_scores`.
    pub fn get_scores(&self, query_tokens: &[String], mask: Option<&[bool]>) -> Vec<f32> {
        let mut scores = vec![0.0f32; self.num_docs];
        for tok in query_tokens {
            if let Some(id) = self.vocab.get(tok) {
                for (doc_id, s) in &self.postings[*id as usize] {
                    scores[*doc_id as usize] += *s;
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
