# trouve

Fast and accurate code search for agents — a Rust port of
[MinishLab/semble](https://github.com/MinishLab/semble) with an incremental,
branch- and worktree-aware index and a fully multithreaded pipeline.

Pronounced **"troov"** (rhymes with *groove*; French /tʁuv/). *Trouver* is
French for "to find" — a nod to upstream's namesake *sembler*, "to seem".

## Why a port?

Upstream Semble is excellent but its cache is all-or-nothing: touch one file
and the whole repository is re-chunked and re-embedded. On a 20,000+ file
codebase that means minutes per rebuild. trouve replaces the cached-index
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
embeddings (via an in-house engine with a memory-mapped embedding table and a
word-caching WordPiece fast path, verified bit-identical to
[model2vec-rs](https://github.com/MinishLab/model2vec-rs) per text),
the same BM25 (Lucene variant) scoring, the same RRF hybrid fusion, and the
same code-tuned reranking heuristics (symbol-definition boosts, file-stem
boosts, multi-chunk coherence, test/example/compat path penalties, per-file
saturation decay).

Assembled indexes are also persisted as memory-mapped snapshots: a warm query
loads embeddings and BM25 postings zero-copy, and an incremental build patches
the previous snapshot — splicing unchanged rows out of the old mapping — so
its cost is proportional to the edit, not the repository.

Measured results ([BENCHMARKS.md](BENCHMARKS.md)) on kubernetes/kubernetes
(30k files): cold indexing drops from ~3 minutes to 3.3 s (54x), an
incremental reindex after touching one file from ~3 minutes to 0.86 s (200x+),
and a fully warm query from ~7 s to 0.55 s (13x). Retrieval quality is
identical — mean NDCG@10 matches upstream to within 0.0002 on the upstream
annotated benchmark, with identical chunk boundaries and BM25 scores.

Everything runs on CPU, like upstream: model2vec static embeddings are table
lookups plus mean pooling, so there is no neural forward pass to accelerate.
Disk and memory footprints are documented in
[BENCHMARKS.md](BENCHMARKS.md#resource-usage); a complete list of differences
from upstream (and the reasoning behind each) is in
[DIFFERENCES.md](DIFFERENCES.md).

## Install

```bash
cargo install trouve
# or download a release binary from GitHub Releases
```

## Usage

```bash
trouve search "authentication flow" ./my-project --max-snippet-lines 10
trouve search "deployment guide" ./my-project --content docs
trouve find-related src/auth.py 42 ./my-project
trouve stats ./my-project        # index + cache-hit stats
trouve savings                   # token savings report
trouve clear all                 # wipe stores + savings
trouve install                   # configure MCP/instructions across agents
trouve                           # run as an MCP stdio server
```

`--content` selects what to index: `code` (default), `docs`, `config`, or
`all`.

For [Claude Code](https://code.claude.com) there is also a
[plugin](plugins/claude) bundling the MCP server, a `trouve-search`
sub-agent, and a workflow skill — installable as one unit:

```
/plugin marketplace add jimsimon/trouve
/plugin install trouve@trouve
```

## Chunking

Tree-sitter grammars are compiled in for ~28 mainstream languages (Rust,
Python, JS/TS/TSX, Java, C, C++, C#, Go, Ruby, PHP, Swift, Kotlin, Scala,
Haskell, OCaml, Elixir, Lua, Zig, Bash, HTML, CSS, JSON, YAML, TOML, Markdown,
…). Files in any other supported language fall back to line-based chunking
with the same target chunk length, so everything remains searchable.

## Cache location

Resolved in order: `TROUVE_CACHE_LOCATION` (absolute path), then the platform
cache dir (`~/.cache/trouve` on Linux). Set `TROUVE_MODEL_NAME` to override
the embedding model.

The store garbage-collects itself: after a snapshot write (at most once per
day per store), entries not referenced by any kept snapshot are deleted, with
a one-hour grace period protecting concurrent builds. Deleted entries are
never wrong — the store is a cache, and a miss just recomputes the file.

## Development

```bash
cargo test                        # unit + integration tests (offline)
TROUVE_E2E=1 cargo test -- --ignored   # end-to-end tests (downloads the model)
./scripts/fetch-reference.sh      # clone upstream Python semble into reference/
python3 tests/parity/run_parity.py --binary target/release/trouve
./benchmarks/run_benchmarks.sh    # hyperfine comparison vs Python semble
```

## Acknowledgements

trouve exists because of [Semble](https://github.com/MinishLab/semble) by
Thomas van Dongen and Stephan Tulkens of [MinishLab](https://github.com/MinishLab),
which pioneered the approach: static [Model2Vec](https://github.com/MinishLab/model2vec)
embeddings fused with BM25 and code-aware reranking, fast enough for agents to
use as a native tool. trouve's retrieval behaviour is a faithful port of their
design (see [DIFFERENCES.md](DIFFERENCES.md)), and the
[potion-code-16M](https://huggingface.co/minishlab/potion-code-16M) embedding
model is theirs. If you find trouve useful, star their repo too.

## Citing

If you use trouve in your research, please cite the original Semble project,
per its [citation guide](https://github.com/MinishLab/semble#citing):

```bibtex
@software{minishlab2026semble,
  author       = {{van Dongen}, Thomas and Stephan Tulkens},
  title        = {Semble: Fast and Accurate Code Search for Agents},
  year         = {2026},
  publisher    = {Zenodo},
  doi          = {10.5281/zenodo.19785932},
  url          = {https://github.com/MinishLab/semble},
  license      = {MIT}
}
```

## License

MIT, same as upstream. Portions derived from
[MinishLab/semble](https://github.com/MinishLab/semble).
