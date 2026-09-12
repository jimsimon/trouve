# ADR 0054: Review UI on the desktop design system

Status: Accepted (2026-09)

Amends ADR 0053 (the shared review UI keeps its own look and stylesheet).

## Context

ADR 0053 unified the review dashboard and the desktop on one Lit codebase but
deliberately kept the dashboard's original visual design: its stylesheet
carried its own palette, control geometry, and typography, hung off `:root`
and `body`, and knew nothing about the desktop's themes. It offered one dark
look regardless of the user's theme, font-size, and contrast choices, drew
its sidebar with Unicode glyphs while the desktop used Font Awesome, and
showed a review task's reasoning, tool output, and response as three raw text
blocks where the desktop renders the same events as a chat transcript with a
turn rail, grouped tool activity, and Markdown.

Review tasks are ordinary trouve threads on the review server. While a task
runs, its thread is readable through the standard protocol (`/v1/threads/*`).
When a job finishes the review server deletes the session and keeps only the
task's prompt, reasoning, tool log, and output columns.

## Decision

- `@trouve-ai/ui-foundation` owns everything both shells need to look the
  same: the semantic `--trouve-*` tokens and `data-theme` themes it already
  had, plus the element defaults, focus and disabled treatments, icon
  animation, and `visually-hidden` utility (`styles/base.css`), the theme
  controller, the Font Awesome icon helper (which now styles glyphs through
  the CSSOM so it survives a `style-src 'self'` policy), and an
  `observeSignal` helper for imperative hosts.
- `@trouve-ai/transcript` owns the transcript stylesheet
  (`styles/transcript.css`), extracted verbatim from the desktop's `app.css`.
  The desktop imports it; the review site loads it too. The transcript view
  accepts any client that implements its `TranscriptClient` surface and
  reports its state through a `trouve-transcript-state` event.
- `@trouve-ai/code-review` is drawn entirely from the shared tokens: its
  stylesheet contains no literal colors, no `:root`/`body`/theme rules, and
  the desktop's control geometry, navigation panel, and Font Awesome subset.
  It never targets a transcript class without scoping, so both stylesheets can
  load into one scope. Charts resolve their series and axis colors from the
  active theme at draw time and repaint when the host's `data-theme` changes.
- Review-task activity renders through `trouve-transcript-view`. A running
  task streams its live thread from the review server through
  `@trouve-ai/protocol`; a finished task's retained columns are folded into a
  static thread snapshot (`RetainedTranscriptClient`) for the same renderer.
  A running task whose thread cannot be opened falls back to streaming its
  retained columns as before.
- Theming stays with the shell. The review site owns its `ThemeController`
  (persisted in `localStorage`, following the OS scheme by default) and shows
  the dashboard's optional theme picker; a host that owns appearance itself
  leaves the picker unset. The dashboard itself never reads or writes a
  theme: it is drawn from the `--trouve-*` tokens of whichever `data-theme`
  its host set.
- The review site loads the same content worker protocol as the desktop for
  Markdown, diffs, and highlighting, and its nginx CSP additionally allows
  same-origin fonts and workers. Inline styles stay forbidden.

## Consequences

Both frontends render one design system, one transcript, and one theme
model. A token or theme change in `ui-foundation` reaches the review site and
the desktop alike; the colorblind and high-contrast themes now apply to
reviews. Finished reviews read like desktop conversations rather than log
dumps, and running reviews show the same live tool activity the desktop does.
Package boundaries are stricter: `code-review` depends on `ui-foundation`,
`protocol`, and `transcript`, and the review-site Docker build copies those
sources.

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
drifted again on the next desktop change. Streaming retained columns of
finished tasks into the transcript incrementally was unnecessary: retained
text is static once a task finishes, and running tasks have a live thread.
