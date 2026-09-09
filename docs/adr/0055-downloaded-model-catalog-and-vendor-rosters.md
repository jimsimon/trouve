# 0055 — Downloaded model catalog and vendor-refreshed rosters

Status: Accepted (2026-09). Amends ADR 0016 and ADR 0020; scoped amendment
to ADR 0050 (see below).

## Context

ADR 0016 made the models.dev `api.json` the provider and model catalog and
bundled a generated full-catalog snapshot into the binary as the offline
fallback. ADR 0020 added a small trouve-owned overlay for serving surfaces
models.dev does not distinguish (Codex, Cursor) and declared live vendor
sources to be availability signals only.

In practice the bundled data became the problem it was meant to solve:

- The Codex roster in the overlay was hand-written. When OpenAI withdrew
  `gpt-5.4` and `gpt-5.4-mini` from ChatGPT-account Codex, trouve kept
  offering them and the configured session-naming model failed silently until
  a release updated the JSON.
- The Cursor roster in the overlay listed 14 models while the account could
  use 39, and modelled every reasoning control as `effort` although Cursor
  expects `reasoning` for GPT models and `reasoning_effort` for some Gemini
  models. Options for those models never reached Cursor.
- The 2.3 MB models.dev snapshot was only served when the first download
  failed or the machine was offline, yet it was shown as if current. Only
  local models work offline, so an offline-first catalog buys little.

Both vendor runtimes report their rosters: the Codex app-server's
`model/list` returns visible models, reasoning efforts and defaults, input
modalities and speed tiers; the Cursor SDK Bridge's
`SdkCursorService/ListModels` returns models with the exact parameter ids,
values and default variant the agent accepts. Neither reports context limits
or pricing.

## Decision

- **The public catalog is downloaded, not bundled.** models.dev `api.json` is
  fetched with ETag revalidation on the existing TTL and cached at
  `<data_dir>/models-dev-cache.json`; that cache is the only persistent copy.
  A checked-in snapshot remains solely as test data behind a Cargo feature.
- **An empty catalog is an explicit state.** Until the first successful
  download `ServerInfo.catalog_available` is false, the server retries every
  connectivity poll (30 s) instead of the steady-state 5 min backoff, and the
  `server.model_catalog_changed` event announces recovery. Clients show
  "downloading model catalog" rather than "no models" and do not cache an
  empty roster as fresh. Provider presets are limited to trouve's own
  integrations and local models until the catalog arrives.
- **Vendor-backed providers own their rosters.** Codex and Cursor backends
  rebuild `<data_dir>/rosters/<provider>.json` in a detached background task
  (startup, connectivity recovery, model-list refresh; same TTL and backoff as
  the catalog; never on a request path). A roster replaces the bundled seed
  for that provider wholesale, so retired models disappear. Live parameter
  ids and defaults are authoritative for option schemas: Cursor parameters
  become option properties under their own ids, Codex efforts and speed tiers
  become `reasoning_effort` and `fast`.
- **Cursor's roster lookup uses its own short-lived Bridge.** ADR 0050 keeps
  one shared warm Bridge per backend for turns; that remains. The
  `SdkCursorService/ListModels` call needs no agent or workspace, runs at
  most once per TTL, and must never wait on or be waited on by a turn, so it
  starts a separate Bridge process and shuts it down when done rather than
  entering the pool's turn admission.
- **The trouve overlay only fills gaps.** Seed entries survive a refresh as
  per-model patches and carry only what vendors and models.dev cannot report:
  Codex-specific context limits, slug remaps to models.dev records, image
  support and limits for vendor-only models. Roster entries for public models
  inherit display metadata, limits and pricing through `base_model` as before.
- Rosters are never rebuilt against an empty catalog, so inheritance links
  are only written once the public records exist.

## Consequences

- Model lists track account entitlements within an hour without a release,
  and per-model options match what the vendor accepts today.
- First launch requires connectivity for anything but local models; a failed
  download is visible and self-heals.
- Release binaries shrink by the snapshot size; the test fixture still needs
  the existing refresh script.
- A vendor list can advertise a model the account cannot actually run (Codex
  listed `gpt-5.5` while rejecting it). Turn-time failures are not yet fed
  back into the roster.

## Alternatives rejected

- **Keep the bundled snapshot as a fallback.** It is stale by construction and
  was indistinguishable from live data in the UI.
- **Query the vendor CLI on every model list.** Adds process startup latency
  to a hot path and still lacks limits and pricing; a persisted roster gives
  the same freshness without the wait.
- **One roster file for all providers.** Each backend refreshes on its own
  clock; per-provider files avoid read-modify-write across backends.
