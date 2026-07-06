//! Hybrid search: RRF-fused semantic + BM25 retrieval with code-tuned
//! reranking (port of `semble/search.py`).
//!
//! Returned scores are alpha-weighted RRF fusion values when `rerank` is
//! off; reranking replaces them with boosted/penalized rerank scores on a
//! different scale (see [`crate::types::SearchResult`]).

use std::collections::HashMap;

use crate::bm25::{selector_to_mask, Bm25Index};
use crate::dense::DenseIndex;
use crate::embed::EmbeddingModel;
use crate::ranking::{
    apply_query_boost, boost_multi_chunk_files, rerank_topk, resolve_alpha, ScoreMap,
};
use crate::tokens::tokenize;
use crate::types::{Chunk, SearchResult};

const RRF_K: f64 = 60.0;

/// Convert raw scores to RRF scores `1/(k + rank)`; higher raw score -> rank 1.
fn rrf_scores(scores: &HashMap<usize, f64>) -> HashMap<usize, f64> {
    let mut ranked: Vec<usize> = scores.keys().copied().collect();
    ranked.sort_by(|a, b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    ranked
        .into_iter()
        .enumerate()
        .map(|(rank0, chunk)| (chunk, 1.0 / (RRF_K + (rank0 + 1) as f64)))
        .collect()
}

/// Run semantic search for a query, returning `(chunk index, similarity)` pairs.
pub fn search_semantic(
    query: &str,
    model: &EmbeddingModel,
    dense: &DenseIndex,
    top_k: usize,
    selector: Option<&[usize]>,
) -> Vec<(usize, f64)> {
    let query_embedding = model.encode_one(query);
    dense
        .query(&query_embedding, top_k, selector)
        .into_iter()
        .map(|(i, distance)| (i, 1.0 - distance as f64))
        .collect()
}

/// Return chunk indices ranked by BM25 score, excluding zero-score results.
fn search_bm25(
    query: &str,
    bm25: &Bm25Index,
    num_chunks: usize,
    top_k: usize,
    selector: Option<&[usize]>,
) -> Vec<(usize, f64)> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mask = selector_to_mask(selector, num_chunks);
    let scores = bm25.get_scores(&tokens, mask.as_deref());
    let mut indexed: Vec<(usize, f32)> = scores
        .into_iter()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .collect();
    indexed.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    indexed.truncate(top_k);
    indexed.into_iter().map(|(i, s)| (i, s as f64)).collect()
}

/// Hybrid search: alpha-weighted combination of semantic and BM25 scores.
///
/// Both score sets are converted to RRF scores before combining, so alpha has
/// a consistent meaning regardless of raw score magnitude.
#[allow(clippy::too_many_arguments)]
pub fn search(
    query: &str,
    model: &EmbeddingModel,
    dense: &DenseIndex,
    bm25: &Bm25Index,
    chunks: &[Chunk],
    top_k: usize,
    alpha: Option<f64>,
    selector: Option<&[usize]>,
    rerank: bool,
) -> Vec<SearchResult> {
    let alpha_weight = resolve_alpha(query, alpha);

    // Over-fetch candidates so the merged pool is large enough after union and re-ranking.
    let candidate_count = top_k * 5;

    let semantic: HashMap<usize, f64> =
        search_semantic(query, model, dense, candidate_count, selector)
            .into_iter()
            .collect();
    let bm25_scores: HashMap<usize, f64> =
        search_bm25(query, bm25, chunks.len(), candidate_count, selector)
            .into_iter()
            .collect();

    let normalized_semantic = rrf_scores(&semantic);
    let normalized_bm25 = rrf_scores(&bm25_scores);

    let mut combined: ScoreMap = HashMap::new();
    for i in normalized_semantic.keys().chain(normalized_bm25.keys()) {
        combined.entry(*i).or_insert_with(|| {
            alpha_weight * normalized_semantic.get(i).copied().unwrap_or(0.0)
                + (1.0 - alpha_weight) * normalized_bm25.get(i).copied().unwrap_or(0.0)
        });
    }

    let ranked: Vec<(usize, f64)> = if rerank {
        boost_multi_chunk_files(&mut combined, chunks);
        apply_query_boost(&mut combined, query, chunks);
        rerank_topk(&combined, chunks, top_k, alpha_weight < 1.0)
    } else {
        let mut sorted: Vec<(usize, f64)> = combined.into_iter().collect();
        sorted.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        sorted.truncate(top_k);
        sorted
    };

    ranked
        .into_iter()
        .map(|(i, score)| SearchResult {
            chunk: chunks[i].clone(),
            score,
        })
        .collect()
}

/// Return chunks semantically similar to a seed chunk (port of `find_related`).
pub fn find_related(
    seed: &Chunk,
    model: &EmbeddingModel,
    dense: &DenseIndex,
    chunks: &[Chunk],
    top_k: usize,
    language_selector: Option<&[usize]>,
) -> Vec<SearchResult> {
    let results = search_semantic(&seed.content, model, dense, top_k + 1, language_selector);
    results
        .into_iter()
        .filter(|(i, _)| &chunks[*i] != seed)
        .take(top_k)
        .map(|(i, score)| SearchResult {
            chunk: chunks[i].clone(),
            score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_conversion_orders_by_rank() {
        let mut scores = HashMap::new();
        scores.insert(10usize, 0.9);
        scores.insert(20usize, 0.5);
        scores.insert(30usize, 0.7);
        let rrf = rrf_scores(&scores);
        assert!((rrf[&10] - 1.0 / 61.0).abs() < 1e-12);
        assert!((rrf[&30] - 1.0 / 62.0).abs() < 1e-12);
        assert!((rrf[&20] - 1.0 / 63.0).abs() < 1e-12);
    }
}
