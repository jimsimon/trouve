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
| Cold index + query | 93 ms ± 8 ms | 1.26 s ± 0.03 s | 13.5x |
| Warm query (cached) | 73 ms ± 5 ms | 764 ms ± 25 ms | 10.5x |
| Incremental (1 file touched) | 77 ms ± 5 ms | 1.27 s ± 0.05 s | 16.4x |
| Branch switch (3.1.0 <-> 3.0.0) | 79 ms ± 8 ms | 1.26 s ± 0.01 s | 16.0x |

On a small repo Python's incremental and branch-switch times are dominated by a
full re-index (its cache is all-or-nothing), while the Rust store recomputes
only the files whose git blob OIDs changed.

## Large repo: kubernetes/kubernetes (30,563 tracked files)

Single timed runs (the effect sizes dwarf run-to-run noise):

| Scenario | Rust | Python | Speedup |
| --- | --- | --- | --- |
| Cold index + query | 3.3 s | 2 m 59 s | 54x |
| Warm query (snapshot mmap) | 0.55 s | 7.2 s | 13.1x |
| Incremental (1 file touched) | 0.86 s | 3 m 2 s | 212x |
| Worktree first query (flask, other branch) | 0.34 s | n/a (full rebuild) | — |

This is the headline result: touching a single file in a 30k-file repo costs
Python a full ~3-minute rebuild, while Rust patches the previous snapshot in
under a second — the cost is proportional to the edit, not the repository.
The same content-addressed store is shared across branches and worktrees, so
a new worktree of an already-indexed branch is a pure cache hit.

After every assembly the finished index is written to a single snapshot file
keyed by a hash of the manifest, which enables two fast paths:

- **Warm query** (nothing changed): the snapshot is memory-mapped and
  embeddings/BM25 postings are used zero-copy straight out of the mapping —
  0.54 s end-to-end on kubernetes, most of it model load and `git status`.
- **Incremental** (a few files changed): the newest snapshot is diffed
  against the new manifest; unchanged rows are spliced out of the old
  mapping and only changed files are re-chunked/re-embedded. BM25 postings
  store raw term frequencies (corpus statistics are applied at query time),
  so the patched index is exactly what a full rebuild would produce.

The MCP server additionally keeps hot indexes in an in-process LRU, so
repeated agent queries skip even the snapshot load.

Embedding runs on an in-house model2vec engine rather than `model2vec-rs`:
the embedding table is memory-mapped from safetensors (model load drops from
~100 ms to ~55 ms), pure-ASCII text goes through a byte-level
BertNormalizer/BertPreTokenizer/WordPiece scanner with a sharded word→ids
memo (code is repetitive, so hit rates are high), and pooling gathers rows
straight out of the mapping. This cut the kubernetes embed phase from 4.6 s
to 0.6 s. Non-ASCII text falls back to the exact HF `tokenizers` pipeline,
and `tests/embed_parity.rs` verifies bit-identical output against
`model2vec-rs` (samples + property tests + the real potion-code-16M model).
One deliberate difference from upstream: texts are never padded, so
embeddings do not depend on batch composition (upstream mean-pools `[PAD]`
rows, making its vectors vary with batching); retrieval quality is unchanged.

The rest of the cold path is engineered around allocation traffic: BM25
tokens live in flat arenas (`TokenDocs`: one byte blob + offset arrays
instead of tens of millions of heap `String`s) end to end — in store
entries, through assembly, and into the BM25 builder, which hashes byte
slices and sorts `(term, doc, tf)` triples in parallel. Chunk line numbers
are computed with a single forward newline scan per file, and fresh
embeddings are written straight into one flat buffer.

Cold-path phase breakdown on kubernetes (`SEMBLE_TIMING=1`): chunk+tokenize
1.3 s (mostly tree-sitter parsing), embedding 0.5 s, BM25 build 0.46 s,
everything else under 0.25 s each.

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
