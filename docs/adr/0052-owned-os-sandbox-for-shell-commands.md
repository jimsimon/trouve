# ADR 0052: Trouve owns an OS sandbox for shell commands

Status: Proposed (2026-09)

Supersedes the "no OS sandbox in local mode" decision of
[ADR 0004](0004-no-os-sandbox-permission-modes.md). ADR 0004's permission
layer and `ToolExecutor` chokepoint remain the authorization story; this ADR
adds containment underneath it.

## Context

ADR 0004 deferred OS sandboxing because it is platform-divergent and every
harness that ships one still needs an escape hatch. Two things changed.

First, vendor backends on the full tool bridge now route reads and writes
through trouve's tools, and the read-only shell classification (ADR 0004
amendment) lets `git log` or `rg` run without a prompt. The classifier is
conservative and fails closed, but it is text analysis of a command line. A
sandbox turns a misclassification from a possible write into a denied write.

Second, containment today comes from the vendors. Codex's built-in shell runs
in Codex's read-only sandbox (bubblewrap and Landlock on Linux, Seatbelt on
macOS), and that sandbox can read anything the user can read, including
`~/.ssh` and `~/.codex/auth.json`. Retiring those built-ins in favour of
trouve's shell (the goal behind the classifier work) means trouve inherits the
job of confining commands, and it should not depend on a downloaded vendor
runtime to do it.

## Decision

### Sandbox policies

Trouve's shell tool runs each command under one of three policies:

| Policy | Filesystem | Network | Used for |
| --- | --- | --- | --- |
| `ReadOnly` | Whole filesystem read-only; a private writable temp dir | Off | Read-only-classified commands; every command in read-only personas |
| `WorkspaceWrite` | Read everywhere; writes only inside the session worktree, its linked Git directory, and a private temp dir | Off by default | Mutating commands in `ask` and `allow-list` after approval |
| `Unrestricted` | No sandbox | On | Explicit escalation (see below); `yolo` |

`WorkspaceWrite` must include the worktree's resolved external Git directory.
ADR 0004 notes that Codex's workspace-write mode broke linked worktrees by
protecting `.git`; trouve's worktrees are linked worktrees, so `git` must be
able to create `index.lock` under the shared gitdir.

The permission layer decides *whether* a command runs. The sandbox decides
*what it can touch while running*. The read-only classifier stays as it is:
a command that names an absolute path still falls back to the normal gate,
even though the read-only sandbox would also have contained it. Two
independent controls fail independently.

### Escalation

A command that fails inside `WorkspaceWrite` because it needed something the
sandbox denied (a write outside the worktree, network for a package install)
is reported to the model with the sandbox policy that applied and a hint that
the failure looks like a denial. The model may retry with `escalate: true`.
Escalation is a distinct approval in trouve's UI, worded as "run outside the
sandbox", and is never auto-approved in `ask` or `allow-list`. Its allow-list
key is the command's normal key with an `escalated:` prefix, so a standing
approval for `cargo` does not silently become a standing approval for
unsandboxed `cargo`. `yolo` auto-approves escalation, consistent with ADR
0004's definition of `yolo` as opt-in full trust.

Denial detection is heuristic (`EACCES`, `EROFS`, `EPERM`, and connection
failures in stderr, plus the exit code). A false positive only produces a
hint; a false negative only means the model reads the raw error.

### Platform implementations

- **Linux**: bubblewrap creates the mount namespace (`--ro-bind /`, tmpfs on
  the private temp dir, `--bind` for writable roots, `--unshare-net` when
  network is off, `--die-with-parent`). Landlock, through the `landlock`
  crate, is applied inside the sandbox as a second layer, and alone when
  bubblewrap cannot run (no unprivileged user namespaces, WSL1). When only
  Landlock is available, network is blocked with a seccomp filter on socket
  creation (`seccompiler`), because Landlock's own network rules need ABI v4
  (kernel 6.7). The effective backend is recorded in the tool result.
- **macOS**: `sandbox-exec` with a generated Seatbelt profile: deny by
  default, allow reads, allow writes under the worktree, gitdir, and temp
  dir, deny network unless on. `sandbox-exec` is deprecated but present
  through current macOS releases and is what every shipping harness uses.
- **Windows**: no sandbox in this ADR. Commands run as today, the tool
  result reports `sandbox: "none"`, and escalation is a no-op. A
  restricted-token or AppContainer backend is future work.

### Shipping bubblewrap with trouve

Trouve does not rely on a system `bwrap`, and it does not download one from
a vendor release. It builds bubblewrap from source and ships the result.

- A new `crates/trouve-bwrap` binary crate compiles the vendored bubblewrap
  sources (`vendor/bubblewrap`, pinned to an upstream release) with `cc` and
  `pkg-config libcap`, renaming `main` so a thin Rust `main` forwards `argv`.
  This is the same shape Codex uses and keeps the C build inside Cargo.
- The binary is embedded into `trouve-server` with `include_bytes!` for
  Linux targets and extracted on first use to
  `<data_dir>/sandbox/bwrap-<sha256 prefix>`, verified by digest before each
  launch. Trouve's release artifacts stay single-binary tarballs, and the
  desktop `.deb` needs no extra path. Resolution order at runtime: the
  `TROUVE_BWRAP` override, the embedded copy, a system `bwrap` that passes a
  user-namespace probe, then Landlock only.
- musl targets link libcap statically so the gnu and musl artifacts behave
  the same. The `cross` images used for the aarch64 musl build need
  `libcap-dev` added through a `Cross.toml` pre-build hook.
- bubblewrap is LGPL-2.0-or-later. Trouve is MIT. Shipping a separately
  built LGPL executable alongside (or embedded as an opaque blob that is
  extracted and executed, not linked) is aggregation, not derivation. The
  vendored source stays in the repository, the release compliance step adds
  bubblewrap to the third-party notices with its license text, and the
  notices state where the source lives.

### What this does not change

- The permission modes and their meaning.
- MCP stdio servers, which trouve spawns as long-lived processes outside any
  shell call. Sandboxing them is a separate decision.
- Vendor-native tools on non-bridge threads, which remain under the vendor's
  own sandbox and trouve's approval relay.

## Consequences

- Read-only-classified commands become safe against classifier mistakes and
  cannot read outside the worktree even when they name paths the classifier
  did not catch, because the read-only policy still applies. This is the
  precondition for retiring Codex's built-in shell without a containment
  regression.
- Mutating commands in `ask` and `allow-list` gain a real boundary: an
  approved `cargo test` can no longer write to `~/.cargo/config.toml` or
  reach the network without a second, explicitly worded approval. Some
  workflows (package installs, `git push`) will hit escalation prompts they
  did not see before. That is the intended trade.
- Trouve carries a C build in its Rust workspace and an LGPL component in
  its release compliance. Both are contained to one crate and one notices
  entry.
- Windows keeps today's behaviour. Documentation and the tool result make
  that visible rather than implying containment that does not exist.

## Rollout

1. `trouve-bwrap` crate, vendored source, embedding, digest-verified
   extraction, and a `trouve_core::sandbox` module exposing
   `SandboxPolicy` and a `wrap(command) -> Command` for Linux and macOS.
   Read-only-classified commands and read-only personas run under
   `ReadOnly`. No user-visible UX change beyond a `sandbox` field in shell
   results.
2. `WorkspaceWrite` for mutating commands in `ask` and `allow-list`, denial
   hints, and the escalation approval with its own allow-list key.
3. Release workflow: libcap in the build images, static linking for musl,
   notices and SBOM entries, and a smoke test that runs `trouve-server`'s
   embedded bubblewrap on each Linux target.
4. Landlock-only fallback with the seccomp network block, and the
   user-namespace probe that selects it.
