# 0020 — Canonical model catalog with availability overlays

Status: Accepted (2026-07).

## Context

ADR 0016 made models.dev the shared provider and model catalog while retaining
live discovery for account-specific availability. In practice, some provider
APIs and vendor CLIs also supplied display names, context limits, pricing, and
option schemas. Their partial and version-dependent records caused the desktop
app and review dashboard to expose different settings for the same public
model depending on its transport.

Some Trouve integrations are not model API providers and therefore cannot be
represented by models.dev: subscription CLIs, arbitrary compatible gateways,
Cursor-owned models, and user-installed local models.

## Decision

- For every catalog-covered provider and model, models.dev is authoritative
  for provider identity, display metadata, capability flags, context limits,
  pricing, and option schemas. The refreshable cache and bundled snapshot are
  the online and offline forms of that same source.
- Live provider APIs and vendor CLIs contribute only account-visible model
  identifiers. Trouve intersects those identifiers with models.dev and rebuilds
  each protocol `ModelInfo` from the catalog. Unknown live identifiers are not
  assigned guessed metadata.
- Trouve-specific integration records contain execution facts only: transport
  kind, authentication flow, CLI/runtime, endpoint, and catalog mapping.
  Codex maps to OpenAI metadata and Claude Code maps to Anthropic metadata.
- Explicit metadata adapters remain for sources models.dev cannot describe:
  custom gateways, Cursor-only models and controls, and local runtime models.
  Public OpenAI, Anthropic, and Google models exposed through Cursor are still
  canonicalized from models.dev; Cursor-only context and fast controls remain
  transport-owned.
- The server's `/v1/models` response is the single client catalog. Desktop and
  code-review clients render model settings from its option schemas and do not
  maintain per-model tables.

## Consequences

- A model has consistent metadata and settings across transports and clients,
  and a models.dev refresh takes effect without a vendor-CLI cache refresh.
- A newly released account-visible model may remain hidden until models.dev
  contains it. When live discovery fails without a stale allowlist, catalog
  fallback can temporarily include a model the account cannot use.
- Uncatalogued integrations require small, reviewed adapters, but those
  adapters do not become a second public provider/model catalog.

## Alternatives rejected

- **Let every live source override catalog fields.** Partial records recreate
  divergent settings and make CLI versions part of the UI schema.
- **Never consult a live source.** Catalog offerings do not prove account
  access and cannot enumerate user-installed or Cursor-owned models.
