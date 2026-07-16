# ADR 0005: Split UI — Slint native clients + separate web client

Status: Accepted (2026-07)

## Context

We evaluated one UI codebase for desktop/mobile/web versus a split approach.
Candidates examined in depth:

- **Tauri 2** (system webviews): mature, but webview divergence
  (WebKitGTK on Linux) burned significant time in evaluation; Verso/Servo
  integration is unmaintained; CEF bundling works but costs ~100+ MB per
  install and gives up the "light" advantage.
- **Flutter**: excellent mobile, good desktop, but drags in Dart alongside
  Rust and its desktop text/IME handling still lags.
- **Dioxus**: attractive Rust-native DX, but its native renderer (Blitz) was
  judged too immature for a code-heavy IDE-like UI.
- **Slint**: Rust-native, compiled UI language, strong desktop + embedded
  track record, credible iOS/Android story, no webview anywhere. Weakness:
  thin widget ecosystem — code views, diff views, markdown, terminal all
  need to be built.

## Decision

- **Native clients (desktop now, mobile later) are Slint** (`trouve-app`),
  sharing `trouve-client-core` (protocol client, session state, view
  models).
- The code-centric widgets Slint lacks are built as **standalone, reusable
  crates** (`slint-code-view`, `slint-diff-view`, `slint-markdown`,
  `slint-terminal`) — useful to the wider Slint ecosystem and testable in
  isolation.
- A **Phase 3 spike gates the bet**: a virtualized, selectable code view
  rendering large files smoothly. If the spike fails, fall back to
  web-in-Tauri before further investment.
- The **web client is a separate TypeScript SPA** (CodeMirror/Shiki), served
  by the backend, deferred until after the desktop client ships. The
  protocol split (ADR 0002) is what keeps this cheap: both UIs are thin.

## Consequences

- No webview variance on native targets; small binaries; one language.
- We own four non-trivial widgets; the spike de-risks the hardest one first.
- Two UI codebases eventually — accepted, deferred cost, and each UI is thin
  because all logic lives behind the protocol.
