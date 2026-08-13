# ADR 0026: Desktop frontend asset sources

Status: Accepted (2026-08)

## Context

ADR 0023 puts the Lit application behind an app-owned loopback gateway so its
native capabilities and protocol proxy remain same-origin. The first Wry
preview embedded Vite output at Rust compile time, while the isolated Servo
harness loaded the same output at process startup. That drift made Wry retain
stale frontend bytes and forced a Rust rebuild after every web build. Loading a
Vite server directly would improve iteration but would move the page away from
the gateway origin and complicate the host bridge, CSRF, and navigation
boundaries.

## Decision

The desktop host owns one shared frontend-source policy used by Wry and every
Servo qualification launcher:

- Shipping product builds serve only a compile-time packaged asset manifest.
- Debug and qualification hosts may snapshot a validated desktop Vite output
  selected by `TROUVE_APP_UI_DIST` at process startup.
- Debug and qualification hosts may instead proxy frontend HTTP requests to an
  explicit credential-free loopback Vite origin selected by
  `TROUVE_APP_UI_DEV_URL`. The Vite HMR socket connects only to that exact
  loopback origin under a source-specific development CSP.
- The gateway remains the page origin in every mode. Native host routes and
  `/v1` protocol routes always take precedence over frontend proxying.
- Runtime-directory and Vite sources are mutually exclusive. They are disabled
  for shipping product hosts even if the corresponding environment variables
  are present.

## Consequences

Wry and Servo exercise identical asset selection and gateway routing. A normal
desktop build can be refreshed without recompiling Rust, and `vite dev` can
provide HMR without widening the release CSP or exposing native capabilities
cross-origin. Qualification binaries may use runtime sources in optimized
builds because they are never shipping hosts.

The development server must already be running, use the configured loopback
origin, and expose its HMR socket there. Packaged builds retain the existing
offline and immutable-asset guarantees.

## Alternatives rejected

- Navigate the webview directly to Vite: this splits the page from the trusted
  gateway origin and requires new CORS/CSRF rules for native capabilities.
- Embed debug assets at compile time: this recreates stale processes and makes
  frontend iteration depend on Cargo.
- Map arbitrary request paths directly onto a live directory: a startup
  snapshot is simpler to validate and avoids turning the host into a general
  filesystem server.
