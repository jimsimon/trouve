# ADR 0030: Parallel tool execution with per-session mutation confinement

Status: Accepted (2026-08)

## Context

Provider APIs can return several tool calls in one assistant response, and
vendor harnesses can start several native tools during one turn. Trouve
previously executed native provider calls sequentially. Its session turn lock
also serialized mutation-capable turns against other turns, but it did not
serialize two edits owned by the same vendor turn. That combination left
read-only parallelism unused and allowed concurrent vendor edits to race in a
shared session worktree.

The durable event protocol already represents overlapping tool lifecycles:
each call has its own requested, started, output, and completed events. A new
event type or protocol version is therefore unnecessary.

## Decision

- Native provider tool calls from one assistant response execute with bounded
  fan-out (currently eight calls).
- The engine owns a narrower per-session tool execution lane in addition to
  the whole-turn session lock:
  - known read-only calls share a read permit;
  - mutation-capable and unknown calls take an exclusive write permit;
  - cancellation can interrupt both permit acquisition and execution.
- Tool lifecycle events record actual execution timing. Tool-result messages
  are nevertheless appended to the provider transcript in the provider's
  original call order, preserving provider API requirements.
- Missing or duplicate provider call ids are normalized before the assistant
  message is persisted or execution starts, so events and results retain a
  unique identity under parallel execution.
- Claude Code and Codex use trouve's full MCP tool bridge by default. User MCP
  servers are exposed through `ToolExecutor`, not mounted directly alongside
  the full bridge.
  - Claude's native tools stand down.
  - Codex does not currently provide a supported switch to remove all native
    tools, so its native tools run in a read-only sandbox with loopback/network
    access; every mutation is directed to the trouve bridge.
- Vendor protocols that expose approval hooks but cannot use the full bridge
  acquire an exclusive per-session mutation permit after approval and retain
  it until the matching completion event. Approval waits run concurrently
  with stream consumption so multiple vendor requests cannot deadlock the
  event loop.
- An explicit `tool_bridge = false` remains a compatibility escape hatch. It
  opts the affected Claude/Codex backend out of full mutation confinement and
  should not be used when concurrent mutation safety is required.

## Consequences

- Independent searches, reads, and other non-mutating tools can reduce turn
  latency by running concurrently.
- File edits, shell commands, git operations, and conservatively classified
  MCP calls cannot overlap within one session, including calls from the same
  native batch or a supported vendor-owned turn.
- A long mutation delays subsequent reads as well as writes at the tool lane,
  giving callers a consistent worktree view instead of observing a partially
  applied mutation.
- Event consumers need no migration. They may observe several calls in the
  started state and completions in an order different from provider transcript
  order, which the existing call ids already support.
- Full bridging changes the default tool surface for Claude and Codex, but it
  removes duplicate direct MCP mounts and centralizes permissions, audit, and
  worktree serialization in `ToolExecutor`.

## Alternatives considered

- **Run every call concurrently without a mutation lane.** Rejected because
  parallel edits and git commands can corrupt or unpredictably overwrite the
  shared worktree state.
- **Keep all tool calls sequential.** Rejected because it leaves provider
  parallel-tool support unused and needlessly serializes independent reads.
- **Serialize only known file-edit tools.** Rejected because shell, git, and
  third-party MCP tools can mutate the same worktree.
- **Emit tool results in completion order.** Rejected because several provider
  protocols expect result blocks to correspond to the preceding assistant
  call list in its original order.
- **Trust vendor-native concurrency controls.** Rejected because those
  controls vary by vendor and do not establish trouve's session-worktree
  invariant.
