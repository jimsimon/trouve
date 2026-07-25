# Preact application for the review dashboard

Status: Accepted (2026-07)

## Context

ADR 0005 established the review dashboard as a separately deployed TypeScript
web client. Its first implementation used imperative DOM replacement in one
large file. That approach made navigation, live job updates, filters, elapsed
timers, accessible dialogs, and historical charts increasingly difficult to
compose and test.

The dashboard remains a small operational application rather than a general
component platform. It needs a lightweight component model and charting, but
does not need server-side rendering or a full-stack web framework.

## Decision

- Build the existing `@trouve-ai/review-ui` application with Preact and
  TypeScript.
- Use URL-addressable application sections rendered inside a persistent
  sidebar shell. A narrow viewport collapses the sidebar into a tab-style
  navigation bar.
- Use Chart.js for time-series and distribution charts. Every chart is backed
  by the same textual summary or table so the information remains accessible.
- Keep all server communication in a typed API module. Components never reach
  into server internals and continue to use the versioned HTTP/SSE protocol.
- Prefer small local components and CSS over introducing a second design
  system dependency. Native controls retain their platform semantics.

## Consequences

The dashboard gains explicit state and lifecycle management for reconnecting
streams, polling fallbacks, filters, timers, and charts. Preact and Chart.js
become runtime dependencies of the independently deployed web application.
The existing Vite deployment model and protocol boundary do not change.

## Alternatives rejected

Continuing with whole-page `innerHTML` replacement would keep the dependency
count lower but compound state-loss and maintainability problems. A larger
full-stack framework would add routing and rendering machinery that the static
dashboard does not use. A hand-built canvas chart library would duplicate
well-tested interaction and accessibility behavior.
