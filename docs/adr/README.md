# Architectural Decision Records

Short, immutable records of the significant architectural decisions in the
trouve monorepo. Each ADR captures the context at the time, the decision, and
its consequences. When a decision is reversed, write a new ADR that supersedes
the old one — don't rewrite history.

Format: [MADR-ish](https://adr.github.io/), one file per decision, numbered
sequentially.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-monorepo-cargo-workspace.md) | Single Cargo workspace monorepo | Accepted |
| [0002](0002-protocol-first-client-server-split.md) | Protocol-first client/server split (OpenAPI + SSE event log) | Accepted |
| [0003](0003-worktree-per-session.md) | Git worktree per session; threads share the session worktree | Accepted |
| [0004](0004-no-os-sandbox-permission-modes.md) | No OS sandbox in local mode; ToolExecutor chokepoint + permission modes | Accepted |
| [0005](0005-split-ui-slint-native-plus-web.md) | Split UI: Slint native clients + separate web client | Accepted |
| [0006](0006-slint-royalty-free-license.md) | Slint under the Royalty-Free license | Accepted |
