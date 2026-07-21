# ADR 0011: Bounded, coalesced event ingestion

Status: Accepted (2026-07)

## Context

ADR 0002 makes the persisted event log the UI, replay, and audit source of
truth. Streaming providers and vendor agents can emit thousands of tiny text,
thinking, or command-output fragments in a burst. Persisting every
transport-selected fragment with a synchronous durability wait lets database
latency propagate back through bounded adapter channels. A full Codex route
then discarded the whole route, failing an otherwise healthy turn.

The event log itself was not full. The overload was in the live ingestion
pipeline, and an unbounded channel would merely trade turn failures for
unbounded memory growth.

## Decision

- Delta chunk boundaries are not semantic. Adjacent text, thinking, and
  same-call tool-output fragments are losslessly coalesced for a short bounded
  window and size before core consumes them. Control and terminal events are
  never coalesced.
- Adapter mailboxes remain bounded by both message count and approximate
  bytes. Saturation applies ordered backpressure to deltas and control events;
  it never deletes a live route. An indivisible message larger than the byte
  budget is admitted only to an otherwise empty mailbox.
- The event writer accepts same-caller batches asynchronously. It assigns
  cursors in input order, commits the transaction, and only then broadcasts
  and acknowledges each event, preserving ADR 0002's persist-before-publish
  guarantee.
- Queue high-water marks, coalescing ratios, backpressure, batch sizes, and
  slow commits are observable through structured tracing.

## Consequences

- Replay preserves byte-for-byte content and event ordering, but clients must
  not depend on provider-selected delta boundaries or per-fragment timestamps.
- Normal bursts consume fewer SQLite rows, transactions, broadcasts, and UI
  updates. A single turn benefits from batching rather than requiring
  coincident traffic from other turns.
- Truly stalled consumers eventually backpressure their producer within a
  bounded memory budget. A shared vendor transport can therefore slow while a
  route is stalled, but healthy turns are not silently detached.
- The protocol schema and event taxonomy do not change.

## Alternatives rejected

- Unbounded channels: permit runaway command output to exhaust process memory.
- Only increasing channel capacity: delays the same failure at a higher
  threshold.
- Dropping deltas or routes: breaks exact replay and can discard terminal
  events.
- Relaxing SQLite durability: changes crash guarantees without addressing
  arbitrary producer bursts.
