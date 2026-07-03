# semble-rs

Fast and accurate code search for agents — a Rust port of
[MinishLab/semble](https://github.com/MinishLab/semble) with an incremental,
branch- and worktree-aware index and a fully multithreaded pipeline.

## Why a port?

Upstream Semble is excellent but its cache is all-or-nothing: touch one file
and the whole repository is re-chunked and re-embedded. On a 20,000+ file
codebase that means minutes per rebuild. semble-rs replaces the cached-index
model with a **content-addressed chunk store**:

- Every per-file artifact (chunks, embedding rows, BM25 token lists) is keyed
  by content hash — a git blob OID for clean files (no file reads at all) or a
  BLAKE3 hash for dirty/untracked files.
- **Incremental**: editing one file re-embeds one file. Everything else is a
  store hit.
- **Branch/worktree aware**: the store is keyed by the git *common* directory,
  so all branches and worktrees of a repository share one store. Switching
  branches only pays for content the store has never seen.
- **Multithreaded**: file hashing, parsing, chunking, tokenizing, and
  embedding all run in parallel via rayon; BM25 corpus statistics are
  recomputed at assembly time (cheap relative to embedding).

Retrieval behaviour is a faithful port: the same `potion-code-16M` model2vec
embeddings (via [model2vec-rs](https://github.com/MinishLab/model2vec-rs)),
the same BM25 (Lucene variant) scoring, the same RRF hybrid fusion, and the
same code-tuned reranking heuristics (symbol-definition boosts, file-stem
boosts, multi-chunk coherence, test/example/compat path penalties, per-file
saturation decay).

Assembled indexes are also persisted as memory-mapped snapshots: a warm query
loads embeddings and BM25 postings zero-copy, and an incremental build patches
the previous snapshot — splicing unchanged rows out of the old mapping — so
its cost is proportional to the edit, not the repository.

Measured results ([BENCHMARKS.md](BENCHMARKS.md)) on kubernetes/kubernetes
(30k files): cold indexing drops from ~3 minutes to 9.9 s (18x), an
incremental reindex after touching one file from ~3 minutes to 0.87 s (200x+),
and a fully warm query from ~7 s to 0.54 s (13x). Retrieval quality is
identical — mean NDCG@10 matches upstream to within 0.0002 on the upstream
annotated benchmark, with identical chunk boundaries and BM25 scores.

## Install

```bash
cargo install semble
# or download a release binary from GitHub Releases
```

## Usage

```bash
semble search "authentication flow" ./my-project --max-snippet-lines 10
semble search "deployment guide" ./my-project --content docs
semble find-related src/auth.py 42 ./my-project
semble stats ./my-project        # index + cache-hit stats
semble savings                   # token savings report
semble clear all                 # wipe stores + savings
semble install                   # configure MCP/instructions across agents
semble                           # run as an MCP stdio server
```

`--content` selects what to index: `code` (default), `docs`, `config`, or
`all`.

## Chunking

Tree-sitter grammars are compiled in for ~28 mainstream languages (Rust,
Python, JS/TS/TSX, Java, C, C++, C#, Go, Ruby, PHP, Swift, Kotlin, Scala,
Haskell, OCaml, Elixir, Lua, Zig, Bash, HTML, CSS, JSON, YAML, TOML, Markdown,
…). Files in any other supported language fall back to line-based chunking
with the same target chunk length, so everything remains searchable.

## Cache location

Resolved in order: `SEMBLE_CACHE_LOCATION` (absolute path), then the platform
cache dir (`~/.cache/semble` on Linux). Set `SEMBLE_MODEL_NAME` to override
the embedding model.

## Development

```bash
cargo test                        # unit + integration tests (offline)
SEMBLE_E2E=1 cargo test -- --ignored   # end-to-end tests (downloads the model)
./scripts/fetch-reference.sh      # clone upstream Python semble into reference/
python3 tests/parity/run_parity.py --binary target/release/semble
./benchmarks/run_benchmarks.sh    # hyperfine comparison vs Python semble
```

## License

MIT, same as upstream. Portions derived from
[MinishLab/semble](https://github.com/MinishLab/semble).
