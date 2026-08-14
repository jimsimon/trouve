# Process-wide child-launch synchronization

Status: Accepted (2026-08)

## Context

trouve owns complete subprocess trees on Unix with a pipe sentinel inherited by
the leader and its descendants. Linux creates that pipe atomically with
`O_CLOEXEC`; macOS has no `pipe2`, so there is a short interval between
creating the descriptors and marking them close-on-exec. A concurrent child
launch during that interval can inherit a foreign sentinel writer, causing
cleanup to wait for or target an unrelated process.

Serializing only process-tree launches is insufficient because ordinary
`Command`, PTY, daemon, probe, and system-opener launches can race with the
same setup. Per-crate mutexes are also insufficient when those launch paths are
linked into one product process.

## Decision

A dependency-light `trouve-process` crate owns the single process-wide macOS
launch lock. Every trouve-owned path that creates a child process must enter
that shared boundary, including standard and Tokio commands, PTY spawns, and
library entry points that launch system handlers.

Process-tree creation holds the boundary from sentinel pipe creation through
the child launch. Ordinary callers hold it only around the operation that
creates the child; waiting and output collection happen after releasing it.
The boundary is a no-op on platforms where sentinel creation is atomic.

## Consequences

- macOS child launches cannot observe a partially configured sentinel.
- Independent launches are serialized only for their short creation section,
  not for child lifetimes.
- New process-launching code must use `trouve-process`, even when the actual
  launch is performed by a dependency.
- The small shared crate is intentionally below agents, core, search, server,
  and app in the dependency graph so those crates use the same mutex instance.

## Alternatives rejected

- Locking only process-tree helpers leaves direct launches racy.
- Duplicating a lock in each crate does not create a process-wide boundary.
- Holding a lock across `output` or `status` waits would serialize
  long-running children unnecessarily.
