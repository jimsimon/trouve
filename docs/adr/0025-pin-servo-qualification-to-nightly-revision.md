# ADR 0025: Pin Servo qualification to a nightly revision

Status: Superseded by ADR 0039

## Context

The published Servo 0.4.0 crate no longer represents the current engine state
that the Lit frontend needs to qualify. In particular, selection remains a
moving, partial implementation: current builds support keyboard selection in
editable controls, while ordinary mouse/touch document selection is still an
open upstream issue. Servo's public embedding API also remains under active
development, while a floating dependency on `main` would make builds and test
results irreproducible.

Servo publishes dated nightly artifacts tied to exact upstream revisions. The
nightly source still conflicts with the product server's native SQLite link
version, so ADR 0024's isolated nested workspace remains necessary.

## Decision

Build the disposable Servo embedding harness from the exact upstream revision
identified by the most recent successfully published Servo nightly at the time
of a deliberate refresh. Record both the nightly date and full commit in the
dependency, lockfile, startup diagnostics, and harness documentation. Never
track a branch or an unpinned Git reference.

Nightly refreshes are explicit qualification changes. They require rebuilding
the isolated harness and rerunning the relevant compatibility gates; they do
not promote Servo or change the shipping desktop host.

## Consequences

The harness can evaluate improvements newer than the last crates.io release
while remaining reproducible. Its Git dependency and independent lockfile need
separate dependency, license, and security review. Public API churn may require
adapter changes on each refresh.

The nightly's partial selection support does not satisfy the full interaction
gate. Mouse/touch selection of ordinary document text remains blocked on Servo
upstream support and must be retested on future refreshes.

## Alternatives rejected

- Keep using Servo 0.4.0: this knowingly tests an outdated engine and misses
  relevant qualification progress.
- Track Servo `main`: two builds of the same Trouve revision could use different
  engine code and produce incomparable results.
- Use only a prebuilt servoshell nightly: this would not test the chrome-free,
  in-process ownership and lifecycle boundary required by ADR 0023.
