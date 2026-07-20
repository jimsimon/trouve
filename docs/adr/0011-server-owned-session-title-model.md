# 0011 — Server-owned session title model with heuristic fallback

Status: Accepted (2026-07)

## Context

Session titles also seed their Git branch names, so a title must be available
before the session worktree is created. The original client-side extractor was
instant and offline-safe but could only rearrange prompt text; fluent short
summaries require a generative model. Using the user's selected coding model
would add provider cost, require credentials, and make naming depend on thread
configuration. Reusing the integrated local-provider sidecar would evict its
one active coding model.

## Decision

- Automatic title derivation is server functionality exposed through the
  versioned HTTP protocol. Clients request a title before creating a session,
  then pass that final title to normal session creation so the display title
  and branch slug agree from the start.
- The server may run a dedicated, managed, short-context title model in a
  separate `llama-server` sidecar. It is CPU-first and never replaces or
  reconfigures the local coding-model sidecar.
- A persisted load policy chooses adaptive preload, always-ready preload,
  on-demand loading with idle release, or no model. Model installation is
  explicit and reports progress through the persisted server event stream.
- Deterministic built-in naming heuristics remain mandatory. Disabled,
  missing, loading, memory-constrained, timed-out, malformed, or failed model
  results fall back to them; title generation must never prevent session
  creation.

## Consequences

- New sessions can receive fluent names without a paid provider, and branch
  names remain stable because there is no post-creation rename.
- Keeping the model ready trades roughly a model-sized resident-memory cost
  for sub-second warm naming. Adaptive and on-demand modes reduce that cost at
  the expense of occasional cold-start latency.
- The server owns another child-process lifecycle and managed model artifact,
  including licensing, integrity verification, cancellation, crash cleanup,
  and shutdown.
- Remote clients receive the same behavior as the desktop app and never need
  direct access to inference or engine internals.

## Alternatives rejected

- **Use the search embedding model.** It ranks text by similarity but cannot
  generate or rewrite a title.
- **Rename asynchronously after session creation.** The title and Git branch
  would diverge, and the navigation label would visibly change after submit.
- **Reuse the local coding-model sidecar.** Its one-model lifecycle would
  evict an active coding model and couple naming to provider settings.
- **Call the selected remote provider.** Naming would add cost, latency, and a
  credential/network dependency to session creation.
