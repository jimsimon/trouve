# Thread-owned todo snapshots

Status: Accepted (2026-07)

## Context

An agent's todo list was held only in memory by `todo_write` and keyed by the
session worktree. Sessions may contain several independent threads shown as
tabs, so that ownership allowed one thread's plan to appear in another and
lost the current plan when the server restarted. Tool-call events preserved
history but made clients replay and interpret tool-specific result JSON to
discover current state.

## Decision

The current todo list is a persisted property of a thread. The `Thread`
protocol model carries the snapshot for initial loads, and the thread-scoped
`thread.todos_updated` event carries a full replacement snapshot for replay
and live updates. Successful trouve or vendor todo tool results update the
snapshot; their ordinary tool events remain unchanged as the historical audit
trail.

Clients treat the latest snapshot as authoritative and never derive one
thread's current list from session or worktree state.

## Consequences

- Threads in one session can maintain independent plans while sharing files.
- Reconnects and restarts recover the current list without tool-call parsing.
- New UI surfaces can show current progress consistently across clients.
- Todo updates add a small denormalized thread-row write alongside the
  append-only event; both are performed before the update is exposed.

## Alternatives rejected

- Worktree/session ownership conflates independent threads.
- Deriving current state only from `tool.completed` couples every client to
  vendor-specific tool names and result shapes.
- A separate non-event endpoint would bypass the event log for UI-visible
  state.
