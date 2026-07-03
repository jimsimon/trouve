//! Shared test support: a tiny deterministic local model2vec model so
//! integration tests run offline and fast, plus an isolated cache dir.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const DIM: usize = 16;

/// Words the toy model can embed. Anything else maps to [UNK] and is dropped
/// by model2vec, which mirrors real out-of-vocabulary behaviour.
const VOCAB_WORDS: &[&str] = &[
    "def",
    "fn",
    "class",
    "struct",
    "impl",
    "pub",
    "return",
    "import",
    "from",
    "let",
    "const",
    "var",
    "if",
    "else",
    "for",
    "while",
    "match",
    "pass",
    "self",
    "none",
    "true",
    "false",
    "print",
    "println",
    "authenticate",
    "authentication",
    "login",
    "logout",
    "password",
    "user",
    "session",
    "token",
    "database",
    "connection",
    "connect",
    "query",
    "cursor",
    "commit",
    "save",
    "load",
    "model",
    "disk",
    "file",
    "path",
    "read",
    "write",
    "open",
    "close",
    "parse",
    "parser",
    "handler",
    "request",
    "response",
    "http",
    "client",
    "server",
    "route",
    "router",
    "config",
    "settings",
    "cache",
    "store",
    "index",
    "search",
    "result",
    "chunk",
    "embed",
    "embedding",
    "vector",
    "score",
    "rank",
    "test",
    "assert",
    "main",
    "run",
    "start",
    "stop",
    "process",
    "thread",
    "worker",
    "job",
    "queue",
    "message",
    "event",
    "signal",
    "error",
    "exception",
    "raise",
    "try",
    "except",
    "finally",
    "with",
    "as",
    "in",
    "and",
    "or",
    "not",
    "is",
    "value",
    "key",
    "name",
    "data",
    "list",
    "dict",
    "map",
    "set",
    "get",
    "put",
    "post",
    "delete",
    "update",
    "create",
    "remove",
    "add",
    "insert",
    "find",
    "related",
    "branch",
    "worktree",
    "git",
    "commit",
    "merge",
    "checkout",
    "helper",
    "util",
    "utils",
    "core",
    "api",
    "app",
    "application",
    "x",
    "y",
    "z",
    "a",
    "b",
    "c",
    "foo",
    "bar",
    "baz",
    "qux",
    "one",
    "two",
    "three",
];

fn hash_word(word: &str) -> u64 {
    // FNV-1a for determinism across runs and platforms.
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in word.bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn word_vector(word: &str) -> Vec<f32> {
    let mut state = hash_word(word);
    (0..DIM)
        .map(|_| {
            // xorshift
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state % 2000) as f32 / 1000.0) - 1.0
        })
        .collect()
}

fn tokenizer_json() -> String {
    let mut vocab = BTreeMap::new();
    vocab.insert("[UNK]".to_string(), 0u32);
    for (i, w) in VOCAB_WORDS.iter().enumerate() {
        vocab.insert((*w).to_string(), (i + 1) as u32);
    }
    serde_json::json!({
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [],
        "normalizer": {"type": "Lowercase"},
        "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": null,
        "decoder": null,
        "model": {
            "type": "WordLevel",
            "vocab": vocab,
            "unk_token": "[UNK]"
        }
    })
    .to_string()
}

fn safetensors_bytes() -> Vec<u8> {
    let rows = VOCAB_WORDS.len() + 1;
    let mut data: Vec<u8> = Vec::with_capacity(rows * DIM * 4);
    // Row 0 = [UNK], zeros.
    for _ in 0..DIM {
        data.extend_from_slice(&0.0f32.to_le_bytes());
    }
    for word in VOCAB_WORDS {
        for v in word_vector(word) {
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    let header = serde_json::json!({
        "embeddings": {
            "dtype": "F32",
            "shape": [rows, DIM],
            "data_offsets": [0, data.len()],
        }
    })
    .to_string();
    let mut out = Vec::new();
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&data);
    out
}

fn write_model(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("tokenizer.json"), tokenizer_json()).unwrap();
    std::fs::write(dir.join("model.safetensors"), safetensors_bytes()).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"model_type": "model2vec", "normalize": true}"#,
    )
    .unwrap();
}

/// One shared environment for the whole test binary: an isolated semble
/// cache location and a local toy model. Returns the model path.
pub fn test_env() -> &'static str {
    static ENV: OnceLock<String> = OnceLock::new();
    ENV.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("semble-test-env-{}", std::process::id()));
        let model_dir = base.join("model");
        write_model(&model_dir);
        let cache_dir = base.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        // Safety: set before any test touches the cache; tests within one
        // binary share this environment.
        std::env::set_var("SEMBLE_CACHE_LOCATION", &cache_dir);
        model_dir.to_string_lossy().into_owned()
    })
}

/// Initialise a git repo with deterministic author info.
pub fn git(root: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("git not available");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn write_file(root: &Path, rel: &str, content: &str) -> PathBuf {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path
}
