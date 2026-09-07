# ADR 0043: Background jobs release the session mutation lane

Status: Partially superseded by ADR 0046 (2026-09) — descendants that detach
into their own session are released from the job and owned by the session
worktree instead of being reaped with the tree.

## Context

ADR 0030 made mutation-capable tool calls exclusive within a session and
transferred a background shell call's write permit to its process waiter until
the complete process tree exited. That prevented concurrent worktree mutations,
but it also made the documented long-running-job mode unusable for development
services. A Vite server, for example, retained the lane needed by a browser MCP
call, so the browser could not inspect the server until the server stopped.

A background service is intentionally useful while later agent work continues.
Treating its entire lifetime as one unfinished tool call confused process
ownership with the synchronization boundary of the launch operation.

## Decision

- A background shell launch holds the exclusive session mutation lane through
  process-tree creation, job registration, and publication of its job id.
- The lane is released when that tool call returns. It is not transferred to
  the process waiter.
- The job registry continues to own the full process tree. Poll, kill,
  cancellation, session cleanup, and lifetime-cap behavior still wait for and
  reap every descendant.
- Background commands remain mutation-capable for permission purposes. This
  decision changes concurrency after an approved launch, not whether launching
  the command requires approval.
- Callers accept that a deliberately persistent background process may observe
  later worktree changes or perform its own activity while subsequent tools
  run. Agents should use foreground shell execution when a command and its
  worktree effects must remain isolated until completion.

## Consequences

- Development servers can remain available while browser automation, edits,
  and other tools run in the same session.
- Mutation-capable tool invocations remain serialized, but a process explicitly
  launched as a managed background job is no longer considered an active tool
  invocation after its job id is returned.
- Process cleanup guarantees are unchanged and remain independent of the
  session mutation lane.
- Long-running background commands are a deliberate concurrency boundary;
  they must not be used when their ongoing writes require exclusive worktree
  access.

## Alternatives rejected

- Classifying browser MCP tools as read-only would not help while a background
  shell retained an exclusive write permit, and would conflate permission risk
  with worktree behavior.
- Special-casing Vite or Playwright would leave every other development server
  and browser tool combination broken.
- Keeping lifetime-long leases preserves strict exclusion but contradicts the
  purpose and documented use of managed background jobs.
