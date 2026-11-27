# ADR 0058: Roster rebuilds start from the bundled seed

Status: Accepted (2026-11)

Amends ADR 0055 (seed entries survive a refresh as per-model patches).

## Context

ADR 0055 lets the Codex and Cursor backends rebuild their rosters from the
vendor's live model list and keeps the trouve overlay as per-model patches
that fill what vendors and models.dev cannot report. The rebuild took those
patches from "the patches currently in effect": the previous roster when one
existed, otherwise the seed bundled with the release.

Once an install had saved a roster, the bundled seed was therefore never read
again:

- A release that added or corrected a seed entry (a Codex context window, a
  Cursor slug remap) had no effect on existing installs.
- A seed entry removed from a release lived on in the saved roster.
- A model first listed before models.dev had a record for it was saved
  without a `base_model` link and never gained one, so it stayed without
  limits or display metadata.

## Decision

- Every roster rebuild takes its patches from the seed bundled with the
  running release, never from the previous roster. Everything else in a
  roster entry (availability, reasoning levels, speed tiers, Cursor
  parameters and context defaults, inferred `base_model` links) is re-derived
  from the live list on each rebuild. The previous roster is read only to log
  which models were added or removed.
- A seed change takes effect at the next roster refresh after upgrading.
- A seed patch inherits from its `base_model` only when the public catalog
  has that record. When it does not, the link is dropped and the entry falls
  back to what the vendor reports plus the seed's own fields, so a model the
  vendor serves is never hidden by a seed entry the catalog cannot resolve.

## Consequences

- Seed corrections reach existing installs within one refresh interval
  instead of never.
- Nothing hand-authored can be carried in a roster file: anything that must
  survive a refresh belongs in the bundled seed.
- A model whose public record appears later gains its limits and metadata at
  the next refresh without a release.
