//! Split embedding time into tokenization vs pooling on a real corpus.
//!
//! Usage: cargo run --release --example profile_embed -- <dir> [model]

use std::time::Instant;

use model2vec_rs::model::StaticModel;
use rayon::prelude::*;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: profile_embed <dir> [model]");
    let model_id = args
        .next()
        .or_else(|| std::env::var("SEMBLE_MODEL_NAME").ok())
        .unwrap_or_else(|| "minishlab/potion-code-16M".to_string());

    // Collect chunk texts the same way the indexer does.
    let index = {
        let content = [semble::types::ContentType::Code];
        semble::index::SembleIndex::from_path(std::path::Path::new(&dir), &content, Some(&model_id))
            .expect("index build failed")
    };
    let texts: Vec<String> = index.chunks.iter().map(|c| c.content.clone()).collect();
    println!("chunks: {}", texts.len());

    // Load the raw tokenizer separately for the tokenize-only pass.
    let tokenizer_path = std::path::Path::new(&model_id).join("tokenizer.json");
    let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).expect("tokenizer load");

    let model = StaticModel::from_pretrained(&model_id, None, None, None).expect("model load");

    for round in 0..2 {
        // Full encode (tokenize + pool), parallel batches like embed_texts.
        let t = Instant::now();
        let out: Vec<Vec<f32>> = texts
            .par_chunks(512)
            .flat_map(|batch| model.encode_with_args(batch, Some(512), 512))
            .collect();
        let full = t.elapsed();

        // Tokenize only, same batching.
        let t = Instant::now();
        let n_tokens: usize = texts
            .par_chunks(512)
            .map(|batch| {
                let inputs: Vec<String> = batch.to_vec();
                tokenizer
                    .encode_batch_fast::<String>(inputs, false)
                    .expect("tokenize")
                    .iter()
                    .map(|e| e.get_ids().len())
                    .sum::<usize>()
            })
            .sum();
        let tok = t.elapsed();

        // Normalize + pre-tokenize only (no WordPiece), same batching.
        use tokenizers::tokenizer::{
            NormalizedString, Normalizer, OffsetReferential, OffsetType, PreTokenizedString,
            PreTokenizer,
        };
        let t = Instant::now();
        let n_words: usize = texts
            .par_chunks(512)
            .map(|batch| {
                let mut words = 0usize;
                for text in batch {
                    let mut norm = NormalizedString::from(text.as_str());
                    if let Some(n) = tokenizer.get_normalizer() {
                        n.normalize(&mut norm).unwrap();
                    }
                    let mut pts = PreTokenizedString::from(norm);
                    if let Some(p) = tokenizer.get_pre_tokenizer() {
                        p.pre_tokenize(&mut pts).unwrap();
                    }
                    words += pts
                        .get_splits(OffsetReferential::Normalized, OffsetType::Byte)
                        .len();
                }
                words
            })
            .sum();
        let pre = t.elapsed();

        if round == 1 {
            println!("full encode: {:.2?}  ({} vectors)", full, out.len());
            println!("tokenize only: {:.2?}  ({} tokens)", tok, n_tokens);
            println!("normalize+pretokenize only: {:.2?}  ({} words)", pre, n_words);
            println!(
                "pooling+overhead (full - tokenize): {:.2?}",
                full.saturating_sub(tok)
            );
        }
    }
}
