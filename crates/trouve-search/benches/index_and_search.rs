//! Criterion micro-benchmarks for chunking, BM25, and dense search.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use trouve_search::bm25::Bm25Index;
use trouve_search::chunk::chunk_source;
use trouve_search::dense::DenseIndex;
use trouve_search::tokens::tokenize;

fn synthetic_source(functions: usize) -> String {
    let mut source = String::new();
    for i in 0..functions {
        source.push_str(&format!(
            "def function_number_{i}(value):\n    \"\"\"Process value {i}.\"\"\"\n    result = value * {i}\n    return result + 7\n\n\n"
        ));
    }
    source
}

fn bench_chunking(c: &mut Criterion) {
    let source = synthetic_source(200);
    c.bench_function("chunk_python_200_functions", |b| {
        b.iter(|| chunk_source(black_box(&source), "bench.py", Some("python")))
    });
}

fn bench_bm25(c: &mut Criterion) {
    let docs: Vec<Vec<String>> = (0..5000)
        .map(|i| {
            tokenize(&format!(
                "fn handler_{i}(request) {{ process_request_{i}(request) }}"
            ))
        })
        .collect();
    let index = Bm25Index::build(&docs);
    let query = tokenize("process request handler");
    c.bench_function("bm25_build_5k_docs", |b| {
        b.iter(|| Bm25Index::build(black_box(&docs)))
    });
    c.bench_function("bm25_query_5k_docs", |b| {
        b.iter(|| index.get_scores(black_box(&query), None))
    });
}

fn bench_dense(c: &mut Criterion) {
    let dim = 256;
    let rows: Vec<Vec<f32>> = (0..20000)
        .map(|i| (0..dim).map(|j| ((i * j) % 97) as f32 / 97.0).collect())
        .collect();
    let index = DenseIndex::new(rows);
    let query: Vec<f32> = (0..dim).map(|j| (j % 13) as f32 / 13.0).collect();
    c.bench_function("dense_query_20k_rows", |b| {
        b.iter(|| index.query(black_box(&query), 50, None))
    });
}

criterion_group!(benches, bench_chunking, bench_bm25, bench_dense);
criterion_main!(benches);
