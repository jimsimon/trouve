# Event log design

The event log is the spine of the trouve harness: every piece of UI-visible
state flows through it, replay/reconnect reads from it, checkpoints/undo
reference it, and it doubles as the audit trail (ADR 0002, invariant 2).
This document defines its semantics before any endpoint exists; changes here
are protocol changes.

## Model

- Events are **append-only** and **per-thread**. A thread is the unit of
  conversation; sessions and workspaces have lifecycle events too, carried on
  a synthetic per-scope stream (see "Scopes").
- Every event has a **cursor**: a `u64` strictly increasing *within its
  scope*, assigned at append time by the store (SQLite `AUTOINCREMENT`
  rowid). Cursors are opaque to clients except for ordering and resumption.
- Events are **immutable**. Corrections are new events (e.g.
  `message.aborted`), never rewrites.
- Streaming deltas are events like everything else. High-frequency deltas
  (`assistant.delta`) are persisted so that replay reproduces the exact
  transcript; they are small and SQLite handles the write rate comfortably.
  A future compaction pass may fold deltas older than the last checkpoint
  into their final `assistant.message` — clients must not depend on deltas
  being retained forever, only on the folded form being equivalent.

## Scopes

Each event row belongs to exactly one scope:

| Scope | Stream | Examples |
| --- | --- | --- |
| `thread:<id>` | `GET /v1/threads/:id/events` | deltas, tool calls, approvals, turns |
| `session:<id>` | `GET /v1/sessions/:id/events` | checkpoints, undo/redo, worktree lifecycle |
| `server` | `GET /v1/events` | workspace registered, session created/deleted |

A client rendering a thread subscribes to the thread stream and its parent
session stream.

## Delivery

- Transport is SSE. Each SSE message carries `id: <cursor>` and a JSON body.
- Resumption: clients send `Last-Event-ID: <cursor>` (or `?after=<cursor>`);
  the server replays every persisted event after that cursor, then continues
  live. Replay and live delivery are indistinguishable to the client.
- The server never skips cursors within a scope; a gap means data loss and
  is a bug.

## Event envelope

```json
{
  "cursor": 4132,
  "scope": { "thread": "th_01H..." },
  "ts": "2026-07-05T17:03:21.114Z",
  "event": { "type": "assistant.delta", "turn": 3, "text": "..." }
}
```

`event.type` is a dot-namespaced string. Unknown types must be ignored by
clients (forward compatibility); removing or changing the meaning of a type
requires a protocol version bump.

## Event taxonomy (initial)

Thread scope:

- `turn.started` `{turn, mode, model}` / `turn.completed` `{turn, usage,
  checkpoint_id?}` / `turn.failed` `{turn, error}`
- `user.message` `{turn, content}`
- `assistant.delta` `{turn, text}` — streamed model output
- `assistant.message` `{turn, content}` — folded final text for the turn
- `tool.requested` `{turn, call_id, tool, args, requires_approval}`
- `approval.requested` `{turn, call_id}` / `approval.resolved` `{call_id,
  decision, by}`
- `tool.started` `{call_id}` / `tool.output` `{call_id, chunk}` /
  `tool.completed` `{call_id, status, result}`
- `thread.command_catalog_updated` `{commands}` — Trouve's authoritative
  typed slash-command and skill completion catalog; each entry declares
  whether it is a model `prompt` or deterministic Trouve `action`, plus its
  usage. It replaces the prior catalog for the thread.
  `thread.commands_updated` is legacy vendor data retained only for old
  event-log replay.
- `thread.command_executed` `{name, arguments, output}` — a deterministic
  Trouve action completed. Persisting its rendered output makes command
  history identical on replay and across clients; any response navigation
  hint is deliberately not state.
- `thread.queue_updated` `{prompts}` — the thread's queue of pending prompts
  changed (enqueue/edit/reorder/delete/dispatch); carries the full remaining
  queue in run order, so replaying to the tail reproduces the current queue
- `thread.todos_updated` `{todos}` — the thread's current todo snapshot
  changed; carries the full replacement list while `tool.*` events retain
  the history of how it changed

Session scope:

- `checkpoint.created` `{checkpoint_id, turn, thread_id, ref}`
- `checkpoint.restored` `{checkpoint_id, direction}` (undo/redo)
- `worktree.created` / `worktree.removed` `{path, branch}`

Server scope:

- `workspace.registered` `{workspace_id, path}`
- `workspace.pull_requests_updated` `{workspace_id, pull_requests}` — full
  dashboard snapshot for one workspace, emitted after a requested refresh
- `session.created` / `session.deleted` `{session_id, workspace_id}`
- `server.connectivity_changed` `{online}` — the server's internet
  reachability flipped; while offline `/v1/models` lists only models that
  run without internet, and clients gate prompt entry on that list
  (`ServerInfo.online` carries the same state for initial fetches)

## Persistence

One SQLite table:

```sql
CREATE TABLE events (
  cursor     INTEGER PRIMARY KEY AUTOINCREMENT,
  scope_kind TEXT NOT NULL,      -- 'thread' | 'session' | 'server'
  scope_id   TEXT NOT NULL,      -- '' for server scope
  ts         TEXT NOT NULL,      -- RFC 3339
  payload    TEXT NOT NULL       -- JSON event body
);
CREATE INDEX events_scope ON events (scope_kind, scope_id, cursor);
```

The cursor is globally unique (single AUTOINCREMENT) which trivially
guarantees per-scope monotonicity; per-scope density is *not* guaranteed and
clients must not assume consecutive cursors.

Writes go through a single `EventLog::append` chokepoint that (1) inserts the
row and (2) publishes to in-process subscribers — in that order, so a
subscriber can never observe an event that would not survive a crash.

## Retention & privacy

Events contain prompts, file contents, and command output. They stay local
(SQLite in the user's data dir), are covered by the session-deletion flow
(deleting a session deletes its threads' events), and a `retention_days`
setting (default: keep forever) prunes old scopes. Nothing here is uploaded.

## Relationship to checkpoints and audit

- `turn.completed` references the checkpoint created for that turn; undo
  emits `checkpoint.restored` rather than deleting events — the log records
  what happened, the worktree reflects the restore.
- The audit view is a filter over the log (`tool.*`, `approval.*`), not a
  separate store.
