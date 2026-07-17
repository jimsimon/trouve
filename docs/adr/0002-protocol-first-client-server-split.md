# ADR 0002: Protocol-first client/server split (OpenAPI + SSE event log)

Status: Accepted

## Context

The harness must support desktop, mobile, and web clients today and cloud
agents later, without rewriting the core. OpenCode and Codex both converged on
a headless server speaking a typed protocol, with thin clients. Transport
candidates: WebSockets, JSON-RPC over stdio, gRPC, and HTTP + Server-Sent
Events.

Doing nothing — baking agent logic into a UI process — would make every
future client a rewrite and remote/cloud modes impossible.

## Decision

- All agent functionality lives in a headless Rust server (`trouve-server`,
  axum). Clients — including the bundled desktop app — talk to it over the
  protocol only. The desktop app spawns a local server; remote/cloud modes
  reuse the same protocol.
- Commands are HTTP POST endpoints defined in an OpenAPI schema (generated
  from `trouve-protocol` types via utoipa). The schema is versioned; a
  snapshot test fails CI when the schema changes without a version bump.
- Server → client updates are a single append-only event log per thread,
  delivered over SSE. Every event has a monotonically increasing cursor;
  clients reconnect with `Last-Event-ID` and replay from their cursor. The
  event log is persisted (SQLite), so replay survives server restarts and
  doubles as the audit trail. Checkpoints/undo hang off the same log.
- The event-log schema is designed before the first endpoint is built
  (see `docs/design/event-log.md`).

## Alternatives rejected

- WebSockets: bidirectional but heavier to proxy, no built-in replay
  semantics; SSE + POST covers our needs and is trivially debuggable
  (`curl`).
- gRPC: poor browser story without a proxy; we want the web client to talk
  to the server directly.
- stdio JSON-RPC (Codex-style): great for a single local UI, but we need
  remote clients from day one of the mobile phase.

## Consequences

- Clients are replaceable; protocol compatibility is testable in CI.
- Cursor-based replay makes reconnects (mobile!) and multi-client views
  cheap.
- SSE is one-directional; interactive tools (PTY input) use dedicated POST
  endpoints, which is acceptable.
