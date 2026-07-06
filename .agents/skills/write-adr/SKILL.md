---
name: write-adr
description: Record an architectural decision as an ADR in docs/adr/. Use when a change reverses, extends, or adds a load-bearing architecture decision (protocol shape, crate boundaries, licensing, sandboxing, UI stack).
---

# Writing an ADR for this repo

1. Check `docs/adr/README.md` for the next number and existing decisions —
   never renumber or rewrite an accepted ADR.
2. Create `docs/adr/NNNN-short-kebab-title.md` with sections: title,
   `Status: Accepted (YYYY-MM)`, `## Context`, `## Decision`,
   `## Consequences` (and `## Alternatives rejected` when alternatives were
   seriously considered).
3. Keep it under a page. Capture *why*, not implementation detail — code
   shows the what.
4. If the ADR supersedes an older one, set the old one's status to
   `Superseded by NNNN` (that status line is the only permitted edit).
5. Add the ADR to the table in `docs/adr/README.md`.
6. If the decision changes an architecture invariant, update the
   "Architecture invariants" section of the root `AGENTS.md` in the same
   change.
