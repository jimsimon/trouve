# Architectural Decision Records

Short, immutable records of the significant architectural decisions in the
trouve monorepo. Each ADR captures the context at the time, the decision, and
its consequences. When a decision is reversed, write a new ADR that supersedes
the old one — don't rewrite history.

Format: [MADR-ish](https://adr.github.io/), one file per decision, numbered
sequentially.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-monorepo-cargo-workspace.md) | Single Cargo workspace monorepo | Superseded by 0012 |
| [0002](0002-protocol-first-client-server-split.md) | Protocol-first client/server split (OpenAPI + SSE event log) | Accepted |
| [0003](0003-worktree-per-session.md) | Git worktree per session; threads share the session worktree | Accepted |
| [0004](0004-no-os-sandbox-permission-modes.md) | No OS sandbox in local mode; ToolExecutor chokepoint + permission modes | Accepted |
| [0005](0005-split-ui-slint-native-plus-web.md) | Split UI: Slint native clients + separate web client | Superseded by 0023 |
| [0006](0006-slint-royalty-free-license.md) | Slint under the Royalty-Free license | Accepted |
| [0007](0007-shared-search-daemon.md) | Shared trouve-search MCP daemon over a unix socket | Accepted |
| [0008](0008-embedded-server-in-desktop-app.md) | Desktop app embeds trouve-server in-process | Accepted |
| [0009](0009-thread-owned-todo-snapshots.md) | Thread-owned todo snapshots | Accepted |
| [0010](0010-account-centric-multi-instance-github.md) | Account-centric, multi-instance GitHub integration | Accepted |
| [0011](0011-github-app-backed-code-review-service.md) | GitHub App-backed code review service | Accepted |
| [0012](0012-single-version-monorepo-release-train.md) | Single-version monorepo release train | Accepted |
| [0013](0013-preact-review-dashboard.md) | Preact application for the review dashboard | Accepted |
| [0014](0014-durable-code-review-job-artifacts.md) | Durable code-review job artifacts and event streams | Accepted |
| [0015](0015-read-shared-turns-and-prioritized-capacity.md) | Read-shared turns and prioritized model capacity | Accepted |
| [0016](0016-catalog-backed-provider-transports.md) | Catalog-backed provider transports | Accepted |
| [0017](0017-server-owned-session-title-model.md) | Server-owned session title model with heuristic fallback | Accepted |
| [0018](0018-bounded-coalesced-event-ingestion.md) | Bounded, coalesced event ingestion | Accepted |
| [0019](0019-multiple-ephemeral-terminals-per-session.md) | Multiple ephemeral terminals per session | Accepted |
| [0020](0020-canonical-model-catalog-with-availability-overlays.md) | Canonical model catalog with availability overlays | Accepted |
| [0021](0021-derived-thread-view-snapshots.md) | Derived thread-view snapshots | Accepted |
| [0022](0022-bounded-thread-view-pages.md) | Bounded thread-view pages | Accepted |
| [0023](0023-lit-web-frontend-and-webview-host.md) | Lit web frontend and gated webview host | Accepted |
| [0024](0024-isolated-servo-embedding-qualification-workspace.md) | Isolated Servo embedding qualification workspace | Accepted |
| [0025](0025-pin-servo-qualification-to-nightly-revision.md) | Pin Servo qualification to a nightly revision | Accepted |
| [0026](0026-desktop-frontend-asset-sources.md) | Desktop frontend asset sources | Accepted |
