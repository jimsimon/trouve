//! Model-backed end-to-end tests.
//!
//! These download the real default embedding model (potion-code-16M) from
//! the Hugging Face Hub, so they are `#[ignore]`d by default and run with:
//!
//! ```bash
//! TROUVE_E2E=1 cargo test -- --ignored
//! ```
//!
//! Without `TROUVE_E2E=1` they skip themselves, so a plain
//! `cargo test -- --ignored` stays offline-safe.

mod common;

use common::write_file;
use trouve::index::TrouveIndex;
use trouve::types::ContentType;

const CODE: &[ContentType] = &[ContentType::Code];

/// Gate on `TROUVE_E2E=1` and isolate the trouve cache (the Hugging Face
/// model cache is left alone so CI can cache the model download across runs).
fn e2e_env() -> Option<&'static str> {
    if std::env::var("TROUVE_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping: set TROUVE_E2E=1 to run model-backed e2e tests");
        return None;
    }
    static ENV: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    Some(ENV.get_or_init(|| {
        // The cache must stay isolated per run (tests assert cold-build
        // stats), but dirs from previous runs are stale: sweep them so
        // repeated local runs don't accumulate garbage in the temp dir.
        let temp = std::env::temp_dir();
        if let Ok(entries) = std::fs::read_dir(&temp) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("trouve-e2e-cache-")
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        let cache = temp.join(format!("trouve-e2e-cache-{}", std::process::id()));
        std::fs::create_dir_all(&cache).unwrap();
        // Safety: set before any test builds an index; e2e tests within this
        // binary share the isolated cache.
        std::env::set_var("TROUVE_CACHE_LOCATION", &cache);
        cache.to_string_lossy().into_owned()
    }))
}

fn sample_project(root: &std::path::Path) {
    write_file(
        root,
        "src/auth.py",
        "def authenticate(username, password):\n    \"\"\"Validate credentials and open a session.\"\"\"\n    session = create_session(username, password)\n    return session\n",
    );
    write_file(
        root,
        "src/db.py",
        "def connect(config):\n    \"\"\"Open a database connection from settings.\"\"\"\n    return Database(config.host, config.port)\n",
    );
    write_file(
        root,
        "src/storage.py",
        "def save_model(model, path):\n    \"\"\"Serialize the model to disk.\"\"\"\n    with open(path, 'wb') as f:\n        f.write(serialize(model))\n",
    );
}

#[test]
#[ignore = "downloads the embedding model; run with TROUVE_E2E=1 cargo test -- --ignored"]
fn e2e_index_search_and_find_related() {
    if e2e_env().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sample_project(root);

    // `None` resolves TROUVE_MODEL_NAME or the default potion-code-16M,
    // exercising the real download / load / tokenize / embed pipeline.
    let index = TrouveIndex::from_path(root, CODE, None).unwrap();
    assert_eq!(index.build_stats.files_total, 3);
    assert_eq!(index.build_stats.files_computed, 3);

    // These assert the expected file appears in the top results rather than
    // pinning the exact top-1: this suite is a pipeline sanity check, and
    // exact ranking is covered by the parity/quality harnesses. A model
    // version bump or platform float differences must not flake the gate.
    let hits = |results: &[trouve::types::SearchResult]| -> Vec<String> {
        results.iter().map(|r| r.chunk.file_path.clone()).collect()
    };
    let results = index.search("validate user credentials", 3, None, None, None, None, None);
    assert!(
        hits(&results).contains(&"src/auth.py".to_string()),
        "semantic query should surface the auth code, got {:?}",
        hits(&results)
    );

    let results = index.search("save_model", 3, None, None, None, None, None);
    assert!(
        hits(&results).contains(&"src/storage.py".to_string()),
        "identifier query should surface the definition, got {:?}",
        hits(&results)
    );

    let seed = index
        .chunks
        .iter()
        .find(|c| c.file_path == "src/storage.py")
        .expect("storage chunk is indexed")
        .clone();
    let related = index.find_related(&seed, 3, None);
    assert!(related.iter().all(|r| r.chunk != seed));
}

#[test]
#[ignore = "downloads the embedding model; run with TROUVE_E2E=1 cargo test -- --ignored"]
fn e2e_warm_rebuild_is_fully_cached_and_identical() {
    if e2e_env().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sample_project(root);

    let cold = TrouveIndex::from_path(root, CODE, None).unwrap();
    let warm = TrouveIndex::from_path(root, CODE, None).unwrap();
    assert_eq!(warm.build_stats.files_computed, 0);
    assert_eq!(warm.chunks, cold.chunks);

    let query = "open a database connection";
    let a = cold.search(query, 3, None, None, None, None, None);
    let b = warm.search(query, 3, None, None, None, None, None);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.chunk, y.chunk);
        assert!((x.score - y.score).abs() < 1e-9);
    }
}
