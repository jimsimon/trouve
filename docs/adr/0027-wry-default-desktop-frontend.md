# ADR 0027: Wry as the default desktop frontend

Status: Superseded by 0028

## Context

ADR 0023 introduced the shared Lit frontend, retained Slint as the initial
default, and required evidence before promotion. The functional port and its
automated parity coverage are now broad enough for sustained daily use, while
keeping Slint as the default splits real-world feedback across two frontends
and delays discovery of system-webview-specific defects. Hardware,
assistive-technology, packaging, and soak evidence is still incomplete, so the
promotion must remain reversible and must not be described as full platform
qualification.

## Decision

- The normal `trouve` desktop entry point uses the Lit frontend in Wry. Wry's
  system webview is the staged product default; Trouve does not adopt Tauri or
  a second durable-state protocol.
- With no `TROUVE_SERVER_URL`, the default process owns exactly one embedded
  server through `trouve_server::bind_local`. Supplying the variable connects
  to that server instead. All durable state and effects still use HTTP/SSE.
- Keep `trouve-slint` as an explicit rollback binary through rollout and soak.
  Keep `trouve-web-preview` and both Servo harnesses as explicit comparison or
  qualification paths that require an existing server and never open the
  default database.
- Continue recording platform, accessibility, security, memory, performance,
  packaging, and soak evidence. This staged default does not mark those gates
  complete and does not authorize removing Slint.
- The shared Lit PWA remains the initial mobile solution; later mobile
  packaging choices remain evidence-driven.

## Consequences

Daily desktop use now exercises the frontend intended to replace Slint, so
browser-engine and host regressions surface earlier. Ordinary builds acquire
Wry and platform-webview dependencies and shipping builds must include the
validated desktop Vite artifact. Only one process may own the default database;
comparison hosts remain explicit-server clients. Slint still provides a fast
rollback and visual reference, and its license attribution remains required
while that artifact is distributed. Retirement still requires a later
decision after successful rollout evidence.
