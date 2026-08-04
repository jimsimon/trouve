# Bounded thread-view pages

Status: Accepted (2026-08)

## Context

ADR 0021 replaced cursor-zero event replay with a server-derived thread view.
That bounds replay work but not the folded result: a long thread can still
produce a multi-megabyte response containing thousands of rich chat items.
The desktop client then formats every item before applying its rendered-row
window, blocking the UI during thread selection and live updates.

## Decision

Pagination-aware clients request bounded pages of the durable folded
projection, beginning with its newest page. Responses carry the page's
folded-item offset, the total item count, and whether older items remain.
Clients subscribe to events at the response cursor as before and request
preceding pages by exclusive item offset when the reader approaches the top of
loaded history. Omitting the page limit retains the complete response for
compatibility with protocol 2.3 clients.

The server keeps the complete projection as a rebuildable cache; pagination is
a transport and client-materialization boundary, not a second source of truth.

## Consequences

- Opening a thread has bounded response size and UI formatting work regardless
  of retained event history.
- Clients must preserve the reader's row anchor while prepending older pages.
- Folded-item offsets are projection-local, not durable event cursors. Clients
  discard non-contiguous pages and can refresh if history changes concurrently.
- Loading the entire history remains possible, but only through explicit,
  incremental reader navigation.
