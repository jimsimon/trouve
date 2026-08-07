# Web frontend parity and qualification ledger

**Status:** Existing Slint functionality ported; promotion qualification remains open

**Last updated:** 2026-08-07

**Migration plan:** [Web frontend migration implementation plan](web-frontend-migration-plan.md)

**Implementation audit:** [Web frontend implementation audit](web-frontend-implementation-audit.md)

**Source audit:** [Rust/Slint to TypeScript frontend source-parity audit](web-frontend-source-parity-audit.md)

**Decision record:** [ADR 0023: Lit web frontend and webview host](../adr/0023-lit-web-frontend-and-webview-host.md)

## Purpose and interpretation

This document records the current, source-inspected Lit coverage for the 21
migration surfaces in section 10 of the migration plan. The existing Slint
frontend's user-visible callback contract is now functionally represented in
the Lit frontend. That claim is enforced by
[the exhaustive callback manifest](../../web/app-ui/src/app/slint-callback-parity.test.ts),
which extracts all 134 `AppWindow` callbacks from `app.slint` and requires
exactly one documented Lit implementation or intentional browser-native
consolidation for each callback.
The complementary source audit inventories all 50 retained Rust/Slint frontend
files, compares their non-callback state/performance/failure behavior, and
mechanically rejects an unaudited source file or Rust/TypeScript thread-event
reducer mismatch.

This is still an implementation and evidence ledger, not a declaration that
the web frontend is ready to replace Slint. Functional port closure and
promotion qualification are deliberately separate claims.

The current labels describe how much of a workflow can be exercised in the
feature-gated preview:

| Status | Meaning |
| --- | --- |
| **functionally-ported** | The current Slint surface, its user-visible callbacks, and its represented state are implemented in Lit. This does not imply visual or platform qualification. |
| **gated** | The workflow is deliberately unavailable or cannot be promoted until a capability or external qualification gate is satisfied. |

No status in this ledger means that visual, keyboard, accessibility,
desktop-device, mobile-device, performance, memory, lifecycle, failure,
security, packaging, or soak acceptance has passed. Existing unit tests,
component markup, the component gallery, and static visual-contract tests are
useful implementation evidence, but they are not substitutes for the
qualification evidence defined below.

Current status totals:

| Status | Surfaces |
| --- | ---: |
| functionally-ported | 20 |
| gated | 1 |
| **Total** | **21** |

## Non-negotiable parity contract

The existing Slint frontend is the visual and interaction baseline throughout
the migration. Slint remains the default and rollback frontend until the
promotion gates in the migration plan pass. The primary baseline sources are
the [Slint application](../../crates/trouve-app/ui/app.slint), the
[authoritative Slint theme definitions](../../crates/trouve-app/src/theme.rs),
the retained generic
[code](../../crates/trouve-slint-code-view),
[diff](../../crates/trouve-slint-diff-view),
[Markdown](../../crates/trouve-slint-markdown), and
[terminal](../../crates/trouve-slint-terminal) widgets, and the
[UX screen map](ux-screen-map.md).

The Lit frontend must preserve:

- all five existing themes: dark, light, high-contrast-dark,
  colorblind-dark, and colorblind-light;
- the semantic color roles and status meanings in every theme;
- the core desktop layout, panel proportions, density, hierarchy, spacing,
  selection treatment, action placement, and responsive information order;
- the current workflows, keyboard flow, disclosure defaults, warning
  hierarchy, trust cues, and recovery behavior; and
- the recognizable look, feel, and core UX of Trouve. This migration is not a
  redesign.

Ordinary widget chrome may differ where the browser component cannot or
should not reproduce Slint pixel-for-pixel. For example, WebAwesome tabs may
have different internal chrome from Slint tabs. Such differences are allowed
only when semantic colors, placement, density, information hierarchy,
interaction semantics, and the surrounding experience remain faithful. Any
intentional difference that affects the visible or interactive contract must
be documented and approved in the deviation register before promotion.

The web architecture continues to use
[@lit/context](../../web/app-ui/src/contexts/app-contexts.ts) for stable
service and scoped-store injection. It contains
[@lit-labs/signals](../../web/app-ui/src/state/reactivity.ts) behind the small,
Trouve-owned reactivity adapter rather than exposing the experimental package
throughout the component tree.

The initial mobile solution is the installable PWA that shares this Lit
frontend and speaks the public HTTP/SSE protocol to a remote Trouve server.
Browser-unsupported behavior must be explicitly capability-gated and must
never imply arbitrary access to the phone or host filesystem. Native mobile,
embedded-webview, and other packaging options will be evaluated later, after
the PWA produces real usage, device, accessibility, security, and performance
evidence. Those alternatives are not part of the initial mobile acceptance
target.

## Baseline and preview evidence

The main cross-cutting implementation anchors are:

- [Lit application shell](../../web/app-ui/src/app/trouve-app.ts)
- [application router](../../web/app-ui/src/router/app-router.ts)
- [normalized application store](../../web/app-ui/src/state/app-store.ts)
- [cold-start durable protocol ingress](../../web/app-ui/src/services/protocol-ingress.ts)
- [attention-first inbox projection](../../web/app-ui/src/state/session-inbox-model.ts)
- [command palette](../../web/app-ui/src/components/command-palette.ts)
- [command palette model](../../web/app-ui/src/components/command-palette-model.ts)
- [stable application contexts](../../web/app-ui/src/contexts/app-contexts.ts)
- [contained signals adapter](../../web/app-ui/src/state/reactivity.ts)
- [reactivity import-boundary test](../../web/app-ui/src/state/reactivity-boundary.test.ts)
- [theme controller](../../web/app-ui/src/services/theme-controller.ts)
- [generated semantic themes](../../web/app-ui/src/styles/themes.generated.css)
- [application tokens](../../web/app-ui/src/styles/tokens.css)
- [Slint-derived theme generator](../../scripts/generate_web_themes.py)
- [visual parity component gallery](../../web/app-ui/src/app/component-gallery.ts)
- [static visual contract tests](../../web/app-ui/src/styles/visual-contract.test.ts)
- [exhaustive Slint callback parity manifest](../../web/app-ui/src/app/slint-callback-parity.test.ts)
- [application parity contract tests](../../web/app-ui/src/app/trouve-app-parity.test.ts)
- [humanized tool, inline-diff, todo, and activity presentation](../../web/app-ui/src/components/tool-presentation.ts)
- [session pull-request integration and lifecycle panel](../../web/app-ui/src/components/session-pr-panel.ts)
- [local model, runtime, catalog, and fit controls](../../web/app-ui/src/components/local-model-settings.ts)
- [desktop notification and lifecycle coordinator](../../web/app-ui/src/services/desktop-host-coordinator.ts)
- [event-derived session notifications](../../web/app-ui/src/services/session-notifications.ts)
- [PWA install controller](../../web/app-ui/src/services/pwa-install.ts)
- [PWA pull-to-refresh controller](../../web/app-ui/src/services/pull-to-refresh.ts)
- [typed host capability controller](../../web/app-ui/src/services/capabilities.ts)
- [desktop/PWA host client boundary](../../web/app-ui/src/services/host-client.ts)
- [desktop gateway and webview host](../../crates/trouve-desktop-host/src/lib.rs)
- [Wry database-safe preview bootstrap](../../crates/trouve-app/src/web_preview_support.rs)
- [chrome-free pinned Servo nightly embedding harness](../../crates/trouve-servo-embed-preview/README.md)
- [Servo database-safe host bootstrap](../../crates/trouve-servo-embed-preview/src/web_preview_support.rs)
- [Wry fallback preview](../../crates/trouve-app/src/web_preview.rs)
- [PWA service worker](../../web/app-ui/src/pwa/service-worker.ts)
- [shared Rust/web thread projection fixture](../../crates/trouve-client-core/fixtures/thread-turn.json)
- [bounded live tool-output projection](../../web/app-ui/src/state/tool-output.ts)
- [approval action controller](../../web/app-ui/src/components/approval-controls.ts)
- [lazy content-worker client](../../web/app-ui/src/services/content-worker-client.ts)
- [content worker](../../web/app-ui/src/workers/content-worker.ts)
- [Playwright browser matrix](../../web/app-ui/playwright.config.ts)
- [visual and accessibility browser suite](../../web/app-ui/e2e/visual-accessibility.spec.ts)
- [shared pull-request projection browser suite](../../web/app-ui/e2e/app-shell.spec.ts)
- [bundle budget gate](../../web/app-ui/scripts/check-bundle-budget.mjs)
- [source format check](../../web/app-ui/scripts/check-source-format.mjs)
- [source policy lint](../../web/app-ui/scripts/check-source-policy.mjs)
- [npm dependency notice gate](../../web/app-ui/scripts/generate-third-party-notices.mjs)
- [npm CycloneDX SBOM generator](../../web/app-ui/scripts/generate-npm-sbom.mjs)
- [Rust dependency notice gate](../../scripts/generate_rust_third_party_notices.py)

These paths establish that the preview has real implementation coverage. They
do not establish visual equivalence or platform qualification.

### Current engine and visual qualification snapshot

Desktop engine testing is Servo-first. The qualification harness in
`crates/trouve-servo-embed-preview` pins the 2026-08-02 Servo nightly at exact
revision `35672cc3d4beb768489f5218e73bee7aff0ddb01` and embeds one `WebView`
directly in a native window over the full client area, with normal
operating-system decorations but no servoshell address bar, tabs, or browser
chrome. The nightly crate still reports Servo 0.4.0, but the dependency is the
reproducible upstream revision recorded by
[ADR 0025](../adr/0025-pin-servo-qualification-to-nightly-revision.md), not the
published 0.4.0 source. It loads the packaged Lit frontend through the hardened
loopback gateway with the required experimental web-platform features enabled.
This nightly provides the current keyboard-selection path in editable web
controls; general mouse selection/copy across rendered document text is still
an explicit qualification item rather than a claimed engine capability.
This establishes a real in-process, chrome-free embedding path; it does not
close text-selection, accessibility-action, native-capability,
renderer-recreation, lifecycle, crash/OOM-recovery, memory/performance,
visual-parity, packaging, or six-platform gates. Wry remains the fallback and
comparison host and is also unqualified.

The Servo harness is an excluded nested Cargo workspace with its own lockfile,
as recorded in [ADR 0024](../adr/0024-isolated-servo-embedding-qualification-workspace.md).
The pinned nightly graph uses rusqlite 0.37 and libsqlite3-sys 0.35, while the
root workspace uses rusqlite 0.40 and libsqlite3-sys 0.38; Cargo cannot resolve
both native SQLite link packages in one graph. The harness therefore cannot
link or start `trouve-server`. It requires and probes an explicit
`TROUVE_SERVER_URL`, reaches
that server only through the hardened loopback gateway, and gives both Servo
storage and host preferences retained temporary directories. It cannot open
Trouve's default database. The Wry comparison host likewise requires an
explicit server URL and must not create a second database owner. These are
data-integrity constraints: two live engines over one SQLite database would
have competing writers and separate in-memory event broadcasts, schedulers,
turn state, and worktree locks.

From the repository root, after building `web/app-ui/dist/desktop`, run Servo
first:

```sh
TROUVE_SERVER_URL=http://127.0.0.1:7433 \
TROUVE_APP_UI_DIST=/absolute/path/to/trouve/web/app-ui/dist/desktop \
  cargo run \
    --manifest-path crates/trouve-servo-embed-preview/Cargo.toml \
    --locked
```

Use Wry only as the fallback/comparison run:

```sh
TROUVE_SERVER_URL=http://127.0.0.1:7433 \
TROUVE_APP_UI_DIST=/absolute/path/to/trouve/web/app-ui/dist/desktop \
  cargo run -p trouve-app --features web-preview --bin trouve-web-preview
```

Both hosts use ADR 0026's shared desktop-host source selector. In debug and
qualification runs, `TROUVE_APP_UI_DIST` is loaded at process startup and does
not trigger a Cargo rebuild. For HMR, omit that variable, run `npm run dev`
from `web/app-ui`, and set
`TROUVE_APP_UI_DEV_URL=http://127.0.0.1:5173` on the selected preview. The
gateway remains the page origin and reserves its native and `/v1` routes.

The first live preview did not visually match the Slint desktop closely
enough. The 2026-08-03 rendered-parity correction pass now covers the shell,
workspace and session hierarchy, new-session and new-thread setup, chat and
composer cards, approval and question presentation, queue controls, Files,
unified Diff, Todos, MCP, Terminal, session PR, the account Pull Requests
dashboard, Automations and its editor, every Settings section, session action
menus, destructive dialogs, and the desktop close dialog. The pass uses the
Slint source and local side-by-side captures as implementation references;
ordinary web-widget chrome may vary, but the Slint density, layout,
information hierarchy, semantic colors, action order, and core workflows
remain the contract.

The 2026-08-04 follow-up parity pass also aligns the transcript's composer
separation, transparent grouped-activity container, disabled turn controls,
full-access error treatment, and Slint-style tail-follow recovery: reaching
the rendered bottom manually now dismisses “Jump to latest” and resumes live
following even while virtual row measurements are converging. It also aligns
subscription meter thresholds and labels, the Vendor CLI uninstall action,
the informational type scale in Git & Worktrees, Integrations, and Appearance,
and the GitHub Enterprise add-form baseline.

The same pass closes the initial responsive implementation gaps: Files is a
tree-to-viewer flow on narrow screens, approvals become a large-target bottom
sheet above mobile navigation, pull-request group ordering has explicit touch
controls, full-screen routes honor display cutouts and safe areas, Settings
uses horizontal section navigation, and terminal touch modifiers remain
available without changing the desktop layout.

The 2026-08-06 chat-hierarchy pass replaces repeated boxed activity rows with
a restrained timeline for each contiguous activity sequence. A faint neutral
rail and small nodes connect related thoughts and tools; completed work stays
neutral, active or expanded work receives the blue accent, and approval or
failure nodes use their semantic warning or error colors. Prose, questions,
and context-compaction markers break the timeline, so final responses separate
through whitespace instead of another container. Response text is selectable
in the web frontend, so the Slint-era response copy/raw-view header buttons are
absent. Right-click or Shift+F10 on an Agent response instead opens an
accessible menu with **Copy as markdown** and, when text is selected, the
ordinary **Copy** action. Native link, image, tool, thought, question, and
compaction context menus remain untouched.

This work is implementation progress, not promotion evidence. The local
captures were not produced by the complete deterministic five-theme/device
matrix, so the evidence register remains open and all Servo and Wry
qualification gates remain unpassed.

## Current functional port closure ledger

The table below is the authoritative implementation checkpoint. “Ported”
means that the current Slint behavior represented by `AppWindow` callbacks,
controller state, and the screen map has a Lit implementation. The remaining
column names promotion evidence, not missing product functionality. Browser
primitives consolidate Slint callbacks where the platform already owns the
interaction: native form value changes, drag/drop, DOM text selection, and
xterm input/resize are examples.

| # | Surface | Functional port coverage | Principal evidence | Remaining promotion qualification |
| ---: | --- | --- | --- | --- |
| 1 | Shell and inbox | Three-column desktop shell, persisted splitters, responsive pane routes, workspace register/close/reorder, active and archived session groups, attention-first sorting, selection recovery, session rename/archive/delete, PR badges, command palette, and connection/retry states. | [application shell](../../web/app-ui/src/app/trouve-app.ts), [session list](../../web/app-ui/src/components/session-list.ts), [workspace settings](../../web/app-ui/src/components/workspace-settings.ts) | Paired Slint/Lit screenshots, focus and AT matrix, real desktop/PWA lifecycle, performance, and memory. |
| 2 | Session and thread management | Prompt-first session creation with workspace, branch/fetch, mode, model, thinking, permission, bounded attachments, provisional creation recovery, and cancelable new-thread setup with inherited defaults. Thread select, create, rename, archive, delete, and route restoration are wired. | [new-session model](../../web/app-ui/src/app/new-session-model.ts), [new-thread setup](../../web/app-ui/src/components/new-thread-setup.ts), [thread screen](../../web/app-ui/src/components/thread-screen.ts) | Failure-injection, slow/offline races, full keyboard/IME/AT runs, and visual evidence. |
| 3 | Chat | Streaming user/assistant/thinking/tool/error turns, sanitized selectable Markdown, safe links, hover/focus response copy plus a **Copy as markdown** context action, attachments, disclosure state, raw and formatted tool regions, humanized tool names, file links, inline diffs, todos, timeline-based activity hierarchy, usage, tool/thought/attachment copy, Slint-shaped inline desktop approvals and a large-target mobile approval sheet, question interaction, bounded output, keyed virtualization, follow-tail, stable anchoring with invalid-bookmark recovery, tail-only live-log announcements, active-stream foreground resume, reduced motion, and an accessible full-history fallback. | [thread screen](../../web/app-ui/src/components/thread-screen.ts), [chat presentation](../../web/app-ui/src/components/chat-presentation.ts), [tool presentation](../../web/app-ui/src/components/tool-presentation.ts), [thread ingress](../../web/app-ui/src/services/thread-ingress.ts) | Large-history measurements, renderer/selection testing on both engines, screen readers, mobile memory, and screenshot comparison. |
| 4 | Composer, completion, queue, and attachments | Autogrow input, IME-safe keyboard handling, slash and file completion with DOM UTF-16/protocol UTF-8 conversion, acknowledgement-aware start/cancel/queue/send-after-cancel controls, queued prompt edit/delete/reorder/send-now and paused-queue restart, durable thread-scoped unsubmitted text/cursor/attachment drafts, thread-scoped async mutation recovery, context and session usage, file picker, drag/drop, pasted images, attachment limits, and PWA quick replies. | [thread screen](../../web/app-ui/src/components/thread-screen.ts), [draft persistence](../../web/app-ui/src/services/composer-drafts.ts), [turn controls](../../web/app-ui/src/components/chat-turn-controls.ts), [completion model](../../web/app-ui/src/components/composer-completion.ts), [queue controls](../../web/app-ui/src/components/queue-controls.ts), [attachments service](../../web/app-ui/src/services/attachments.ts) | Cross-engine IME/dead-key/mobile-keyboard matrix, picker denial/cancel, queue recovery soak, and visual evidence. |
| 5 | Diff | Unified/split modes, per-file grouping, line numbers, changed-file keyboard navigation, copy, responsive unified-only behavior, refresh, checkpoint undo/redo, expansion/collapse, and parsed status/error states. | [diff view](../../web/app-ui/src/components/diff-view.ts), [inspection diff controls](../../web/app-ui/src/components/inspection-diff-controls.ts), [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts) | Large-patch performance/memory, selection/AT alternatives, theme screenshots, touch, and engine disposal. |
| 6 | Files and code | Lazy cached directory tree, roving keyboard navigation, retry/error/empty states, file loading, syntax-aware code view, line/range reveal, Markdown preview, selection/copy, capability-gated desktop open/reveal actions, and a narrow list-to-viewer flow whose tree toggle provides the return path. | [inspection file tree](../../web/app-ui/src/components/inspection-file-tree.ts), [code view](../../web/app-ui/src/components/code-view.ts), [file reveal model](../../web/app-ui/src/components/file-reveal.ts) | Large-tree/file budgets, binary fixtures, engine selection, mobile copy/scroll, visual and AT evidence. |
| 7 | Terminal | Multiple PTY tabs, create/select/restart/close/exit state, xterm input/paste/copy/selection/search/links/mouse/wheel/resize/IME, offset resume, duplicate-free streaming, OSC 52 confirmation, and renderer disposal. | [terminal panel](../../web/app-ui/src/components/terminal-panel.ts), [terminal view](../../web/app-ui/src/components/terminal-view.ts), [terminal clipboard policy](../../web/app-ui/src/components/terminal-clipboard.ts) | Native clipboard and IME matrices, one/five-terminal budgets, suspend/resume and renderer recreation, AT alternative, touch controls. |
| 8 | Todos and plan | Current plan snapshot, pending/in-progress/completed/cancelled semantics, progress summary, empty state, and conditional inspection tab. | [todo plan panel](../../web/app-ui/src/components/todo-plan-panel.ts), [todo plan model](../../web/app-ui/src/components/todo-plan-model.ts) | Streaming/stale fixture comparison, responsive screenshots, semantics and live-region verification. |
| 9 | Session pull request | Explicit GitHub setup route, PR eligibility/create form, list/detail state, checks, reviews, reviewers, mergeability, safe external open, refresh/errors, and lifecycle controls advertised by the server. The pane and session status indicators consume one shared session projection: durable account snapshots provide current GitHub state while the authoritative session lookup preserves cross-branch associations discovered from session activity. | [session PR panel](../../web/app-ui/src/components/session-pr-panel.ts), [shared application store](../../web/app-ui/src/state/app-store.ts), [session PR model](../../web/app-ui/src/components/session-pr-panel-model.ts) | Live GitHub enterprise/host runs, OAuth expiry, browser navigation, lifecycle failure recovery, visual/AT evidence. |
| 10 | Pull-request dashboard | Repository filters, grouped/reorderable/collapsible PR inbox with keyboard, drag, and explicit coarse-pointer ordering controls, countdown and refresh, status/reviewer/check summaries, open/copy/chat/fix actions, App health and administration, repository policy, reviewer personas, review jobs, and Review operations. Cold startup replays durable server projections through the session-summary boundary so an unchanged refresh cannot leave the dashboard or session indicators empty. | [PR dashboard](../../web/app-ui/src/components/pull-requests-dashboard.ts), [durable protocol ingress](../../web/app-ui/src/services/protocol-ingress.ts), [code-review dashboard](../../web/app-ui/src/components/code-review-dashboard.ts) | Large-list and live-provider soak, route/focus restore, screenshots, keyboard/AT/mobile matrices. |
| 11 | Automations | List/detail, templates, create/edit/delete confirmation, enable/disable, run-now, schedule/day/time/time-zone controls, workspace/mode/model/permission configuration, validation, history, selection, loading, and failure states. | [automations screen](../../web/app-ui/src/components/automations-screen.ts), [automation model](../../web/app-ui/src/components/automations-model.ts) | Live scheduler failures/concurrency, touch schedule editing, screenshots, keyboard/AT and lifecycle runs. |
| 12 | General and appearance | All five themes, system preference, font scale, reduced motion, semantic preview, layout preference persistence, keep-awake/sleep preference, and capability-aware PWA/desktop explanations. | [settings screen](../../web/app-ui/src/components/settings-screen.ts), [appearance preferences](../../web/app-ui/src/services/appearance-preferences.ts), [general preferences](../../web/app-ui/src/services/general-preferences.ts) | Five-theme paired captures, forced colors/zoom, persistence/restart, OS sleep behavior, and supported-device matrix. |
| 13 | Notifications | Preference toggles, permission/capability state, user-initiated test, exact durable approval/question/completion/failure edges, repeated attention requests, compact failure/question detail, focused-session suppression, activation routing, desktop attention/sound, and unsupported/reliability explanations. | [notification preferences](../../web/app-ui/src/services/notification-preferences.ts), [session notifications](../../web/app-ui/src/services/session-notifications.ts), [desktop host coordinator](../../web/app-ui/src/services/desktop-host-coordinator.ts) | Real OS/browser permission matrices, background/suspend reliability, activation routes, quiet/offline and PWA publication evidence. |
| 14 | Providers and onboarding | Provider presets and custom endpoints, secret entry, reset/validation, health and models, API-key and OAuth/device/callback login, polling, cancellation, failure/expiry, delete, and vendor CLI install/update/cancel/uninstall lifecycle. | [provider settings](../../web/app-ui/src/components/provider-settings.ts), [CLI settings](../../web/app-ui/src/components/cli-settings.ts) | Live provider matrices, secret redaction audit, OAuth interruption/expiry, onboarding screenshots, keyboard/mobile/AT evidence. |
| 15 | Modes and models | Data-driven modes, per-mode provider/model/thinking/permission defaults, inheritance, availability/health cues, model options, search, reset, and refresh. | [mode settings](../../web/app-ui/src/components/mode-settings-panel.ts), [model picker](../../web/app-ui/src/components/model-picker.ts), [model option controls](../../web/app-ui/src/components/model-option-controls.ts) | Unsupported-combination fixtures, live catalog churn, visual density, keyboard combobox and mobile evidence. |
| 16 | Local models | Enabled/status/hardware state, llama.cpp runtime install/update/cancel/uninstall, server start/stop/restart controls, installed model management, download progress/cancel/delete, catalog search, GPU/CPU/too-large fit filters, and manual model addition. | [local model settings](../../web/app-ui/src/components/local-model-settings.ts) | Live runtime/download/disk/concurrency failures, remote-host wording on devices, progress screenshots, memory and AT evidence. |
| 17 | Git and worktrees | Workspace/branch management represented by the current frontend plus Git/title-model status, resource/install progress, cancellation, warnings, and protocol-backed configuration. No web client bypasses the protocol or session-owned worktree boundary. | [workspace settings](../../web/app-ui/src/components/workspace-settings.ts), [management settings panels](../../web/app-ui/src/components/management-settings-panels.ts) | Dirty/conflict/remote/failure fixtures exposed by the server, destructive confirmations, visual hierarchy, and live worktree soak. |
| 18 | MCP | User/workspace scoped server CRUD, command/args/environment editing, enable/disable, effective per-session scopes, health refresh/reconnect, logs, copying, masking, validation, and responsive long-output behavior. | [management settings panels](../../web/app-ui/src/components/management-settings-panels.ts), [session MCP panel](../../web/app-ui/src/components/session-mcp-panel.ts) | Live reconnect/restart/secret audit, large-log memory/disposal, mobile long lines, screenshots and AT evidence. |
| 19 | Integrations | GitHub.com and enterprise host add/remove, configuration status, login/device/callback flows, polling/cancel, disconnect, health/errors, validated navigation, and integration deep links from PR surfaces. | [management settings panels](../../web/app-ui/src/components/management-settings-panels.ts), [session PR panel](../../web/app-ui/src/components/session-pr-panel.ts) | Live multi-host OAuth and re-auth, PWA redirect origins, cancellation/expiry, security and visual/AT evidence. |
| 20 | About and licensing | Frontend/server/protocol/deployment/connectivity/version data, packaged dependency notices, conditional Slint attribution while shipped, and desktop/PWA capability/revision information. | [settings screen](../../web/app-ui/src/components/settings-screen.ts), [generated host schema](../../web/app-ui/src/generated/host.ts) | Packaged offline artifact inspection, final inventories, platform/version screenshots, link and compliance review. |
| 21 | Desktop integration and web capabilities | Versioned typed host v8, hardened asset/API/SSE gateway, preferences, pickers, clipboard, validated file/HTTPS open, notifications, attention, sleep, focus/visibility/occlusion/window/quit lifecycle, Wry host, direct chrome-free Servo host, and PWA service worker/install/wake-lock/pull-refresh adapters are implemented. | [desktop host](../../crates/trouve-desktop-host/src/lib.rs), [Servo harness](../../crates/trouve-servo-embed-preview/src/main.rs), [Wry preview](../../crates/trouve-app/src/web_preview.rs), [PWA worker](../../web/app-ui/src/pwa/service-worker.ts) | **Gated:** Servo AT actions and complete engine matrix, Wry matrix, host security review, crash/OOM recovery, packaging/signing, six-platform artifacts, production PWA HTTPS/auth/update/deployment, and soak. |

Surfaces 1–20 are **functionally-ported**. Surface 21 is **gated** because its
implementation exists but no desktop engine or production PWA deployment may
be promoted without the independent evidence above. This state does not
authorize deleting Slint or changing the default frontend.

## Historical detailed implementation record

The detailed entries below record the incremental state from before the
exhaustive callback audit. Their older `partial`, `foundation`, and
`functional-preview` labels are retained as implementation history and are
superseded by the current closure table above. Their open lists remain useful
as qualification scenarios, but they are not the authoritative current
functional status.

### 1. Shell and inbox — partial

**Current functional port coverage**

- A routed Lit shell renders the desktop three-column composition, resizable
  splitters, primary navigation, session selection, and inspection region.
- The five Slint-derived semantic theme maps and a shared shell styling
  foundation exist. A correction pass is actively bringing the rendered shell
  back toward Slint's proportions, density, hierarchy, spacing, colors, and
  recognizable desktop experience after the first preview diverged.
- Compact mobile route navigation switches between list, conversation, and
  inspection panes rather than squeezing the desktop layout.
- Workspace registration/closure and session select/rename/archive/delete
  flows exist, including explicit confirmation for destructive actions.
- In a local desktop host, **Open** invokes the typed native directory picker
  and registers the selected UTF-8 path through the existing protocol. PWA and
  remote deployments continue to show the manual-path workflow and never
  advertise arbitrary local filesystem access.
- Active sessions use deterministic attention-first ordering, while archived
  sessions are grouped per workspace behind an accessible disclosure and a
  directly selected archived session remains visible.
- Ctrl/Cmd-K opens a keyboard-navigable command palette for primary routes,
  session/thread switching, and common session/thread actions.
- Desktop navigation and inspection splitters persist their widths through
  the typed host-preferences boundary.
- Loading, disconnected, empty, and not-found presentations are represented.
- A failed initial protocol bootstrap can recover on online/visible
  transitions or an explicit Retry action. Run generations prevent responses
  from a stopped ingress overwriting a newer connection, and coalesced session
  lifecycle events force a trailing metadata refresh.

**Primary Lit evidence**

- [application shell](../../web/app-ui/src/app/trouve-app.ts)
- [shell and responsive styling](../../web/app-ui/src/styles/app.css)
- [session list](../../web/app-ui/src/components/session-list.ts)
- [session inbox model](../../web/app-ui/src/state/session-inbox-model.ts)
- [command palette](../../web/app-ui/src/components/command-palette.ts)
- [command palette model](../../web/app-ui/src/components/command-palette-model.ts)
- [workspace management](../../web/app-ui/src/components/workspace-settings.ts)
- [workspace management model](../../web/app-ui/src/components/workspace-settings-model.ts)
- [typed host client](../../web/app-ui/src/services/host-client.ts)
- [router](../../web/app-ui/src/router/app-router.ts)
- [application store](../../web/app-ui/src/state/app-store.ts)
- [cursor-safe protocol ingress](../../web/app-ui/src/services/protocol-ingress.ts)
- [protocol ingress race/recovery tests](../../web/app-ui/src/services/protocol-ingress.test.ts)

**Missing parity and qualification work**

- Complete workspace reorder/replacement-screen, remaining selection recovery,
  picker cancellation/error and cross-platform qualification, focus
  restoration, quit, and restart-recovery behavior.
- Compare Slint and Lit panel proportions, row density, badges, selection,
  empty/recovery screens, and all five themes at canonical desktop widths.
- Complete the current visual-foundation/shell correction pass, then capture
  paired evidence; implementation changes alone do not qualify shell parity.
- Verify keyboard-only resizing/navigation, focus order and announcements,
  forced colors, large text, reduced motion, safe areas, mobile back-stack
  semantics, touch reorder alternatives, and restart selection recovery.
- Run desktop OS/DPI and phone/tablet PWA matrices; capture layout-shift,
  startup, memory, and long-session evidence.

**Qualification state:** Unqualified. No screenshot, assistive-technology,
desktop-device, mobile-PWA, or performance evidence is recorded.

### 2. Session and thread management — partial

**Current functional preview coverage**

- Existing threads can be selected. The tab-strip **+** action and command
  palette open the same provisional new-thread setup surface; cancelling it
  performs no server mutation.
- New-thread setup loads available modes, models, and providers and accepts
  mode, model, thinking, permission, an optional initial prompt, and bounded
  file/paste attachments. It warns for full-access/YOLO permission, prevents
  duplicate submission, and keeps create failures in the form for retry.
- Submitting creates the thread through the protocol and selects it. If an
  optional initial prompt fails after creation, the created thread remains
  available and the UI reports that the prompt was not queued.
- The new-session preview accepts an initial prompt, fetch-latest and branch
  choices, mode/model/permission/thinking overrides, and bounded attachments.
  It requests a generated title and uses a sanitized, length-bounded
  prompt-derived fallback when title generation is unavailable.
- Mode, model, permission, and thinking controls are wired into the creation
  flow, while existing-thread controls remain part of the thread workflow.
- Thread tabs use route-backed automatic activation with one roving tab stop
  and Arrow Left/Right, Home, and End navigation.
- The active thread's permission mode is repeated in the desktop status bar;
  full-access/YOLO state uses the existing semantic warning color in the
  active-thread selector, new-session selector, and status presentation.
- Session/thread data is projected through the application store and scoped
  view model rather than pushed through component-global state.

**Primary Lit evidence**

- [thread screen](../../web/app-ui/src/components/thread-screen.ts)
- [new-thread setup](../../web/app-ui/src/components/new-thread-setup.ts)
- [new-thread setup model](../../web/app-ui/src/components/new-thread-setup-model.ts)
- [new-thread integration tests](../../web/app-ui/src/components/thread-new-setup-integration.test.ts)
- [new-session shell flow](../../web/app-ui/src/app/trouve-app.ts)
- [new-session request and fallback model](../../web/app-ui/src/app/new-session-model.ts)
- [new-session model tests](../../web/app-ui/src/app/new-session-model.test.ts)
- [thread view model](../../web/app-ui/src/state/thread-view-model.ts)
- [shared tab-navigation model](../../web/app-ui/src/components/tab-navigation.ts)
- [application store](../../web/app-ui/src/state/app-store.ts)
- [application contexts](../../web/app-ui/src/contexts/app-contexts.ts)

**Missing parity and qualification work**

- Complete branch/remote health and status, default and attachment inheritance,
  unavailable combinations, offline blocking, provider/model churn during an
  open provisional form, richer field validation and recovery, and
  restart-recovery behavior. Qualify new-thread cancellation, create failure,
  and post-create initial-message failure against the Slint workflow.
- Apply the semantic full-access warning treatment to every remaining
  permission selector and summary, then verify it in all five themes.
- Build and qualify the owned virtualized health combobox, including fuzzy
  search, unavailable/unsupported states, pointer/touch use, and current
  information ordering and status-color meanings.
- Verify disabled-state semantics, keyboard flow, screen-reader naming,
  live-state announcements, mobile virtual keyboard behavior, offline/resume,
  and selection persistence after restart.
- Capture all-theme visual comparisons and desktop/mobile device,
  accessibility, and performance evidence.

**Qualification state:** Unqualified.

### 3. Chat — functionally-ported

**Current functional preview coverage**

- Durable thread events fold into streaming assistant/user messages, tool
  activity, approvals, questions, queue state, and todo state.
- The renderer preserves the Slint turn hierarchy: prompts are individual
  cards, each uninterrupted assistant/work run is one Agent card, consecutive
  work items form a summarized activity group, and processing state nests in
  the active Agent card whenever that card is open.
- Streaming Markdown is sanitized, and links are divided into validated
  internal and external navigation behavior. Network-path, slash-backslash,
  credential-bearing, control-character, and non-HTTPS links are removed
  before rendered anchors can be used through ordinary or auxiliary browser
  navigation.
- A bounded virtualizer supports a visible window, tail following, anchoring,
  persisted stable-ID restoration, variable-height correction, heavyweight
  unmounting, keyed Lit row identity, virtualized tail status rows, stale
  bookmark fallback, and an accessible nonvirtual history mode. Its chat log
  announces additions only while following the tail so scrolling parked
  history does not repeatedly announce remounted rows.
- Fresh clients install the server's newest 256-item folded thread snapshot
  and start SSE after that response's exact event cursor instead of replaying
  from zero. Approaching the top lazily prepends contiguous older folded pages;
  absolute item identities plus height correction keep the reader anchored,
  while accessible full-history mode loads the remaining pages explicitly.
- User, Agent, thinking, grouped activity, and individual tool disclosures
  remove their collapsed bodies from the DOM. Thinking and historical work
  use the same disclosure defaults and collapsed previews as the retained
  frontend.
- Assistant output remains styled and selectable. Its mouse/keyboard context
  menu copies the complete raw Markdown source across tool-separated response
  segments and preserves ordinary selected-text copy without permanent header
  chrome. Tool output supports formatted and raw data views, humanized tool names, all tool
  states, readable arguments/results, bounded live output, inline edit/write
  diffs, read-file targets and ranges, todo state, file navigation,
  additions/deletions, duration, and exit metadata.
- Tool cards preserve separate arguments, bounded UTF-8-safe live output, and
  final-result regions; omitted early output is explicitly marked.
- Completed turns show token/cost/duration metadata; failures and
  cancellations remain in transcript order, and copy feedback remains scoped
  to the response context action or currently selected tool presentation.
- Approval cards expose approve, always approve, and deny actions with scoped
  Y/A/N shortcuts, per-call single-flight behavior, and focus restoration.
- Pending and resolved question cards preserve wizard progression, answer
  review, skip, focus, and submission state. Starting, thinking, tool-specific
  activity, cancellation, and compaction use live processing messages.
- Reopening the same route reconnects its thread stream from the retained
  cursor, so a route-generation change cannot leave a live stream silently
  discarding subsequent chat events. Foreground and online transitions also
  force the active thread stream to reconnect from that cursor instead of
  relying on a suspended browser or webview to revive EventSource correctly.
- The explicit Reduce motion preference reaches the streaming Markdown shadow
  root and leaves processing dots/tool state static; the operating-system
  reduced-motion media query supplies the same behavior.

**Primary Lit evidence**

- [thread screen](../../web/app-ui/src/components/thread-screen.ts)
- [chat hierarchy](../../web/app-ui/src/components/chat-layout.ts)
- [turn controls](../../web/app-ui/src/components/chat-turn-controls.ts)
- [thread event projection](../../web/app-ui/src/state/thread-view-model.ts)
- [active thread ingress](../../web/app-ui/src/services/thread-ingress.ts)
- [bounded tool-output model](../../web/app-ui/src/state/tool-output.ts)
- [approval controls](../../web/app-ui/src/components/approval-controls.ts)
- [Markdown view](../../web/app-ui/src/components/markdown-view.ts)
- [owned virtualizer](../../web/app-ui/src/components/virtualization/virtualizer.ts)
- [virtualizer tests](../../web/app-ui/src/components/virtualization/virtualizer.test.ts)
- [stateful browser chat tests](../../web/app-ui/e2e/chat-session.spec.ts)

**Missing parity and qualification work**

- Qualify stable anchored restoration, follow-tail transitions, reduced
  motion, occlusion, foreground resume, and duplicate-free recovery under
  real reconnect/failure injection and substantially larger release-build
  histories. The deterministic browser fixture currently proves a bounded DOM
  for 400+ rendered turn units and the explicit full-history fallback.
- Match Slint card hierarchy, spacing, metadata prominence, disclosure
  defaults, attachment layout, status colors, and streaming behavior across
  all themes.
- Run keyboard, selection/copy, screen-reader/live-region, zoom/reflow, narrow
  touch, virtual-keyboard, foreground-resume, large-history memory, and
  rendering-performance qualification.

**Qualification state:** Functional port closed; every promotion gate remains
open.

### 4. Composer, completion, queue, and attachments — functionally-ported

**Current functional port coverage**

- The autogrowing, IME-aware composer can start, queue, and cancel turns,
  accepts browser file input, drag/drop, and pasted images, shows staged
  attachments, and applies current attachment size/count limits.
- Turn controls bridge HTTP acknowledgement to durable SSE state: an accepted
  start immediately shows `Starting…`, a second message can queue during that
  gap, cancellation remains `Stopping…` until its terminal event, and an
  explicitly labeled `Send next` action can queue a follow-up after cancel is
  acknowledged without waiting for the event round trip.
- Slash-command completion consumes the durable command snapshot; `@` file
  completion lazily refreshes the existing bounded session-path endpoint.
  Both use bounded fuzzy ranking, filter unsafe control text, and support
  keyboard, mouse, and touch selection with combobox/listbox semantics and a
  shared tested DOM UTF-16/protocol UTF-8 position conversion.
- Queue items can be displayed and acted on through edit, move, delete,
  drag/drop reorder, keyboard/pointer reorder, dispatch/send-now, failure
  recovery, and deterministic focus restoration.
- Composer and queue state are integrated with the same scoped thread model as
  the chat history. Send/cancel, attachments, thread settings, queue actions,
  approvals, and question submissions capture their originating thread and
  ignore stale UI completion after navigation, so an old response cannot
  clear busy state, focus controls, or surface an error in a newly selected
  thread.
- Unsubmitted composer text, selection position, and pending attachments are
  stored per thread, survive reloads, and restore independently across thread
  and session navigation. A successfully accepted message clears only its
  originating thread's draft; a failed request leaves that draft intact.

**Primary Lit evidence**

- [thread screen and composer](../../web/app-ui/src/components/thread-screen.ts)
- [thread-scoped draft controller](../../web/app-ui/src/services/composer-drafts.ts)
- [turn-state control model](../../web/app-ui/src/components/chat-turn-controls.ts)
- [bounded composer completion model](../../web/app-ui/src/components/composer-completion.ts)
- [queue control model](../../web/app-ui/src/components/queue-controls.ts)
- [attachment service](../../web/app-ui/src/services/attachments.ts)
- [thread view model](../../web/app-ui/src/state/thread-view-model.ts)

**Missing parity and qualification work**

- Qualify image paste/clipboard, paused-queue restart, autogrow edge cases,
  command-unavailable states, the server path cap, and the queue
  enabled/disabled/failure matrix against live desktop hosts and installed
  PWAs.
- Match Slint placement, density, action ordering, focus return, warning
  language, queued-state cues, attachment previews, and destructive-action
  confirmations in all themes.
- Test IME composition, dead keys, multiline editing, virtual keyboards,
  focus/selection restoration, touch reordering alternatives, permission
  denial/cancellation, large files, offline transitions, and screen readers.

**Qualification state:** Unqualified.

### 5. Diff — partial

**Current functional preview coverage**

- CodeMirror-based unified and split views render parsed multi-file patches
  with syntax-aware text and bounded parsing/fallback behavior.
- The inspection workspace can select a file and present diff content in the
  shared right-side workflow.
- The workspace refreshes live while visible, preserves a selected path and
  the last good diff across refreshes, and exposes checkpoint undo/redo with
  immediate post-restore refresh and generic failure messages.
- A user-initiated Clipboard API action copies the exact last-good raw patch,
  reports generic success/unavailable/failure feedback, and avoids a legacy
  DOM fallback. The changed-file listbox has one roving tab stop with Arrow
  Up/Down, Home, and End selection and focus movement.

**Primary Lit evidence**

- [diff view](../../web/app-ui/src/components/diff-view.ts)
- [bounded diff parser](../../web/app-ui/src/components/diff-parser.ts)
- [responsive diff-mode contract](../../web/app-ui/src/components/diff-mode.ts)
- [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts)
- [diff copy and keyboard model](../../web/app-ui/src/components/inspection-diff-controls.ts)

**Missing parity and qualification work**

- Complete and verify line numbers, editor selection/copy, expansion,
  proactive undo/redo boundary state, PR navigation, broader file navigation,
  malformed/binary fallbacks, and disposal. Qualify raw-diff Clipboard API
  denial, revocation, and platform behavior.
- Preserve Slint addition/deletion semantics, gutter and header hierarchy,
  line density, selection, action placement, and all-theme contrast.
- Provide an accessible textual representation and keyboard path that does
  not depend on editor internals.
- Qualify large patches, repeated mount/dispose, memory, keyboard and screen
  readers; enforce and verify the single-file unified view on narrow/mobile
  layouts, with side-by-side available only on desktop.

**Qualification state:** Unqualified.

### 6. Files and code — partial

**Current functional preview coverage**

- The inspection workspace can request directories/files, select a source
  file, infer a language, and render syntax-aware source.
- Directories load lazily on first expansion through the existing files
  endpoint, sort deterministically with directories first, and cache each
  successful directory listing until the user requests a refresh. Stale
  generations cannot overwrite the current tree.
- The tree has localized unloaded/loading/error/empty states plus ARIA
  `tree`/`treeitem` structure, one roving tab stop, visible-node Up/Down,
  Home/End, Left/Right, Enter/Space behavior, and focus recovery when a subtree
  collapses or refreshes.
- Large or unsupported content has a bounded fallback rather than forcing an
  unbounded editor render.

**Primary Lit evidence**

- [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts)
- [lazy file-tree model](../../web/app-ui/src/components/inspection-file-tree.ts)
- [file-tree model tests](../../web/app-ui/src/components/inspection-file-tree.test.ts)
- [code view](../../web/app-ui/src/components/code-view.ts)
- [protocol client](../../web/app-ui/src/services/protocol-client.ts)

**Missing parity and qualification work**

- Complete range reveal, search, multi/range selection and copy, gutter
  behavior, external-open actions, binary handling, watcher-driven refresh,
  file mutations, and large-tree virtualization/performance. Qualify manual
  refresh and directory-error recovery against real local and remote trees.
- Match Slint source palette, gutter hierarchy, selection, file-row density,
  panel composition, and theme behavior.
- Capability-gate PWA actions so the browser never appears to have arbitrary
  local filesystem access; test denial, cancellation, stale paths, and remote
  host wording.
- Qualify keyboard tree semantics, accessible source fallback, forced colors,
  desktop native-open integration, phone/tablet navigation, huge files, and
  repeated editor disposal.

**Qualification state:** Unqualified.

### 7. Terminal — functional-preview

**Current functional preview coverage**

- Multiple terminal tabs maintain distinct terminal IDs and endpoint
  lifecycles.
- xterm.js handles input/output, fit, resize, search, Unicode, selection, and
  validated web links; the preview exposes a touch modifier row.
- The output stream service separates ephemeral PTY bytes from durable
  protocol state and tracks offsets for reconnect behavior.

**Primary Lit evidence**

- [terminal panel](../../web/app-ui/src/components/terminal-panel.ts)
- [xterm view](../../web/app-ui/src/components/terminal-view.ts)
- [terminal output stream](../../web/app-ui/src/services/terminal-output-stream.ts)

**Missing parity and qualification work**

- Complete clipboard confirmation, mouse/wheel modes, IME edge cases, exit
  presentation, duplicate-free suspend/resume, inactive renderer disposal
  without PTY closure, and every terminal failure/lifecycle transition.
- Match the Slint terminal container, tab/status placement, palette, focus
  behavior, selection, empty state, and all-theme contrast.
- Test screen-reader fallback and focus announcements, keyboard interception,
  composition, Unicode width, links, touch modifiers, virtual keyboards,
  foregrounding, occlusion, and platform clipboard behavior.
- Benchmark one and five active desktop terminals plus a bounded mobile
  workload; capture CPU, memory, resize latency, long-output behavior, and
  disposal evidence.

**Qualification state:** Core preview workflow exists; every promotion gate
remains open.

### 8. Todos and plan — partial

**Current functional preview coverage**

- Projected plan items render in the inspection region with status text,
  counts, ordering, and an empty state.
- Todo/plan changes flow from the thread event projection rather than an
  independent durable side channel.

**Primary Lit evidence**

- [plan rendering in the application shell](../../web/app-ui/src/app/trouve-app.ts)
- [thread view model](../../web/app-ui/src/state/thread-view-model.ts)

**Missing parity and qualification work**

- Complete semantic empty, stale, current, in-progress, completed, cancelled,
  failed, and streaming-update states, including ownership and progress.
- Preserve Slint status symbols, ordering, density, color semantics, wrapping,
  long-text treatment, and compact mobile presentation in all five themes.
- Test rapid updates, restart/replay, offline recovery, keyboard navigation,
  screen-reader announcements, zoom/reflow, touch, and long lists.

**Qualification state:** Unqualified.

### 9. Session pull request — functional-preview

**Current functional preview coverage**

- A session PR panel can collect title/body/base/draft input, create a pull
  request, show check/review/merge state, open a validated external URL, and
  request a merge method with confirmation.
- Source-derived state is separated into a panel model rather than assembled
  solely in the template.

**Primary Lit evidence**

- [session PR panel](../../web/app-ui/src/components/session-pr-panel.ts)
- [session PR panel model](../../web/app-ui/src/components/session-pr-panel-model.ts)

**Missing parity and qualification work**

- Complete eligibility, branch/remote detail, aggregate badges,
  in-progress/failure/retry/cancel states, stale state, and all merge/check/
  review combinations.
- Match Slint placement, hierarchy, status colors, form density, warnings,
  confirmations, and result presentation.
- Qualify safe user-initiated HTTPS navigation in the PWA and the implemented
  typed native-external-open behavior on every desktop target.
- Test keyboard form flow, validation announcements, screen readers,
  duplicate submissions, offline/resume, refresh/restart, narrow layouts, and
  device/platform navigation behavior.

**Qualification state:** Core preview workflow exists; every promotion gate
remains open.

### 10. Pull-request dashboard — functional-preview

**Current functional preview coverage**

- A full-screen Lit dashboard renders grouped repositories/jobs, status
  filters, refresh, cancel/retry confirmations, job links, and repository/
  reviewer settings.
- A dedicated model transforms dashboard/settings protocol data for the
  component.

**Primary Lit evidence**

- [code-review dashboard](../../web/app-ui/src/components/code-review-dashboard.ts)
- [code-review dashboard model](../../web/app-ui/src/components/code-review-dashboard-model.ts)

**Missing parity and qualification work**

- Complete grouped-card virtualization, pagination, route restoration, all
  review-job artifacts, repository/provider distinctions, and the complete
  loading/empty/stale/offline/error/cancellation matrix.
- Treat the separate Preact review UI only as an API/fixture reference; verify
  parity against the current Slint dashboard’s hierarchy and theme.
- Match group ordering, card density, metadata prominence, filters, status
  colors, actions, and responsive cards in every theme.
- Test keyboard filtering/card navigation, screen readers, large job sets,
  refresh races, restart/replay, mobile route restoration, and render/memory
  budgets.

**Qualification state:** Core preview workflow exists; every promotion gate
remains open.

### 11. Automations — functional-preview

**Current functional preview coverage**

- The Lit screen provides list/detail states, templates, create/edit forms,
  schedule editing, enable/disable, run, delete with confirmation, refresh,
  and last-run status presentation.
- Hourly, daily, and weekly schedule choices and responsive/touch-oriented
  form layout are represented.

**Primary Lit evidence**

- [automations screen](../../web/app-ui/src/components/automations-screen.ts)
- [automations model](../../web/app-ui/src/components/automations-model.ts)
- [automations model tests](../../web/app-ui/src/components/automations-model.test.ts)
- [protocol client](../../web/app-ui/src/services/protocol-client.ts)

**Missing parity and qualification work**

- Complete run history, concurrent operations, selection restoration,
  validation details, schedule/time-zone edge cases, offline behavior,
  partial failures, and restart recovery.
- Match Slint screen hierarchy, list/card density, status language and colors,
  form grouping, destructive confirmations, and every theme.
- Test full keyboard form/list use, error and progress announcements, screen
  readers, touch schedule editing, virtual keyboards, phone/tablet layouts,
  large histories, polling cleanup, and background/foreground transitions.

**Qualification state:** Core preview workflow exists; every promotion gate
remains open.

### 12. General and appearance — partial

**Current functional preview coverage**

- Settings expose all five themes, system preference, theme previews, and
  semantic CSS generated from the authoritative Slint palette.
- Base size and font-family preferences match the Slint interaction: Font is
  a selector populated from installed families. Desktop hosts return a
  bounded cross-platform list through bridge bootstrap v8; the PWA/browser
  uses the permission-gated Local Font Access API when available and retains
  **System default** when enumeration is unsupported or denied.
- The shell has responsive layout tokens and honors at least the implemented
  host/browser preference hooks such as reduced motion and stored layout
  choices.

**Primary Lit evidence**

- [settings screen](../../web/app-ui/src/components/settings-screen.ts)
- [system-font discovery](../../web/app-ui/src/services/system-fonts.ts)
- [typed desktop-host bootstrap](../../crates/trouve-desktop-host/src/gateway.rs)
- [theme controller](../../web/app-ui/src/services/theme-controller.ts)
- [generated themes](../../web/app-ui/src/styles/themes.generated.css)
- [theme generator](../../scripts/generate_web_themes.py)
- [component gallery](../../web/app-ui/src/app/component-gallery.ts)
- [visual contract tests](../../web/app-ui/src/styles/visual-contract.test.ts)

**Missing parity and qualification work**

- Complete layout defaults, compact/touch preferences, restart indicators,
  desktop window/sleep options, and their persistence/migration behavior.
- Hide or clearly explain desktop-only settings in PWA mode; never display a
  control that the active capability adapter cannot perform.
- Capture Slint/Lit screenshots for all five themes, forced colors, large
  fonts, reduced motion, narrow layout, canonical pages, and difficult widget
  states. Static token tests and the gallery are not screenshot sign-off.
- Verify contrast, browser/system preference changes, theme persistence,
  focus visibility, responsive reflow, multiple DPI values, desktop
  window/restart behavior, and mobile safe-area/browser-chrome behavior.

**Qualification state:** Unqualified.

### 13. Notifications — foundation

**Current functional preview coverage**

- Settings and the host capability model can represent whether notifications
  are supported instead of relying on user-agent detection.
- Desktop and PWA adapters are structurally separate, allowing unsupported
  behavior to remain explicit.
- The browser/PWA path provides a user-initiated foreground permission and
  smoke-test flow. It refreshes the conservative capability snapshot when the
  window regains focus, detects denial/revocation, prevents concurrent test
  requests, and exposes only generic failure text.
- The UI explicitly says this smoke test does not establish reliable
  background notification delivery; desktop native notifications remain
  unavailable until a typed host capability is implemented.

**Primary Lit evidence**

- [settings screen](../../web/app-ui/src/components/settings-screen.ts)
- [browser notification adapter and test model](../../web/app-ui/src/services/browser-notifications.ts)
- [capability controller](../../web/app-ui/src/services/capabilities.ts)
- [host client](../../web/app-ui/src/services/host-client.ts)
- [desktop host boundary](../../crates/trouve-desktop-host/src/lib.rs)

**Missing parity and qualification work**

- Implement event-derived delivery, per-event toggles, focus suppression,
  click routing, attention requests, quiet/offline behavior, persistence, and
  the native desktop path. Qualify the implemented foreground permission/test
  flow, denial/revocation, constructor failures, and browser state changes.
- Preserve Slint grouping, support/permission status presentation, trust
  language, and action hierarchy.
- Keep desktop delivery host-owned; measure PWA/browser reliability and state
  limitations without overstating background delivery.
- Run permission, focus, click-route, offline, restart, OS notification-center,
  screen-reader, keyboard, multi-window, and supported browser/device matrices.

**Qualification state:** Foundation only; no end-to-end notification workflow
or platform gate is recorded.

### 14. Providers and onboarding — partial

**Current functional preview coverage**

- Provider settings render configured and known providers and support
  credential/subscription/login/health-related operations exposed by the
  protocol.
- Vendor CLI settings cover detected state and install/uninstall/update/login
  operations.
- Provider/model defaults can be reached from the settings routing structure.

**Primary Lit evidence**

- [provider settings](../../web/app-ui/src/components/provider-settings.ts)
- [vendor CLI settings](../../web/app-ui/src/components/cli-settings.ts)
- [settings router/screen](../../web/app-ui/src/components/settings-screen.ts)

**Missing parity and qualification work**

- Complete first-run onboarding progression, every login/device-auth window,
  cancellation, expiry/recovery, subscription state, provider/CLI health,
  model overlays, defaults, and offline/restart recovery.
- Preserve Slint onboarding order, information density, validation,
  provider-status colors, warnings, and visual trust cues.
- Audit that secrets never enter persistence, logs, signals, DOM attributes,
  clipboard by default, telemetry, screenshots, or displayed raw errors.
- Test keyboard and screen-reader progression, expired/cancelled flows,
  browser popup/navigation constraints, desktop CLI lifecycle, narrow/touch
  layouts, virtual keyboards, and every supported provider state.

**Qualification state:** Unqualified.

### 15. Modes and models — functional-preview

**Current functional preview coverage**

- Global defaults, built-in/custom modes, create/edit/delete/reset flows,
  inherited settings, and model selections are exposed in Lit settings.
- Mode data remains protocol-driven rather than introducing mode-specific
  frontend control flow.

**Primary Lit evidence**

- [mode settings panel](../../web/app-ui/src/components/mode-settings-panel.ts)
- [settings screen](../../web/app-ui/src/components/settings-screen.ts)
- [thread controls](../../web/app-ui/src/components/thread-screen.ts)

**Missing parity and qualification work**

- Complete fuzzy search, provider/model availability and health, unsupported
  combinations, refresh behavior, inheritance edge cases, validation,
  concurrent changes, and offline/restart recovery.
- Match Slint option density, status/health cues, inheritance presentation,
  defaults, warnings, and selection behavior across all themes.
- Qualify full keyboard combobox/listbox behavior, screen-reader naming and
  state, pointer/touch interaction, large model inventories, phone layouts,
  and settings-to-composer consistency.

**Qualification state:** Core preview workflow exists; every promotion gate
remains open.

### 16. Local models — functional-preview

**Current functional preview coverage**

- Settings expose local-server status, enablement, a searchable model catalog,
  download progress, cancellation, deletion, and server stop/restart actions.
- PWA-facing wording can explain that local models belong to the remote server
  host rather than the phone.

**Primary Lit evidence**

- [local-model settings](../../web/app-ui/src/components/local-model-settings.ts)
- [local-model settings tests](../../web/app-ui/src/components/local-model-settings.test.ts)
- [protocol client](../../web/app-ui/src/services/protocol-client.ts)

**Missing parity and qualification work**

- Complete installed/available/update states, retry, disk-space and cleanup
  failures, concurrent downloads/actions, stale progress, partial artifacts,
  restart recovery, and server-health transitions.
- Match Slint progress/status colors, card hierarchy, density, confirmations,
  failure prominence, and every theme.
- Verify remote-host wording and capability behavior in PWA mode; do not imply
  that model files or inference processes live on the mobile device.
- Test keyboard and screen-reader progress/status, large catalogs, rapid
  polling changes, cancellation races, offline/resume, phone/tablet layouts,
  and CPU/memory/network behavior.

**Qualification state:** Core preview workflow exists; every promotion gate
remains open.

### 17. Git and worktrees — partial

**Current functional preview coverage**

- The current settings panel exposes the session-title model resource policy,
  install/cancel actions, and related load/status presentation.
- Settings operations use the protocol client rather than direct filesystem,
  git, or worktree access.

**Primary Lit evidence**

- [management settings panels](../../web/app-ui/src/components/management-settings-panels.ts)
- [settings screen](../../web/app-ui/src/components/settings-screen.ts)
- [protocol client](../../web/app-ui/src/services/protocol-client.ts)

**Missing parity and qualification work**

- Add identity/defaults, worktree policy and status, dirty/conflict/error
  states, remote/default branch, cleanup, confirmation, recovery, and the
  complete title-model resource lifecycle.
- Preserve Slint form grouping, density, field ordering, warning hierarchy,
  destructive confirmations, and status colors in every theme.
- Ensure every effect stays a protocol operation against session-owned
  worktrees; neither desktop bridge nor PWA may expose direct git/filesystem
  escape hatches.
- Test keyboard/forms, screen readers, validation, dirty/conflict recovery,
  concurrent session changes, restart, desktop/PWA wording, and long paths.

**Qualification state:** Unqualified.

### 18. MCP — partial

**Current functional preview coverage**

- A settings panel provides scoped server configuration creation/update/
  removal, environment entry, status presentation, and log-related views
  supported by the current protocol client.
- MCP changes remain server protocol operations rather than desktop-host or
  browser operations.

**Primary Lit evidence**

- [management settings panels](../../web/app-ui/src/components/management-settings-panels.ts)
- [settings screen](../../web/app-ui/src/components/settings-screen.ts)
- [protocol client](../../web/app-ui/src/services/protocol-client.ts)

**Missing parity and qualification work**

- Complete effective-configuration display, scope inheritance, validation,
  enable/disable masking, health, restart/reconnect, tool inventory, virtual
  logs, copy feedback, large output, disposal, and failure recovery.
- Preserve Slint scope/status/log hierarchy, secret masking, row density,
  warnings, action placement, and theme semantics.
- Prove that environment secrets do not enter unsafe DOM attributes,
  diagnostics, persistence, screenshots, or unredacted error/log output.
- Test keyboard log/editor access, screen readers, long-line mobile behavior,
  huge logs, virtualized disposal, reconnection races, offline/restart, and
  all scope combinations.

**Qualification state:** Unqualified.

### 19. Integrations — partial

**Current functional preview coverage**

- GitHub integration settings can display configured/available hosts and
  support add/remove/configuration operations, including enterprise-host
  input represented by the current protocol.
- Capability and host-client boundaries exist for separating validated
  desktop navigation from browser/PWA navigation.

**Primary Lit evidence**

- [management settings panels](../../web/app-ui/src/components/management-settings-panels.ts)
- [settings screen](../../web/app-ui/src/components/settings-screen.ts)
- [host client](../../web/app-ui/src/services/host-client.ts)

**Missing parity and qualification work**

- Complete connect/disconnect/re-auth, multiple accounts, scopes, health,
  defaults, unavailable-capability, OAuth navigation, callback cancellation,
  confirmation, expiry, and recovery flows.
- Preserve Slint grouping, status/trust language, warning hierarchy,
  confirmation behavior, density, and all-theme presentation.
- Validate desktop external URLs and PWA HTTPS redirect origins; handle popup
  blocking, cancellation, stale callbacks, state mismatch, denial, offline,
  and revoked accounts securely.
- Test keyboard/screen readers, browser and native navigation, phone/tablet
  redirects, focus restoration, multi-account scale, restart, and failure
  recovery.

**Qualification state:** Unqualified.

### 20. About and licensing — partial

**Current functional preview coverage**

- About settings show frontend/source/deployment/server/protocol/connectivity
  data, licensing information, conditional Slint attribution, and PWA
  deployment wording.
- The page is available through the common settings route and theme system.

**Primary Lit evidence**

- [settings screen and About section](../../web/app-ui/src/components/settings-screen.ts)
- [application metadata](../../web/app-ui/src/app/trouve-app.ts)
- [PWA service worker](../../web/app-ui/src/pwa/service-worker.ts)

**Missing parity and qualification work**

- Complete product, workspace, engine, deployment, frontend, protocol, and
  service-worker build revisions; diagnostics; validated links; offline
  notices; full npm license inventory; and conditional attribution rules.
- Match the existing Slint visual treatment, information hierarchy, link
  styling, density, selectable/copyable version values, and every theme.
- Verify packaged notices and inventory offline, source/version consistency,
  service-worker revision/update behavior, link capabilities, and attribution
  in every artifact where Slint remains linked or distributed.
- Test keyboard/screen readers, copy/link feedback, narrow layouts, long
  versions/licenses, disconnected states, and desktop/PWA packaged builds.

**Qualification state:** Unqualified.

### 21. Desktop integration and web capabilities — gated

**Current functional preview coverage**

- A typed desktop gateway can serve packaged assets, proxy protocol HTTP/SSE,
  validate its security boundary, and expose a conservative capability
  snapshot.
- The in-process Servo harness pinned to the exact 2026-08-02 nightly at
  revision `35672cc3d4beb768489f5218e73bee7aff0ddb01` exercises that packaged
  gateway first; Wry provides the fallback/comparison path. Both connect to one
  explicitly selected server. The nested Servo harness structurally cannot
  link or start `trouve-server`, uses temporary storage and host-preference
  directories, and cannot open the default database.
- A native Wayland smoke run on 2026-08-02 created the window successfully and
  completed gateway and protocol requests. Servo 0.4.0 also logged unsupported
  selectors, properties, and media features including `:has()`, `color-scheme`,
  `prefers-reduced-motion`, `forced-colors`, `text-overflow`, `user-select`,
  `resize`, and `touch-action`. This is successful embedding/transport smoke
  evidence but failed/open visual-platform compatibility evidence that helps
  explain the current mismatch; it is not a visual-parity or native Wayland
  qualification pass. X11/XWayland and every other release target likewise
  remain unqualified.
- The bridge exposes validated HTTPS navigation only when the desktop app has
  attached a concrete opener. The mutation requires exact Origin and Host,
  the ephemeral CSRF credential, a bounded body, and an HTTPS URL without
  credentials or control characters; PWA-kind gateways cannot invoke it.
- Desktop bridge version 8 includes a single-flight, typed native directory picker
  behind the same exact Host, Origin, and CSRF boundary. Wry and Servo attach
  platform dialogs to the application window and restore focus afterward. The
  capability is advertised only for a local desktop gateway with a concrete
  picker action and loopback protocol upstream; cancellation returns no path,
  while PWA and remote deployments remain explicitly unsupported.
- The Lit client consumes a typed host/capability abstraction, while the PWA
  uses a separate adapter with no implied native access.
- The desktop webview remains feature-gated, with Slint as the default and
  rollback path.

**Primary Lit/host evidence**

- [host client](../../web/app-ui/src/services/host-client.ts)
- [capability controller](../../web/app-ui/src/services/capabilities.ts)
- [application contexts](../../web/app-ui/src/contexts/app-contexts.ts)
- [desktop gateway and host](../../crates/trouve-desktop-host/src/lib.rs)
- [desktop gateway security boundary](../../crates/trouve-desktop-host/src/gateway.rs)
- [Wry database-safe preview bootstrap](../../crates/trouve-app/src/web_preview_support.rs)
- [Servo embedding harness](../../crates/trouve-servo-embed-preview/src/main.rs)
- [Servo database-safe host bootstrap](../../crates/trouve-servo-embed-preview/src/web_preview_support.rs)
- [Servo harness runbook](../../crates/trouve-servo-embed-preview/README.md)
- [Wry fallback preview](../../crates/trouve-app/src/web_preview.rs)
- [ADR 0023](../adr/0023-lit-web-frontend-and-webview-host.md)
- [ADR 0024](../adr/0024-isolated-servo-embedding-qualification-workspace.md)

**Missing parity and qualification work**

- Complete and qualify the narrow typed bridge for attachment pickers,
  clipboard images, validated local-file open, notifications, attention, sleep,
  window/focus/visibility/occlusion, geometry, quit, lifecycle, and crash
  recovery; qualify the implemented directory picker and HTTPS opener on every
  target. Do not add
  durable agent state or arbitrary filesystem, shell, URL, git, MCP, tool, or
  Rust invocation to the host boundary.
- Match the native-dialog/action placement, focus return, progress, warnings,
  and lifecycle behavior users experience in Slint.
- Test schema/version mismatch, exact origin/host/path/scheme controls, user
  gestures, DPI, IME, drag/drop, downloads, remote mode, shutdown, crashes,
  restart/recovery, and all supported desktop OS/webview combinations.
- Complete and qualify the embedded Servo harness for accessibility actions,
  native capabilities, renderer recreation, crash/OOM containment, packaging,
  lifecycle, memory/performance budgets, visual parity, and the complete
  platform/display-backend matrix. The direct embedding smoke test is not an
  engine-promotion result. Qualify Wry independently as the fallback.
- Finish production PWA HTTPS authentication/deployment, allowed origins,
  service-worker scope/caching rules, OAuth behavior, offline shell, install/
  update behavior, and real phone/tablet/browser qualification. Never expose a
  remote server by relying on `TROUVE_ALLOW_REMOTE` alone: publication requires
  authenticated users, mutation CSRF protection, and deployment security
  headers in front of the protocol.

**Qualification state:** Gated. The typed foundation is not a complete native
capability implementation. The in-process Servo harness establishes an
embedding path only; neither Servo, Wry, nor the PWA has passed its platform
qualification matrix.

## Migration-added enhancements

This register tracks useful additions discovered while porting. An enhancement
does not alter the Slint parity baseline, close a qualification gap, or become
an approved visual deviation merely because it is implemented. Its placement,
accessibility, responsive behavior, and interaction with the existing UX must
still be qualified.

| ID | Surface | Enhancement | Rationale | Relationship to Slint baseline | Status and evidence |
| --- | --- | --- | --- | --- | --- |
| ENH-001 | Shell and inbox | A compact icon control beside the Workspaces heading opens the same route/session/thread/action palette as Ctrl/Cmd-K. | Makes the palette discoverable and gives pointer and touch users a direct entry point without adding another primary navigation row. | Additive control not present in the Slint baseline; its compact placement preserves the existing navigation hierarchy, density, and primary-action prominence. | Preview implemented and unqualified; [application shell](../../web/app-ui/src/app/trouve-app.ts), [command palette](../../web/app-ui/src/components/command-palette.ts), and [palette tests](../../web/app-ui/src/components/command-palette.test.ts). |
| ENH-002 | Pull-request dashboard | **Review operations** exposes code-review service health, recent jobs, limits, GitHub App setup, repository routing, and reviewer personas in the shared dashboard. | Keeps review administration adjacent to the PR inbox instead of hiding it in a disconnected utility. | Additive management view; it must not displace the existing grouped PR workflow or change its priority. | Implemented and unqualified; [code-review dashboard](../../web/app-ui/src/components/code-review-dashboard.ts) and [configuration](../../web/app-ui/src/components/code-review-configuration.ts). |
| ENH-003 | Session pull request | Merge, squash, and rebase controls are shown only when the server and PR state permit them, with explicit confirmation. | Lets users complete the visible PR lifecycle without leaving Trouve. | Additive mutation beyond the Slint inspection baseline; server policy remains authoritative and safe external-open remains available. | Implemented and unqualified; [session PR panel](../../web/app-ui/src/components/session-pr-panel.ts) and [model](../../web/app-ui/src/components/session-pr-panel-model.ts). |
| ENH-004 | Diff and files | Raw-diff/file copy plus capability-gated local file open/reveal actions. | Makes common review handoffs faster while using the existing typed host boundary. | Additive shortcuts; they do not replace the diff/code views and are hidden or explained when unsupported. | Implemented and unqualified; [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts), [file reveal](../../web/app-ui/src/components/file-reveal.ts), and [host client](../../web/app-ui/src/services/host-client.ts). |
| ENH-005 | Chat | An accessible full-history mode can disable virtualization for assistive-technology review. | Provides a deliberate semantic fallback when virtualized history impedes navigation. | Additive accessibility mode; default density, anchoring, and bounded rendering remain unchanged. | Implemented and unqualified; [thread screen](../../web/app-ui/src/components/thread-screen.ts) and [virtualizer](../../web/app-ui/src/components/virtualization/virtualizer.ts). |
| ENH-006 | Mobile PWA composer | Quick-reply chips for **Continue**, **Explain**, and **Undo** on narrow layouts. | Reduces virtual-keyboard friction for frequent steering actions. | Mobile-only additive affordance; it sends ordinary prompts and does not create a second command path. | Implemented and unqualified; [thread screen](../../web/app-ui/src/components/thread-screen.ts). |
| ENH-007 | Mobile PWA shell | Pull-to-refresh plus an explicit refresh action. | Gives touch users a familiar recovery gesture while retaining an accessible non-gesture control. | Additive refresh entry points; protocol cursor and recovery semantics remain unchanged. | Implemented and unqualified; [pull-to-refresh controller](../../web/app-ui/src/services/pull-to-refresh.ts) and [application shell](../../web/app-ui/src/app/trouve-app.ts). |
| ENH-008 | Mobile PWA shell | Capability-aware **Install app** affordance with deferred browser prompt handling and installed-state suppression. | Makes the initial mobile PWA delivery discoverable without overstating browser support. | Additive packaging affordance; absent when the browser does not expose installation and unrelated to desktop parity. | Implemented and unqualified; [PWA install controller](../../web/app-ui/src/services/pwa-install.ts) and [application shell](../../web/app-ui/src/app/trouve-app.ts). |
| ENH-009 | Shell and inbox | Session rows can display aggregate pull-request state badges. | Brings review attention into the session inbox where users already triage work. | Additive status signal; it uses existing semantic tones and must not outrank needs-attention session state. | Implemented and unqualified; [session PR badge model](../../web/app-ui/src/components/session-pull-request-badge.ts) and [session list](../../web/app-ui/src/components/session-list.ts). |
| ENH-010 | About and capabilities | A direct `settings/capabilities` diagnostic view enumerates the typed desktop/PWA capability boundary. | Makes unsupported host behavior inspectable and prevents a PWA limitation from looking like a broken or successful native action. | Additive diagnostic route associated with About; it is not a replacement for ordinary capability-aware wording on each workflow. | Implemented and unqualified; [settings screen](../../web/app-ui/src/components/settings-screen.ts), [capability controller](../../web/app-ui/src/services/capabilities.ts), and [host schema](../../web/app-ui/src/generated/host.ts). |
| ENH-011 | Git, worktrees, and shell | A full workspace administration surface supplements the compact Open/close/reorder controls in the inbox. | Gives users one place to inspect, register, and close repositories while retaining the fast shell actions. | Additive management presentation over existing protocol operations; the Slint-shaped inbox remains the primary workspace hierarchy. | Implemented and unqualified; [workspace settings](../../web/app-ui/src/components/workspace-settings.ts) and [workspace settings model](../../web/app-ui/src/components/workspace-settings-model.ts). |
| ENH-012 | Appearance | A **System** theme preference follows the browser/OS light-dark preference while resolving to one of the existing Trouve palettes. | Avoids forcing PWA and webview users to duplicate an OS-level appearance choice. | Additive preference; it does not add a sixth palette or change any Slint-derived semantic color role. | Implemented and unqualified; [theme controller](../../web/app-ui/src/services/theme-controller.ts), [settings screen](../../web/app-ui/src/components/settings-screen.ts), and [application shell](../../web/app-ui/src/app/trouve-app.ts). |
| ENH-013 | Shell status | A normally hidden, context-sensitive status strip appears for host/protocol recovery, PWA install/update, and related actionable state; when present it also identifies the active model and permission mode. | Keeps routine desktop geometry faithful to Slint while putting recovery and trust information next to the action that needs it. | Additive status surface, hidden during ordinary desktop operation and always adapted into the mobile navigation row. | Implemented and unqualified; [application shell](../../web/app-ui/src/app/trouve-app.ts) and [shell styles](../../web/app-ui/src/styles/app.css). |
| ENH-014 | Diff, files, terminal, providers, PR, and integrations | Keyboard-focus-only expert actions expose manual refresh, local reveal, terminal Copy/Paste, and integration disconnect without permanently adding visible chrome. | Preserves recovery and explicit browser/native actions for keyboard users while keeping the Slint action hierarchy visually stable. | Additive controls are visually suppressed until focused or until their state is relevant; primary baseline actions retain their original placement. | Implemented and unqualified; [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts), [terminal panel](../../web/app-ui/src/components/terminal-panel.ts), [session PR panel](../../web/app-ui/src/components/session-pr-panel.ts), [CLI settings](../../web/app-ui/src/components/cli-settings.ts), and [integration settings](../../web/app-ui/src/components/management-settings-panels.ts). |
| ENH-015 | Files and diff | Read-only CodeMirror views provide Ctrl/Cmd-F search and match highlighting in source and opt-in editor-based diff views. | Adds the expected browser/editor search path for large source and review tasks. | Additive editor capability; the default Slint-shaped Files and continuous unified-diff composition remains unchanged. | Implemented and unqualified; [code view](../../web/app-ui/src/components/code-view.ts) and [diff view](../../web/app-ui/src/components/diff-view.ts). |
| ENH-016 | Diff | A keyboard-reachable **Split view** opens the selected file in CodeMirror MergeView; the default stays the continuous Slint-style unified diff and narrow layouts stay unified-only. | Supports detailed desktop before/after review without making the migration a visual redesign. | Additive, opt-in desktop view. Returning to Unified restores the baseline presentation; binary changes use an explicit fallback. | Implemented and unqualified; [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts), [diff view](../../web/app-ui/src/components/diff-view.ts), and [responsive diff contract](../../web/app-ui/src/components/diff-mode.ts). |
| ENH-017 | Mobile PWA | Explicit coarse-pointer PR-group move controls, tree-to-viewer Files navigation, a large-target approval sheet, and safe-area-aware full-screen headers adapt desktop workflows to phones and tablets. | Supplies usable non-drag and non-hover paths and avoids display cutouts without creating a second client implementation. | Mobile-only adaptation of existing operations; desktop hierarchy and default visuals remain unchanged. | Implemented and unqualified; [PR dashboard](../../web/app-ui/src/components/pull-requests-dashboard.ts), [inspection workspace](../../web/app-ui/src/components/inspection-workspace.ts), [thread screen](../../web/app-ui/src/components/thread-screen.ts), [Automations](../../web/app-ui/src/components/automations-screen.ts), and [responsive shell styles](../../web/app-ui/src/styles/app.css). |
| ENH-018 | Chat turn controls | Explicit `Sending…`, `Queueing…`, `Starting…`, `Stopping…`, `Send next`, and retained cancellation transcript messages bridge request acknowledgements to durable turn events. | Makes in-flight and accepted-but-not-yet-streamed work visible and keeps the follow-up path discoverable during cancellation instead of looking unresponsive. | Additive acknowledgement and cancellation feedback over the same send/cancel/queue protocol; it does not create a second turn state or bypass durable SSE truth. | Implemented and browser-tested; [turn control model](../../web/app-ui/src/components/chat-turn-controls.ts), [thread screen](../../web/app-ui/src/components/thread-screen.ts), and [stateful chat tests](../../web/app-ui/e2e/chat-session.spec.ts). |
| ENH-019 | Chat and settings | A **Chat** preference can opt thinking output into collapsed tool activity. The default instead keeps every thought visible as a labeled, non-collapsible top-level boundary and splits tool groups on either side. | Makes reasoning output and transitions difficult to miss without removing the compact transcript option for users who prefer it. | Additive frontend-owned presentation policy implemented in both Slint and Lit; it changes neither durable thread state nor the harness protocol. | Implemented and unqualified; [Lit chat preferences](../../web/app-ui/src/services/chat-preferences.ts), [Lit transcript](../../web/app-ui/src/components/thread-screen.ts), [Slint settings](../../crates/trouve-app/ui/settings-window.slint), and [Slint transcript fold](../../crates/trouve-app/src/render.rs). |
| ENH-020 | Composer | Unsubmitted composer text, cursor position, and pending attachments persist independently for each thread across reloads and session/thread navigation. Accepted submissions clear only the originating draft; rejected submissions retain it. | Prevents partially written prompts and staged context from being lost when users compare threads, follow notifications, or refresh a preview. | Additive local frontend state. It does not enter the durable protocol/event log, alter submitted messages, or bypass attachment limits; browser storage is bounded and malformed restored data is rejected. | Implemented and browser-tested; [draft controller](../../web/app-ui/src/services/composer-drafts.ts), [thread integration](../../web/app-ui/src/components/thread-screen.ts), [unit tests](../../web/app-ui/src/services/composer-drafts.test.ts), and [cross-session/reload browser test](../../web/app-ui/e2e/chat-session.spec.ts). |
| ENH-021 | Session pull request | Compact heading actions open the existing create form or launch the associated repository's GitHub pull-request list through the safe external-open boundary. | Keeps the create workflow available without a full-width primary button and makes repository-level PR navigation available beside it. | Additive browser shortcut plus a denser placement of the existing create action; eligibility, form behavior, protocol mutations, and PR state remain unchanged. | Implemented and unqualified; [session PR panel](../../web/app-ui/src/components/session-pr-panel.ts), [safe repository-link model](../../web/app-ui/src/components/session-pr-panel-model.ts), and [browser shell coverage](../../web/app-ui/e2e/app-shell.spec.ts). |

## Approved deviations

This register contains only explicitly reviewed differences. A widget-chrome
variation is not automatically an approved deviation; every row still needs
objective qualification evidence before promotion.

| ID | Surface | Slint baseline behavior | Approved Lit deviation | Why parity is preserved | Approver and date | Evidence packet |
| --- | --- | --- | --- | --- | --- | --- |
| DEV-001 | Chat response actions | Slint exposes response copy and raw-Markdown header buttons because general rendered-text selection is limited. | Lit exposes the formatted-response copy action only on hover or keyboard focus and omits the raw-Markdown header button. Rendered response text remains selectable, while right-click or Shift+F10 on the Agent response exposes **Copy as markdown** and an ordinary **Copy** item when a selection exists. Native link/image and nested activity menus are not replaced. | Quick copy retains parity without permanent header chrome; the complete Markdown source remains available from the context menu, ordinary browser selection is preserved, and both actions are keyboard-operable. | User approval, 2026-08-07 | [thread screen](../../web/app-ui/src/components/thread-screen.ts) and [desktop/mobile browser interaction test](../../web/app-ui/e2e/chat-session.spec.ts). |
| DEV-002 | Chat activity hierarchy | Slint uses bordered/collapsible activity rows and grouped tool presentation. | Lit uses a faint neutral rail and small nodes for each contiguous thought/tool sequence. Completed work is neutral; active, expanded, hovered, or focused work is blue; approval and failure nodes retain semantic colors. Tool groups remain transparent, visible thoughts use quiet labels, compaction and prose split the timeline, and response prose uses a readable maximum measure. | Disclosure, status color, grouping, ordering, output separation, and action semantics are unchanged; the treatment reduces repeated chrome and gives activity, compaction, and the final response distinct hierarchy. | User approval, 2026-08-06 | [chat styling](../../web/app-ui/src/styles/app.css), [visual contract](../../web/app-ui/src/styles/visual-contract.test.ts), and [desktop/mobile browser interaction test](../../web/app-ui/e2e/chat-session.spec.ts). |

## Open decisions and deviations

| ID | Surface | Current implementation | Plan tension | Required resolution |
| --- | --- | --- | --- | --- |
| ODV-001 | Shared ordinary controls | WebAwesome Free is self-hosted, token-mapped, and used for selected brand/action controls; many compact buttons, inputs, selects, and text areas remain semantic native controls with Trouve styling. | The plan names WebAwesome Free as the default ordinary-control library. The broader native-control use has preserved the Slint density and browser form semantics, but it has not been reviewed as an intentional exception. | Before promotion, either approve the native-control boundary with paired engine/visual/accessibility evidence or migrate the affected controls to qualified WebAwesome components without regressing the Slint UX. |

## Required qualification evidence

Promotion requires evidence for every surface, not a single application-wide
smoke result. Evidence must use the same deterministic fixtures and meaningful
state on Slint and Lit whenever both frontends can render the surface.

### Visual evidence

- Capture paired Slint and Lit screenshots for all five themes at canonical
  desktop, narrow desktop, phone portrait, phone landscape, and tablet
  viewports where the surface is supported.
- Include default, selected, focused, hover where meaningful, disabled,
  loading, streaming/progress, empty, error, confirmation, destructive,
  disconnected, and recovered states.
- Record viewport, device-pixel ratio, OS, font rendering environment, browser
  or webview version, fixture ID, baseline revision, Lit revision, masking,
  crop, and reviewer.
- Evaluate layout, density, hierarchy, semantic colors, typography, spacing,
  action placement, focus rings, status prominence, and disclosure defaults.
  Do not approve parity from a token test alone.
- Link every accepted visible difference to an approved-deviation ID.

### Keyboard and accessibility evidence

- Record complete keyboard paths, initial and restored focus, tab order,
  roving focus where used, shortcuts, Escape behavior, focus visibility, and
  behavior during asynchronous replacement.
- Run automated axe-core checks, then record manual accessible-name, role,
  state, relationship, live-region, error, progress, table/list/tree/editor,
  and disclosure verification.
- Exercise the supported screen-reader/OS/browser matrix, text zoom and
  reflow, browser zoom, forced colors, reduced motion, large fonts, selection
  and copy, and accessible nonvirtual fallbacks.
- A zero-result automated scan does not by itself pass accessibility.

### Desktop and mobile-device evidence

- Record supported desktop OS, webview engine/version, DPI/scale, pointer,
  keyboard layout/IME, window sizes, multi-window behavior, native capability
  actions, suspend/resume, occlusion, restart, shutdown, and crash recovery.
- Record the PWA on supported phone/tablet/browser combinations in portrait
  and landscape, including install/update, safe areas, browser chrome, touch,
  virtual keyboard, selection, clipboard, permission denial, network loss,
  foreground/background, memory pressure, and remote-server wording.
- Mark browser-unsupported behavior as capability-gated with a useful
  explanation. Do not translate an unsupported native feature into an
  apparent success.

### Performance, memory, lifecycle, and failure evidence

- Record fixture size, run count, warm/cold state, hardware, software
  versions, measurement method, raw artifact, median/tail result, budget,
  regression threshold, and pass/fail reviewer.
- Cover startup, event replay, streaming chat, large histories, large trees
  and source files, large diffs, one/five terminals, large PR/review sets,
  automation history, local-model progress, MCP logs, route churn, theme
  switching, and repeated mount/dispose.
- Include reconnect, dropped SSE, stale responses, cancellation races,
  partial failures, offline/online, foreground/background, restart, and
  duplicate-free recovery.
- A source-level bounded algorithm or unit test is not measured performance or
  memory evidence.

## Evidence packet template

Create one packet per surface and qualification run. Keep artifact paths
repository-relative or link to the immutable CI artifact.

~~~yaml
surface:
surface_status:
fixture_id:
baseline_revision:
lit_revision:
protocol_version:
host_schema_version:
reviewer:
date:

visual:
  themes:
    - dark
    - light
    - high-contrast-dark
    - colorblind-dark
    - colorblind-light
  captures:
    - state:
      viewport:
      device_pixel_ratio:
      platform:
      slint_artifact:
      lit_artifact:
      diff_artifact:
      result:
      approved_deviation_ids: []

keyboard_accessibility:
  keyboard_script:
  axe_artifact:
  manual_semantics_result:
  screen_reader_matrix:
  forced_colors_result:
  zoom_reflow_result:
  reduced_motion_result:
  focus_restoration_result:

desktop_devices:
  matrix_artifact:
  native_capability_result:
  lifecycle_result:

mobile_pwa:
  matrix_artifact:
  install_update_result:
  portrait_landscape_result:
  touch_virtual_keyboard_result:
  background_resume_result:
  capability_gating_result:

performance_memory:
  fixture_sizes:
  hardware_software:
  measurement_method:
  raw_artifact:
  budget:
  result:

failure_recovery:
  scenarios:
  raw_artifact:
  result:

security_packaging:
  artifact:
  result:

overall_result:
open_findings:
approved_deviation_ids: []
~~~

## Current evidence register

The register starts empty by design. “Not captured” and “Not run” are facts,
not failures hidden behind a preview status. Update a cell only with a linked,
reviewable artifact and reviewer/date.

| # | Surface | Status | Slint/Lit screenshots | Keyboard and a11y | Desktop devices | Mobile PWA devices | Performance and memory | Evidence owner/date |
| ---: | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Shell and inbox | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 2 | Session/thread management | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 3 | Chat | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 4 | Composer/completion/queue/attachments | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 5 | Diff | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 6 | Files/code | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 7 | Terminal | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 8 | Todos/plan | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 9 | Session PR | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 10 | PR dashboard | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 11 | Automations | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 12 | General/appearance | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 13 | Notifications | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 14 | Providers/onboarding | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 15 | Modes/models | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 16 | Local models | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 17 | Git/worktrees | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 18 | MCP | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 19 | Integrations | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 20 | About/licensing | functionally-ported | Not captured | Not run | Not run | Not run | Not measured | Unassigned |
| 21 | Desktop integration/web capabilities | gated | Not captured | Not run | Not run | Not run | Not measured | Unassigned |

## Promotion rule

The functional inventory is closed by the source-derived callback manifest
and the current closure table. That result is rerun in the ordinary Vitest
suite so adding a new Slint callback without a Lit disposition fails CI.
Functional closure does not advance any evidence field automatically: failure
injection, lifecycle, visual, keyboard, accessibility, device, performance,
memory, security, packaging, and soak results must be recorded independently.

No surface may be treated as parity-qualified, and the Lit frontend may not
replace Slint by default, until:

1. every required evidence field has a linked artifact and reviewer;
2. all five themes preserve the Slint visual/semantic contract;
3. keyboard and accessibility checks pass on the supported matrix;
4. desktop and mobile-PWA device matrices pass for supported capabilities;
5. performance, memory, failure, lifecycle, security, packaging, and soak
   budgets pass;
6. every accepted variation is present in the approved-deviation register;
   and
7. rollback remains proven until the migration plan explicitly retires it.

The PWA can be evaluated and released on its own evidence path without making
an unqualified desktop engine the default. A later decision to pursue another
mobile packaging model requires evidence from the initial PWA and, when it
changes the load-bearing architecture, a new or superseding ADR.
