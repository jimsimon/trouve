# ADR 0049: Explicit ownership for managed background work

Status: Accepted (2026-09)

## Context

ADR 0046 distinguishes shell daemons that deliberately leave their process
session from descendants that a call must terminate. That reactive distinction
is necessary for shell commands, where trouve does not control what the command
starts, but it is not a general background-work contract: holder enumeration is
platform-limited, a detached process has already escaped its original process
tree, and no owner receives the locks or cancellation policy required by the
work.

This became visible when an authenticated review fetch triggered Git's detached
automatic maintenance. The fetch completed, but the maintenance descendant
outlived it and process-tree cleanup timed out. Disabling maintenance forever
would bound the fetch while allowing a persistent review object store to
degrade.

## Decision

- **Background lifetime is declared before launch.** Trouve-owned subsystems
  submit intentional long-lived work to a managed-background registry. A
  descendant daemonizing itself is not an ownership transfer and retains the
  caller's existing terminate-or-release policy.
- **The registry is the owner.** Registered work receives a cancellation token
  and owns its process-tree handle, output drains, and any resource lease for
  its full lifetime. Dropping the registry cancels its work. Processes run in
  foreground mode beneath that owner rather than escaping into an untracked
  daemon.
- **Equal work is coalesced.** A stable scope key permits one active execution
  and at most one requested rerun. Repeated triggers therefore preserve a final
  pass without creating an unbounded task queue.
- **Git maintenance is repository-scoped.** Review fetches suppress their
  inline auto-maintenance, then schedule git maintenance run --auto. The
  managed task acquires the same repository mutex as fetch and ref cleanup,
  forces maintenance.autoDetach=false, and runs under the ordinary bounded
  process-tree owner. Maintenance failure is logged and may be retried by a
  later coalesced trigger; it does not invalidate an otherwise successful
  immutable fetch.
- **Shell daemon adoption remains specialized.** ADR 0046 continues to govern
  commands that trouve does not control. The managed registry does not broaden
  which arbitrary descendants may survive a call.

## Consequences

- Background work can outlive the request that scheduled it without becoming
  unowned, leaking pipes, or dropping a required repository lock.
- Repository housekeeping is kept off the review-fetch critical path while
  still running whenever Git's automatic thresholds say it is needed.
- New intentional background consumers have a reusable ownership and
  coalescing primitive, but still must define a scope key, resource lease,
  timeout, cancellation behavior, and failure policy.
- Shutdown cancellation is cooperative until the managed command observes its
  token; its ProcessTreeChild remains the final terminate-all fallback.

## Alternatives rejected

- Let every detached descendant survive: this treats accidental or hostile
  self-daemonization as authorization and cannot transfer resource ownership.
- Run maintenance synchronously at the end of fetch: it is safe, but makes
  review admission wait for potentially expensive repository housekeeping.
- Disable Git maintenance permanently: persistent shared review repositories
  would accumulate avoidable loose objects and packs.
