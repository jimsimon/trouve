//! Brute-force cosine-similarity vector index with selector support.
//!
//! Port of the `SelectableBasicBackend` in `semble/index/dense.py`: stores
//! L2-normalized vectors and answers top-k queries by exact cosine distance,
//! optionally restricted to a selector of row indices.

use rayon::prelude::*;

/// Rows below this count are scored serially; above it, rayon splits the work.
const PARALLEL_THRESHOLD: usize = 4096;

pub struct DenseIndex {
    /// Row-major normalized vectors.
    vectors: Vec<f32>,
    dim: usize,
    rows: usize,
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

impl DenseIndex {
    /// Build an index from embedding rows, normalizing each row.
    pub fn new(embeddings: Vec<Vec<f32>>) -> DenseIndex {
        let rows = embeddings.len();
        let dim = embeddings.first().map(|r| r.len()).unwrap_or(0);
        let mut vectors = Vec::with_capacity(rows * dim);
        for mut row in embeddings {
            debug_assert_eq!(row.len(), dim);
            normalize(&mut row);
            vectors.extend_from_slice(&row);
        }
        DenseIndex { vectors, dim, rows }
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    fn similarity(&self, row: usize, query: &[f32]) -> f32 {
        let start = row * self.dim;
        let v = &self.vectors[start..start + self.dim];
        v.iter().zip(query).map(|(a, b)| a * b).sum()
    }

    /// Return the `k` nearest rows by cosine distance (`1 - similarity`),
    /// sorted ascending by distance. When `selector` is provided, only those
    /// row indices are considered.
    pub fn query(&self, query: &[f32], k: usize, selector: Option<&[usize]>) -> Vec<(usize, f32)> {
        if self.rows == 0 || k == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let mut q: Vec<f32> = query.to_vec();
        normalize(&mut q);

        let score_rows = |rows: &mut dyn Iterator<Item = usize>| -> Vec<(usize, f32)> {
            rows.map(|i| (i, 1.0 - self.similarity(i, &q))).collect()
        };

        let mut scored: Vec<(usize, f32)> = match selector {
            Some(sel) => {
                if sel.len() >= PARALLEL_THRESHOLD {
                    sel.par_iter()
                        .filter(|i| **i < self.rows)
                        .map(|i| (*i, 1.0 - self.similarity(*i, &q)))
                        .collect()
                } else {
                    score_rows(&mut sel.iter().copied().filter(|i| *i < self.rows))
                }
            }
            None => {
                if self.rows >= PARALLEL_THRESHOLD {
                    (0..self.rows)
                        .into_par_iter()
                        .map(|i| (i, 1.0 - self.similarity(i, &q)))
                        .collect()
                } else {
                    score_rows(&mut (0..self.rows))
                }
            }
        };

        let effective_k = k.min(scored.len());
        scored.select_nth_unstable_by(effective_k.saturating_sub(1), |a, b| {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(effective_k);
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> DenseIndex {
        DenseIndex::new(vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.7, 0.7, 0.0],
            vec![-1.0, 0.0, 0.0],
        ])
    }

    #[test]
    fn returns_nearest_first() {
        let results = index().query(&[1.0, 0.0, 0.0], 2, None);
        assert_eq!(results[0].0, 0);
        assert!(results[0].1 < 1e-6);
        assert_eq!(results[1].0, 2);
    }

    #[test]
    fn selector_restricts_rows() {
        let results = index().query(&[1.0, 0.0, 0.0], 4, Some(&[1, 3]));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1);
        assert_eq!(results[1].0, 3);
    }

    #[test]
    fn k_larger_than_rows() {
        let results = index().query(&[0.0, 1.0, 0.0], 100, None);
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn empty_index() {
        let idx = DenseIndex::new(Vec::new());
        assert!(idx.query(&[1.0], 5, None).is_empty());
    }
}
