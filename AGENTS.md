# trouve monorepo — agent instructions

This repository is a Cargo workspace containing the **trouve** AI coding
harness and the **trouve-search** code search tool. Read this file before
making changes; it encodes the architecture invariants the project is built
on. Decisions live in `docs/adr/` — check there before re-litigating one.

## Layout

- `crates/trouve-search` — code search library + CLI (published to crates.io
  and npm; its version drives `scripts/sync_versions.py`).
- `crates/trouve-protocol` — protocol types + OpenAPI schema. No logic.
- `crates/trouve-core` — sessions, threads, worktrees, event log,
  checkpoints, agent loop, tools, permissions.
- `crates/trouve-providers` — LLM provider abstraction and implementations.
- `crates/trouve-server` — axum HTTP/SSE server exposing core over the
  protocol.
- `crates/trouve-client-core` — shared client logic (protocol client, session
  state, view models) for native clients.
- `crates/slint-*` — standalone, reusable Slint widgets (code view, diff
  view, markdown, terminal). No trouve-specific types in their public APIs.
- `crates/trouve-app` — thin Slint desktop/mobile app composing the above.
- `docs/adr/` — architectural decision records. `docs/design/` — living
  design docs (event log schema, UX screen map).

## Architecture invariants

These are load-bearing. Do not violate them without a new ADR.

1. **Clients never bypass the protocol.** All agent functionality is exposed
   by `trouve-server`; the desktop app, CLI, and future clients speak
   HTTP + SSE only. No client imports `trouve-core`. The desktop app embeds
   the server in-process (ADR 0008), but only through its one bootstrap
   entry point (`trouve_server::bind_local`) — it still talks to it over
   loopback HTTP + SSE and never touches engine internals.
2. **One event log.** Server→client state flows through the append-only,
   persisted, cursor-addressed event log. New UI-visible state means a new
   event type, not a side channel.
3. **Every side effect goes through `ToolExecutor`.** File edits, shell,
   git, MCP calls — one chokepoint for permissions, audit, and (later)
   sandboxed executors. Never spawn a process or write a file from the agent
   loop directly.
4. **Sessions own worktrees.** Agent file operations happen in the session's
   git worktree, never in the user's checkout. Threads share the session
   worktree; worktree mutations are serialized.
5. **Protocol changes are versioned.** `trouve-protocol` is the single
   source of truth; the OpenAPI schema snapshot test must be updated
   deliberately with a version bump.
6. **Agent modes are data.** Modes (plan/code/review/…) are prompt + tool
   policy + default permission mode. Adding a mode must not require new Rust
   control flow.
7. **Widget crates stay generic.** `slint-*` crates take plain data (text,
   spans, hunks), not trouve protocol types.
8. **Team coordination is server-owned and durable.** Team roles map to
   persistent threads; canonical messages, mentions, and deliveries use the
   session event log and thread prompt queues. Clients and provider backends
   never coordinate agents through side channels.

## Conventions

- Rust edition/lints come from the workspace; run `cargo fmt --all` and
  `cargo clippy --all-targets -- -D warnings` before finishing.
- Tests: `cargo test --workspace` must stay offline-safe. Model-downloading
  and network tests are `#[ignore]` behind env flags (`TROUVE_E2E=1`).
- Releases are tagged per crate (`trouve-search-v1.2.3`). After bumping
  `crates/trouve-search/Cargo.toml`, run `python3 scripts/sync_versions.py`.
- Commit style: imperative, concise subject; explain *why* in the body when
  it isn't obvious.
- Licensing: workspace code is MIT. Slint is used under its Royalty-Free
  license (ADR 0006); keep the AboutSlint attribution in the app.
