# Benchmarks

Rust `semble` vs upstream Python [`MinishLab/semble`](https://github.com/MinishLab/semble),
measured with [hyperfine](https://github.com/sharkdp/hyperfine) (end-to-end) and
[criterion](https://github.com/bheisler/criterion.rs) (micro-benchmarks).

Test machine: AMD Ryzen 9 5950X (16C/32T), 64 GB RAM, Linux, NVMe.
Model: `minishlab/potion-code-16M`. Every timing below is a full CLI invocation
(`semble search <query> <repo> -k 5 --max-snippet-lines 0`), including process
startup and model load.

## Small repo: pallets/flask (249 tracked files)

`benchmarks/run_benchmarks.sh`, 3 runs (9 for warm), mean ± σ:

| Scenario | Rust | Python | Speedup |
| --- | --- | --- | --- |
| Cold index + query | 322 ms ± 16 ms | 2.99 s ± 2.98 s | 9.3x |
| Warm query (cached) | 137 ms ± 12 ms | 721 ms ± 26 ms | 5.3x |
| Incremental (1 file touched) | 156 ms ± 12 ms | 1.31 s ± 0.04 s | 8.4x |
| Branch switch (3.1.0 <-> 3.0.0) | 161 ms ± 4 ms | 1.33 s ± 0.02 s | 8.3x |

On a small repo Python's incremental and branch-switch times are dominated by a
full re-index (its cache is all-or-nothing), while the Rust store recomputes
only the files whose git blob OIDs changed.

## Large repo: kubernetes/kubernetes (30,563 tracked files)

Single timed runs (the effect sizes dwarf run-to-run noise):

| Scenario | Rust | Python | Speedup |
| --- | --- | --- | --- |
| Cold index + query | 17.9 s | 2 m 59 s | 10.0x |
| Warm query (cached) | 8.7 s | 7.2 s | ~1x |
| Incremental (1 file touched) | 8.9 s | 3 m 2 s | 20.5x |
| Worktree first query (flask, other branch) | 0.34 s | n/a (full rebuild) | — |

This is the headline result: touching a single file in a 30k-file repo costs
Python a full ~3-minute rebuild, while Rust recomputes one file and reassembles
the index in under 9 seconds. The same content-addressed store is shared across
branches and worktrees, so a new worktree of an already-indexed branch is a pure
cache hit.

Warm-query time on huge repos is dominated by loading and reassembling ~200k
chunk entries from the store on both sides, so the two implementations converge;
the Rust MCP server additionally keeps hot indexes in an in-process LRU, so
repeated agent queries skip that load entirely.

## Retrieval quality (NDCG@10)

`benchmarks/run_quality.py` runs the upstream annotated benchmark tasks against
both implementations (repos pinned by `reference/semble/benchmarks/sync_repos.py`):

| Repo | Rust NDCG@10 | Python NDCG@10 |
| --- | --- | --- |
| chi | 0.8457 | 0.8457 |
| click | 1.0000 | 1.0000 |
| flask | 0.8561 | 0.8554 |
| redux | 0.9171 | 0.9171 |
| requests | 0.9674 | 0.9674 |
| **mean** | **0.9173** | **0.9171** |

Delta +0.0001 — well within the 1% parity target. The parity harness
(`tests/parity/run_parity.py`) additionally verifies identical chunk boundaries
(52/52 files), identical BM25 tokenization (34/34 samples), BM25 scores within
1e-4 (4/4 queries), and 98% mean top-5 search overlap with 10/10 top-1 agreement.

## Micro-benchmarks (criterion)

`cargo bench`:

| Benchmark | Time |
| --- | --- |
| Chunk 200-function python file (tree-sitter) | 2.3 ms |
| BM25 build, 5k docs | 4.9 ms |
| BM25 query, 5k docs | 6.5 µs |
| Dense query, 20k x 256 vectors | 1.03 ms |

HTML reports land in `target/criterion/`.

## Reproducing

```bash
# one-time setup
cargo build --release
scripts/fetch-reference.sh
python3 -m venv .venv && .venv/bin/pip install './reference/semble[mcp]'
cargo install hyperfine

# speed suite (flask by default; pass any git repo dir)
benchmarks/run_benchmarks.sh

# quality suite
(cd reference/semble && PYTHONPATH=. ../../.venv/bin/python benchmarks/sync_repos.py \
    --repo flask --repo click --repo requests --repo chi --repo redux)
.venv/bin/python benchmarks/run_quality.py --binary target/release/semble \
    --repo flask --repo click --repo requests --repo chi --repo redux

# micro-benchmarks
cargo bench
```

Set `SEMBLE_MODEL_NAME=/path/to/local/model` to run offline with a pre-downloaded
copy of `potion-code-16M` (`config.json`, `tokenizer.json`, `model.safetensors`).
