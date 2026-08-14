# ADR 0028: Retire the Slint frontend

Status: Partially superseded by ADR 0039 (2026-08)

## Context

ADR 0023 introduced the shared Lit frontend and required Slint to remain until
a desktop webview passed the migration gates. ADR 0027 then made Wry the
reversible default while rollout evidence accumulated. The Lit/Wry frontend is
now the daily product path, implements the application surface, and has the
protocol, browser, accessibility, visual, and host regression coverage needed
to own that path. Keeping the unused rollback duplicates a large UI, retains
four bespoke widget crates, lengthens builds, and keeps a licensing obligation
for an artifact that is no longer distributed.

## Decision

- Wry hosting the shared Lit application is the sole shipping desktop
  frontend. The same Lit application remains the PWA frontend.
- Remove the `trouve-slint` rollback binary, Slint application sources, the
  four `trouve-slint-*` widget crates, and all Slint build dependencies.
- Keep the Servo embedding workspace as a qualification host for the same Lit
  application; it is not a second product UI.
- Make Trouve's CSS theme palettes and browser regression suite authoritative
  for visual continuity. Preserve historical migration audits as records, not
  live build inputs.
- Remove AboutSlint attribution and Slint-specific license handling once no
  workspace or distributed artifact links Slint.

This decision supersedes ADRs 0006 and 0027 and completes the retirement step
anticipated by ADR 0023.

## Consequences

The workspace has one product UI implementation, a smaller Rust dependency
graph, faster builds, and no Slint attribution obligation. There is no native
Slint rollback binary; regressions must be handled through Wry fixes, the
protocol-compatible PWA, or a normal source rollback. Historical Slint sources
remain available through version control, while current visual behavior is
guarded by the Lit component gallery, semantic tokens, and browser tests.

## Alternatives rejected

- Keep Slint indefinitely as an emergency binary: this preserves immediate
  rollback but also preserves the duplicated maintenance and licensing cost.
- Retain the standalone Slint widgets after deleting the app: no current
  product consumes them, so maintaining and releasing them would be unrelated
  to Trouve's active UI architecture.
