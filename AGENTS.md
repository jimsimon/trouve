# trouve monorepo — agent instructions

This repository is a Cargo workspace containing the **trouve** AI coding
harness and the **trouve-search** code search tool. Read this file before
making changes; it encodes the architecture invariants the project is built
on. Decisions live in `docs/adr/` — check there before re-litigating one.

## Layout

- `crates/trouve-search` — code search library + CLI (published to crates.io
  and npm; the root workspace version drives `scripts/sync_versions.py`).
- `crates/trouve-protocol` — protocol types + OpenAPI schema. No logic.
- `crates/trouve-core` — sessions, threads, worktrees, event log,
  checkpoints, agent loop, tools, permissions.
- `crates/trouve-providers` — LLM provider abstraction and implementations.
- `crates/trouve-server` — axum HTTP/SSE server exposing core over the
  protocol.
- `crates/trouve-client-core` — shared client logic (protocol client, session
  state, view models) for native clients.
- `crates/trouve-thread-view` — shared deterministic fold from thread events
  into rebuildable protocol snapshots; no transport or UI dependencies.
- `crates/trouve-desktop-host` — app-owned static-asset gateway, typed native
  capability boundary, and replaceable desktop webview host.
- `crates/trouve-slint-*` — standalone, reusable Slint widgets (code view, diff
  view, markdown, terminal). No trouve-specific types in their public APIs.
- `crates/trouve-app` — main desktop application; ships Lit in Wry by default
  and retains the Slint frontend as an explicit rollback during rollout.
- `crates/trouve-servo-embed-preview` — disposable, chrome-free Servo
  embedding qualification harness. It is an excluded nested Cargo workspace
  with its own lockfile, not the shipping desktop host (ADR 0024).
- `web/app-ui` — Lit application shared by the desktop webview and mobile PWA.
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
2. **One durable event log.** Durable server→client state flows through the
   append-only, persisted, cursor-addressed event log. New durable UI-visible
   state means a new event type, not a side channel. Explicitly ephemeral
   transports such as integrated PTY instances and their byte streams may use
   request/SSE endpoints and are not reconstructed after server restart (ADR
   0019).
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
7. **Widget crates stay generic.** `trouve-slint-*` crates take plain data (text,
   spans, hunks), not trouve protocol types.
8. **One workspace version.** Every first-party Cargo crate, Node package,
   plugin manifest, internal package pin, and release artifact uses root
   `[workspace.package].version`. Repository releases use `vX.Y.Z` tags (ADR
   0012). Protocol and storage-format compatibility versions remain separate.
   `crates/trouve-servo-embed-preview` is the sole Cargo-membership and
   lockfile exception: its resolver graph is isolated because the pinned Servo
   nightly and the product server require incompatible native SQLite link
   versions, but its first-party version and internal pins are still
   synchronized to the root version (ADRs 0024 and 0025).
9. **The web host is not a second client protocol.** Wry/Lit is the default
   desktop frontend and Slint remains the explicit rollback (ADR 0027). The
   desktop gateway may
   serve assets, proxy HTTP/SSE, and expose narrowly typed native capabilities
   such as window state, pickers, clipboard, notifications, and external-open.
   It never carries durable agent state or arbitrary filesystem, shell, URL,
   git, MCP, or tool operations. The desktop webview and mobile PWA obtain all
   harness state and effects through `trouve-server` (ADR 0023). Runtime asset
   directories and Vite proxying are explicit development/qualification
   sources, remain loopback-only behind the same gateway origin, and are never
   enabled by shipping product hosts (ADR 0026). The default Wry process owns
   one embedded server through `trouve_server::bind_local`; comparison and
   Servo qualification hosts require an explicit server URL and never open
   the default database.

## Conventions

- Every Cargo package and crate directory we create is prefixed `trouve-`,
  and every Node package is scoped under `@trouve-ai/`, including private
  apps. Keep `trouve-app` as the main application package. Use the
  `create-trouve-crate` skill whenever adding or renaming a workspace crate
  or Node package.
- Rust edition/lints come from the workspace; run `cargo fmt --all` and
  `cargo clippy --all-targets -- -D warnings` before finishing.
- Tests: `cargo test --workspace` must stay offline-safe. Model-downloading
  and network tests are `#[ignore]` behind env flags (`TROUVE_E2E=1`).
- Releases are tagged repository-wide (`v1.2.3`). Edit only root
  `[workspace.package].version`, then run `python3 scripts/sync_versions.py`.
  Use the `publish-release` skill for releases, version changes, and new
  version-bearing artifacts.
- Commit style: imperative, concise subject; explain *why* in the body when
  it isn't obvious.
- Licensing: workspace code is MIT. Slint is used under its Royalty-Free
  license (ADR 0006); keep the AboutSlint attribution while any distributed
  artifact links or contains Slint.
