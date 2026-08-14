# ADR 0039: Retire Servo qualification hosts

Status: Accepted (2026-08)

## Context

ADR 0023 made Servo a gated desktop-engine candidate, ADR 0024 isolated its
in-process embedder from the product workspace, and ADR 0025 pinned that
harness to a reproducible nightly. Wry has since become the sole shipping
desktop host under ADR 0028. The two Servo qualification paths now maintain a
large non-product dependency graph, platform build prerequisites, security
waivers, release-compliance artifacts, and dedicated CI without contributing
coverage to the shipping engine.

## Decision

Retire Servo as a desktop-engine qualification target. Remove the external
servoshell launcher, the isolated in-process embedding workspace, its lockfile
and dependency notices, and all Servo-specific CI, release-compliance, and
version-synchronization machinery.

Wry remains the only desktop host for the shared Lit application. Preserve the
engine-neutral desktop gateway, typed native-capability boundary, explicit
comparison-host mode, and PWA because they are shipping architecture rather
than Servo scaffolding. Historical migration and qualification documents may
retain their original Servo evidence, but they are not current runbooks.

This decision supersedes ADRs 0024 and 0025 and the Servo-qualification parts
of ADRs 0023 and 0028.

## Consequences

The workspace returns to one Cargo resolver graph and one Rust dependency
notice/SBOM path. CI no longer installs Servo's native toolchain, compiles its
nightly graph, audits its independent lockfile, or carries upstream advisory
waivers. Release compliance no longer publishes Servo-only artifacts.

Trouve no longer has an in-repository path for evaluating Servo. Reintroducing
an alternative desktop engine would require a new decision and qualification
plan; the current product continues to depend on Wry and each platform's
system webview.

## Alternatives rejected

- Keep the harness but stop scheduled CI: this leaves an unverified dependency
  graph and a misleading qualification path.
- Keep only the external servoshell launcher: it does not exercise the
  in-process host boundary and still creates a second engine-specific path.
