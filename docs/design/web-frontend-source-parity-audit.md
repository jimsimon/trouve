# Historical Rust/Slint to TypeScript frontend source-parity audit

**Audit date:** 2026-08-04

**Migration plan:** [Web frontend migration implementation plan](web-frontend-migration-plan.md)

**Surface ledger:** [Web frontend parity and qualification ledger](web-frontend-parity-ledger.md)

**Implementation audit:** [Web frontend implementation audit](web-frontend-implementation-audit.md)

**Archived 2026-08-07:** This audit records the source comparison that enabled
retirement. ADR 0028 removed the audited Slint sources after their relevant
behavior was ported. The current
[`native-source-contract.test.ts`](../../web/app-ui/src/app/native-source-contract.test.ts)
keeps the remaining Rust host boundary explicit, and
[`app-action-contract.test.ts`](../../web/app-ui/src/app/app-action-contract.test.ts)
preserves executable evidence for the established application actions. Paths
in the historical matrices below are available through version control.

**Servo retirement update (2026-08-14):** ADR 0039 also removed the audited
Servo sources. Their entries below remain historical source-parity evidence.

## Verdict

Every retained Rust or Slint frontend source is accounted for below. The audit
covers 51 files across the native application, shared client view model,
desktop host, direct Servo qualification embedder, and four reusable Slint
widgets. It compares behavior, state transitions, performance controls,
failure handling, security boundaries, and accessibility-relevant interaction;
it does not assume that a line-for-line translation is appropriate between
Slint and the browser platform.

The comparison found and closed source-level gaps that the screen/callback
inventory could not detect: global replay coalescing, cursor-ordered title-model
state, live automation/connectivity reactions, native title fallback and
timeout behavior, tail-attention restoration, client-local unread state,
process reaping, oversized code/diff virtualization, old-history replay
batching, terminal control sequences, and several streaming/chat presentation
edges. The final adversarial pass also replaced lossy aggregate-summary
notification inference with exact durable notification edges, preserving
failure/question detail and repeated attention requests. After those changes,
no relevant native feature or optimization is known to be absent from the
TypeScript application.

This is a repository implementation verdict, not a qualification verdict. ADR
0027 separately authorizes a reversible Wry default; the external visual,
assistive-technology, platform, packaging, performance, security, and soak
gates in the implementation audit remain open.

## Scope and method

The source set was executable during the migration. The current native-source
contract recursively discovers the remaining Rust host sources and fails if a
second native UI grows unnoticed. The application-action contract retains the
149-action inventory and implementation evidence. The source contract also
compares all 21 event variants folded by the Rust `ThreadViewModel` with the
TypeScript reducer's wire-event cases.

For each file, the audit checked whichever of the following apply:

- rendered information, empty/error/loading states, action placement, keyboard
  behavior, selection, disclosure, drag/drop, focus, and window lifecycle;
- event ordering, cursor/replay rules, stale-response rejection, cancellation,
  reconnect, notification, unread, queue, and turn transitions;
- bounded memory, virtualization, batching, coalescing, lazy loading, retained
  widget identity, repaint behavior, process cleanup, and stream disposal;
- validation and trust boundaries for protocol data, host actions, URLs,
  filesystem targets, clipboard payloads, terminal control sequences, and
  persisted preferences; and
- platform substitutions where browser-native or library behavior is a strict
  superset of the hand-built Slint widget.

Core/server/provider Rust is outside the frontend source inventory except where
the web port already needed an additive projection or host contract. Generated
TypeScript and OpenAPI snapshots are treated as generated evidence, not a
second handwritten implementation. `web/review-ui` is included as a
corresponding web surface where the Rust protocol client exposes detailed code
review operations that the Lit shell deliberately delegates to that existing
review application.

## Source-level gaps found and closed

| Native source behavior | Gap found in the web implementation | Closure |
| --- | --- | --- |
| `controller.rs` coalesces replayed server snapshots for 250 ms and keeps only the newest GitHub snapshot per host and Git/worktree settings snapshot. | Cold server history was being applied event by event, causing needless rendering and allowing bootstrap-only state to trigger live behavior. | `ServerReplayBuffer` in `protocol-ingress.ts` now mirrors the keyed coalescing, cursor ordering, idle flush, live-boundary flush, and stop/error discard rules. |
| `controller.rs` consumes Git/worktree install/load events in cursor order. | Settings could show a stale GET/PUT response after a newer live install-progress event. | `AppStore.gitWorktreeSettings`, cursor-bearing protocol helpers, and the management panel now reject stale snapshots and render live progress. |
| `controller.rs` reacts immediately to `automation.fired` and `server.connectivity_changed`. | Automations relied only on polling; the model catalog and offline shell did not react to live connectivity. | Automation revision invalidation, immediate refresh, live server-info projection, offline/local-only shell state, and transient recovery notice were added. |
| `SESSION_TITLE_TIMEOUT` and `session_title_fallback` in `controller.rs`. | Web title generation had no 48-second bound and fallback could retain later lines/control text. | Abortable title requests now time out at 48 seconds; fallback strips invisible controls, chooses the first logical line, collapses whitespace, and limits to 48 Unicode code points. |
| `should_open_chat_at_tail` in `controller.rs`. | A persisted scroll bookmark could restore away from current output while a turn or queue needed attention. | Navigation suppresses/clears bookmarks for running or queued threads and the virtualized screen resumes follow-tail on the corresponding state transition. |
| Native `unread_sessions`/`error_sessions` are client-local, arise only from new terminal turns, and clear on focus or selection. | Durable successful/failed outcomes were displayed permanently, cold history appeared unread, and an unrelated later summary cursor could resurrect a read badge. | The store now compares locally seen and latest summary cursors, seeds the cold snapshot as read, distinguishes live terminal transitions from ordinary replacement updates, conservatively detects missed reconnect work, and clears/suppresses unread state on focused route, selection, and notification activation. |
| Slint `ListView` virtualizes code rows and flat diff rows regardless of document size. | The web large-file safety path replaced CodeMirror with a full-document `<pre>`, bounding parser cost while losing DOM virtualization. | Oversized code keeps a virtualized CodeMirror view while omitting its parser; oversized diffs retain two virtualized read-only editors while omitting expensive MergeView/diff computation. |
| Folded thread snapshots, bounded history pages, `CHAT_WINDOW_ROWS`, history refill, stable chat identity, and replay batching in `controller.rs`/`ui.rs`. | The web client still opened SSE from cursor zero on a fresh process, replaying every historical delta; simply batching those events did not bound transfer/folding work. | `ThreadIngress` now installs the server's newest 256-item folded page at its exact response cursor before opening SSE, validates the snapshot at runtime, lazily prepends contiguous older pages with stable absolute ids and preserved scroll anchoring, and retains the replay batcher only for post-snapshot reconnect backlog. The owned virtualizer keeps bounded default DOM and explicit full-history accessibility mode. |
| `opener.rs` uses a bounded worker rather than leaking a waiter thread per launched process. | Preview hosts could accumulate unreaped external-open child processes. | Wry, app Servo launcher, and direct Servo embedder now use bounded process-reaping openers with a queue capacity of 16. |
| `sleep.rs` retries failed acquire/release transitions and reconciles desired state. | A failed desktop sleep-inhibition transition could leave UI and host state divergent. | `DesktopHostCoordinator` serializes/coalesces transitions and retries the desired state after failures; PWA wake lock independently reacquires on visibility. |
| Native window/resume state persists geometry, appearance, ordering, and scroll bookmarks. | Initial Wry preview did not restore/persist window geometry and running navigation could restore stale scroll. | Typed host preferences now carry debounced/final position, size, maximized state plus all existing appearance/general/notification/order/resume state. |
| Terminal widget handles retained grid state, 5,000-line scrollback, input, search, selection, OSC 52, resize notices, title, bell, mouse/paste/key modes. | The initial preview was an output box with no real prompt/input and later revisions still omitted control-sequence and retained-tab behavior. | xterm-backed keyed tabs open on selection, focus their prompt, retain views, replay at most 512 KiB, use 100×28 defaults/5,000 scrollback, support search/selection/mobile keys, validate OSC 52, surface resize notices, sanitize/bound titles, ring visual bell, and dispose streams/views. |
| Markdown streaming keeps an unstable tail separate; native chat formats tools, raw output, activity, turns, questions, approvals, and attachments. | Early web rendering reparsed full streaming Markdown and flattened or permanently mounted several heavyweight chat bodies. | Stable-tail parsing, tilde/backtick fences, sanitized highlighted code, conditional disclosure mounting, raw/formatted copy, structured tool cards, inline diffs/todos, processing messages, keyed turn rows, queue/cancel/send-next races, and bounded UTF-8 previews are implemented and tested. |
| `theme.rs` obtains installed fonts and applies five semantic palettes. | Font began as free text and early screens used generic spacing/colors. | The host supplies normalized installed families; PWA uses the browser/system fallback list; settings use a select; generated CSS carries all five native semantic themes and compact layout tokens. |
| Native clipboard attachment flow prefers textual composer content before interpreting image data. | Web paste could stage an image when the clipboard also contained text. | Clipboard handling now preserves ordinary text paste first and requests an image only when no usable text exists, with bounded native payload validation. |
| `maybe_notify` in `controller.rs` reacts to every fresh completion, failure, approval, and question event and includes compact failure/question detail. | Aggregate summary transitions omitted detail and could collapse a second approval/question while session attention stayed unchanged. | Protocol 3.2 transactionally derives a `session.notification` edge after the matching summary. The web coordinator preserves category, source thread, Unicode-safe bounded detail, freshness, preferences, focus suppression, sound, and activation without background per-thread streams. |

## Intentional platform translations

These are not missing web features:

- Slint's Skia partial-rendering switch becomes browser compositor invalidation,
  keyed Lit updates, CodeMirror/xterm viewport rendering, and the owned chat/list
  virtualizer. There is no useful JavaScript equivalent to setting
  `SLINT_SKIA_PARTIAL_RENDERING`.
- The native fixed 160-row chat window becomes a viewport-measured virtualizer
  with overscan. It keeps fewer or more rows according to actual geometry,
  preserves the same tail/bookmark behavior, and can temporarily expose the
  complete history for accessibility.
- The browser uses CodeMirror, xterm, and unified/remark rather than porting the
  bespoke Slint layout engines. Those libraries preserve the native contract
  while adding browser selection, IME, accessibility, syntax, Unicode, and
  viewport behavior.
- Background session state uses the durable server summary projection instead
  of one SSE follower per inactive thread. Notification category, freshness,
  preference gating, focus suppression, sound, activation, repeated attention
  edges, and optional failure/question detail are preserved by the adjacent
  durable `session.notification` event. This is the scalable platform
  translation; reopening unbounded background thread streams remains an
  unacceptable substitute.
- External browser navigation is HTTPS-only in the web host even though the
  legacy native helper accepted HTTP. This is an intentional security
  strengthening. Local file actions are likewise confined to verified paths
  inside the active session worktree.
- OSC 52 is supported but has a stricter browser-side payload cap than the
  native widget. This reduces memory and clipboard abuse without changing
  ordinary terminal behavior.
- Servo does not persist product window geometry because it remains a disposable
  embedding qualification harness. The Wry shipping candidate does persist it.
- Native notification activation was Linux-specific in `notify.rs`; the typed
  host lifecycle supports activation where each preview engine/OS notification
  backend exposes it. Browser notification activation uses the standard API.

## File-by-file disposition matrix

### Native application and Slint shell

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-app/build.rs` | `web/app-ui/vite.config.ts`, `web/app-ui/scripts/verify-build-modes.mjs`, desktop `FrontendSource`/`AssetManifest` | Slint compilation maps to Vite's separate desktop/PWA builds. Hashed local assets, no desktop service worker, explicit release dist selection, and embedded asset metadata are enforced. Debug builds deliberately omit web assets so runtime dist snapshots and loopback Vite HMR do not require recompiling Rust. |
| `crates/trouve-app/src/controller.rs` | `src/app/trouve-app.ts`; `src/services/protocol-ingress.ts`, `thread-ingress.ts`, `session-notifications.ts`, `subscription-health-controller.ts`; `src/state/app-store.ts`; route components/models | Full controller decomposition. Commands, navigation, title creation, projections, refresh cadence, PR grouping/actions, models/modes, settings, automation, terminal, notification, unread, close, queue, turn, scroll, and replay behaviors are represented; async generations/cursors replace mutable monolithic controller state. |
| `crates/trouve-app/src/main.rs` | `src/main.ts`, `src/app/trouve-app.ts`, composer/chat-file/clipboard/drag models, `desktop-host-coordinator.ts` | Bootstrap and callback wiring map to custom elements and scoped contexts. Fuzzy completion, `@` token detection, file links, clipboard precedence, provider validity, drag payloads, focus tracking, close flow, and command dispatch are covered. Skia-only rendering flags are intentionally engine-specific. |
| `crates/trouve-app/src/notify.rs` | `src/services/session-notifications.ts`, `browser-notifications.ts`, `host-client.ts` plus native host sender | Nonblocking delivery, exact durable categories, compact failure/question detail, repeated attention edges, sound/policy gating, safe body/title, session/thread activation, and window focus are preserved. |
| `crates/trouve-app/src/opener.rs` | `src/services/host-client.ts`, `src/components/file-reveal.ts`, Rust host adapters | Safe HTTPS/session-file requests cross the typed bridge; every host backend uses a bounded reaping worker. |
| `crates/trouve-app/src/render.rs` | `src/state/thread-view-model.ts`, `tool-output.ts`; `src/components/chat-presentation.ts`, `tool-presentation.ts`, `streaming-markdown.ts`, `markdown-view.ts`, diff/file-language helpers, `thread-screen.ts` | Chat segmentation, tool naming/activity, status, output formatting/collapse/copy, questions, turn metadata, attachment summaries, raw/formatted modes, syntax, diffs, processing state, and UTF-8 bounds are preserved or richer. |
| `crates/trouve-app/src/servo_preview.rs` | Desktop production build plus `trouve-desktop-host`; no presentation-layer TS equivalent | External servoshell qualification launcher remains intentionally separate from the direct embedder. Exact version verification, chrome suppression, display backend choice, signals, and child exit handling are native harness concerns. |
| `crates/trouve-app/src/sleep.rs` | `src/services/desktop-host-coordinator.ts`, `browser-wake-lock.ts` | Desired-state reconciliation, failure retry, running-count gating, and PWA reacquisition are implemented. |
| `crates/trouve-app/src/theme.rs` | `src/styles/themes.css`, `tokens.css`, `src/services/theme-controller.ts`, `appearance-preferences.ts`, `system-fonts.ts` | All five palettes/semantic roles, system theme, font family/size, terminal/syntax colors, and installed-font enumeration were mapped; the CSS palettes are now authoritative. |
| `crates/trouve-app/src/ui.rs` | `src/app/trouve-app.ts`, `src/state/app-store.ts`, `src/contexts/app-contexts.ts`, all Lit components | Native setter façade becomes signal-backed normalized state and scoped `@lit/context`; list identity uses keyed `repeat`, terminal views are retained by id, and chat/list history is virtualized. The 134 callbacks are source-checked separately. |
| `crates/trouve-app/src/web_preview.rs` | `src/services/host-client.ts`, `capabilities.ts`, `desktop-host-coordinator.ts`, generated host client | Wry adapter owns window geometry, lifecycle, attachment/clipboard bridges, notification/attention, safe opening, sleep, CSRF bootstrap, and chrome-free content. These are native host actions consumed by TS rather than reimplemented in JS. |
| `crates/trouve-app/src/web_preview_support.rs` | `src/services/host-client.ts`, `protocol-client.ts`, `protocol-ingress.ts`, Vite/build environment | The product host owns one embedded server by default; explicit comparison hosts require a server URL. Protocol compatibility, one gateway, shared packaged/runtime/Vite frontend source, and teardown map to validated bootstrap clients. The gateway remains the page origin and reserves native and protocol routes; the frontend never opens the database or bypasses HTTP/SSE. |
| `crates/trouve-app/src/winstate.rs` | preference services (`appearance`, `general`, `notification`, `resume`, workspace/PR order) and host preferences | Defaults, corruption fallback, bounded values, geometry, route/thread resume, scroll bookmarks, and ordering are preserved. Desktop persists through the host; PWA uses browser storage where native-only fields do not apply. |
| `crates/trouve-app/src/wry_main.rs` | Default desktop build/entry point; no presentation-layer TS equivalent | Selects the product Wry bootstrap while `web_preview.rs` remains reusable by the explicit comparison target. All state and presentation continue through the shared Lit frontend and protocol. |
| `crates/trouve-app/ui/app.slint` | `src/app/trouve-app.ts`, `src/components/thread-screen.ts`, `session-list.ts`, `inspection-workspace.ts`, composer/queue/approval/question/todo/terminal/MCP/PR components, `src/styles/app.css` | The full shell, chat, composer, inspection tabs, resize/pane behavior, overlays, mobile pane order, titlebar-less desktop surface, and close modal are ported. Every callback has executable mapping evidence. |
| `crates/trouve-app/ui/automations-screen.slint` | `src/components/automations-screen.ts`, `automations-model.ts` | List/form/template/schedule validation, enable/run/delete, loading/error/empty states, status/last run, refresh polling plus live invalidation, and responsive layout are implemented. |
| `crates/trouve-app/ui/connectivity-banner.slint` | `src/app/trouve-app.ts`, `src/components/model-health.ts`, `src/styles/app.css` | Offline severity, subscription/account health, recovery notice, and local-model-only behavior are implemented without blocking usable local workflows. |
| `crates/trouve-app/ui/pull-requests-screen.slint` | `src/components/pull-requests-dashboard.ts`, `pull-requests-dashboard-model.ts`, `code-review-dashboard.ts`, session PR components/badge, shared `AppStore` PR projection | Repository filter, seven ordered/collapsible groups, drag/keyboard reordering, refresh age, checks/reviews/findings, chat/fix actions, configuration CTA, loading/error/empty states, and shared dashboard/pane/badge state are implemented. |
| `crates/trouve-app/ui/scroll-keys.slint` | `src/components/tab-navigation.ts`, component key handlers, native scroll containers | Arrow/Home/End/Enter/Space navigation is handled with semantic controls/roving logic where required; browser scroll, focus, selection, and platform shortcuts remain native. |
| `crates/trouve-app/ui/settings-window.slint` | `src/components/settings-screen.ts`, provider/persona/local/CLI/workspace/management/code-review settings components | General, Sessions & Chat (including session naming), Providers, Personas & Models, MCP, Integrations, Appearance, Notifications, About, validation, login/device flow, installed fonts, install/download progress, and centered desktop layout are implemented. |
| `crates/trouve-app/ui/theme.slint` | `src/styles/tokens.css`, `themes.css`, `app.css` | Semantic colors, density, radii, typography, selection, status, focus, and forced-colors contracts map to CSS custom properties; widget-internal chrome may differ as allowed by the plan. |

### Shared Rust client/view model and native host

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-client-core/src/client.rs` | `src/services/protocol-client.ts`, `cursor-event-stream.ts`, `protocol-ingress.ts`, `thread-ingress.ts`; `web/review-ui/src/api.ts` for detailed review jobs | Generated-path HTTP mutations/queries, response validation, safe errors, URL encoding, cursor-bearing snapshots, bounded thread-view pages, snapshot-to-SSE cursor handoff, SSE replay/resume/reconnect, and empty responses are present. App-only review methods are consolidated behind the current review APIs; detailed job/task/stat/event operations remain in the existing review web app. |
| `crates/trouve-client-core/src/lib.rs` | Direct ES module imports and `src/contexts/app-contexts.ts` | Rust's module re-export barrel has no behavioral web equivalent. TypeScript uses explicit modules and stable context interfaces. |
| `crates/trouve-client-core/src/protocol_compatibility.rs` | `src/services/protocol-client.ts`, `src/services/protocol-ingress.ts` | Native preview hosts share one compatibility parser while the generated TypeScript client reads and validates the same server-info protocol version before starting ingress. |
| `crates/trouve-client-core/src/viewmodel.rs` | `src/state/thread-view-model.ts`, `tool-output.ts`, `src/services/thread-ingress.ts` | Every folded event and protocol `ThreadViewItem` snapshot variant maps explicitly. Tool output head/tail truncation and UTF-8 safety, approvals/questions, commands, queue, todos, compaction including failure, usage, turn state, idempotency, cursor ordering, folded-page offsets, and bounded buffers are preserved. |
| `crates/trouve-desktop-host/src/gateway.rs` | `src/services/host-client.ts`, generated host types/validators, `runtime-validation-contract.test.ts` | This remains the native half of the typed bridge. TS validates capability/preferences/lifecycle/action responses; origin/CSRF, no-store, body/path bounds, safe proxy/header behavior, assets, and SSE streaming stay host-enforced. No durable agent state crosses it. |
| `crates/trouve-desktop-host/src/lib.rs` | `src/services/capabilities.ts`, `host-client.ts`, preference and attachment services | Host capability model, defaults, system fonts, lifecycle cursor feed, pending close, safe URL/file verification, notification/attachment bounds, preferences, assets, and validation have generated TS contracts and explicit degradation paths. |
| `crates/trouve-desktop-host/tests/openapi_snapshot.rs` | generated host schema/types/validators and `runtime-validation-contract.test.ts` | Cross-language drift gate; it has no shipping UI behavior. |

### Servo qualification embedder

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-servo-embed-preview/src/main.rs` | Shared desktop Lit artifact, `src/services/host-client.ts`, capability adapters | Direct chrome-free Servo embedding supplies mouse/touch/wheel/keyboard/IME, clipboard, navigation confinement, theme/cursor/title, rendering, lifecycle, attachments, notifications, sleep, safe open, and minimum size. The TS application is identical to Wry; unsupported host capabilities degrade from the advertised capability set. |
| `crates/trouve-servo-embed-preview/src/system_opener.rs` | `src/services/host-client.ts` plus Servo native actions | Same bounded, process-reaping safe-open behavior as the application host. |
| `crates/trouve-servo-embed-preview/src/web_preview_support.rs` | `src/services/host-client.ts`, `protocol-client.ts`, shared desktop `FrontendSource` | Same explicit protocol/bootstrap/gateway and packaged/runtime/Vite source contract as Wry; isolated Cargo/SQLite resolution is a native qualification concern. |

### Reusable code-view widget

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-slint-code-view/build.rs` | Vite component imports/build | Build-only compilation maps to ordinary TS bundling. |
| `crates/trouve-slint-code-view/examples/code_view_demo.rs` | `src/app/component-gallery.ts`, `gallery.ts` | Gallery fixture replaces the native demo and participates in browser visual/selection/accessibility checks. |
| `crates/trouve-slint-code-view/src/lib.rs` | `src/components/code-view.ts`, `file-language.ts`, `src/services/content-worker-client.ts` | Plain text, syntax spans, line numbers, selection/copy bounds, language detection, and large-document behavior are preserved; CodeMirror adds virtualized Unicode/IME/search/accessibility behavior. |
| `crates/trouve-slint-code-view/ui/code-view-window.slint` | Component gallery wrapper | Demo window only; no product behavior to port. |
| `crates/trouve-slint-code-view/ui/code-view.slint` | `src/components/code-view.ts` | Read-only selectable viewport, line gutters, theme/font updates, label/ARIA, scroll, and large-source virtualization are implemented. |

### Reusable diff-view widget

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-slint-diff-view/build.rs` | Vite component imports/build | Build-only compilation maps to TS bundling. |
| `crates/trouve-slint-diff-view/examples/diff_view_demo.rs` | `src/app/component-gallery.ts` | Browser gallery covers unified/split modes, selection, theme, and visual behavior. |
| `crates/trouve-slint-diff-view/src/lib.rs` | `src/components/diff-parser.ts`, `diff-mode.ts`, `diff-line-numbers.ts`, `diff-view.ts` | Multi-file/hunk parsing, old/new line tracking, stats, collapsed files, flattened rows, and selection are preserved; web parsing additionally handles more quoted/binary/path cases. |
| `crates/trouve-slint-diff-view/ui/diff-view-window.slint` | Component gallery wrapper | Demo window only. |
| `crates/trouve-slint-diff-view/ui/diff-view.slint` | `src/components/diff-view.ts`, inspection diff controls/workspace | Unified/split presentation, search/selection, line mapping, mode policy, color/status semantics, and virtualization are implemented. Oversized input remains virtualized while skipping the expensive parser/MergeView layer. |

### Reusable Markdown widget

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-slint-markdown/build.rs` | Vite component imports/build | Build-only compilation maps to TS bundling. |
| `crates/trouve-slint-markdown/examples/markdown_demo.rs` | `src/app/component-gallery.ts` | GFM/streaming/browser fixture replaces the native example. |
| `crates/trouve-slint-markdown/src/lib.rs` | `src/components/streaming-markdown.ts`, `markdown-view.ts`, `src/services/markdown-renderer.ts`, content worker | Streaming stable prefix/tail, headings/lists/quotes/fences/tables/code, syntax, links, and copy are preserved. unified/remark GFM is a parser superset; sanitization and HTTPS policy strengthen untrusted-content handling. |
| `crates/trouve-slint-markdown/ui/markdown-view.slint` | `src/components/markdown-view.ts` | Semantic browser markup replaces hand-laid blocks while preserving hierarchy, spacing, code/table scrolling, selection, and theme. |
| `crates/trouve-slint-markdown/ui/markdown-window.slint` | Component gallery wrapper | Demo window only. |

### Reusable terminal widget

| Retained source | Corresponding TS/JS source(s) | Disposition |
| --- | --- | --- |
| `crates/trouve-slint-terminal/build.rs` | Vite/xterm component imports/build | Build-only compilation maps to TS bundling. |
| `crates/trouve-slint-terminal/examples/terminal_demo.rs` | Component gallery and live `terminal-panel.ts` | The real protocol PTY plus gallery replaces the synthetic native demo. |
| `crates/trouve-slint-terminal/src/lib.rs` | `src/components/terminal-view.ts`, `terminal-control-sequences.ts`, `terminal-clipboard.ts`, `src/services/terminal-output-stream.ts` | ANSI/grid state, scrollback cap, output framing, keys/mouse/paste, search, selection, hyperlinks, title/bell/OSC 52/resize callbacks, UTF-8 partial bounds, and palette mapping are preserved or delegated to xterm's mature terminal engine. |
| `crates/trouve-slint-terminal/ui/terminal-grid.slint` | xterm viewport inside `terminal-view.ts` | Grid paint, cursor, selection, hyperlinks, scroll, focus, keyboard, paste, and mouse reporting are supplied by xterm with explicit Trouve control-sequence policy. |
| `crates/trouve-slint-terminal/ui/terminal-view.slint` | `src/components/terminal-view.ts`, `terminal-panel.ts` | PTY opens on first selection, tabs retain independent state, prompt/input is focusable, search/restart/mobile key controls work, status/history/bell are visible, and teardown kills/disposes ephemeral terminals. |
| `crates/trouve-slint-terminal/ui/terminal-window.slint` | Component gallery wrapper | Demo window only. |

## Behavioral coverage by concern

| Concern | Source-level evidence |
| --- | --- |
| Chat event folding | Exact 21-variant Rust/TS reducer comparison; fold-order, duplicate/stale cursor, bounded output, compaction, approval/question, todos/queue/commands, and cancellation tests. |
| Turn management | Start/send acknowledgement gaps, queue/update/reorder/delete/dispatch, cancel, send-next after cancel, stale originating-thread responses, reconnect, and close-while-processing flows. |
| Chat rendering | Cursor-addressed folded bootstrap, contiguous lazy history pages, keyed virtualization, reconnect batching, prepend/tail anchoring, bookmarks, accessible full history, collapsed-body unmounting, Markdown streaming, raw/formatted output, tool metadata/diffs, processing/activity/status rows, attachments, links, copy. |
| Inspection | Diff/files/PR/MCP/terminal tabs, lazy loading, selection, safe reveal/open, shared PR projection, terminal creation/focus/resize/restart/kill. |
| Management/settings | Every native section and callback, cursor-ordered live settings, provider/device flows, installed fonts, downloads/install/cancel/rates, automation event refresh, PR integration consistency. |
| Lifecycle | Foreground/online reconnect, focus/unread/notification policy, sleep inhibition, geometry and resume, pending-close Quit/Wait and quit/Cancel, quit-on-idle cancellation, process cleanup. |
| Bounds/performance | Normalized projections, LRU thread views, keyed retained widgets, virtualized chat/code/diff/lists, 5,000 terminal scrollback, 512 KiB replay, bounded tool/text/clipboard/file/diff payloads, lazy worker/routes, idle worker disposal. |
| Security | Runtime protocol/host validation, safe diagnostics, no arbitrary bridge, CSRF/origin checks, verified session paths, HTTPS-only external open, Markdown sanitization, CSP-safe validators, OSC 52 policy, no API/SSE PWA caching. |

## Validation

The completed comparison was validated with:

- both strict TypeScript projects, generated-validator drift, source format,
  and source-policy checks;
- all 93 Vitest files and 487 tests, including the new 50-source inventory and
  exact Rust/TypeScript event-reducer comparison;
- desktop and PWA production builds, artifact-boundary verification, and
  bundle budgets (821,904/812,381-byte entries, 335,033-byte worker, and
  2,791,353/2,781,830 total JavaScript bytes);
- the combined desktop/mobile Chromium projects: all 20 applicable tests
  passed and four project-inapplicable cases skipped. The terminal gallery
  references changed after restoring the native 100×28 initial grid; desktop
  and mobile expected/actual images were inspected, the intended baselines
  were updated, and the complete matrix then passed;
- `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test --workspace` (offline-safe ignored tests remained ignored);
- the Wry preview check against the built desktop assets and the isolated Servo
  workspace formatting check.

The isolated Servo `cargo check` still stops in mozangle's build script because
this development host does not provide `libclang.so`. It reaches that external
dependency after checking `trouve-desktop-host`; the supported Ubuntu Servo CI
job installs libclang and remains the authoritative compile gate. This is the
same documented qualification-host limitation, not a TypeScript/Rust source
parity failure.

## Conclusion

The source comparison is complete for the current tree and mechanically fails
when the retained frontend source set drifts. Relevant native behavior has a
TypeScript implementation, a deliberate browser/native translation, or an
explicit architecture-level qualification disposition. Slint remains in the
tree as the visual/rollback baseline during the staged Wry rollout, but it is
no longer carrying an undiscovered repository-only UI behavior in the audited
source set.
