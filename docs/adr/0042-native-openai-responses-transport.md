# ADR 0042: Native OpenAI Responses transport

Status: Accepted (2026-08)

## Context

ADR 0016 assigns every catalog provider to an explicit wire adapter instead of
inferring compatibility from model names. OpenAI's official API now recommends
the Responses API for new integrations. Responses represents messages,
reasoning, function calls, and function results as typed items; stateless
reasoning turns may also require encrypted reasoning items to be replayed.

Trouve's official OpenAI preset still used the generic Chat Completions
adapter. Keeping that lowest-common-denominator shape for current reasoning
models discards provider-native semantics. Conversely, many gateways that call
themselves OpenAI-compatible implement Chat Completions but do not implement
the Responses contract.

## Decision

- The official OpenAI API-key preset and `OPENAI_API_KEY` zero-config path use
  a distinct `openai-responses` provider adapter.
- The adapter runs statelessly (`store: false`), sends typed conversation and
  function items, requests encrypted reasoning content, and replays those
  opaque items across Trouve-owned tool iterations. Tool execution remains in
  Trouve's agent loop and crosses `ToolExecutor`.
- Generic gateways, local runtimes, and existing custom endpoints remain on
  `openai-compat` and Chat Completions unless an explicit Responses transport
  is configured.
- Existing explicit official-OpenAI `openai-compat` configurations remain
  valid and catalog-backed; they are not silently rewritten.
- Model discovery and canonical metadata remain shared with the existing
  OpenAI provider machinery. Codex subscription access remains a separate
  app-server backend with vendor-owned authentication, not an API proxy.

## Consequences

- Native reasoning, tool-call, usage, and failure semantics are preserved for
  official OpenAI API models without claiming that every compatible endpoint
  supports Responses.
- Provider configuration gains an additive transport kind, requiring the
  protocol schema/client version to advance together.
- The two HTTP adapters intentionally duplicate a small amount of request and
  SSE handling because their wire contracts and replay rules differ.
- A custom service that genuinely supports Responses can opt into the native
  adapter, while compatibility remains an explicit user choice.

## Alternatives rejected

- **Keep official OpenAI on Chat Completions.** This preserves less information
  and makes current reasoning/tool behavior depend on a legacy wire shape.
- **Move every OpenAI-compatible endpoint to Responses.** Compatibility labels
  do not guarantee typed Responses items or event streams.
- **Proxy Codex or other vendor harnesses through an OpenAI-compatible API.** A
  proxy would flatten turn/session semantics and would not simplify Trouve's
  permission, durability, or event contracts.
