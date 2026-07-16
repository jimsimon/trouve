# ADR 0001: Single Cargo workspace monorepo

Status: Accepted (2026-07)

## Context

The repo started as a single crate: `trouve-search`, a Rust port of semble's
code search (published to crates.io and npm). The trouve project is growing
into an AI coding harness — a protocol-first backend, provider layer, native
Slint clients, and reusable widget crates — and the harness depends on
`trouve-search` as a library (search tool, tree-sitter highlighting,
branch/worktree-aware store).

Options considered: separate repos per crate, a new repo for the harness that
depends on `trouve-search` via crates.io, or one workspace.

## Decision

Convert this repo into a single Cargo workspace under `crates/`:

- `crates/trouve-search` — existing search tool (library + CLI), unchanged
  publicly.
- `crates/trouve-protocol`, `trouve-core`, `trouve-providers`,
  `trouve-server`, `trouve-cli`, `trouve-client-core`, `trouve-app` — the
  harness (added incrementally).
- `crates/slint-*` — reusable Slint widget crates (code view, diff view,
  markdown, terminal), designed to be usable outside trouve.

Crates are versioned independently. Release tags are per crate
(`trouve-search-v1.2.0`); `scripts/sync_versions.py` continues to pin the npm
and plugin manifests to the `trouve-search` crate version only.

## Consequences

- Harness crates use `trouve-search` as a path dependency; cross-cutting
  changes (e.g. exposing highlight tokens) land atomically.
- CI must scope jobs per crate where it matters (benchmarks, release binary
  builds); workspace-wide `cargo test`/`clippy` stays the default.
- The legacy bare `vX.Y.Z` tags predate the monorepo; new releases use
  crate-prefixed tags.
- One `Cargo.lock`, one toolchain, shared `[workspace.dependencies]` for
  common deps.
