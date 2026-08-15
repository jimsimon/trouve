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
- `crates/trouve-client-core` — shared Rust client logic (protocol client,
  compatibility checks, session state, view models) for native hosts and
  tools.
- `crates/trouve-thread-view` — shared deterministic fold from thread events
  into rebuildable protocol snapshots; no transport or UI dependencies.
- `crates/trouve-desktop-host` — app-owned static-asset gateway, typed native
  capability boundary, and replaceable desktop webview host.
- `crates/trouve-app` — main desktop application; ships the Lit frontend in
  Wry and embeds the protocol server for local use.
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
   loop directly. Vendor harnesses use the full tool bridge by default;
   unavoidable vendor-native tools are disabled or confined read-only, and
   approval-only fallbacks hold an engine mutation lease (ADR 0030).
4. **Sessions own worktrees.** Agent mutations happen in the session's git
   worktree, never in the user's checkout. Read-only filesystem tools may also
   inspect canonical host-registered instruction/package roots, but those
   capabilities never widen mutation paths (ADR 0037). Threads share the
   session worktree and their turns may run concurrently; read-only tools may
   overlap, but mutation-capable tool calls and checkpoints are exclusive per
   session (ADRs 0030 and 0034).
5. **Protocol changes are versioned.** `trouve-protocol` is the single
   source of truth; the OpenAPI schema snapshot test must be updated
   deliberately with a version bump. Generated clients require an exact
   protocol-version match because closed wire enums and unions are not
   automatically forward-compatible (ADR 0036).
6. **Agent personas are data.** Personas (plan/code/review/…) are prompt + tool
   policy + default permission mode. Adding a persona must not require new Rust
   control flow.
7. **One product frontend.** `web/app-ui` is the shared Lit application for
   Wry desktop and the PWA. Native hosts provide only the gateway and typed OS
   capabilities; they do not reimplement product screens or durable state.
8. **One workspace version.** Every first-party Cargo crate, Node package,
   plugin manifest, internal package pin, and release artifact uses root
   `[workspace.package].version`. Repository releases use `vX.Y.Z` tags (ADR
   0012). Protocol and storage-format compatibility versions remain separate.
9. **The web host is not a second client protocol.** Wry/Lit is the shipping
   desktop frontend (ADR 0028). The desktop gateway may
   serve assets, proxy HTTP/SSE, and expose narrowly typed native capabilities
   such as window state, pickers, clipboard, notifications, and external-open.
   It never carries durable agent state or arbitrary filesystem, shell, URL,
   git, MCP, or tool operations. The desktop webview and mobile PWA obtain all
   harness state and effects through `trouve-server` (ADR 0023). Runtime asset
   directories and Vite proxying are explicit development/qualification
   sources, remain loopback-only behind the same gateway origin, and are never
   enabled by shipping product hosts (ADR 0026). The first default Wry process
   owns one embedded server through `trouve_server::bind_local`; additional
   default windows attach to that elected owner and never open a second Engine
   or database connection (ADR 0032). Comparison hosts require an explicit
   server URL and never open the default database.
10. **Child launches share one synchronization boundary.** Every
    trouve-owned child-process launch — including standard and Tokio commands,
    PTYs, daemons, probes, and system-opener libraries — goes through
    `trouve-process`. Process-tree creation holds that shared macOS boundary
    from sentinel setup through spawn; ordinary callers release it immediately
    after creating the child and wait outside it (ADR 0038).

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
- Licensing: workspace code is MIT. Keep generated Rust and npm third-party
  notices synchronized with their locked dependency graphs.
