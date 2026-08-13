# Materialized pageable thread history

Status: Accepted (2026-08)

## Context

ADR 0022 bounded thread-view responses but retained one serialized projection
containing the complete folded transcript. Serving any page still decoded that
entire value, and completed tool arguments/results dominated its size. Client
wheel-driven pagination and several independent scroll-correction systems also
competed with native scrolling, causing repeated loads, jumps, and jitter.

## Decision

- The durable event log remains authoritative, while completed folded history
  is materialized into independently addressable, indexed item rows.
- The thread projection cache contains only current interaction state and an
  unmaterialized live tail. Page requests read only the rows and tail slice
  needed for that response.
- Completed tool arguments and results are stored separately. History pages
  carry a bounded presentation summary and clients fetch full details only
  when a tool call is expanded.
- Clients retain bounded thread page caches and merge fresh tail snapshots
  into compatible cached ranges.
- An intersection sentinel prefetches history above the reader. Native
  scrolling is authoritative except for one measured prepend correction,
  explicit bookmark/tail navigation, and height changes above the anchor.

## Consequences

- Page latency, response memory, and steady-state projection memory are
  proportional to the requested page/live turn rather than total history.
- The first access after a projection schema change may rebuild materialized
  rows lazily from the event log.
- Expanding a completed historical tool call may require one additional
  request, while collapsed history stays compact.
- Scroll coordination has one prefetch trigger and one prepend anchor instead
  of wheel-intent queues and competing correction timers.

## Alternatives rejected

- Continuing to slice one large JSON projection: bounds transport only, not
  server work or memory.
- Loading every completed tool payload with each page: preserves avoidable
  multi-megabyte responses for content the reader has not expanded.
- Replacing native scrolling with an application-owned scroll position:
  fights wheel momentum and browser scrollbar behavior.
