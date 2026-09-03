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
  exit without waiting. The record is pruned of daemons that have exited
  once it grows past a threshold; a live daemon is never forgotten, because
  a forgotten daemon is one nobody stops.
- **The tree's sentinel is retained with its daemons.** A released daemon
  keeps forking: a build server spawns workers, a package manager spawns
  helpers, and none of them is on any record. They do inherit the tree's
  sentinel, so the registry keeps a duplicate of the sentinel reader for
  every released tree and, at eviction, enumerates its holders alongside the
  recorded daemons, stops them the same way, and keeps sweeping the holders
  for the grace period so a daemon that forks while being stopped does not
  leave a worker behind. The sentinel is dropped once the worktree is
  evicted or nobody holds it any more.
- **Signals are bound to the recorded incarnation.** Where the kernel offers
  a pidfd (Linux 5.3+), every released daemon and every sentinel holder is
  signalled through one opened while its start time was verified, so a pid
  recycled between a liveness check and the signal is never hit and the
  registry can tell a daemon that has exited from one that is merely
  reparented. Where pidfds are unavailable or refused, the signal goes to
  the pid after an identity check against `/proc`.
- **Hand-over and eviction are atomic.** A daemon enters the registry only
  after the eviction record is checked under the registry lock, and eviction
  drains a worktree and records the eviction under the same lock. A daemon
  released by a call or job that was still finishing while its worktree was
  evicted therefore arrives after the eviction is on record and is stopped
  on the spot instead of being kept for a worktree nobody will evict again;
  its result reports it as `stopped_after_eviction`, not as released. The
  eviction record is bounded (the oldest of 4096 are forgotten first); late
  hand-overs can only come from work that was in flight at eviction.
- **Release is opt-in per spawn.** Only the shell tool's foreground calls and
  background jobs release detached descendants. Provider transports, MCP
  servers, git, and every other process-tree caller keep terminate-all
  semantics.
- **Cleanup acknowledgement is bounded.** Foreground cleanup, `shell_kill`,
  worktree eviction, and the background lifetime-cap path retry an
  unacknowledged termination three times, each attempt bounded by the reap
  timeout, then report the failure (`cleanup_warning`, or the error message
  of an interrupted call or a failed kill) and close the call or job rather
  than retrying for the lifetime of a process trouve cannot see. A job whose
  kill failed is closed all the same, so `shell_output` stops reporting it
  as running.
- **Release exists only where holders can be told apart.** Classifying a
  holder as detached needs its session id, which trouve reads from `/proc`
  on Linux and Android. Elsewhere (macOS, the BSDs) the shell tool keeps
  terminate-all semantics: a daemon in its own session keeps the tree alive
  until it exits or the bounded cleanup gives up and reports it. Releasing
  holders that cannot be named would have meant releasing same-session
  escapees too, and promising an eviction-time stop the registry could not
  keep. A macOS enumeration over libproc (`proc_listpids`,
  `proc_pidfdinfo` with `PROC_PIDFDPIPEINFO` matched against the sentinel's
  pipe identity, `getsid`) is the path to parity; the tool description says
  which behaviour the platform has.
- **Results report remnants.** A shell result gains `detached` (released
  daemons), `killed_escaped` (descendants that left the process group but not
  the session and were killed), `stopped_after_eviction` (daemons released
  after their worktree was evicted and therefore being stopped), and a
  human-readable `note` whenever any is non-empty. Results of commands that
  leave nothing behind are unchanged.
- **Children start with only the sentinel and stdio.** Process-tree spawns
  mark every descriptor from 3 upward close-on-exec before re-arming the
  sentinel writer, so descriptors opened without `O_CLOEXEC` elsewhere in the
  host never reach a child. The strategy is chosen in the parent, where
  allocation and logging are allowed, and only its result runs between fork
  and exec: `close_range(CLOSE_RANGE_CLOEXEC)` where the kernel supports it
  (Linux 5.11+), otherwise the descriptor table listed from `/proc/self/fd`
  or `/dev/fd` — trusted only when it names both ends of the sentinel pipe —
  and as a last resort a walk of every descriptor number below the soft
  `RLIMIT_NOFILE`, logged once because it can be slow. The walk is complete
  or it does not happen: when the soft limit is unlimited or above 2^20 —
  container runtimes hand out limits in the billions, and walking one would
  stall every spawn for minutes — and neither `close_range` nor a listing
  is available, the spawn fails with an error naming the limit rather than
  sanitize part of the table and leak the rest. Only a kernel older than
  5.11 without a listable `/proc` or `/dev/fd` can reach that error.

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
- Each released tree costs the host one retained descriptor, and each
  released daemon one pidfd, until the worktree is evicted. When a pidfd
  cannot be opened the daemon is still recorded and signalled by pid after
  an identity check, so descriptor pressure degrades the pid-reuse guard,
  never the stop at eviction.
- The descriptor hygiene has one residual gap: a descriptor a process opened
  above the soft `RLIMIT_NOFILE` before the limit was lowered survives the
  walk, because the walk stops at the limit. The two strategies ahead of it
  have no such gap, and the walk is never entered where either is available.
- macOS and the BSDs do not release daemons. The call-scoped semantics they
  keep are the ones every platform had before this decision, with the
  bounded cleanup acknowledgement on top; the change there is that an
  unstoppable daemon now fails the call after three attempts instead of
  holding it indefinitely. CI does not exercise these platforms; their
  behaviour is inferred from the Linux implementation and the platform
  contract.

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
