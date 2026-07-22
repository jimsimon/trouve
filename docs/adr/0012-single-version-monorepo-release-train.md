# ADR 0012: Single-version monorepo release train

Status: Accepted (2026-07)

## Context

ADR 0001 established one Cargo workspace but allowed each crate to version and
release independently. Once the harness, search tool, desktop app, plugins,
and code review service began shipping together, that policy produced several
simultaneous product versions and let the search-only sync check pass while
most workspace packages remained at 0.1.0. Users and operators need one number
that identifies a compatible trouve release across every surface.

## Decision

Keep the single workspace established by ADR 0001 and replace independent
component versions with one release train:

- Root `[workspace.package].version` is the sole product-version source.
- Every first-party Cargo package inherits that version. Every first-party
  Node package, local lockfile record, agent plugin manifest, internal
  dependency pin, container tag, and versioned release artifact matches it.
- Repository releases use one `vX.Y.Z` tag. A release may publish only the
  components that are distributable, but every workspace component advances.
- `scripts/sync_versions.py` performs generated updates and CI enforcement.
  New package or manifest formats must be added to its discovery or checks.
- Choose the workspace bump from the most severe user-visible change anywhere
  in the repository; unchanged components still receive the new version.

The wire protocol, OpenAPI compatibility number, database and file-format
schemas, cache formats, vendor CLI versions, fixtures, and third-party
dependency versions remain independent because they describe different
compatibility domains.

## Consequences

- Changelogs, support requests, release tags, packages, and deployments share
  one unambiguous version.
- A component cannot release independently; even isolated changes bump every
  first-party package.
- Component versions no longer communicate independent maturity.
- The 3.0.0 release establishes the unified train and contains a breaking
  search change, so it is a major version.

## Alternatives rejected

- Keep independent versions and broaden search-specific synchronization: this
  preserves the ambiguity that prompted the change.
- Add a separate `VERSION` file: Cargo already provides workspace package
  metadata, so another source would create avoidable drift.
