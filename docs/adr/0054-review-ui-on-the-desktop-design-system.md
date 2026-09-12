# ADR 0054: Review UI on the desktop design system

Status: Accepted (2026-09)

Amends ADR 0053 (the shared review UI keeps its own look and stylesheet).

## Context

ADR 0053 unified the review dashboard and the desktop on one Lit codebase but
deliberately kept the dashboard's original visual design: its stylesheet
carried its own palette, control geometry, and typography, hung off `:root`
and `body`, and knew nothing about the desktop's themes. It offered one dark
look regardless of the user's theme, font-size, and contrast choices, and
drew its sidebar with Unicode glyphs while the desktop used Font Awesome.

## Decision

- `@trouve-ai/ui-foundation` owns everything both shells need to look the
  same: the semantic `--trouve-*` tokens and `data-theme` themes it already
  had, plus the element defaults, focus and disabled treatments, icon
  animation, and `visually-hidden` utility (`styles/base.css`), the theme
  controller, the Font Awesome icon helper (which now styles glyphs through
  the CSSOM so it survives a `style-src 'self'` policy), and an
  `observeSignal` helper for imperative hosts.
- `@trouve-ai/transcript` owns the transcript stylesheet
  (`styles/transcript.css`), extracted verbatim from the desktop's `app.css`,
  so that a shell other than the desktop can render a transcript without
  copying its rules.
- `@trouve-ai/code-review` is drawn entirely from the shared tokens: its
  stylesheet contains no literal colors, no `:root`/`body`/theme rules, and
  the desktop's control geometry, navigation panel, and Font Awesome subset.
  It never targets a transcript class without scoping, so both stylesheets can
  load into one scope. Charts resolve their series and axis colors from the
  active theme at draw time and repaint when the host's `data-theme` changes.
- Theming stays with the shell. The review site owns its `ThemeController`
  (persisted in `localStorage`, following the OS scheme by default) and shows
  the dashboard's optional theme picker; a host that owns appearance itself
  leaves the picker unset. The dashboard itself never reads or writes a
  theme: it is drawn from the `--trouve-*` tokens of whichever `data-theme`
  its host set.
- The review site's nginx CSP additionally allows same-origin fonts for the
  icon subset. Inline styles stay forbidden.

## Consequences

Both frontends render one design system and one theme model. A token or
theme change in `ui-foundation` reaches the review site and the desktop
alike; the colorblind and high-contrast themes now apply to reviews. Package
boundaries are stricter: `code-review` depends on `ui-foundation`, and the
review-site Docker build copies its sources.

The self-hosted site's look changes: it is now the desktop's look. That is
the point of this decision, and the earlier verbatim-port screenshots from
ADR 0053 are no longer the reference. Because the dashboard is drawn from
tokens only and never targets a transcript class without scoping, it can be
mounted inside another shell later without carrying a second design system
with it.

## Alternatives rejected

Keeping the dashboard's own theme and palette alongside the desktop's would
have preserved two design systems, which is what this decision removes.
Re-skinning the existing stylesheet with the desktop's colours while keeping
its own control geometry and icons would have looked similar at a glance and
drifted again on the next desktop change.
