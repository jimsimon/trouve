//! Integration tests for the content-addressed store: incremental edits,
//! branch switches, shared worktree caches, and the non-git fallback.
//!
//! These run offline against a tiny deterministic local model (see
//! `tests/common/mod.rs`).

mod common;

use common::{git, test_env, write_file};
use semble::index::SembleIndex;
use semble::types::ContentType;

const CODE: &[ContentType] = &[ContentType::Code];

fn sample_files(root: &std::path::Path) {
    write_file(
        root,
        "src/auth.py",
        "def authenticate(user, password):\n    session = login(user, password)\n    return session\n",
    );
    write_file(
        root,
        "src/db.py",
        "def connect(config):\n    connection = database(config)\n    return connection\n",
    );
    write_file(
        root,
        "src/storage.py",
        "def save(model, path):\n    write(path, model)\n\n\ndef load(path):\n    return read(path)\n",
    );
}

#[test]
fn non_git_incremental_reuses_cache() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sample_files(root);

    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_total, 3);
    assert_eq!(index.build_stats.files_computed, 3);
    assert_eq!(index.build_stats.files_from_store, 0);

    // Second build: everything comes from the store.
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_from_store, 3);
    assert_eq!(index.build_stats.files_computed, 0);

    // Touch one file: exactly one file recomputed.
    write_file(
        root,
        "src/auth.py",
        "def authenticate(user, password, token):\n    session = login(user, password, token)\n    return session\n",
    );
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_from_store, 2);
    assert_eq!(index.build_stats.files_computed, 1);

    // New file: one more computation, existing entries reused.
    write_file(
        root,
        "src/new.py",
        "def process(job):\n    return run(job)\n",
    );
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_total, 4);
    assert_eq!(index.build_stats.files_computed, 1);

    // Deleted file disappears from the index without recomputation.
    std::fs::remove_file(root.join("src/db.py")).unwrap();
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_total, 3);
    assert_eq!(index.build_stats.files_computed, 0);
    assert!(!index.chunks.iter().any(|c| c.file_path == "src/db.py"));
}

#[test]
fn git_repo_uses_blob_oids_and_shares_across_branches() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    sample_files(root);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "init"]);

    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_computed, 3);

    // Branch with one changed file.
    git(root, &["checkout", "-b", "feature"]);
    write_file(
        root,
        "src/auth.py",
        "def authenticate(user, token):\n    return session(token)\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "change auth"]);

    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_computed, 1, "only the edited file");
    assert_eq!(index.build_stats.files_from_store, 2);

    // Switching back to main: everything is already in the shared store.
    git(root, &["checkout", "main"]);
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_computed, 0, "branch switch is free");
    assert_eq!(index.build_stats.files_from_store, 3);
}

#[test]
fn worktrees_share_the_store() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let main_root = dir.path().join("main");
    std::fs::create_dir(&main_root).unwrap();
    git(&main_root, &["init", "-b", "main"]);
    sample_files(&main_root);
    git(&main_root, &["add", "."]);
    git(&main_root, &["commit", "-m", "init"]);

    let index = SembleIndex::from_path(&main_root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_computed, 3);

    // A second worktree of the same repo pays nothing.
    let wt_root = dir.path().join("wt");
    git(
        &main_root,
        &[
            "worktree",
            "add",
            "-b",
            "wt-branch",
            wt_root.to_str().unwrap(),
        ],
    );
    let index = SembleIndex::from_path(&wt_root, CODE, Some(model)).unwrap();
    assert_eq!(
        index.build_stats.files_computed, 0,
        "worktree shares the store"
    );
    assert_eq!(index.build_stats.files_from_store, 3);
}

#[test]
fn dirty_files_are_hashed_and_cached() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    sample_files(root);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "init"]);
    let _ = SembleIndex::from_path(root, CODE, Some(model)).unwrap();

    // Uncommitted modification: recomputed once, then cached by content hash.
    write_file(root, "src/db.py", "def connect():\n    return database()\n");
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_computed, 1);
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(index.build_stats.files_computed, 0);

    // Untracked file gets indexed too.
    write_file(root, "src/untracked.py", "def helper():\n    return true\n");
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert!(index
        .chunks
        .iter()
        .any(|c| c.file_path == "src/untracked.py"));
}

#[test]
fn search_and_find_related_work_end_to_end() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sample_files(root);

    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    let results = index.search(
        "authenticate user password",
        3,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(!results.is_empty());
    assert_eq!(results[0].chunk.file_path, "src/auth.py");

    // BM25 exact-identifier lookup finds the definition.
    let results = index.search("connect", 3, None, None, None, None, None);
    assert!(!results.is_empty());
    assert_eq!(results[0].chunk.file_path, "src/db.py");

    // find_related returns other chunks, not the seed itself.
    let seed = index
        .chunks
        .iter()
        .find(|c| c.file_path == "src/auth.py")
        .unwrap()
        .clone();
    let related = index.find_related(&seed, 3, None);
    assert!(!related.is_empty());
    assert!(related.iter().all(|r| r.chunk != seed));
}

#[test]
fn stats_reflect_index_contents() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sample_files(root);
    let index = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    let stats = index.stats();
    assert_eq!(stats.indexed_files, 3);
    assert!(stats.total_chunks >= 3);
    assert_eq!(stats.languages.get("python"), Some(&stats.total_chunks));
}

#[test]
fn snapshot_fast_path_returns_identical_results() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    sample_files(root);

    // Cold build assembles from scratch and writes a snapshot.
    let cold = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    let cold_results = cold.search("save model to path", 3, None, None, None, None, None);

    // Snapshots live under this repo's store dir (identity = canonical path).
    let identity = root.canonicalize().unwrap().to_string_lossy().into_owned();
    let snapshots_dir = semble::store::ChunkStore::open(&identity)
        .unwrap()
        .root()
        .join("snapshots");
    let snap_count = |dir: &std::path::Path| -> usize {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "snap"))
                    .count()
            })
            .unwrap_or(0)
    };
    assert_eq!(
        snap_count(&snapshots_dir),
        1,
        "cold build writes one snapshot"
    );

    // Warm build loads the snapshot (mmap) and must rank identically.
    let warm = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(warm.build_stats.files_computed, 0);
    assert_eq!(warm.chunks, cold.chunks);
    let warm_results = warm.search("save model to path", 3, None, None, None, None, None);
    assert_eq!(warm_results.len(), cold_results.len());
    for (w, c) in warm_results.iter().zip(&cold_results) {
        assert_eq!(w.chunk, c.chunk);
        assert!((w.score - c.score).abs() < 1e-9);
    }

    // An edit invalidates the manifest hash: new snapshot, still-correct search.
    write_file(root, "src/db.py", "def connect():\n    return database()\n");
    let edited = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(edited.build_stats.files_computed, 1);
    assert_eq!(
        snap_count(&snapshots_dir),
        2,
        "edit produces a second snapshot"
    );
    let edited_again = SembleIndex::from_path(root, CODE, Some(model)).unwrap();
    assert_eq!(edited_again.chunks, edited.chunks);
}

#[test]
fn empty_dir_errors() {
    let model = test_env();
    let dir = tempfile::tempdir().unwrap();
    let err = SembleIndex::from_path(dir.path(), CODE, Some(model));
    assert!(err.is_err());
}
