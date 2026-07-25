# Durable code-review job artifacts and event streams

Status: Accepted (2026-07)

## Context

ADR 0011 made the server responsible for a durable review queue while each
reviewer still runs through ordinary trouve sessions and threads. Those
sessions are implementation details and are removed after a job finishes.
Consequently their event streams cannot be the durable source for job output,
per-persona progress, historical timing, issue attribution, retries, or
incremental review state.

The dashboard must reconnect to live work, inspect completed work, and compute
global and per-repository history. GitHub publication also needs stable
identifiers for lifecycle comments, Check Runs, inline findings, and the
reviewed commit watermark.

## Decision

- A code-review job is a first-class persisted scope in the event log:
  `code_review_job:<id>`. Reviewer thread events that are useful to operators
  are projected into this scope while the underlying thread runs.
- Persist each reviewer/coordinator execution as a job task, including its
  actual model, status, timestamps, prompt, final output, and issue counts.
  Session and thread identifiers remain traceability links rather than the
  owner of review history.
- Persist published findings and their many-to-many source persona
  attribution. Historical issue counts are immutable facts about that review;
  later resolution is separate state.
- Persist the last successfully published head commit per pull request.
  Incremental jobs use that commit when it remains an ancestor of the new
  head. Full jobs explicitly use the pull request base.
- Cancellation is cooperative and terminal. Retrying creates a new job linked
  to its predecessor; it never reuses or rewrites the old execution.
- GitHub lifecycle comments and Check Runs are projections of durable job
  state. Their remote identifiers and last synchronization error are stored so
  reconciliation is idempotent after process or network failure.
- Historical statistics are derived from durable job, task, and finding rows.
  Active work is a current snapshot; terminal latency percentiles exclude
  right-censored active durations.

## Consequences

Completed review output and timing survive session cleanup, SSE replay works
for review jobs, and the same records drive progress, detail views, comments,
Check Runs, incremental watermarks, and statistics. Storage grows with review
history and is subject to the server's review retention policy rather than
session deletion. Projection code must tolerate GitHub updates failing after a
local state transition and retry them idempotently.

## Alternatives rejected

Keeping reviewer sessions indefinitely would leak implementation details into
the product model and retain unrelated conversation events. An in-memory
output stream would lose reconnect and crash recovery. Rewriting a job in
place for retries would destroy auditability and make remote publication races
ambiguous.
