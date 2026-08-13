# ADR 0037: Capability-scoped external read roots

Status: Accepted (2026-08)

## Context

Agent skills and plugin packages may live in user-scoped host directories
outside a session worktree. Trouve advertised those skill paths but its file
tools rejected every absolute path, so an agent could be instructed to use a
skill it could not read. Allowing arbitrary absolute reads would instead expose
credentials, browser data, unrelated repositories, and other private host
files to the model provider.

## Decision

- Relative filesystem paths continue to resolve exclusively inside the
  session worktree.
- Mutation paths always remain confined to that worktree.
- The host may register canonical files or directories as read-only
  capabilities. Automatic roots are limited to trouve, workspace, and known
  provider skill/plugin-package directories; embedders may explicitly add
  roots through `TROUVE_READ_ONLY_ROOTS`.
- `read_file`, `list_dir`, `glob`, and `grep` may accept absolute paths only
  when the existing canonical target is contained by a registered root.
  Parent traversal, missing targets, unsupported file types, filesystem roots,
  and symlink escapes fail closed.
- Repository-semantic operations such as code search and Git diff remain
  session-worktree scoped.

## Consequences

- Agents can follow installed skill instructions and read their bundled assets
  without copying them into every session worktree.
- Registering a root is a confidentiality grant: its bounded contents may be
  sent to the selected model provider. Hosts must therefore register narrow,
  intentional resource directories rather than home or filesystem roots.
- Read and mutation resolution are separate APIs, making it difficult for a
  future mutating tool to accidentally inherit external access.

## Alternatives rejected

- Allowing every absolute read would turn a convenience feature into broad
  host-data exfiltration capability.
- Copying every discovered plugin into the worktree would duplicate mutable
  caches, obscure provenance, and make updates stale.
