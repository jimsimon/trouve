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

## Amendment (2026-09): read-only shell commands are reads

The `shell` tool is classified as mutating because arbitrary shell text can
do anything. Applied literally, that made `git log` or `rg` prompt on every
call in `ask` mode and denied them outright in read-only personas, while the
same reads were free through `read_file` and `grep`, and it pushed vendor
backends toward their own unsandboxed shells for quiet reads.

`crate::command_safety::shell_command_is_read_only` recognises a small fixed
vocabulary of read-only commands (file readers and text filters, `find`
without `-exec`/`-delete`, a `git` query subset, toolchain version probes),
joined only by `|`, `&&`, `||`, and `;`. A recognised command is gated as a
read: no prompt in `ask`, allowed in read-only personas, shared execution
lane. Everything else keeps the mutating classification. The classifier fails
closed, because a read-only persona must not become a way to read outside the
worktree without a prompt:

- Substitution, redirection, background jobs, escapes, expansion, and flags
  that write, execute helpers (`find -exec`, `rg --pre`, `git -c`,
  `--textconv`, `--filters`), or follow symlinks during recursion reject.
- Operands are confined on the real filesystem, not just lexically: absolute,
  `~`-relative, and `..` paths reject; an operand that exists is
  canonicalized through symlinks and must stay beneath the canonical
  worktree; a glob operand rejects if any symlink it could expand through
  leaves the worktree; `cd` must name one existing directory inside the
  worktree and is tracked so later operands resolve against it (bare `cd`
  and `cd -` reject).
- `git config` reads only with `--local`, exactly one read action, and that
  action's positional grammar, so host-level configuration is never merged
  into the answer and output modifiers cannot disguise a write.

The same classification applies to vendor-native shell approvals that carry
the command text (Claude's `Bash`). `write_stdin`, which feeds a background
job, is a mutation with a per-job allow-list key: the job may be an
interactive shell, so its launch approval does not cover later input.
