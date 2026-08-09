# ADR 0032: Single local server owner per data directory

Status: Accepted (2026-08)

## Context

The default Wry application embeds `trouve-server`, while comparison windows
can attach to an existing server. Starting more than one default application
process could nevertheless create two engines over the same SQLite database.
Durable events were shared through storage, but each server's live SSE
broadcast channel was process-local. A frontend attached to one process could
therefore miss an idle event written by the other and retain an operating
system sleep inhibitor indefinitely.

## Decision

- `trouve_server::bind_local` elects one owner for the configured data
  directory with an operating-system file lock and records that owner's
  loopback address while the serve future is alive.
- The first default desktop process owns the Engine, SQLite store, and server.
  Later default desktop processes attach to the elected owner over HTTP/SSE;
  they do not open the database or construct another Engine.
- Explicit comparison and Servo qualification hosts continue to require
  `TROUVE_SERVER_URL` and never participate in ownership election.
- While sleep inhibition is desired, clients periodically reconcile the
  cursor-fenced session-summary projection. This narrow safety read releases
  an inhibitor after a missed live idle transition without polling unrelated
  workspace, thread, or GitHub state.

## Consequences

- One durable event producer and one live broadcast domain own the default
  database, eliminating cross-process state divergence and SQLite contention.
- Additional product windows remain supported and share the same sessions via
  the protocol, at the cost of depending on the elected process's lifetime.
- A crashed owner releases the file lock; a later process replaces the stale
  address and becomes the new owner.
- Activity reconciliation adds one small session-summary request every 15
  seconds only while work is active and sleep prevention is enabled.

