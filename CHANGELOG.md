# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Kilo Code plugin** (`plugins/kilocode`, npm package `kilocode-trouve`):
  exposes `trouve_search` and `trouve_find_related` as native Kilo Code
  tools (CLI and VS Code extension) backed by a single persistent `trouve`
  server process per session, preserving the server's in-process index
  cache across calls. Installed with `kilo plugin kilocode-trouve`; a
  `content` plugin option selects what the server indexes.

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
  sub-agents across eleven coding agents (Claude Code, Cursor, Codex, Gemini,
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

[1.0.0]: https://github.com/jimsimon/trouve/releases/tag/v1.0.0
