# ADR 0024: Isolated Servo embedding qualification workspace

Status: Accepted (2026-08)

## Context

ADR 0023 requires Servo to pass an embedding qualification before it can be a
desktop engine candidate. The Servo revision selected under ADR 0025 depends
on rusqlite 0.37 and
libsqlite3-sys 0.35, while the root workspace uses rusqlite 0.40 and
libsqlite3-sys 0.38. Cargo permits only one package with `links = "sqlite3"`
in a resolver graph, so the exact Servo release and the product server cannot
be resolved in the same Cargo workspace.

Changing the product database stack solely for a disposable engine test would
increase product risk. Launching servoshell externally would avoid the Cargo
conflict, but would not test a chrome-free in-process embedder or its lifecycle.

## Decision

Keep `crates/trouve-servo-embed-preview` as a nested Cargo workspace excluded
from the root workspace. It has its own lockfile and resolves the exact Servo
nightly revision mandated by ADR 0025 independently. It remains a first-party
package whose product
version and internal Trouve dependency pins are synchronized from the root
workspace version.

The harness embeds one Servo `WebView` directly in a native window without
servoshell browser chrome. It is qualification-only and is not a shipping host.
It cannot link or start `trouve-server`: it requires an explicit
`TROUVE_SERVER_URL`, verifies protocol compatibility, and reaches that server
through the hardened desktop gateway. Servo state and gateway preferences use
process-owned temporary directories; the harness never opens Trouve's default
database.

## Consequences

Servo embedding can be tested at the accepted revision without perturbing the
product database dependency. Root workspace commands do not build or test the
harness, so CI and local qualification must invoke its manifest explicitly with
`--locked`. Its independent lockfile requires separate dependency, license, and
security review.

This isolation is not evidence of engine promotion. Visual parity,
accessibility actions, native capabilities, memory and performance, recovery,
packaging, and the complete platform matrix remain gates under ADR 0023.

## Alternatives rejected

- Add Servo to the root workspace: Cargo rejects the conflicting native SQLite
  link packages.
- Downgrade the product database stack or patch Servo's dependency graph: this
  couples a disposable qualification harness to risky product changes and no
  longer tests the exact published Servo release.
- Use only external servoshell: this does not qualify in-process embedding,
  chrome removal, ownership, or shutdown behavior.
