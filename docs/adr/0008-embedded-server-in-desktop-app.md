# 0008 — Desktop app embeds trouve-server in-process

Status: Accepted (2026-07). Amends the local-server mechanism of ADR 0002.

## Context

ADR 0002 put all agent functionality behind `trouve-server` and had the
desktop app spawn it as a child binary. The child-process mechanism carried
real costs that grew as the app matured:

- **Development friction.** `cargo run --bin trouve` built only the app;
  the sibling `trouve-server` binary in the target dir was missing or stale
  unless built separately (a runtime `cargo build` shim papered over this).
- **Distribution and lifecycle complexity.** Binary lookup with
  version-skew risk between app and sibling server, an ephemeral-port
  reservation race, `PR_SET_PDEATHSIG` (Linux-only) to avoid orphaned
  servers, and kill-on-drop plumbing.
- **Mobile is a hard blocker.** The app targets mobile (ADR 0005), and iOS
  forbids spawning child executables outright. A child-process local server
  can never ship there; an in-process server can.

The protocol boundary itself — the actual load-bearing part of ADR 0002 —
never required a process boundary, only that clients speak HTTP + SSE and
stay out of engine internals.

## Decision

- `trouve-server` exposes one bootstrap entry point, `bind_local(addr,
  security)`: it wires the full local stack (store, real config file, index
  hooks, system connectivity probe), binds the address (port 0 for
  ephemeral), and returns the bound address plus the serve future.
- The desktop app depends on the `trouve-server` **library**, spawns that
  future on its runtime with loopback host enforcement
  (`ServerSecurity::loopback`), and talks to it over loopback HTTP + SSE via
  `ProtocolClient` exactly as before. If the server task dies, the app
  restarts it once on the same address (fresh engine over the persisted
  store) before surfacing failure.
- The protocol boundary is enforced by the dependency graph, not
  convention: `trouve-app` declares no dependency on `trouve-core`, so
  engine types are unnameable there, and `trouve-server`'s public API
  exposes bootstrap and routing, not internals. Invariant 1's wording
  changes from "spawns a child process" to "embeds the server but speaks
  the protocol only".
- The standalone `trouve-server` binary remains (a thin `main` over
  `bind_local`) for hosted and self-hosted deployments, and
  `TROUVE_SERVER_URL` still points the app at an external server.
  `TROUVE_SERVER_BIN` is gone — there is no binary to locate.

## Alternatives rejected

- **Keep the child process, auto-build in dev.** A runtime
  `cargo build -p trouve-server` shim fixed the dev-loop symptom only, and
  left the distribution complexity and the mobile blocker in place.
- **Cargo artifact dependencies (`bindeps`)** would let the app build and
  locate the server binary via cargo, but the feature is still
  nightly-only (tracking: rust-lang/cargo#9096), and it too keeps the
  child-process model mobile-incompatible.
- **Linking `trouve-core` directly into the app** (no HTTP to self) would
  be marginally cheaper at runtime but destroys client replaceability and
  protocol-compatibility testing — the actual point of ADR 0002.

## Consequences

- `cargo run --bin trouve` just works: the server is a lib dependency, so
  cargo builds it; no shim, no version skew, no port race (bind :0 and read
  the address), no orphan handling.
- One code path for desktop today and mobile later.
- **Crash isolation is reduced.** A server panic is contained by the task
  boundary (`JoinError`, then restart), but an abort/OOM/stack overflow now
  takes the UI down with it. Accepted: the engine already isolates risky
  work (shell, tools, local models) in its own child processes.
- The app binary links the server stack (axum, SQLite, reqwest, octocrab):
  bigger binary, longer link times for UI-only iteration.
- Server logs share the app's stderr/tracing subscriber.
- The loopback API has no bearer authentication. Host validation blocks
  browser-based DNS rebinding, but another process running as the user can
  reach it; this is accepted for the single-user desktop deployment model.
