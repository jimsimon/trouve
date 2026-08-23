# ADR 0042: Provider-governed turn admission

Status: Accepted (2026-08)

Partially supersedes ADRs 0015 and 0034.

## Context

Trouve historically bounded model turns with global, background, and
per-provider semaphores. Those fixed engine limits constrained desktop users
who routinely run many sessions and coupled review throughput to harness
defaults that could not represent provider-specific capacity. Removing the
permits also made the durable `turn.capacity_acquired` event inaccurate: no
foreground or background scheduling lane was being acquired.

Existing event logs must remain replayable, and protocol event meanings are
never repurposed.

## Decision

- Ordinary model turns have no fixed engine concurrency ceiling. Providers
  enforce their own capacity; the review service separately bounds admitted
  jobs with its configurable maximum-parallel-reviews setting.
- The engine retains a shared per-provider exponential cooldown after a
  throttling response. Turns honor active and extended cooldowns, then proceed
  without a recovery semaphore or gradually opened engine lane.
- Protocol 7.16 adds `turn.admitted {turn, provider_wait_ms}`. New servers emit
  it when any provider cooldown wait has ended and the turn may start provider
  work.
- `turn.capacity_acquired` remains in the event union only so current clients
  can replay durable logs written by protocol 7.15 and earlier. New servers do
  not emit it, and projections fold both markers to the same running state.

## Consequences

Desktop and spawned-agent throughput scales to provider capacity instead of
engine defaults, while review-job concurrency remains operator controlled.
Provider throttling may produce a burst when a cooldown expires; that is an
explicit consequence of uncapped admission rather than a hidden recovery cap.

The additive event requires a protocol minor-version bump, regenerated schema
and clients, and dual-event replay coverage until legacy logs no longer need to
be supported.

## Alternatives rejected

Keeping the old event and redefining it as cooldown completion would silently
change a durable wire meaning. Introducing a bounded post-cooldown probe would
recreate provider admission control in the engine and conflict with uncapped
turn execution.
