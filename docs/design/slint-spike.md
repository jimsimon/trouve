# Phase 3 spike: scope and go/no-go criteria

The Slint bet (ADR 0005) hinges on rendering we have to build ourselves.
The spike validates the risky rendering, not widget aesthetics. If it
fails, the fallback is web-in-Tauri — decided *before* Phase 4 investment.

## What gets built

Standalone widget crates with no trouve protocol types in their APIs
(invariant 7), each with a runnable example:

1. **`slint-code-view`** — the hard one. Virtualized, scrollable,
   selectable code viewer: renders only visible lines, monospace gutter
   with line numbers, span-based color tokens (server-emitted highlight
   spans; the widget takes plain `(range, style)` data), text selection
   across virtualized lines with copy.
2. **`slint-diff-view`** — unified diff rendering over the same virtualized
   core: hunk headers, add/del line backgrounds, per-file collapse.
3. **`slint-markdown`** — streaming markdown: incremental append without
   full re-layout, headings/lists/inline code/fenced code blocks (code
   blocks reuse `slint-code-view` tokens).
4. **Tool-card foundation** — expand/collapse cards in a long virtualized
   chat list; the pattern for chat stream performance.

## Go/no-go criteria

Measured against the desktop layout in `ux-screen-map.md`, on Linux
(Wayland + X11) and macOS:

- Open a 10k-line file in `slint-code-view`: smooth scrolling (no visible
  hitching at 60 Hz), selection and copy correct across a scroll.
- A 3k-line unified diff renders and scrolls smoothly with per-file
  collapse.
- Streaming markdown at realistic token rates (~50 deltas/s) keeps the UI
  responsive and doesn't re-layout the whole document per delta.
- A chat list with 500 tool cards expands/collapses cards without layout
  stalls.
- Keyboard focus and copy work on all three platforms' clipboard models.

Any hard failure that Slint upstream can't resolve → fall back to
web-in-Tauri and re-plan Phase 4; the protocol and client-core layers are
unaffected by that swap (ADR 0002).

## Explicitly out of scope for the spike

Pixel polish, themes/design tokens, the app shell, settings screens,
`slint-terminal` (needed for the inspection tabs in Phase 4, but it is a
solved pattern — grid of cells — not a spike risk).
