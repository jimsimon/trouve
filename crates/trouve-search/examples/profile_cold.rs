//! Fine-grained cold-path profiling: chunk vs tokenize vs BM25 build.
//!
//! Usage: cargo run --release --example profile_cold -- <dir>

use std::time::Instant;

use rayon::prelude::*;
use trouve_search::chunk::chunk_source;
use trouve_search::languages::detect_language;
use trouve_search::tokens::tokenize;
use trouve_search::types::ContentType;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: profile_cold <dir>");
    let root = std::path::Path::new(&dir);

    let files = trouve_search::walker::walk_files(
        root,
        &trouve_search::languages::get_extensions(&[ContentType::Code]),
        &[],
    );
    println!("files: {}", files.len());

    let t = Instant::now();
    let sources: Vec<(String, String)> = files
        .par_iter()
        .filter_map(|f| {
            let bytes = std::fs::read(f).ok()?;
            // Same size gate as the real pipeline.
            if trouve_search::languages::file_status_for_bytes(&bytes)
                != trouve_search::languages::FileStatus::Valid
            {
                return None;
            }
            let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            Some((rel, String::from_utf8_lossy(&bytes).into_owned()))
        })
        .collect();
    println!(
        "read: {:.2?} ({} MB)",
        t.elapsed(),
        sources.iter().map(|(_, s)| s.len()).sum::<usize>() / (1 << 20)
    );

    for round in 0..2 {
        let t = Instant::now();
        let chunked: Vec<(String, Vec<trouve_search::types::Chunk>)> = sources
            .par_iter()
            .map(|(rel, src)| {
                let language = detect_language(std::path::Path::new(rel));
                (rel.clone(), chunk_source(src, rel, language))
            })
            .collect();
        let chunk_t = t.elapsed();
        let n_chunks: usize = chunked.iter().map(|(_, c)| c.len()).sum();

        let t = Instant::now();
        let docs: Vec<Vec<String>> = chunked
            .par_iter()
            .flat_map_iter(|(_, chunks)| chunks.iter().map(|c| tokenize(&c.content)))
            .collect();
        let tok_t = t.elapsed();
        let n_tokens: usize = docs.iter().map(Vec::len).sum();

        let t = Instant::now();
        let bm25 = trouve_search::bm25::Bm25Index::build(&docs);
        let bm25_t = t.elapsed();

        if round == 1 {
            println!("chunk: {chunk_t:.2?} ({n_chunks} chunks)");
            println!("tokenize: {tok_t:.2?} ({n_tokens} tokens)");
            println!("bm25 build: {bm25_t:.2?} ({} docs)", bm25.num_docs());
        }
    }
}
