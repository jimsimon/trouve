# ADR 0042: Checksummed self-updating release binaries

Status: Accepted (2026-08)

## Context

The release train publishes the desktop app, standalone server, and search
tool as target-specific archives, but users must currently replace those
binaries by hand. Package managers and container orchestrators can deliver
updates for their own installations, while direct release downloads have no
equivalent update path. Implementing the same network, archive, checksum, and
executable-replacement logic independently in three binaries would make the
release contract easy to drift and difficult to audit.

## Decision

Add a generic `trouve-update` crate shared by the shipped binaries. It treats
the latest stable GitHub Release for `jimsimon/trouve` as the update channel
and derives the exact artifact from the component and compile target. An
update is eligible only when its canonical `vX.Y.Z` tag is newer than the
binary's workspace version and the release contains both that artifact and
`SHA256SUMS`.

The updater downloads into a temporary directory, verifies the archive
against its exact checksum entry, extracts only the expected executable, and
uses a platform-aware atomic self-replacement primitive. A failed check,
download, verification, extraction, or replacement leaves the installed
binary untouched.

The desktop checks in the background and requires the user to choose
**Install and restart**. Long-running standalone server and search processes
may replace their on-disk binary in the background but keep the active process
running; the new version takes effect on the next restart. Explicit update
commands remain available, and `TROUVE_DISABLE_AUTO_UPDATE` disables
background behavior without disabling manual updates.

The desktop binary remains the update unit for its embedded server and search
library. Containers and npm/plugin packages continue to be versioned and
published by the release train; their normal package manager may overwrite a
self-updated executable later without creating a compatibility split because
all first-party artifacts share one version.

## Consequences

- Direct binary installations can follow releases without a Rust toolchain or
  manual archive handling.
- Release asset names and `SHA256SUMS` are now a compatibility contract and
  must be validated before a release is published.
- Checksums detect corruption and asset substitution, while trust still
  terminates at the project's GitHub release channel and TLS.
- Running services are never interrupted automatically, so operators retain
  control over restart timing.

## Alternatives rejected

- Platform-specific installers and app-store frameworks do not cover the
  standalone server and search binary uniformly.
- Updating through a new server protocol endpoint would violate the local
  executable ownership boundary and make standalone tools depend on a
  running server.
- Silently restarting active processes could interrupt agent turns, MCP
  clients, or hosted traffic.
