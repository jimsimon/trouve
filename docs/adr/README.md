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
| [0006](0006-slint-royalty-free-license.md) | Slint under the Royalty-Free license | Superseded by 0028 |
| [0007](0007-shared-search-daemon.md) | Shared trouve-search MCP daemon over a unix socket | Accepted |
| [0008](0008-embedded-server-in-desktop-app.md) | Desktop app embeds trouve-server in-process | Accepted |
| [0009](0009-thread-owned-todo-snapshots.md) | Thread-owned todo snapshots | Accepted |
| [0010](0010-account-centric-multi-instance-github.md) | Account-centric, multi-instance GitHub integration | Accepted |
| [0011](0011-github-app-backed-code-review-service.md) | GitHub App-backed code review service | Accepted |
| [0012](0012-single-version-monorepo-release-train.md) | Single-version monorepo release train | Accepted |
| [0013](0013-preact-review-dashboard.md) | Preact application for the review dashboard | Accepted |
| [0014](0014-durable-code-review-job-artifacts.md) | Durable code-review job artifacts and event streams | Partially superseded by 0045 |
| [0015](0015-read-shared-turns-and-prioritized-capacity.md) | Read-shared turns and prioritized model capacity | Partially superseded by 0042 |
| [0016](0016-catalog-backed-provider-transports.md) | Catalog-backed provider transports | Accepted |
| [0017](0017-server-owned-session-title-model.md) | Server-owned session title model with heuristic fallback | Superseded by 0029 |
| [0018](0018-bounded-coalesced-event-ingestion.md) | Bounded, coalesced event ingestion | Accepted |
| [0019](0019-multiple-ephemeral-terminals-per-session.md) | Multiple ephemeral terminals per session | Accepted |
| [0020](0020-canonical-model-catalog-with-availability-overlays.md) | Canonical model catalog with availability overlays | Accepted |
| [0021](0021-derived-thread-view-snapshots.md) | Derived thread-view snapshots | Accepted |
| [0022](0022-bounded-thread-view-pages.md) | Bounded thread-view pages | Superseded by 0033 |
| [0023](0023-lit-web-frontend-and-webview-host.md) | Lit web frontend and gated webview host | Partially superseded by 0028 and 0039 |
| [0024](0024-isolated-servo-embedding-qualification-workspace.md) | Isolated Servo embedding qualification workspace | Superseded by 0039 |
| [0025](0025-pin-servo-qualification-to-nightly-revision.md) | Pin Servo qualification to a nightly revision | Superseded by 0039 |
| [0026](0026-desktop-frontend-asset-sources.md) | Desktop frontend asset sources | Accepted |
| [0027](0027-wry-default-desktop-frontend.md) | Wry as the default desktop frontend | Superseded by 0028 |
| [0028](0028-retire-slint-frontend.md) | Retire the Slint frontend | Partially superseded by 0039 |
| [0029](0029-short-session-branch-names.md) | Short session branch names by default | Accepted |
| [0030](0030-parallel-tool-execution-and-vendor-mutation-confinement.md) | Parallel tool execution with per-session mutation confinement | Partially superseded by 0034 and 0043 |
| [0031](0031-acknowledged-turn-cancellation.md) | Acknowledged turn cancellation | Accepted |
| [0032](0032-single-local-server-owner.md) | Single local server owner per data directory | Accepted |
| [0033](0033-materialized-pageable-thread-history.md) | Materialized pageable thread history | Accepted |
| [0034](0034-concurrent-session-turns.md) | Concurrent turns in a shared session worktree | Partially superseded by 0042 |
| [0035](0035-bounded-recursive-subagent-trees.md) | Bounded recursive subagent trees | Accepted |
| [0036](0036-exact-protocol-version-compatibility.md) | Exact protocol version compatibility | Accepted |
| [0037](0037-capability-scoped-external-read-roots.md) | Capability-scoped external read roots | Accepted |
| [0038](0038-process-wide-child-launch-synchronization.md) | Process-wide child-launch synchronization | Accepted |
| [0039](0039-retire-servo-qualification-hosts.md) | Retire Servo qualification hosts | Accepted |
| [0040](0040-durable-root-cause-history-for-code-review.md) | Durable root-cause history for code review | Accepted |
| [0041](0041-evidence-backed-review-churn-controls.md) | Evidence-backed review churn controls | Accepted |
| [0042](0042-provider-governed-turn-admission.md) | Provider-governed turn admission | Accepted |
| [0043](0043-background-jobs-release-mutation-lane.md) | Background jobs release the session mutation lane | Partially superseded by 0046 |
| [0044](0044-durable-assistant-produced-artifacts.md) | Durable assistant-produced artifacts | Accepted |
| [0045](0045-full-branch-review-on-every-head.md) | Full-branch review on every head | Accepted |
| [0046](0046-detached-descendants-are-session-scoped.md) | Detached descendants of shell calls are session-scoped | Accepted |
| [0047](0047-cursor-sdk-bridge-transport.md) | Cursor SDK Bridge transport | Accepted |
