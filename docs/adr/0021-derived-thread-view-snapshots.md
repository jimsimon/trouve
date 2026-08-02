# ADR 0021: Derived thread-view snapshots

Status: Accepted (2026-08)

## Context

The event log is the durable UI and audit source of truth. A new client,
however, rebuilt a chat by replaying every retained thread event from cursor
zero. Long tool-heavy threads therefore paid growing database, decoding, fold,
and UI handoff costs merely to display their current state. Persisting only a
client cursor cannot help after restart because the client no longer has the
state represented by that cursor.

## Decision

- The server maintains a versioned, rebuildable projection of each thread's
  renderable items and current interaction state.
- A thread-view endpoint returns that projection with the exact thread event
  cursor it includes. Clients install the snapshot and subscribe after that
  cursor, preserving the event stream's gap-free replay semantics.
- Projection updates share the event append transaction once a cache exists.
  Missing, stale, or incompatible projections are rebuilt from the durable
  event log.
- The event log remains authoritative. Projection rows are derived cache data,
  not a second command or mutation channel.

## Consequences

- Opening a previously projected thread is independent of retained event
  count, while first access to an older thread may perform one lazy rebuild.
- Multiple clients and events racing snapshot delivery remain safe because the
  cursor defines the handoff boundary.
- Projection schema changes require a version bump and rebuild path.
- The snapshot currently carries the complete folded transcript; backward
  pagination can be added without changing the source-of-truth decision.

## Alternatives rejected

- Persisting client cursors alone: a fresh client lacks the folded state.
- Deleting or rewriting event history: loses audit data and violates the
  append-only event-log contract.
- Rendering debounce alone: reduces repainting but still replays and folds the
  entire history.
