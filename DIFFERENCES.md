# Differences from upstream Semble

trouve is a Rust port of [MinishLab/semble](https://github.com/MinishLab/semble).
The retrieval behaviour — what a query returns — is a faithful port, verified
by a parity harness (`tests/parity/run_parity.py`) and the upstream annotated
quality benchmark (mean NDCG@10 within 0.0002, see [BENCHMARKS.md](BENCHMARKS.md)).
The indexing and caching architecture underneath is a redesign. This document
lists every deliberate difference and why it exists.

Last audited against Semble v0.5.5 (`9218491`, 2026-08-12). Benchmark figures
below refer to the v0.4.1 comparison recorded in [BENCHMARKS.md](BENCHMARKS.md).

## Module map

Where upstream behaviour lives in this codebase. Paths below are relative to
the search crate root, `crates/trouve-search/` (e.g. `src/chunk.rs` is
`crates/trouve-search/src/chunk.rs`), since the crate moved into the
monorepo workspace.

| Upstream (Python) | trouve (Rust) | Fidelity |
| --- | --- | --- |
| `chunking/core.py`, `chunking/chunking.py` | `src/chunk.rs` | Identical chunk boundaries (parity-verified) |
| `tokens.py` | `src/tokens.rs` | Identical tokens (parity-verified) |
| `index/bm25.py` | `src/bm25.rs` | Scores within 1e-4 (parity-verified) |
| `index/dense.py` + `model2vec` dependency | `src/dense.rs`, `src/embed.rs` | Bit-identical embeddings per text |
| `ranking/boosting.py`, `penalties.py`, `weighting.py` | `src/ranking.rs` | Port |
| `search.py` | `src/search.rs` | Port (same RRF fusion) |
| `index/file_walker.py` | `src/walker.rs` | Port (gitignore semantics) |
| `stats.py` | `src/stats.rs` | Port |
| `cli.py`, `mcp.py` | `src/cli.rs`, `src/mcp.rs` | Port; orphan cleanup is adapted to the store design, plus `stats` |
| `installer/` | — | **Dropped**: manual per-agent setup is documented in [INSTALL.md](INSTALL.md) instead |
| `cache.py`, `index/index.py`, `index/create.py`, `index/files.py` | `src/store.rs`, `src/manifest.rs`, `src/snapshot.rs`, `src/index.rs` | **Redesigned** (everything below) |

## Architectural differences

### 1. Content-addressed chunk store instead of a checkout-local index cache

Since v0.5.2, upstream records file mtimes and can reuse unchanged chunks and
vectors while rebuilding one checkout. trouve stores every per-file artifact
(chunks, embedding rows, BM25 token lists) keyed by a hash of the file's
*content* plus the indexing parameters (`src/store.rs`).

**Why:** exact reuse across edits, branch switches, and worktrees. Editing one
file re-embeds one file, while unchanged content is reusable even when its
mtime or checkout path changes. The recorded v0.4.1 comparison measured
0.86 s instead of ~3 minutes on kubernetes/kubernetes; upstream v0.5.2 is not
represented by that historical timing.

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
Upstream's reusable state remains tied to one checkout path and its file
metadata.

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
index. Upstream loads its persisted index structures into process memory on
every process start.

### 5. Bounded cache: snapshot pruning + mark-and-sweep GC

Only the 4 newest snapshots are kept per store, and a daily mark-and-sweep
pass deletes store entries not referenced by any kept snapshot (one-hour
grace period for concurrent builds). `trouve-search clear orphans`
conservatively removes whole stores whose recorded repository identity no
longer exists, while skipping legacy, corrupt, mismatched, symlinked, or
otherwise unverifiable stores.

**Why:** the content-addressed store would otherwise grow without bound as
branches churn. Upstream's orphan cleanup removes stale checkout caches;
trouve additionally needs entry-level GC inside each shared store. Sweeping is
always safe: the store is a cache, and a miss just recomputes.

### 6. In-house model2vec engine instead of the `model2vec` library

Same model (`potion-code-16M-v2`), same output, different plumbing: the
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

Like upstream, both MCP tools accept a per-call
`content=code|docs|config|all` override, with the server configuration used
when it is omitted. trouve's in-process index cache holds up to 10 indexes
(LRU by canonicalized repo path plus normalized content selection) and
re-validates repos after a cooldown proportional to build time, which the fast
incremental rebuild makes cheap. The CLI adds a `stats` subcommand (index
size, cache hit rate).

**Why:** git/content manifests and snapshot fast paths make repeated
validation inexpensive, while the extra command makes cache behavior visible.

### 10. No remote repository support

Upstream shallow-clones a git URL into a temp directory when given one.
trouve indexes local directories only and rejects git URLs with an error
(trouve 1.1–2.0 had a persistent clone cache; it was removed).

**Why:** managing clones of other people's repositories — credentials,
freshness, eviction, concurrent access — is out of scope for a search tool.
Clone the repository yourself and pass the local path; indexing a local
clone costs the same.

### 11. Shared MCP daemon across sessions

On unix, the bare `trouve-search` MCP entry is a thin proxy: the first
session starts a detached daemon (`trouve-search daemon`) on a unix socket
under the cache folder, and every session with the same configuration
(binary version, content types, and embedding model) forwards its JSON-RPC
lines to it; a session with a different configuration gets its own daemon.
The daemon idles out after 15 minutes with no sessions; a proxy that
cannot reach it (or loses it mid-session) falls back to serving in-process.
Upstream runs one full server per session (see ADR 0007).

**Why:** each server holds up to 10 full in-memory indexes plus the
embedding model; across many concurrent agent sessions that multiplies RAM
for identical state. One daemon per matching configuration bounds it at a
single instance.

## What did *not* change

- The embedding model identifier (`potion-code-16M-v2`) and per-text output.
  trouve embeds every text as a batch of one rather than using upstream batch
  padding, as described above.
- Chunking: same tree-sitter merge algorithm, same 750-byte target, same
  line-based fallback, identical boundaries where both use a grammar. trouve
  compiles a curated native-grammar subset; other recognized languages use
  the line fallback, while upstream's language pack covers more grammars.
- BM25: same Lucene variant (k1=1.5, b=0.75), same identifier tokenization,
  same path/filename enrichment.
- Hybrid fusion: same RRF (k=60), same alpha resolution.
- Reranking: same boosts and penalties, ported constant-for-constant.
- CLI/MCP surface, cache-location resolution, savings tracking. (User-facing
  names are trouve's own — `.trouveignore`, `TROUVE_CACHE_LOCATION`,
  `TROUVE_MODEL_NAME` — with the semble equivalents still honoured as
  deprecated fallbacks. Upstream's interactive installer is not ported;
  [INSTALL.md](INSTALL.md) documents manual setup.)
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
- **Cache or index internals** (`cache.py`, `index/index.py`): translate the
  invariant, not the implementation. That layer was deliberately replaced;
  an upstream fix may be irrelevant, already covered, or need a fresh design
  against the content store and snapshots.

Run `./scripts/fetch-reference.sh` to check out the audited upstream commit and
print the exact reference used for parity. Set `SEMBLE_REFERENCE_REF=main` for
an explicit rolling comparison against upstream HEAD.
