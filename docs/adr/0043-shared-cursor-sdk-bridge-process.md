# ADR 0043: One shared Cursor SDK Bridge per backend

Status: Accepted (2026-08)

Supersedes [ADR 0042](0042-cursor-sdk-bridge-transport.md)'s per-thread
process and callback-server ownership. Its transport, authentication, tool
confinement, managed-runtime, and steering decisions remain in force.

## Context

ADR 0042 isolated one warm Cursor SDK Bridge per Trouve thread because the
Bridge has one process-wide custom-tool callback registration. That made the
callback boundary simple, but a configured backend could retain three Bridge
processes. A live v1.0.28 measurement observed about 228 MiB RSS for one warm
process, so the per-thread pool imposed a material idle-memory cost.

Cursor's pinned Bridge contract supports a stronger boundary. One client owns
one managed Bridge process and multiple agent handles; `AgentOptions` carry
each agent's API key, working directory, and custom-tool catalog, while every
custom-tool callback identifies its owning `agent_id`. The Bridge's local
SQLite store is shared by those agents. See the pinned
[Bridge README](https://github.com/cursor/sdk-bridge/blob/v1.0.28/README.md)
and [service contract](https://github.com/cursor/sdk-bridge/blob/v1.0.28/docs/services.md).

A six-turn live qualification used two agents with distinct workspaces and
tool catalogs in one v1.0.28 Bridge. Concurrent sends, exact callback routing,
warm close/resume, cold-process resume, and cancellation isolation passed.
Cursor did not reliably disconnect the cancelled agent's outstanding callback,
so safe sharing requires Trouve to settle that agent's callback route itself;
it cannot rely on transport disconnect as the cancellation boundary.

## Decision

- Each configured Cursor backend owns at most one warm Bridge process. This is
  the credential boundary: separate provider configurations and credentials
  never share a process. The process and its SQLite state root are shared by
  that backend's sessions and threads.
- The Bridge callback URL and random bearer live for the process lifetime on a
  loopback-only listener. Possessing that bearer grants no Trouve tool by
  itself. A callback must also match an active exact `agent_id` route.
- Each turn registers one route only after `CreateAgent` or `ResumeAgent`
  returns its agent id. The route owns that turn's thread-scoped MCP URL and
  ticket, exact custom-tool allowlist, deduplication records, cancellation
  token, and callback task supervisor. Unknown, stale, duplicate, or mismatched
  agent routes and tools fail closed. Route removal precedes task cancellation
  and `CloseAgent`, so no new callback can enter after teardown starts.
- Bridge RPC clients are cloneable and concurrent; Trouve does not hold the
  child-process mutex across an agent turn. Same-thread turns remain serial,
  and one backend still admits at most three active turns to bound callback and
  model resource use. That limit no longer represents a process count.
- Turn cancellation sends `CancelRun` when the run id is available, removes
  and actively settles only that agent's callback route, and closes only that
  agent. It does not terminate the shared process or interrupt unrelated
  agents. Cursor steering remains disabled as decided in ADR 0042.
- A process exit, protocol failure, ambiguous agent setup, callback-route
  collision, or unacknowledged `CloseAgent` quarantines the process. No new
  agent leases enter it; already-active turns drain, then Trouve terminates and
  recreates the process from the shared durable store. Backend shutdown also
  closes admission, drains thread gates, and terminates the one process and
  callback router. The five-minute idle reaper remains.
- Cursor receives only the `mcp` capability and the complete native-tool
  denylist from ADR 0042. Every callback still enters the thread's internal MCP
  endpoint and `ToolExecutor`; sharing a vendor process does not share Trouve
  permissions, worktrees, tools, or mutation lanes.

## Consequences

- Cursor's warm idle cost is bounded to one Bridge per configured backend
  instead of up to three. Startup and the local SQLite connection are amortized
  across all of that backend's threads.
- A longer-lived loopback callback bearer is acceptable because active
  agent-route membership, exact tool membership, and the turn-scoped MCP ticket
  remain short-lived authorization boundaries. Dropping a route also cancels
  its outstanding callbacks even when Cursor leaves the HTTP request connected.
- Independent Cursor turns can stream concurrently without a global Bridge
  mutex. Cancellation and callback deduplication are isolated per agent, so an
  identical vendor call id in two agents cannot collide.
- Quarantine favors isolation over immediate replacement: a new turn may wait
  for existing agents to drain after an ambiguous shared-process failure. It
  does not kill healthy concurrent agents merely to recover capacity.
- Sharing the vendor store gives one process failure a wider operational blast
  radius than the per-thread design. Durable agents remain cold-resumable, and
  fail-closed quarantine bounds that risk.
- This is an internal backend lifecycle change. It adds no protocol field,
  settings mode, or user-visible transport distinction.

## Alternatives rejected

- **Keep ADR 0042's per-thread process pool.** It preserves physical isolation
  but retains multiple large warm processes and repeats Bridge startup across
  unrelated threads.
- **Share one process across all Cursor provider configurations.** That would
  cross credential and provider lifecycle boundaries.
- **Reuse one process-wide callback as the active turn directly.** Concurrent
  agents would overwrite or mix MCP tickets, tools, worktrees, and cancellation.
- **Terminate the shared process whenever one turn is cancelled.** That would
  interrupt unrelated agents even though `CancelRun`, route settlement, and
  `CloseAgent` provide an agent-scoped cleanup path.
- **Retain several global Bridge processes for capacity.** The pinned Bridge
  already supports concurrent agents; extra processes add memory without a
  demonstrated correctness or throughput requirement.
