# ADR 0034: Concurrent turns in a shared session worktree

Status: Accepted (2026-08)

Supersedes the whole-turn session-lock portion of ADR 0030.

## Context

Threads in one session share a worktree. Trouve historically held a
mode-dependent session lock for an entire turn: read-only turns could overlap,
but a code turn excluded every sibling until its provider run and all of its
tools finished. A long vendor turn therefore left otherwise independent
threads showing `Processing…`, consumed scheduler capacity while waiting for
the lock, and prevented users from editing through several threads at once.

ADR 0030 already introduced a narrower per-session tool-execution lane that
allows reads to overlap while serializing mutation-capable and unknown tool
calls. The whole-turn writer lock duplicated that protection at a much coarser
granularity.

## Decision

- Every active turn takes a shared session lifecycle lease. Turns from
  different threads in the same session may reason, stream, and invoke tools
  concurrently, independent of mode.
- Destructive session lifecycle operations, including checkpoint restore and
  settings changes that require an idle worktree, retain the exclusive
  lifecycle lease.
- The ADR 0030 tool-execution lane is the worktree safety boundary:
  - known read-only tools share a read permit;
  - mutation-capable and unknown tools take an exclusive write permit;
  - the permit remains cancellation-aware and covers cleanup.
- Automatic checkpoints also take the exclusive tool-execution permit for
  their dirty check, sequence allocation, Git snapshot, and durable record.
  Checkpoints are session-wide worktree moments and may include changes made
  by several concurrent turns; they are not isolated per-turn transactions.
- A turn is projected as waiting after `turn.started` and becomes running only
  after the existing durable `turn.capacity_acquired` event. This distinguishes
  scheduler delay from provider work without adding a side channel.

## Consequences

- Several code threads can make progress in one session. Their reads and model
  work overlap, while actual edits, shell commands, Git operations, and
  conservatively classified MCP calls remain serialized.
- A model can reason from worktree state that another turn changes before its
  next tool call. The mutation lane prevents corruption, but it cannot prevent
  semantic conflicts; agents must re-read or resolve ordinary concurrent
  changes when necessary.
- A long mutation still delays other session tools, but no longer blocks an
  entire sibling turn before that sibling reaches a tool.
- Checkpoint attribution names the turn that observed and recorded the shared
  state, not exclusive ownership of every change in the checkpoint.
- Clients must understand the additive `waiting_for_capacity` folded turn
  state. Protocol 3.25 clients retain the same durable event taxonomy.
