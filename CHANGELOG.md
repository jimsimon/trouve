# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.1] - 2026-07-04

### Added

- **Persistent clone cache**: remote git URLs are cloned once into
  `<cache>/clones` and refreshed with a cheap `git fetch` at most once per
  freshness window (`TROUVE_CLONE_TTL` seconds, default 300) instead of
  re-cloned per query. Clones are lock-protected against concurrent trouve
  processes, evicted after a week idle, and removed by `trouve clear index`.
  A stale clone is served (with a warning) when the remote is unreachable.
- The MCP server now re-validates git URLs after the same cooldown as local
  paths — a revalidation is now a TTL-gated fetch plus an incremental
  rebuild, not a re-clone.
- **`.trouveignore` files**: exclude files from indexing without git-ignoring
  them, replacing upstream's `.sembleignore` (same syntax, same per-directory
  inheritance; `.trouveignore` wins where patterns conflict).
- **`.semble/` directories** are now skipped during walks, alongside
  `.trouve/`, matching upstream's default ignore list.
- **Native OpenCode custom tools**: a standalone custom-tool file
  (`src/agents/opencode-tool.ts`, copied to
  `~/.config/opencode/tools/trouve.ts`) exposes `trouve_search` and
  `trouve_find_related` as native OpenCode tools. An alternative to an MCP
  entry: it avoids the MCP transport, needs no JSON config edits, and
  defaults `repo` to the session worktree.
- **[INSTALL.md](INSTALL.md)**: step-by-step manual setup for every
  integration route — plugins, the OpenCode native tool file, and MCP
  server entries (config file, key, and snippet for 14 agents), plus
  optional `trouve-search` sub-agent files.
- **23 new tree-sitter grammars** (~50 languages total): CMake, D, Dart, Elm,
  ERB/EJS embedded templates, Erlang, Fortran, Gleam, GraphQL, Groovy,
  HCL/Terraform, Julia, Make, Nix, Objective-C, Perl, PowerShell, Protocol
  Buffers, R, Solidity, SQL, Svelte, and XML (incl. DTD). Files in these
  languages now get syntax-aware chunk boundaries instead of the line-based
  fallback.
- **Unified agent plugin** (`plugins/trouve`): one package serving four
  harnesses. As the npm package `trouve-plugin` it exposes `trouve_search`
  and `trouve_find_related` as native tools in OpenCode and Kilo Code,
  backed by a single persistent `trouve` server process per session
  (preserving the in-process index cache, including for remote git URLs).
  The same directory carries the Claude Code plugin bundle (MCP server +
  `trouve-search` sub-agent + workflow skill, installed via the marketplace
  catalog at `.claude-plugin/marketplace.json`) and the Codex plugin bundle
  (MCP server + skill, via `.agents/plugins/marketplace.json`). All
  manifests pass their official validators and ship at the crate version.
- **Session-start index warming**: the OpenCode/Kilo plugin builds or
  refreshes the project index in the background when it loads and
  (throttled) on every `session.idle` event, so the first search never pays
  the index build and later searches absorb the agent's own edits
  (`"warm": false` disables). The Claude Code bundle ships an equivalent
  `SessionStart` hook running `trouve stats` in the background.
- **Version sync tooling**: `scripts/sync_versions.py` keeps every published
  artifact (npm plugin packages, Claude Code and Codex plugin manifests) on
  the exact crate version from `Cargo.toml`, and lint CI fails when anything
  drifts (`--check`). The release workflow refuses tags that don't match the
  crate version and publishes all npm plugin packages at the crate version
  alongside the crates.io publish (skipped until `NPM_TOKEN` is configured).
- **Model-backed end-to-end tests**: `TROUVE_E2E=1 cargo test -- --ignored`
  (already documented in the README and run by CI) now actually runs a small
  e2e suite against the real default model — cold index, semantic and
  identifier queries, `find_related`, and a warm rebuild that recomputes
  nothing.

### Changed

- MSRV raised from 1.87 to 1.89 (std file locking for the clone cache).

### Fixed

- **`.trouveignore` now works in git repositories**: ignore rules were only
  consulted by the directory walker (non-git roots); git repositories build
  their manifest from `git ls-files`/`git status` and skipped them entirely.
  Rules are now applied on top of the git listing — before any hashing I/O —
  for tracked and untracked files alike.
- **MCP protocol violations**: tool failures were returned with
  `isError: false` (clients treated them as successful output) and a
  malformed request with an id but no method got no response at all, hanging
  the client; failures now set `isError: true` and malformed requests get a
  `-32600 Invalid Request` error. `top_k: 0` is rejected as the schema
  advertises, and `max_snippet_lines: null` now means the documented default
  instead of being an undocumented full-chunk escape hatch.
- **Git manifest correctness**: tracked symlinks were keyed by the blob OID
  of the link *target path* while indexing read the target file's content,
  serving stale chunks whenever the target changed; symlinks are now skipped
  like the walker already did. Merge-conflicted (unmerged) paths are treated
  as dirty and indexed from the working tree instead of an arbitrary
  conflict stage.
- **Snapshot compatibility checks**: snapshots now record the store format
  version and chunk length, and the incremental patch path rejects
  mismatches instead of silently splicing rows chunked under different rules
  (snapshot format bumped to v4; old snapshots are rebuilt automatically).
  `save()` also verifies a pre-existing snapshot file's embedded hash and
  rewrites partial or foreign files instead of trusting them forever.
- **Model loading robustness**: corrupt or mismatched model artifacts
  (out-of-range mapping entries, undersized embedding tables, token-id gaps)
  are rejected with a clear error at load time instead of panicking
  mid-index, and a tokenizer failure on one text now embeds it as the zero
  vector with a one-time warning instead of aborting the whole build.
- **Accurate cache statistics**: `files_from_store` no longer counts rows
  spliced zero-copy from a previous snapshot (reported separately as
  `files_from_snapshot`), and `trouve stats` now emits the documented
  `cache_hit_rate`.

### Removed

- **`trouve install` / `trouve uninstall`**: the interactive installer is
  gone. Every integration it configured is now documented as a manual (and
  easily reversible) step in [INSTALL.md](INSTALL.md): plugins for
  OpenCode/Kilo/Claude Code/Codex, the OpenCode native tool file, one MCP
  config entry per agent, and optional sub-agent files. Editing user
  configs programmatically was the installer's main risk (JSONC files had
  to be skipped, TOML edits could clobber user changes); a documented
  one-line config entry per agent is simpler and safer.

### Deprecated

- **`.sembleignore` files**: still honoured, but log a warning and will be
  removed in a future release. Rename to `.trouveignore`.
- **`SEMBLE_CACHE_LOCATION`, `SEMBLE_MODEL_NAME`, `SEMBLE_CLONE_TIMEOUT`**:
  now honoured as fallbacks when the `TROUVE_*` equivalent is unset, but log
  a warning and will be removed in a future release. Use
  `TROUVE_CACHE_LOCATION`, `TROUVE_MODEL_NAME`, and `TROUVE_CLONE_TIMEOUT`.

## [1.0.0] - 2026-07-03

First stable release. trouve is a Rust port of
[MinishLab/semble](https://github.com/MinishLab/semble) — fast, accurate code
search for agents — rebuilt around an incremental, branch- and worktree-aware
index. Retrieval behaviour matches upstream (mean NDCG@10 within 0.0002 on the
upstream annotated benchmark, identical chunk boundaries and BM25 scores).

### Added

- **Faithful retrieval port**: tree-sitter chunking for ~28 languages with
  line-based fallback, `potion-code-16M` model2vec embeddings, BM25 (Lucene
  variant) with identifier tokenization and path enrichment, RRF hybrid
  fusion, and upstream's code-tuned reranking heuristics.
- **Content-addressed chunk store**: per-file artifacts (chunks, embedding
  rows, BM25 token lists) keyed by content hash — git blob OIDs for clean
  files (no file reads), BLAKE3 for dirty/untracked files. Editing one file
  re-embeds one file.
- **Branch- and worktree-aware caching**: one store per repository (keyed by
  the git common directory), shared across all branches and worktrees.
- **Memory-mapped snapshots**: warm queries load embeddings and BM25 postings
  zero-copy; incremental builds patch the newest snapshot so cost is
  proportional to the edit, not the repository.
- **Bounded cache**: snapshot pruning (4 newest kept) plus a daily
  mark-and-sweep GC that deletes store entries unreferenced by any kept
  snapshot, with a one-hour grace period for concurrent builds.
- **In-house model2vec engine**: memory-mapped embedding table, byte-level
  WordPiece fast path with a sharded word memo for ASCII text, bit-identical
  output to `model2vec-rs` per text. Embeddings are batch-independent (no
  `[PAD]` pooling).
- **Fully parallel pipeline**: hashing, parsing, chunking, tokenizing,
  embedding, and BM25 construction run across all cores via rayon, with flat
  token arenas to minimise allocation traffic.
- **CLI**: `search`, `find-related`, `stats`, `savings`, `clear`, `install`,
  `uninstall`; bare `trouve` starts an MCP stdio server with `search` and
  `find_related` tools and an in-process LRU index cache.
- **Agent installer**: MCP server config, instruction blocks, and dedicated
  sub-agents across 14 coding agents (Claude Code, Cursor, Codex, Gemini,
  OpenCode, and more).
- **Test and parity suite**: offline unit/integration tests against a
  deterministic toy model, embedding parity tests against `model2vec-rs`,
  property tests, and a parity harness verifying chunk boundaries, tokens,
  BM25 scores, and search results against the upstream Python implementation.
- **Release automation**: binaries for Linux (glibc and static musl, x64 and
  arm64), macOS (x64 and arm64), and Windows (x64 and arm64) built from
  semantic tags, with SHA-256 checksums.

### Performance

Measured on kubernetes/kubernetes (30,563 tracked files) vs upstream Python
semble ([BENCHMARKS.md](BENCHMARKS.md)):

- Cold index + query: 3.3 s vs ~3 min (54x)
- Incremental reindex (1 file touched): 0.86 s vs ~3 min (212x)
- Warm query: 0.55 s vs 7.2 s (13x)

[1.0.1]: https://github.com/jimsimon/trouve/releases/tag/v1.0.1
[1.0.0]: https://github.com/jimsimon/trouve/releases/tag/v1.0.0
