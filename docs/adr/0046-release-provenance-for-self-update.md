# ADR 0046: Release provenance for self-update

Status: Accepted (2026-08)

## Context

ADR 0042 made writable direct-release binaries self-updating, but Cargo's
release profile is also used for local qualification and `cargo install`.
Debug assertions therefore cannot distinguish an official artifact from a
developer's writable build. Treating every release-profile binary as shipped
would let a local test executable silently replace itself and would give the
generic installer no durable proof that its component and version match the
release selected earlier.

The release matrix also publishes the Wry desktop only for glibc Linux while
publishing standalone server and search binaries for both glibc and musl.

## Decision

Only artifacts compiled by the repository release workflow with an explicit
compile-time release marker may use self-update. Debug builds, local
release-profile builds, and Cargo-built installations remain outside both
automatic and manual update paths. The runtime opt-out remains a separate
policy layered on top of this immutable build provenance.

An installation carries its component, checked version, eligible executable
path, and observed executable identity from release selection through the
replacement commit. The updater revalidates that identity immediately before
replacement and refuses stale, cross-component, package-managed, or
non-monotonic installs. Direct Linux archives document a user-owned prefix;
system-owned installations continue to defer to their package manager.

Desktop self-update rejects Linux musl targets before release lookup. Server
and search self-update retain musl support.

## Consequences

- `cargo run`, local `cargo build --release`, and `cargo install` cannot mutate
  their own binaries; their originating tool remains the update mechanism.
- Official GitHub and npm-bundled native artifacts retain automatic and manual
  update support when installed in a user-owned location.
- Adding a release build path requires setting the provenance marker and
  satisfying the release-asset contract deliberately.
- A changed executable or stale release requires a fresh process or check
  instead of guessing which version is safe to replace.

## Alternatives rejected

- Inferring provenance from the Cargo profile or a writable path conflates
  local builds with distributed artifacts.
- Publishing a musl Wry desktop would add an unsupported build target solely
  to make updater target selection appear uniform.
