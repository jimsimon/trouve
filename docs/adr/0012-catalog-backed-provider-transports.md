# 0012 — Catalog-backed provider transports

Status: Accepted (2026-07).

## Context

Provider `/models` endpoints usually report account-visible identifiers, but
often omit context limits, pricing, reasoning controls, setup fields, and even
the provider roster. Other services have no `/models` endpoint. Treating every
provider as either OpenAI Chat Completions or Anthropic Messages also exposes
models on wire protocols that cannot call them.

Maintaining a complete hand-written provider/model table would duplicate
rapidly changing external data. Trusting a catalog alone would be equally
wrong: a catalog describes offerings, not what one account deployed or can
access.

## Decision

- The refreshable, tokenless models.dev `api.json` is the catalog source for
  provider names, documented endpoints, key environment variables, model
  metadata, and model-specific option schemas. A generated full-catalog
  snapshot is the offline fallback.
- A live provider model endpoint remains authoritative for account-specific
  availability. Trouve enriches those live identifiers from models.dev and
  falls back to catalog models only when live discovery is absent or fails.
- Provider transport is an explicit adapter selected from catalog identity:
  OpenAI-compatible, Anthropic Messages, Azure OpenAI v1, Amazon Bedrock
  ConverseStream, Vertex Gemini, or Anthropic on Vertex. An adapter may filter
  catalog models that belong to another wire surface.
- Endpoint, header, and query templates use literal `${NAME}` placeholders.
  The protocol advertises generated setup fields; non-secret values are stored
  in provider config, while named secrets and API keys stay in the secret
  store. Template expansion never invokes a shell.
- A catalog entry is shown only when its transport and authentication flow are
  implemented and documented. Unknown SDK package names are not guessed from
  model names.

## Alternatives rejected

- **Only call `/models`.** This loses metadata and excludes native services
  without that endpoint.
- **Treat every endpoint as OpenAI-compatible.** Similar model names do not
  imply compatible auth, request, streaming, reasoning, or tool-call shapes.
- **Expose every models.dev provider and fail at request time.** Setup choices
  should represent working integrations, not an aspirational roster.

## Consequences

- Catalog refreshes add providers with already-supported transports without an
  app release, while native transport changes remain reviewed code.
- The bundled catalog is larger and must be regenerated when its retained
  schema changes.
- Live discovery and catalog fallback can differ temporarily; live identifiers
  win, and catalog metadata is best-effort enrichment.
- New transport/auth families require an adapter and compatibility tests before
  their catalog entries become visible.
