# ADR 0046: Detached descendants of shell calls are session-scoped

Status: Accepted (2026-09)

## Context

trouve owns the complete process tree of every child it launches. On Unix the
tree is bounded by a pipe sentinel that every descendant inherits across
fork, exec, and `setsid()` (ADR 0038); a shell call or background job is only
finished once its process group is empty and nobody holds the sentinel, and
cleanup signals every remaining holder.

That rule treated a daemon that deliberately detaches into its own session
the same as a stray child. The `sccache` server, the `trouve-search` daemon,
package-manager and build-tool daemons all outlive the command that started
them by design, and all inherit the sentinel. On Linux they were killed at
every foreground return, `shell_kill`, cancellation, timeout, and lifetime
cap, so a warm cache never survived a call and a background job whose daemon
was started elsewhere never completed. On platforms that cannot enumerate
sentinel holders (macOS), the foreground cleanup retried without bound and
froze the call — and with it the session mutation lane — for as long as the
daemon lived.

Two smaller defects shared the mechanism. Nothing told the model which
processes a call had killed or left behind. And the desktop host's WebKitGTK
opens descriptors without `O_CLOEXEC` (`/dev/urandom`, `/proc/meminfo`,
cgroup accounting files) that leaked into every child.

## Decision

- **Session, not call, is the ownership boundary for daemons.** A descendant
  whose session differs from the leader's session — one that called
  `setsid()` — is *detached*. The shell tool releases it: it does not keep the
  tree alive, is not signalled when the tree is terminated, and does not
  count toward the completion of a background job. A descendant that only
  left the process group (job control, `setpgid`) is still a tree member and
  is still killed with the tree.
- **Released daemons belong to the session worktree.** Every released daemon
  is recorded, with its process start time as an identity guard against pid
  reuse, in a registry keyed by the worktree that started it. Evicting the
  worktree asks each of its daemons to exit (`SIGTERM`), waits up to two
  seconds, then kills the survivors, after the worktree's background jobs
  have been stopped. Dropping the registry asks every remaining daemon to
  exit without waiting.
- **Release is opt-in per spawn.** Only the shell tool's foreground calls and
  background jobs release detached descendants. Provider transports, MCP
  servers, git, and every other process-tree caller keep terminate-all
  semantics.
- **Cleanup acknowledgement is bounded.** Foreground cleanup and the
  background lifetime-cap path retry an unacknowledged termination three
  times, each attempt bounded by the reap timeout, then report the failure
  (`cleanup_warning`, or the error message of an interrupted call) and close
  the call or job rather than retrying for the lifetime of a process trouve
  cannot see.
- **Platforms that cannot enumerate holders release untracked.** Once the
  process group is empty, such platforms wait at most 500 ms for the sentinel
  to close, then treat the remaining holders as released and mark the result
  `released_untracked`; those processes are not stopped at worktree eviction
  and the result says so.
- **Results report remnants.** A shell result gains `detached` (released
  daemons), `killed_escaped` (descendants that left the process group but not
  the session and were killed), and a human-readable `note` whenever either
  is non-empty. Results of commands that leave nothing behind are unchanged.
- **Children start with only the sentinel and stdio.** Process-tree spawns
  mark every descriptor from 3 upward close-on-exec before re-arming the
  sentinel writer, so descriptors opened without `O_CLOEXEC` elsewhere in the
  host never reach a child.

## Consequences

- Build caches and other daemons keep their state across shell calls; a
  background job completes when the part of the tree it owns has exited.
- The invariant "nothing survives the call" is deliberately weakened to
  "nothing in the leader's session survives the call". The residual is
  bounded: a daemon must have called `setsid()` to be released, it is
  reported to the model, and it is stopped when the session worktree goes
  away. ADR 0043's statement that process cleanup guarantees are unchanged
  is amended accordingly.
- A daemon that inherits the call's stdout or stderr no longer holds the
  foreground call open: once the tree is done the pipes are drained for a
  bounded interval and the call returns what it captured.
- An unacknowledged cleanup is now a reported failure instead of an
  indefinitely quarantined mutation lane. The abandoned tree receives a final
  best-effort kill when its handle is dropped.
- macOS behaviour is inferred from the Linux implementation and the platform
  contract; CI does not exercise it.

## Alternatives rejected

- Keep killing everything and tell users to run daemons outside trouve: the
  daemons in question (`sccache`, package managers) are started implicitly by
  the tools the model is asked to run, and the macOS lane freeze remained.
- Release anything that left the process group: job-control escapees and
  `setpgid` children are ordinary stragglers of the call and must still be
  cleaned up; `setsid()` is the explicit signal that a process intends to
  outlive its parent.
- A global daemon list without worktree ownership: eviction of one session
  must not stop a daemon another session is still using.
