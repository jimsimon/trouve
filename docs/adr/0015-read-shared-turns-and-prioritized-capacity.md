# Read-shared turns and prioritized model capacity

Status: Partially superseded by 0042 (2026-08)

## Context

ADR 0003 requires worktree mutations from threads in one session to be
serialized. An exclusive lock around every turn also serialized modes that
cannot mutate the worktree, making multi-persona reviews and parallel desktop
research take the sum of every model's latency. Unattended reviews can also
consume all capacity of a shared provider and make interactive desktop work
wait behind them.

The global live-event broadcast compounded the problem: every active turn and
SSE follower woke for every unrelated scope.

## Decision

- A session uses a read/write access lock. Ordinary read-only turns share read
  access; mutating turns, checkpoint restoration, and settings changes take
  exclusive write access. The existing lock-free exception for a read-only
  child spawned by a writing parent remains.
- Model capacity is coordinated in the engine, across direct providers and
  vendor backends. Global and per-provider background limits are lower than
  their total limits, reserving capacity for interactive desktop turns.
- Capacity wait is persisted as a thread event and, for review tasks, durable
  task telemetry.
- The store retains the single persisted event log and global broadcast, but
  also publishes committed envelopes to scope-specific live channels. Replay
  remains a database query and preserves the same cursor semantics.

## Consequences

Read-only desktop and review work can overlap safely while all worktree
mutations remain serialized. Background reviews cannot occupy every model
slot. Provider quotas still require bounded configuration and may reduce ideal
parallelism. Scope channels add small in-memory routing state, which is pruned
after receivers disappear; they do not become a second source of truth.

## Alternatives rejected

Marking review threads as spawned children would bypass the lock but
misrepresent their ownership and would not help ordinary desktop read-only
threads. Separate review-only provider pools would duplicate quota management
and still allow the embedded desktop server to overload one account.
