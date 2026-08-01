# Event log design

The event log is the spine of the trouve harness: every piece of UI-visible
state flows through it, replay/reconnect reads from it, checkpoints/undo
reference it, and it doubles as the audit trail (ADR 0002, invariant 2).
This document defines its semantics before any endpoint exists; changes here
are protocol changes.

## Model

- Events are **append-only** and **per-scope**. A thread is the unit of
  conversation; sessions, code-review jobs, and workspaces have lifecycle
  events too, carried on their own streams (see "Scopes").
- Every event has a **cursor**: a `u64` strictly increasing *within its
  scope*, assigned at append time by the store (SQLite `AUTOINCREMENT`
  rowid). Cursors are opaque to clients except for ordering and resumption.
- Events are **immutable**. Corrections are new events (e.g.
  `message.aborted`), never rewrites.
- Streaming deltas are events like everything else. Adjacent transport
  fragments (`assistant.delta`, thinking, and same-call `tool.output`) may be
  losslessly concatenated for a short bounded window before persistence;
  chunk boundaries and per-fragment timestamps are not semantic. Replay must
  reproduce the exact concatenated content and control-event ordering. A
  future compaction pass may fold deltas older than the last checkpoint into
  their final message/output — clients must not depend on deltas being
  retained forever, only on the folded form being equivalent.

## Scopes

Each event row belongs to exactly one scope:

| Scope | Stream | Examples |
| --- | --- | --- |
| `thread:<id>` | `GET /v1/threads/:id/events` | deltas, tool calls, approvals, turns |
| `session:<id>` | `GET /v1/sessions/:id/events` | checkpoints, undo/redo, worktree lifecycle |
| `code_review_job:<id>` | `GET /v1/code-review/jobs/:id/events` | task state, agent output, progress |
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

- `turn.capacity_acquired` `{turn, wait_ms, background}` — shared/provider
  capacity was acquired; background review work uses a lane that reserves
  capacity for interactive desktop turns
- `turn.started` `{turn, mode, model}` / `turn.completed` `{turn, usage,
  checkpoint_id?}` / `turn.failed` `{turn, error}`
- `model.route_selected` `{turn, model, provider_id, provider_model, reason}`
  — the concrete route chosen for an `auto/` model; the first route
  uses `reason: initial`, then repeats with `reason: capacity_failover` when an
  exhausted route hands off the turn, or `reason: route_failover` when a safely
  retryable unavailable route does so
- `user.message` `{turn, content}`
- `assistant.delta` `{turn, text}` — streamed model output
- `assistant.message` `{turn, content}` — folded final text for the turn
- `tool.requested` `{turn, call_id, tool, args, requires_approval}`
- `approval.requested` `{turn, call_id}` / `approval.resolved` `{call_id,
  decision, by}`
- `tool.started` `{call_id}` / `tool.output` `{call_id, chunk}` /
  `tool.completed` `{call_id, status, result}`
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
- `settings.git_worktrees_updated` `{settings}` — full replacement snapshot
  after the session-title model's load policy, installation progress, or
  runtime state changes
- `settings.code_review_updated` `{settings}` — full replacement snapshot
  after the automated-review total, reviewer, or final-editor deadline
  changes

Code-review-job scope:

- `code_review.task_updated` `{job_id, task}` — a reviewer or coordinator task
  changed status or accumulated durable output
- `code_review.output_delta` `{job_id, task_id, stream, text}` — live
  assistant, reasoning, or tool output projected from the underlying thread
- `code_review.progress_updated` `{job_id, completed_reviewers,
  total_reviewers, percent}` — reviewer-level progress changed
- `code_review.job_updated` `{job_id}` — other durable job state changed

## Persistence

One SQLite table:

```sql
CREATE TABLE events (
  cursor     INTEGER PRIMARY KEY AUTOINCREMENT,
  scope_kind TEXT NOT NULL,      -- 'thread' | 'session' | 'code_review_job' | 'server'
  scope_id   TEXT NOT NULL,      -- '' for server scope
  ts         TEXT NOT NULL,      -- RFC 3339
  payload    TEXT NOT NULL       -- JSON event body
);
CREATE INDEX events_scope ON events (scope_kind, scope_id, cursor);
```

The cursor is globally unique (single AUTOINCREMENT) which trivially
guarantees per-scope monotonicity; per-scope density is *not* guaranteed and
clients must not assume consecutive cursors.

Writes go through a single event-writer chokepoint. Callers may submit one
event or an ordered same-scope batch. The writer assigns cursors, commits the
transaction, and only then publishes envelopes to in-process subscribers and
acknowledges the caller. A subscriber can therefore never observe an event
that would not survive a crash. Per-turn coalescing buffers are bounded by
count and approximate bytes and apply backpressure. Routes on a multiplexed
vendor transport have a bounded event budget and report overload to only the
affected turn, keeping the shared reader available to unrelated turns and
JSON-RPC responses.

## Retention & privacy

Events contain prompts, file contents, and command output. They stay local
(SQLite in the user's data dir). Deleting a session deletes its thread and
session events; durable code-review-job events remain with review history.
A `retention_days` setting (default: keep forever) prunes old scopes. Nothing
here is uploaded except the deliberately published review result.

## Relationship to checkpoints and audit

- `turn.completed` references the checkpoint created for that turn; undo
  emits `checkpoint.restored` rather than deleting events — the log records
  what happened, the worktree reflects the restore.
- The audit view is a filter over the log (`tool.*`, `approval.*`), not a
  separate store.
