# Benchmarks

`trouve` vs upstream Python [`MinishLab/semble`](https://github.com/MinishLab/semble),
measured with [hyperfine](https://github.com/sharkdp/hyperfine) (end-to-end) and
[criterion](https://github.com/bheisler/criterion.rs) (micro-benchmarks).

Test machine: AMD Ryzen 9 5950X (16C/32T), 64 GB RAM, Linux, NVMe.
Model: `minishlab/potion-code-16M`. Every timing below is a full CLI invocation
(`trouve search <query> <repo> -k 5 --max-snippet-lines 0`), including process
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

Cold-path phase breakdown on kubernetes (`TROUVE_TIMING=1`): chunk+tokenize
1.3 s (mostly tree-sitter parsing), embedding 0.5 s, BM25 build 0.46 s,
everything else under 0.25 s each.

## Git vs non-git roots

trouve does not require git: a plain folder falls back to a filesystem
manifest (walk + BLAKE3 content hash, with an mtime+size fast path persisted
in the store) instead of the git manifest (`git ls-files` blob OIDs +
`git status`). `benchmarks/run_git_vs_nogit.sh` indexes the same kubernetes
tree both ways — once as the git checkout, once as an identical copy with
`.git` removed. Measured on a 4-vCPU cloud VM (so absolute times are ~2.5x
the 16-core numbers above; the comparison is what matters), warm OS page
cache, hyperfine mean ± σ:

| Scenario | git | non-git | Delta |
| --- | --- | --- | --- |
| Cold index + query (3 runs) | 9.99 s ± 0.80 s | 10.40 s ± 0.60 s | tie (1.04x) |
| Warm query (9 runs) | 548 ms ± 13 ms | 673 ms ± 25 ms | git 1.23x |
| Incremental, 1 file modified (3 runs) | 574 ms ± 21 ms | 661 ms ± 13 ms | git 1.15x |
| Touch-only, mtime bumped (3 runs) | 537 ms ± 7 ms | 681 ms ± 17 ms | git 1.27x |

The whole difference is the manifest phase (`TROUVE_TIMING=1`); everything
downstream — store hits, snapshot patching, query — is identical:

- **Cold**: git manifest 1.10 s (`git ls-files` + `git status` over 30k
  files) vs non-git 0.41 s (parallel walk + hash of every file). Hashing is
  actually *cheaper* than shelling out to git on a warm page cache, and
  either way it is noise against the ~7 s of chunking and embedding.
- **Warm**: git manifest 133 ms vs non-git 271 ms. The non-git side walks
  and stats all ~17.7k indexed files to check the mtime+size fast path; git
  gets the same answer from `git status`. This fixed ~140 ms gap is the
  entire delta in the warm, incremental, and touch-only rows.

Both variants index the same file set (the walker honours `.gitignore`
directly) and return the same results, and the touch-only row confirms the
mtime fast path: a bumped mtime re-hashes one file, hits the store, and pays
only the manifest walk. What a non-git root gives up is the branch/worktree
store sharing (identity is the folder path, not the git common dir) and the
no-read manifest for clean files — on kubernetes scale that costs roughly
90–150 ms per invocation.

## Resource usage

Peak resident memory (`/usr/bin/time -v`, full CLI invocation) and on-disk
cache size, same machine and model as above:

| | flask (249 files) | kubernetes (30,563 files) |
| --- | --- | --- |
| Cold build peak RSS | 92 MB | 2.29 GB |
| Incremental (patch) peak RSS | — | 1.33 GB |
| Warm query peak RSS | 40 MB | 0.74 GB |
| Store entries on disk | 2.4 MB | ~0.7 GB |
| One snapshot on disk | 2.0 MB | 577 MB |
| Snapshot cap (4 kept) | 8 MB | 2.3 GB |

What scales with what:

- **RAM scales with repository size, not branch count.** Each invocation
  loads one branch's index. On the warm path embeddings and BM25 postings are
  read zero-copy from the mmap'd snapshot, so the OS pages them in and out on
  demand; resident memory is mostly the materialized chunk texts plus the
  memory-mapped model (63 MB on disk, shared and paged).
- **Disk scales with unique content, not branch count.** Store entries are
  content-addressed by git blob OID, so all branches and worktrees share one
  copy of identical file content; a feature branch adds only the entries for
  the files it changed. Snapshots are capped at the 4 newest per store, and a
  daily mark-and-sweep pass deletes entries no kept snapshot references
  (60 ms on the kubernetes store), so the store converges to the content
  reachable from recent snapshots instead of growing forever.
- The MCP server keeps up to 10 indexes in its in-process LRU (keyed by
  repository path, so 20 branches of one checkout occupy a single slot).

## CPU scaling

Same kubernetes checkout, thread count pinned with `RAYON_NUM_THREADS`
(16 physical cores / 32 SMT threads; single runs on a warm OS page cache, so
the cold times sit slightly below the first-ever-run headline number):

| Threads | Cold index + query | Incremental (1 file) | Warm query |
| --- | --- | --- | --- |
| 1 | 23.3 s | 0.62 s | 0.44 s |
| 2 | 12.0 s | 0.61 s | 0.41 s |
| 4 | 7.0 s | 0.63 s | 0.42 s |
| 8 | 3.8 s | 0.57 s | 0.39 s |
| 16 | 2.8 s | 0.58 s | 0.41 s |
| 32 | 2.5 s | 0.59 s | 0.40 s |

- **Cold builds scale with physical cores**: 9.2x end-to-end from 1 to 32
  threads — near-linear through 8 threads, tapering into 16 (the parallel
  phases keep scaling but the ~0.3 s serial floor grows in relative weight),
  with SMT adding a final ~10%. Per-phase speedups at 32 threads:
  chunk+tokenize 15.2x (15.6 s → 1.0 s), embedding 10x (4.0 s → 0.4 s),
  BM25 build 5.4x (1.9 s → 0.34 s), store writes 7.9x.
- **The serial floor is ~0.3 s**: `git ls-files`/`git status` manifest
  (~110 ms), model load (~38 ms), snapshot write (~95 ms), and store GC when
  it fires (~36 ms). This is what cold builds converge to as cores increase.
- **Warm and incremental paths don't need cores.** They are latency-bound
  serial work — git status, mmap load, patch splice — and run in ~0.4–0.6 s
  even single-threaded. A laptop gets the same interactive experience as a
  32-thread workstation; extra cores only buy faster first-time indexing.
- Even at 1 thread, the 23.3 s cold build is ~8x faster than upstream's
  ~3 minutes (which also uses one core).

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

## CI regression gating

`.github/workflows/bench.yml` runs two gated suites on every PR and push to
main:

- **micro**: the criterion benchmarks above, gated at 150%.
- **e2e**: full CLI invocations on a pinned flask 3.1.0 checkout
  (`benchmarks/run_ci_bench.sh`) — cold index, warm query, incremental
  reindex, and the non-git warm path — gated at 175% (wall-clock times on
  shared runners are noisier).

Each run is compared against the baseline from the last push to main; a
benchmark that regresses past its threshold fails the job. PRs only compare,
pushes to main append the new results and push, and comparisons use medians
(`benchmarks/to_gha_bench.py`) to shrug off single slow outliers.

Benchmark history is persisted by
[github-action-benchmark](https://github.com/benchmark-action/github-action-benchmark)
on the `gh-pages` branch under `dev/bench/` — full history in `data.js` plus
an interactive chart dashboard in `index.html` (served at
`https://<owner>.github.io/trouve/dev/bench/` once GitHub Pages is enabled
for the `gh-pages` branch).

## Reproducing

```bash
# one-time setup
cargo build --release
scripts/fetch-reference.sh
python3 -m venv .venv && .venv/bin/pip install './reference/semble[mcp]'
cargo install hyperfine

# speed suite (flask by default; pass any git repo dir)
benchmarks/run_benchmarks.sh

# git vs non-git roots (kubernetes by default; pass any git repo dir)
benchmarks/run_git_vs_nogit.sh

# quality suite
(cd reference/semble && PYTHONPATH=. ../../.venv/bin/python benchmarks/sync_repos.py \
    --repo flask --repo click --repo requests --repo chi --repo redux)
.venv/bin/python benchmarks/run_quality.py --binary target/release/trouve \
    --repo flask --repo click --repo requests --repo chi --repo redux

# micro-benchmarks
cargo bench
```

Set `TROUVE_MODEL_NAME=/path/to/local/model` to run offline with a pre-downloaded
copy of `potion-code-16M` (`config.json`, `tokenizer.json`, `model.safetensors`).
