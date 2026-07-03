# Differences from upstream Semble

trouve is a Rust port of [MinishLab/semble](https://github.com/MinishLab/semble).
The retrieval behaviour — what a query returns — is a faithful port, verified
by a parity harness (`tests/parity/run_parity.py`) and the upstream annotated
quality benchmark (mean NDCG@10 within 0.0002, see [BENCHMARKS.md](BENCHMARKS.md)).
The indexing and caching architecture underneath is a redesign. This document
lists every deliberate difference and why it exists.

## Module map

Where upstream behaviour lives in this codebase:

| Upstream (Python) | trouve (Rust) | Fidelity |
| --- | --- | --- |
| `chunking/core.py`, `chunking/chunking.py` | `src/chunk.rs` | Identical chunk boundaries (parity-verified) |
| `tokens.py` | `src/tokens.rs` | Identical tokens (parity-verified) |
| `index/sparse.py` + `bm25s` dependency | `src/bm25.rs` | Scores within 1e-4 (parity-verified) |
| `index/dense.py` + `model2vec` dependency | `src/dense.rs`, `src/embed.rs` | Bit-identical embeddings per text |
| `ranking/boosting.py`, `penalties.py`, `weighting.py` | `src/ranking.rs` | Port |
| `search.py` | `src/search.rs` | Port (same RRF fusion) |
| `index/file_walker.py` | `src/walker.rs` | Port (gitignore semantics) |
| `stats.py` | `src/stats.rs` | Port |
| `cli.py`, `mcp.py`, `installer/` | `src/cli.rs`, `src/mcp.rs`, `src/installer.rs` | Port, plus a `stats` subcommand |
| `cache.py`, `index/index.py`, `index/create.py`, `index/files.py` | `src/store.rs`, `src/manifest.rs`, `src/snapshot.rs`, `src/index.rs` | **Redesigned** (everything below) |

## Architectural differences

### 1. Content-addressed chunk store instead of an all-or-nothing index cache

Upstream pickles one cached index per path; if anything changed, the whole
repository is re-chunked and re-embedded. trouve stores every per-file
artifact (chunks, embedding rows, BM25 token lists) keyed by a hash of the
file's *content* plus the indexing parameters (`src/store.rs`).

**Why:** incremental cost. Editing one file re-embeds one file; on
kubernetes/kubernetes that is 0.86 s instead of ~3 minutes. This is the
change that motivated the port — everything else in this section follows
from it.

### 2. Git-aware manifests: blob OIDs as content keys

The list of files to index is built from `git ls-files -s`, using each blob's
OID as its content key; only dirty/untracked files (from `git status`) are
read and hashed (BLAKE3). Non-git roots fall back to a walk with an
mtime+size fast path (`src/manifest.rs`).

**Why:** identifying clean content requires zero file reads — git already
hashed it. Building the 30k-file kubernetes manifest takes ~120 ms.

### 3. One store per repository, shared across branches and worktrees

The store is keyed by the canonicalized git *common* directory, so every
branch and worktree of a repository shares one store. Path-dependent data
(chunk `file_path`, BM25 path-enrichment tokens) is injected at assembly
time from the manifest rather than baked into stored entries.

**Why:** branch switches and new worktrees only pay for content the store
has never seen — identical content across 20 branches is stored once.
Upstream would rebuild from scratch per checkout state.

### 4. Memory-mapped snapshots with incremental patching

After every assembly the finished index is written to a single snapshot file
keyed by a manifest hash (`src/snapshot.rs`). An identical manifest is a pure
mmap load with embeddings and BM25 postings used zero-copy; a changed
manifest patches the newest compatible snapshot, splicing unchanged rows out
of the old mapping. BM25 postings store raw term frequencies (corpus
statistics are applied at query time) precisely so a patched index is
bit-equal to a full rebuild.

**Why:** warm-start latency. A fully warm kubernetes query is 0.55 s
end-to-end, and RAM is bounded by what the OS pages in rather than the full
index. Upstream deserializes its whole pickle on every process start.

### 5. Bounded cache: snapshot pruning + mark-and-sweep GC

Only the 4 newest snapshots are kept per store, and a daily mark-and-sweep
pass deletes store entries not referenced by any kept snapshot (one-hour
grace period for concurrent builds).

**Why:** the content-addressed store would otherwise grow without bound as
branches churn. Upstream has no equivalent problem (one cache per path,
overwritten in place) — this is the cost of difference #1, paid back here.
Sweeping is always safe: the store is a cache, and a miss just recomputes.

### 6. In-house model2vec engine instead of the `model2vec` library

Same model (`potion-code-16M`), same output, different plumbing: the
embedding table is memory-mapped from safetensors instead of copied, and
pure-ASCII text (virtually all source code) goes through a byte-level
WordPiece scanner with a sharded per-word memo instead of the HF
`tokenizers` pipeline. Non-ASCII text falls back to the exact HF pipeline.
`tests/embed_parity.rs` verifies bit-identical output against `model2vec-rs`.

**Why:** throughput. Model load drops ~100 ms → ~55 ms and the kubernetes
embed phase drops 4.6 s → 0.6 s, because code is repetitive and the word
memo hit rate is very high.

### 7. No padding: embeddings are batch-independent

The one deliberate *semantic* difference. Upstream model2vec pads batches
and mean-pools the `[PAD]` rows, so a text's vector varies with the batch it
was embedded in. trouve never pads; every text embeds as a batch of one.

**Why:** correctness requirement of the content-addressed store — a cached
embedding must not depend on which other files happened to miss the cache in
the same build. Retrieval quality is unchanged (see BENCHMARKS.md).

### 8. Everything is parallel

File hashing, store lookups, parsing, chunking, tokenizing, embedding, and
BM25 construction all run across cores via rayon. BM25 tokens live in flat
arenas (one byte blob + offset arrays) end to end instead of per-token heap
strings.

**Why:** Python's GIL keeps upstream effectively single-threaded; a Rust
port that didn't use the cores would leave most of the win on the table. The
flat token representation exists because allocation traffic, not compute,
dominated the cold path at this speed.

### 9. MCP server details

Same tool surface, plus: the in-process index cache holds up to 10 indexes
(LRU by canonicalized repo path) and re-validates local paths after a
cooldown proportional to build time, which the fast incremental rebuild makes
cheap. The CLI adds a `stats` subcommand (index size, cache hit rate).

**Why:** the revalidation policy leans on rebuilds being sub-second; upstream
cannot re-validate cheaply, so it caches more conservatively.

## What did *not* change

- The embedding model (`potion-code-16M`) and its semantics.
- Chunking: same tree-sitter merge algorithm, same 750-byte target, same
  line-based fallback, identical boundaries.
- BM25: same Lucene variant (k1=1.5, b=0.75), same identifier tokenization,
  same path/filename enrichment.
- Hybrid fusion: same RRF (k=60), same alpha resolution.
- Reranking: same boosts and penalties, ported constant-for-constant.
- CLI/MCP surface, cache-location resolution, savings tracking, agent
  installer flow.
- CPU-only execution (static embeddings are table lookups + mean pooling;
  there is no neural forward pass to put on a GPU).

## Backporting upstream changes

How hard an upstream change is to carry over depends on which layer it
touches:

- **Retrieval logic** (chunking, tokenization, ranking constants, fusion,
  new languages): straightforward. The module map above is close to 1:1, the
  Rust files cite their Python sources, and the parity harness
  (`tests/parity/run_parity.py`, driven against `reference/semble/`) verifies
  the port empirically. Translate the diff, run parity.
- **Model changes** (new potion model, different dimensions): configuration,
  not code — the engine reads any model2vec safetensors layout.
- **CLI/MCP surface changes** (new tools, new flags): mechanical ports.
- **Cache or index internals** (`cache.py`, `index/index.py`): do not apply.
  That layer was deliberately replaced; an upstream fix there is either
  already irrelevant or needs a from-scratch design against the store (as
  the GC did).

Run `./scripts/fetch-reference.sh` to pin the upstream checkout that parity
runs against.
