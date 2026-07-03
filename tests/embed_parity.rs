//! Parity tests for the in-house model2vec encoder against the official
//! `model2vec-rs` implementation (single-text batches, where upstream's
//! padding has no effect).

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use model2vec_rs::model::StaticModel;
use trouve::embed::EmbeddingModel;

/// Build a small Bert-style WordPiece model (BertNormalizer +
/// BertPreTokenizer + WordPiece + quantization mapping/weights) on disk so
/// the fast ASCII path is exercised end to end.
fn write_bert_model(dir: &std::path::Path) {
    let pieces: Vec<&str> = vec![
        "[UNK]", "[PAD]", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n",
        "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "0", "1", "2", "##a", "##b",
        "##c", "##d", "##e", "##f", "##g", "##h", "##i", "##j", "##k", "##l", "##m", "##n", "##o",
        "##p", "##q", "##r", "##s", "##t", "##u", "##v", "##w", "##x", "##y", "##z", "##0", "##1",
        "##2", "def", "return", "self", "fn", "let", "##ing", "##er", "##tion", "(", ")", "{", "}",
        "[", "]", ".", ",", ":", ";", "=", "+", "-", "*", "/", "_", "##_", "\"", "'", "#", "!",
        "?", "<", ">", "&", "|", "%", "@", "the", "value", "index",
    ];
    let mut vocab = BTreeMap::new();
    for (i, p) in pieces.iter().enumerate() {
        vocab.insert((*p).to_string(), i as u32);
    }
    let tokenizer = serde_json::json!({
        "version": "1.0",
        "truncation": {"direction": "Right", "max_length": 512, "strategy": "LongestFirst", "stride": 0},
        "padding": {"strategy": "BatchLongest", "direction": "Right", "pad_to_multiple_of": null,
                     "pad_id": 1, "pad_type_id": 0, "pad_token": "[PAD]"},
        "added_tokens": [
            {"id": 0, "content": "[UNK]", "single_word": false, "lstrip": false, "rstrip": false,
             "normalized": false, "special": true},
            {"id": 1, "content": "[PAD]", "single_word": false, "lstrip": false, "rstrip": false,
             "normalized": false, "special": true}
        ],
        "normalizer": {"type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true,
                        "strip_accents": null, "lowercase": true},
        "pre_tokenizer": {"type": "BertPreTokenizer"},
        "post_processor": null,
        "decoder": null,
        "model": {
            "type": "WordPiece",
            "unk_token": "[UNK]",
            "continuing_subword_prefix": "##",
            "max_input_chars_per_word": 100,
            "vocab": vocab
        }
    });

    let rows = pieces.len();
    let dim = 8usize;
    // Deterministic pseudo-random embeddings, plus mapping/weights tensors to
    // exercise the vocabulary-quantization path.
    let mut emb: Vec<u8> = Vec::new();
    for i in 0..rows * dim {
        let v = (i as f32 * 37.0 + 11.0).sin() * 0.5;
        emb.extend_from_slice(&v.to_le_bytes());
    }
    let mut weights: Vec<u8> = Vec::new();
    for i in 0..rows {
        weights.extend_from_slice(&(0.5f64 + (i as f64) * 0.01).to_le_bytes());
    }
    let mut mapping: Vec<u8> = Vec::new();
    for i in 0..rows {
        // Swap a couple of rows so mapping != identity.
        let row = match i {
            2 => 3i64,
            3 => 2i64,
            other => other as i64,
        };
        mapping.extend_from_slice(&row.to_le_bytes());
    }

    let emb_end = emb.len();
    let w_end = emb_end + weights.len();
    let m_end = w_end + mapping.len();
    let header = serde_json::json!({
        "embeddings": {"dtype": "F32", "shape": [rows, dim], "data_offsets": [0, emb_end]},
        "weights": {"dtype": "F64", "shape": [rows], "data_offsets": [emb_end, w_end]},
        "mapping": {"dtype": "I64", "shape": [rows], "data_offsets": [w_end, m_end]},
    })
    .to_string();
    // Pad header so tensor data is 8-byte aligned (safetensors convention).
    let mut header = header.into_bytes();
    while (8 + header.len()) % 8 != 0 {
        header.push(b' ');
    }

    let mut file: Vec<u8> = Vec::new();
    file.extend_from_slice(&(header.len() as u64).to_le_bytes());
    file.extend_from_slice(&header);
    file.extend_from_slice(&emb);
    file.extend_from_slice(&weights);
    file.extend_from_slice(&mapping);

    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("tokenizer.json"), tokenizer.to_string()).unwrap();
    std::fs::write(dir.join("model.safetensors"), file).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"normalize": true}"#).unwrap();
}

fn bert_model_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("trouve-bert-parity-{}", std::process::id()));
    if !dir.join("config.json").exists() {
        write_bert_model(&dir);
    }
    dir
}

const SAMPLES: &[&str] = &[
    "",
    "def foo(bar):\n    return bar * 2",
    "the value of the index",
    "SELF.Value_Index != other",
    "fn main() { let x = [1, 2, 3]; }",
    "words with trailing spaces   \t\n",
    "control\x01chars\x02every\x03where",
    "unknown§tokens…here",         // non-ASCII: falls back to HF pipeline
    "mixed ascii and 日本語 text", // chinese-char handling path
    "café naïve résumé",           // accents (strip_accents via lowercase)
    "punc!!!((()))...___===",
    "supercalifragilisticexpialidocious antidisestablishmentarianism",
    "[UNK] literal added token",
    "a",
    "\u{0}\u{fffd}",
];

fn assert_matches_reference(model_dir: &str) {
    let ours = EmbeddingModel::load(Some(model_dir)).unwrap();
    let reference = StaticModel::from_pretrained(model_dir, None, None, None).unwrap();

    for text in SAMPLES {
        let a = ours.encode_one(text);
        // Batch of one: upstream padding is a no-op, output is deterministic.
        let b = reference
            .encode_with_args(&[text.to_string()], Some(512), 1)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(a.len(), b.len(), "dim mismatch for {text:?}");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                x.to_bits() == y.to_bits(),
                "mismatch for {text:?} at dim {i}: {x} vs {y}"
            );
        }
    }

    // A long text to hit both truncate_str and token truncation.
    let long = "word ".repeat(4000) + &"x".repeat(300);
    let a = ours.encode_one(&long);
    let b = reference
        .encode_with_args(std::slice::from_ref(&long), Some(512), 1)
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
}

#[test]
fn matches_model2vec_on_bert_wordpiece_model() {
    let dir = bert_model_dir();
    assert_matches_reference(dir.to_str().unwrap());
}

#[test]
fn matches_model2vec_on_wordlevel_test_model() {
    // The shared toy model is WordLevel + Whitespace: no fast path, exercises
    // the HF fallback pipeline.
    let model_dir = common::test_env();
    assert_matches_reference(model_dir);
}

#[test]
fn matches_model2vec_on_real_model_if_present() {
    // Bit-exact parity against the real potion-code-16M when available
    // locally (kept out of CI: the model is ~60 MB).
    let Ok(dir) = std::env::var("TROUVE_MODEL_NAME") else {
        eprintln!("skipping: TROUVE_MODEL_NAME not set");
        return;
    };
    if !std::path::Path::new(&dir).join("config.json").exists() {
        eprintln!("skipping: no local model at {dir}");
        return;
    }
    assert_matches_reference(&dir);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]
    /// Random printable-ASCII strings agree with model2vec bit-for-bit.
    #[test]
    fn fast_ascii_path_matches_reference(text in "[ -~\\t\\n\\r]{0,200}") {
        let dir = bert_model_dir();
        let ours = EmbeddingModel::load(Some(dir.to_str().unwrap())).unwrap();
        let reference = StaticModel::from_pretrained(dir.to_str().unwrap(), None, None, None).unwrap();
        let a = ours.encode_one(&text);
        let b = reference.encode_with_args(std::slice::from_ref(&text), Some(512), 1)
            .into_iter().next().unwrap();
        proptest::prop_assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn batching_does_not_change_embeddings() {
    let dir = bert_model_dir();
    let model = EmbeddingModel::load(Some(dir.to_str().unwrap())).unwrap();
    let solo = model.encode_one("def foo():");
    let batch = model.embed_texts(&[
        "def foo():".to_string(),
        "a much longer text that would have forced padding upstream ".repeat(20),
    ]);
    assert_eq!(
        solo.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        batch[0].iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
}
