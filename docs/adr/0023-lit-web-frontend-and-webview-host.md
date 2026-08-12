# ADR 0023: Lit web frontend and gated webview host

Status: Partially superseded by ADR 0028 (2026-08)

ADR 0028 deliberately retired the Slint rollback and made Wry/Lit the sole
shipping product frontend. The migration and rollback requirements below are
retained as the historical decision that governed the transition; ADR 0028 is
authoritative for the current frontend and rollback policy.

## Context

ADR 0005 selected Slint for native clients and deferred a separate web SPA.
The shipped Slint client proved the protocol-first boundary, but maintaining
custom code, diff, Markdown, and terminal widgets now costs more than using
the mature web ecosystem. A web frontend also provides the shortest path to
an installable mobile client. Servo is attractive as a Rust-controlled engine,
but its embedding, accessibility-action, and target support are not yet strong
enough to assume it can ship.

## Decision

- Build the application frontend as a Lit and TypeScript package named
  `@trouve-ai/app-ui`. Use `@lit/context` for stable scoped services and
  stores. Use `@lit-labs/signals` only behind a Trouve-owned adapter so its
  experimental API is replaceable.
- Preserve the Slint frontend's themes, semantic colors, layout, density,
  information hierarchy, and core interactions. Web control chrome may differ
  when it retains Trouve styling and behavior; the migration is not a redesign.
- Use self-hosted WebAwesome Free for ordinary controls and project-owned
  components around CodeMirror, MergeView, xterm.js, and sanitized Markdown
  for product-specific surfaces.
- Keep `trouve-app` as the desktop product. An app-owned loopback gateway
  serves static assets, transparently proxies HTTP/SSE to the embedded
  `trouve_server::bind_local` server or a configured remote server, and exposes
  a narrow, separately versioned native-capability API. Durable agent state
  never flows through that native API.
- Qualify Servo against explicit accessibility, platform, security, memory,
  and widget gates. Qualify a maintained system-webview implementation in
  parallel. Ship Servo only if it passes; otherwise use the system webview.
  Keep Slint until a webview passes the complete parity and soak gates.
- Deliver the initial mobile client as a PWA using the same Lit frontend and
  the public HTTP/SSE protocol against a remote server. Capability-gate browser
  limitations. Evaluate native or embedded mobile alternatives later from PWA
  usage and platform evidence.
- Retire Slint and its attribution only in a later change after the selected
  desktop webview has shipped successfully and no distributed artifact links
  Slint.

## Consequences

Trouve gains mature web widgets, one responsive frontend for desktop and PWA,
and browser development and testing tools. It also gains a Node build chain,
browser-engine and mobile test matrices, a security-sensitive local gateway,
and higher memory risk. Two desktop frontends coexist during migration.
Engine promotion, visual parity, accessibility, memory, offline packaging,
and rollback are release gates rather than follow-up work.

## Alternatives rejected

- Continue investing only in Slint: avoids the migration but retains bespoke
  widget cost and delays the web/mobile path.
- Require Servo without a fallback: current accessibility and target gaps make
  that an unacceptable release dependency.
- Use Preact for the main app: it fits the review dashboard, but Lit integrates
  more directly with WebAwesome and reusable custom elements.
- Ship Electron: its bundled runtime and process model conflict with the
  application's memory goals.
