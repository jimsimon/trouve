# ADR 0031: Acknowledged turn cancellation

Status: Accepted (2026-08)

## Context

Cancelling a turn originally tripped an engine-owned cancellation token, but
several blocking phases did not observe it: scheduler admission, provider and
vendor startup requests, steering, interactive questions, and native tool
execution. The UI correctly waited for the durable `turn.cancelled` event, so
these gaps appeared as a long-lived “Stopping…” state.

Returning as soon as the token is tripped is not safe either. Vendor harnesses
and native tools may still own a model turn, child process, JSON-RPC request,
or worktree mutation. Publishing cancellation and starting a replacement turn
before those resources stop can misattribute late output or race mutations in
the session worktree.

The existing event protocol already distinguishes the cancellation request
from its terminal durable state: the request is an HTTP action and
`turn.cancelled` is the persisted result. No new event type is required.

## Decision

- One cancellation token follows a turn through admission, session-lock and
  model-catalog waits, provider/vendor startup, steering, questions,
  approvals, compaction, and `ToolCtx`.
- Blocking startup and control requests use cancellation-aware waits and
  bounded response/transport timeouts. Partially written transports are
  completed safely or invalidated rather than reused in an ambiguous state.
- A backend adapter owns vendor cleanup after cancellation:
  - Codex interrupts the exact vendor turn and waits for the interrupt
    acknowledgement while retaining its per-thread lifecycle guard.
  - Cursor sends `session/cancel` and waits for the outstanding prompt
    response; an unresponsive process is recycled after a bounded deadline.
  - Claude removes and kills/reaps its persistent process because its
    stream-json mode has no per-turn cancellation acknowledgement.
- A long-running `ToolExecutor` implementation observes `ToolCtx::cancel` and
  returns only after its process/protocol cleanup is complete. The engine
  retains the per-session execution lane while awaiting that acknowledgement.
  Shell cancellation terminates the process group and reaps the child; a
  cancelled MCP handshake or request explicitly terminates and reaps its
  process before evicting the now-desynchronized connection.
- Cancellation resolves transient approval and question waits so no pending
  responder can target a terminal turn.
- `turn.cancelled` remains the single durable terminal event and is appended
  only after the whole turn future returns from cleanup. Cancellation wins a
  race with provider errors or completion, preventing a simultaneous
  `turn.failed` or `turn.completed` event.
- Cleanup waits are bounded where an external process or transport can stop
  responding. A timeout invalidates the owned process/transport rather than
  releasing it for a replacement turn in an unknown state. If a custom tool
  executor cannot be invalidated generically, its session execution lane is
  quarantined in a cleanup task until the executor finally returns.

## Consequences

- Cancellation becomes prompt during scheduler, startup, interactive, stream,
  and tool waits while preserving deterministic turn boundaries.
- The UI may continue to show “Stopping…” during the short cleanup interval;
  that interval now means cleanup is genuinely in progress rather than that a
  cancellation point was missed.
- An unresponsive persistent vendor or MCP process may be recycled, trading a
  later cold start for safety and bounded cancellation latency.
- A custom executor that ignores cancellation can leave its session tool lane
  quarantined. The turn may terminate after the bounded acknowledgement wait,
  but replacement worktree operations cannot overtake the stale call.
- Custom backends and tool executors have an explicit cleanup contract. Tests
  must not model cancellation solely by dropping their stream/future.
- Existing clients and persisted event logs remain compatible because the
  protocol shape is unchanged.

## Alternatives considered

- **Publish `turn.cancelled` immediately when the HTTP request arrives.**
  Rejected because durable state would claim the turn ended while vendor or
  tool work could still mutate the shared worktree.
- **Drop every in-flight future and rely on destructor cleanup.** Rejected
  because async destructors cannot generally await process reaping or vendor
  acknowledgements, and detached cleanup can race a replacement turn.
- **Wait indefinitely for graceful vendor acknowledgement.** Rejected because
  a wedged transport would leave both cancellation and the session blocked
  forever.
- **Add a separate `turn.cancellation_requested` durable event.** Rejected for
  now because the request is transient UI state and the existing terminal
  event is sufficient for reconstruction; adding an event would not solve the
  cleanup race.
