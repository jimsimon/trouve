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
  fragments (`assistant.delta`, `assistant.progress`, thinking, and same-call
  `tool.output`) may be losslessly concatenated for a short bounded window
  before persistence;
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
- The store allocates one globally monotonic cursor across all scopes. Scopes
  only filter which events are delivered, so cursors are ordered but not dense:
  clients must drop duplicates and older events but must never require
  `next_cursor == previous_cursor + 1`.
- A client may seed itself from a server-derived snapshot carrying
  `x-trouve-event-cursor`, then subscribe after that cursor. Large folded
  transcripts are transferred as bounded newest-first pages. Snapshots are
  rebuildable projections of this log; they do not replace it as the durable
  source of truth.

Completed folded transcript rows are materialized in the indexed
`thread_view_items` projection, while `thread_view_cache` retains only the
currently mutable turn suffix and its fold indexes. A page request therefore
deserializes at most the live suffix and the requested rows; it never loads a
thread's complete folded history merely to slice one page. Completed tool rows
carry bounded presentation arguments and a `details_deferred` marker. Their
full arguments and terminal result live in the rebuildable
`thread_tool_details` projection and are fetched only when the user expands
that call. Both tables are disposable cache state: invalidating their schema
version rebuilds them from the authoritative thread event stream.

### Session-list bootstrap and resume

Clients must not retain every background thread merely to render the session
inbox. The server therefore maintains a durable `SessionSummary` projection
containing session/workspace ids, archive and activity state, aggregate
approval/question attention, the latest terminal outcome, latest thread id,
source-event cursor, and timestamp.

`GET /v1/session-summaries` returns `{summaries, cursor}` from one SQLite read
transaction. A client replaces its normalized summary map with that snapshot,
then opens the existing server stream at `GET /v1/events?after=<cursor>` and
applies `session.summary_updated` replacements or tombstones. Because each
projection mutation and its derived server event are committed in the same
writer transaction, an update is either already represented by the snapshot
or appears after its cursor; there is no snapshot/stream race window.

`GET /v1/server-projection` supplies the durable replacement state not carried
by `SessionSummary`: the newest cached account PR list per configured GitHub
host, the branch-, `session.pr_opened`-, and chat-mention-derived PR
associations for every session, and session-naming settings. Canonical PR URLs
in durable user-visible chat emit `session.pr_mentioned` on the server scope.
A number without repository identity is not globally unique, so repository-local
shorthand remains limited to live repository-scoped discovery. This keeps
mention-only navigation distinct from verified session creation and lets clients
rank session-created PRs first. Each host slice retains its source event
cursor and timestamp, and the response carries the current server cursor in
`x-trouve-event-cursor`. Clients fetch it after the session-summary boundary,
apply it before opening SSE, and still resume at the earlier session-summary
cursor. Any replacement event that raced the projection request is therefore
replayed and ordered by its own cursor, while cold startup no longer scans the
complete retained server log merely to find the latest replacement events.

Completion, failure, approval, and question source events also derive a
compact `session.notification` edge after their replacement summary in that
same transaction. The edge carries the exact category and source thread plus
an optional bounded failure excerpt or question subtitle. It lets inactive
thread notifications retain the native behavior without one SSE follower per
thread. Notifications remain client policy: snapshot-covered history is not
shown, replay is freshness-gated, focused visible threads are suppressed, and
preference/sound/activation handling stays with each client.

The projection's `latest_cursor` is the source event cursor, while the
snapshot `cursor` is the latest server-scope cursor used for SSE resumption.
They serve different purposes and clients must not interchange them. Because
turn responders cannot survive their owning process, restart appends a durable
`session.recovered` source event and replacement summary for each interrupted
session. Recovery clears stale activity and approval/question attention and
marks the interrupted outcome failed; snapshot and resumed-stream clients
therefore converge on the same cursor-addressed transition.

Conversation tab status uses the same projection pattern at thread scope.
`GET /v1/thread-statuses?session_id=…` returns compact `ThreadStatus` rows for
the selected session, and source turn/approval/question events transactionally
append `thread.status_updated` replacements on the server stream. Clients can
therefore keep open and closed thread tabs current without retaining each
thread transcript or opening one SSE connection per tab. Each row's
`latest_cursor` is the source-event cursor and also drives client-local
seen/unseen terminal indicators. `started_at` records the current or most
recent turn start, while `completed_at` records its terminal event; clients use
that pair for live and completed child-agent elapsed time without replaying the
thread.

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

- `turn.admitted` `{turn, provider_wait_ms}` — provider admission completed;
  the wait measures time spent behind a shared throttling cooldown and is zero
  for immediate admission
- `turn.capacity_acquired` `{turn, wait_ms, background}` — legacy protocol
  7.15-and-earlier marker retained only when replaying existing durable logs;
  new servers do not emit it
- `turn.started` `{turn, mode, model, thinking_level?}` (the effective
  provider-native thinking selection for that turn) / `turn.usage_updated`
  `{turn, usage}`
  (live current-context replacement without ending the turn) /
  `turn.completed` `{turn, usage, checkpoint_id?}` / `turn.failed`
  `{turn, error}`
- `turn.phase_changed` `{turn, phase}` — replaces the transient activity
  label for the running turn (for example, `connecting_tools`); it does not
  add a transcript or tool-rail item
- `user.message` `{turn, content}` — user-authored input only; the legacy
  `background` field is read solely when replaying protocol 7.19–7.26 logs
- `turn.background_activity` `{turn}` — the server attached a turn to
  vendor-autonomous activity; no user prompt is implied
- `subagent.spawned` `{turn, thread_id, session_id, prompt, model, call_id?}` —
  a separately navigable child-agent transcript was attached to the parent
  turn; the optional call id identifies a redundant trouve spawn tool row
- `assistant.delta` `{turn, text}` — streamed model output
- `assistant.progress` `{turn, text}` — streamed user-facing progress authored
  by the agent harness / `assistant.progress_completed` `{turn}` — the harness
  explicitly closed the current progress item
- `assistant.thinking` `{turn, text}` — streamed display-only model reasoning /
  `assistant.thinking_completed` `{turn}` — the provider explicitly closed
  the current thinking item, even when no visible output follows immediately
- `assistant.message` `{turn, content}` — folded final text for the turn
- `assistant.artifacts` `{turn, call_id?, attachments}` — durable
  model/tool-produced files stored behind the attachment endpoint
- `tool.requested` `{turn, call_id, tool, args, requires_approval}`
- `approval.requested` `{turn, call_id}` / `approval.resolved` `{call_id,
  decision, by}`
- `tool.started` `{call_id}` / `tool.output` `{call_id, chunk}` /
  `tool.completed` `{call_id, status, result, execution_duration_ms?}`. The
  optional duration covers only `ToolExecutor::execute`, measured with a
  monotonic clock; clients fall back to durable timestamps for older or
  provider-owned calls.
  Multiple call ids may be live concurrently. Cursor order reflects actual
  request/start/output/completion timing and is not required to match the
  provider transcript's tool-call order; `call_id` is the correlation key.
- `question.requested` `{turn, request_id, title?, questions}` /
  `question.resolved` `{request_id, answers?}`
- `thread.queue_updated` `{prompts}` — the thread's queue of pending prompts
  changed (enqueue/edit/reorder/delete/dispatch); carries the full remaining
  queue in run order, so replaying to the tail reproduces the current queue
- `thread.todos_updated` `{todos}` — the thread's current todo snapshot
  changed; carries the full replacement list while `tool.*` events retain
  the history of how it changed

Session scope:

- `checkpoint.created` `{checkpoint_id, turn, thread_id, ref}`
- `checkpoint.restored` `{checkpoint_id, direction}` (undo/redo/exact)
- `worktree.created` / `worktree.removed` `{path, branch}`

Server scope:

- `workspace.registered` `{workspace_id, path}`
- `thread.status_updated` `{status}` — compact replacement activity,
  attention, outcome, and source cursor for one thread
- `github.pull_requests_updated` `{pull_requests}` — full account-centric
  dashboard snapshot for one configured GitHub host
- `session.created` / `session.deleted` `{session_id, workspace_id}`
- `session.updated` `{session_id, workspace_id}` — session metadata changed
- `thread.created` / `thread.updated` `{thread_id, session_id}`
- `session.activity` `{session_id, workspace_id, active}` — one or more
  threads in the session started work, or the final active thread stopped
- `session.recovered` `{session_id, workspace_id}` — restart reconciled
  process-owned activity and unresolved attention that cannot be resumed
- `session.summary_updated` `{session_id, summary}` — full replacement of
  the transactionally materialized session projection; explicit `null`
  `summary` is the durable deletion tombstone
- `session.notification` `{session_id, thread_id, kind, detail?}` — compact
  notification edge for a completed/failed turn or a newly requested
  approval/question; `detail` is a bounded failure excerpt or question title
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
event or an ordered same-scope batch. Session create/update/delete relational
changes also execute in that writer transaction. For session-relevant source
events, the same transaction updates the `session_summaries` and
unresolved-attention projection tables and appends the derived server-scope
`session.summary_updated` event immediately after its source. Notification-
worthy source events append `session.notification` immediately after that
replacement. The writer then commits and publishes every source and derived
envelope in exact cursor order before acknowledging callers. A subscriber can
therefore never observe an event that would not survive a crash, and summary
state cannot diverge from its notification edge. Per-turn coalescing buffers
are bounded by count and approximate bytes and apply backpressure. Routes on a
multiplexed vendor transport have a bounded event budget and report overload
to only the affected turn, keeping the shared reader available to unrelated
turns and JSON-RPC responses.

## Retention & privacy

Events contain prompts, file contents, and command output. They stay local
(SQLite in the user's data dir). Deleting a session deletes its thread and
session events; durable code-review-job events remain with review history.
A `retention_days` setting (default: keep forever) prunes old scopes. Nothing
here is uploaded except the deliberately published review result.

## Relationship to checkpoints and audit

- `turn.completed` references the checkpoint created for that turn, and the
  folded `ThreadTurnState::Completed` retains that id so clients can offer
  turn-boundary restore/fork actions after replay. Relative undo/redo and
  checkpoint-targeted restore emit `checkpoint.restored` rather than deleting
  events — the log records what happened, while the worktree reflects the
  restore. Forking creates a normal new session/thread lifecycle at the named
  checkpoint and does not copy the source transcript.
- The audit view is a filter over the log (`tool.*`, `approval.*`), not a
  separate store.
