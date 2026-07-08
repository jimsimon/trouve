# 0007 — Shared trouve-search MCP daemon over a unix socket

Status: Accepted (2026-07)

## Context

Every MCP-capable agent spawns one `trouve-search` stdio server per session.
Each server holds an embedding model and an LRU cache of up to 10 fully
materialized indexes (0.7–2.3 GB peak RSS on large repos). Working across
many concurrent sessions multiplies that memory for what is overwhelmingly
identical state — sessions index the same repositories. The on-disk store is
already shared; the duplication is purely in-process RAM.

## Decision

The bare `trouve-search` MCP entry becomes a thin stdio⇄socket proxy. The
first session starts a detached daemon (`trouve-search daemon`) that owns
the single `IndexCache` and serves newline-delimited JSON-RPC over a unix
domain socket under the trouve cache folder; all sessions forward to it.

- The socket name hashes binary version, content types, and embedding
  model, so mismatched configurations get separate daemons, and a binary
  upgrade strands the old daemon rather than letting it answer new clients.
- A lock file serializes competing daemon starts; the daemon removes its
  socket and exits after 15 idle minutes with no connections
  (`TROUVE_DAEMON_IDLE_SECONDS`).
- The proxy rewrites relative `repo` arguments to absolute paths (the
  daemon has its own cwd) and falls back to serving in-process if the
  daemon is unreachable or dies mid-session — a session never loses search.
- `TROUVE_DAEMON=0` opts out; Windows (no unix sockets) always serves
  in-process, as before.

## Consequences

- Memory across N sessions is bounded by one daemon instead of N servers.
  Cold builds serialize across sessions on the shared cache mutex (same
  policy as the harness's in-process shared cache).
- Every stdio entry route — MCP configs, the OpenCode/Kilo plugin's
  persistent child — gains sharing without configuration changes.
- The daemon trusts any process that can connect; the socket directory is
  owner-only (0700), the same trust boundary as the cache files beside it.
- The trouve harness keeps its in-process cache (no daemon hop); the CLI
  subcommands are unchanged one-shot processes.

## Alternatives rejected

- **HTTP/SSE MCP transport with a shared server**: heavier dependency
  surface for the published crate, port management instead of a socket
  path, and agents' stdio configs would all need changing.
- **Shrinking the per-process cache (e.g. LRU of 1)**: reduces the
  multiplier but keeps per-session models and re-builds, and degrades
  multi-repo sessions.
- **mmap-only indexes to make instances cheap**: chunk texts are
  materialized per instance by design (query-time snippet formatting);
  making instances cheap would be a much deeper redesign than sharing one.
