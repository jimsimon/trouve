# ADR 0045: Cursor shared-store transition and callback quarantine

Status: Accepted (2026-08)

Extends [ADR 0044](0044-shared-cursor-sdk-bridge-process.md). Its one-Bridge-
per-backend ownership and exact-agent callback routing remain in force.

## Context

Before ADR 0044, each Cursor thread hashed its provider and thread ids into a
private Bridge state directory. A stored Cursor agent id can be resumed only
against the SQLite store that created it. Moving every thread to one backend-
wide store therefore makes legacy ids invalid even though Trouve still has
them in its durable backend-session record.

Cursor cancellation also does not reliably disconnect an outstanding custom-
tool callback. Trouve removes the agent route and cancels its callback tasks,
but bounded shutdown can itself time out. A process whose old route has not
provably settled must not admit a replacement agent. Even clean settlement
cannot prove that an unseen callback was not delayed before HTTP ingress: the
process-wide callback wire identifies the durable agent but carries no turn
nonce.

## Decision

- On a thread's first turn after the shared-store upgrade, Trouve detects its
  legacy per-thread state directory and deliberately creates a replacement
  agent in the backend-wide store instead of sending the old id to
  `ResumeAgent`. After creation, it atomically records a per-thread transition
  marker containing the replacement shared-store agent id. Later turns resume
  that id. The legacy directory is retained for recovery and is never merged
  into the shared SQLite store.
- Callback-route shutdown reports whether all supervised callback tasks
  settled before its deadline and every callback id was corroborated by that
  turn's Send stream. A timeout or identity mismatch quarantines the shared
  Bridge even when `CloseAgent` succeeds. No new turn enters that process;
  existing leases drain before Trouve recycles it against the durable shared
  store.
- Clean route shutdown permanently retires that agent id from the process-wide
  callback listener. Before a later turn resumes the durable agent, Trouve
  drains any unrelated active leases, reaps the one Bridge, and starts one
  replacement with a fresh listener and bearer. This forbids an unseen old
  callback from acquiring the later turn's MCP ticket or worktree authority.
- The direct vendor qualification proves concurrent agents, exact callback
  routing, and Cursor's cancellation transport behavior. Production adapter
  tests own the evidence that Trouve settles a cancelled callback route and
  rotates the process before resuming that agent while never running more than
  one Bridge for the backend.

## Consequences

- Legacy conversations are not imported into the new store. The affected
  thread has one safe context reset, publishes and persists its replacement
  agent id, and resumes normally thereafter.
- One small marker is retained per transitioned thread, and old Bridge state
  remains available for manual recovery instead of being destructively moved.
- A stuck callback can delay new Cursor turns until healthy active turns drain
  and the process is recycled. This preserves concurrent turns while failing
  closed at the process-reuse boundary.
- Repeated turns for one durable agent pay Bridge restart latency. Distinct
  agents can still share the one process concurrently, preserving the memory
  objective without relying on undocumented callback ordering.
- Qualification claims distinguish vendor transport evidence from behavior
  exercised through Trouve's production adapter.

## Alternatives rejected

- **Resume a legacy id in the shared store.** Agent ids are store-local, so
  this fails or can bind to unrelated state.
- **Copy or merge SQLite files.** Cursor does not publish a supported merge
  contract, and manipulating live vendor state risks corruption.
- **Delete legacy state after reset.** Retention is safer and keeps recovery
  possible without widening the runtime path.
- **Assume task cancellation settled the route.** A timed-out supervisor is
  ambiguous; reusing that process could route a late callback into a new turn.
- **Use the direct Bridge probe as adapter-cleanup evidence.** It bypasses the
  production callback supervisor and cannot establish Trouve's teardown
  behavior.
