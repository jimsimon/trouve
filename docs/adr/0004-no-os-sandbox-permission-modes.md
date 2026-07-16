# ADR 0004: No OS sandbox in local mode; ToolExecutor chokepoint + permission modes

Status: Accepted (2026-07)

## Context

OS-level sandboxing (Landlock/seccomp on Linux, Seatbelt on macOS, none
usable on Windows) is a large, platform-divergent investment, and agent
harnesses that ship it still need an escape hatch for real work (network
installs, system tools). We weighed shipping a sandbox from day one against a
permission layer.

## Decision

- Local mode does **not** use OS sandboxing. Safety comes from a permission
  layer instead:
  - `ask` (default): every mutating tool call requires explicit approval.
  - `allow-list`: pre-approved commands/paths run without prompts, the rest
    ask.
  - `yolo`: everything runs; loudly labeled as unsafe.
- Every tool call — file ops, shell, git, MCP tools — flows through a single
  `ToolExecutor` trait: one chokepoint for permission checks, logging, and
  the audit trail. Nothing in the agent loop executes side effects directly.
- Cloud/hosted agents get real isolation later by swapping in a container /
  microVM-backed `ToolExecutor` implementation (see the cloud phase); the
  permission layer is not the isolation story there.

## Consequences

- Massive scope reduction now; identical UX across platforms.
- A malicious or confused model in `yolo` mode can do real damage — this is
  documented, opt-in, and visually flagged in clients.
- Because every side effect already passes through `ToolExecutor`, adding a
  sandboxed executor later is additive, not a refactor.
