# ADR 0003: Git worktree per session; threads share the session worktree

Status: Accepted (2026-07)

## Context

Concurrent agent sessions on one repository must not trample each other's
edits. Candidates: run everything in the user's checkout, clone per session,
or git worktree per session.

## Decision

- A **workspace** is a registered repository.
- A **session** gets its own git worktree on a dedicated branch
  (`trouve/<session-slug>`), created from a configurable base ref. All agent
  file operations happen inside the session worktree.
- **Threads** are parallel conversations within a session. They share the
  session worktree; worktree mutations (file edits, shell commands, git
  operations) are serialized through a per-session lock. Each thread carries
  its own agent mode, model, and model options.
- Per-turn git checkpoints (commits on a hidden ref) provide undo/redo
  without polluting the session branch history.
- Session cleanup removes the worktree; the branch survives until the user
  deletes it or merges a PR from it.

## Alternatives rejected

- Editing the user's checkout directly: no isolation, no parallel sessions,
  destructive mistakes hit the user's working tree.
- Clone per session: correct but slow and disk-hungry on large repos;
  worktrees share the object store.

## Consequences

- Sessions are cheap to create and destroy; parallel sessions are safe.
- The diff surface for review is always `session-branch...base`, which maps
  directly onto PR creation.
- Repos that misbehave with worktrees (submodule-heavy setups) may need a
  clone fallback later; the session model hides which one is used.
