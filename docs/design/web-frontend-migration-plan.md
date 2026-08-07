# Web frontend migration implementation plan

**Status:** Completed; Wry/Lit promoted and Slint retired by ADR 0028
**Last updated:** 2026-08-07
**Source analysis:** <https://chatgpt.com/s/cd_6a6e636d63f48191bfed1ce04ae4bac6>

**Qualification ledger:** [Web frontend parity and qualification ledger](web-frontend-parity-ledger.md)

**Implementation audit:** [Web frontend implementation audit](web-frontend-implementation-audit.md)

> This document preserves the migration plan and its original gates. ADR 0028
> records their final disposition: Wry/Lit is the sole product frontend, the
> Slint rollback and widget crates have been removed, and Servo remains a
> qualification host for the same Lit application.

## Outcome

The recommendation is a conditional go:

- Build the replacement frontend with Lit and TypeScript.
- Use **@lit/context** for stable service and scoped-store injection.
- Use **@lit-labs/signals** behind a small Trouve-owned reactivity adapter. Do
  not expose the experimental package throughout the application or allocate
  one signal per historical field.
- Use WebAwesome Free for ordinary controls, while retaining project-owned
  components for Trouve-specific interactions.
- Use CodeMirror 6, MergeView, xterm.js, and sanitized Markdown rendering for
  the specialized widgets.
- Preserve the current Trouve look, feel, themes, semantic colors, density,
  layout, visual hierarchy, information architecture, and interaction model.
  This is a frontend migration, not a redesign.
- Permit controlled differences in ordinary widget chrome, such as WebAwesome
  tabs versus Slint tabs, as long as the controls are themed to fit Trouve and
  the core experience remains recognizably the same.
- Treat Servo as a promotion candidate, not an assumed shipping engine. Test
  it first through the exact-revision, chrome-free, in-process Servo nightly
  embedding harness.
- Treat that embedder as a qualification harness, not as the shipping desktop
  host. It proves that Trouve can create and drive a direct Servo `WebView`,
  but it does not by itself satisfy accessibility-action, native-capability,
  crash-recovery, packaging, visual-parity, platform, or memory gates.
- Use the maintained Wry system-webview host as the staged default and retain
  its explicit comparison mode. Its qualification matrix remains open under
  ADR 0027 and must pass before Slint retirement.
- Use a **PWA as the initial mobile application**. The PWA will reuse the Lit
  frontend, connect to a remote Trouve server over the public protocol, and
  expose only capabilities supported by its browser. Evaluate native mobile,
  embedded-webview, and other packaging options after the PWA has real usage
  and platform evidence.
- Keep the complete Slint frontend available until functional, visual,
  accessibility, performance, memory, security, and packaging gates pass.
- Record useful features discovered during the port in the parity ledger's
  migration-added enhancement register. Each entry must state its rationale,
  relationship to the Slint baseline, implementation evidence, and
  qualification status so an enhancement never silently hides or redefines a
  parity gap.
- If Servo fails but the system-webview path passes, ship the desktop web
  frontend with the system webview.
- If both desktop engines fail, keep Slint. Failure of a desktop engine does
  not by itself invalidate the independently hosted PWA.

A key finding is that current Servo accessibility support cannot yet handle
accessibility action requests from assistive technologies. That blocks
interactive screen-reader parity today, even if visual rendering succeeds.
The web migration can proceed, but Servo cannot become the desktop default
until that is fixed and verified on all supported platforms. See the
[Servo accessibility documentation](https://book.servo.org/design-documentation/accessibility/for-embedders.html).

## Implementation checkpoint

The implementation now includes the accepted architecture/rollback ADR,
`@trouve-ai/app-ui`, the internal desktop host, the protocol 3.9 session
summary and notification-edge projection, generated and runtime-validated clients, stable
`@lit/context` providers, the contained `@lit-labs/signals` adapter, the shared
five-theme visual system, the feature-gated Wry preview, and distinct desktop
and installable PWA artifacts. It also includes a Servo-first, chrome-free,
in-process embedding harness pinned to the 2026-08-02 Servo nightly at revision
`35672cc3d4beb768489f5218e73bee7aff0ddb01`. The harness is isolated in the
excluded nested Cargo workspace documented by ADR 0024 because Servo and
the product server require incompatible native SQLite link versions. It
requires and probes an explicit `TROUVE_SERVER_URL`, embeds no protocol server,
and keeps Servo storage and gateway preferences in process-owned temporary
directories, so it cannot open the default database. The live preview covers
the shell, session/thread lifecycle, branch-aware prompt-first session creation,
chat/composer/queue, approvals/questions, bounded attachments, todos,
slash-command and `@` file completion, files/code/diff, live diff refresh,
raw-diff copy, checkpoint undo/redo, route-backed roving keyboard navigation
for thread and inspection tabs, keyboard navigation for changed files, session
pull requests, validated desktop HTTPS opening, a user-initiated foreground
browser-notification smoke test, canonical same-origin/HTTPS Markdown link
handling, active permission-mode status with warning treatment for full access,
narrow/mobile unified-only diffs, and multiple terminals. It also has
qualification-preview implementations of the code-review dashboard and its
GitHub App/repository administration, automations, workspace/branch settings,
providers and authentication, vendor CLI lifecycle, local models/runtimes,
mode defaults, Git/worktree settings, MCP, and GitHub integrations. Session
lifecycle rows, source events, summary replacements, deletion tombstones, and
restart recovery are committed consistently through the event-log writer. The
browser ingress sequences metadata after its cursor-bearing snapshot, drains a
trailing refresh for coalesced lifecycle events, rejects stale responses from
stopped generations, shares concurrent bootstraps, and retries an offline
initial load on online/visible transitions or explicit user action. The shell
also includes attention-first active-session ordering, per-workspace archived
session disclosures, persisted desktop splitters, and a Ctrl/Cmd-K command
palette for routes, sessions, threads, and common actions. Tool cards retain a
bounded, UTF-8-safe live-output tail, separate arguments/live-output/result
regions, and scoped approve/always-approve/deny controls with Y/A/N shortcuts.
The thread tab strip and command palette now open a provisional, cancelable
new-thread setup surface that does not mutate server state until submission;
it covers mode, model, thinking, permission, an optional initial prompt, and
bounded file/paste attachments. The files inspection tab now has a lazy,
per-directory cached tree with race-safe loading/error states and accessible
roving-keyboard tree navigation. Desktop workspace setup can use a typed,
loopback-only native directory picker through desktop bridge version 8 and
then registers the selected path through the existing protocol; PWA and remote
deployments retain the explicit manual-path fallback.

The existing Slint frontend's functional surface is now ported. The executable
[application action contract](../../web/app-ui/src/app/app-action-contract.test.ts)
extracts all 134 `AppWindow` callbacks from `app.slint` and requires exactly
one Lit implementation or documented browser-native consolidation for each.
The closure pass covers completion/queue edge states, checkpoint undo/redo,
review behavior, onboarding and failure states, native/file attachment and
clipboard-image ingress, validated local-file actions, event-derived
notifications, notification routing/preferences, desktop lifecycle actions,
local-model fit and runtime lifecycle controls, and the explicit GitHub setup
path for session pull requests. The parity ledger is the authoritative
surface-by-surface evidence and additive-feature register.

ADR 0027 now makes this checkpoint a **reversible staged desktop promotion**,
not a claim of completed qualification or PWA publication. Wry is the default
and Slint remains the rollback frontend. Production PWA HTTPS
authentication/deployment, broader cross-language fixtures, automated
Slint-versus-Lit screenshots, keyboard/focus and assistive-technology/device
matrices, memory and performance budgets, six-platform packaging, security
review, and dual-frontend soak remain hard gates in the phases below.
Widget-level visual variation remains allowed; the existing themes, semantic
colors, density, layout, hierarchy, and core UX remain the acceptance baseline.
The parity ledger is the authoritative per-surface record and must be updated
with evidence rather than inferred from implementation alone.

The first live desktop preview exposed material visual-foundation and shell
differences from Slint. The 2026-08-03 rendered-parity correction pass now
covers the shell and inbox, session actions and dialogs, new-session and
new-thread flows, chat/composer/queue, approvals and questions, every
inspection panel, the Pull Requests dashboard, Automations and its editor,
every Settings section, and the desktop close workflow. Files now preserves
the Slint 210px/22px desktop tree composition and becomes a tree-to-viewer
flow on narrow screens; mobile approvals use a large-target bottom sheet;
full-screen PWA routes honor safe areas; and PR group ordering has an explicit
coarse-pointer alternative. The Slint-shaped continuous unified diff remains
the default, while the plan's desktop split-diff and code/diff search features
are now wired into the real inspection workflow as documented, opt-in
enhancements.

This is implementation and staged-rollout progress only: full visual parity is not claimed,
the existing ad-hoc local comparisons do not satisfy the deterministic paired
evidence matrix, and the remaining Servo/Wry qualification gates stay open.

That run also demonstrated that the desktop gateway's no-dynamic-code CSP is
an effective qualification constraint: runtime Ajv compilation was blocked.
Protocol and host schemas are now compiled into standalone ESM validators at
build time, with synchronization and CSP-safety tests; Ajv is not shipped in
the runtime bundle. Qualification builds must keep that constraint intact.

Current phase status is deliberately conservative:

| Phase | Current state | What remains before exit |
| --- | --- | --- |
| 0 — baseline | In progress | Complete Slint screenshots, interaction fixtures, workloads, and measured baselines. |
| 1 — decisions and gates | Architecture accepted | Approve the evidence matrix and close all unresolved deviations. |
| 2 — engine qualification | Chrome-free, exact-revision Servo nightly harness and default Wry host implemented; both remain incompletely qualified | Run the complete six-platform accessibility-action, hard-widget, lifecycle, recovery, packaging, memory, performance, and visual-parity matrix. The existence of the embedder is only first-pass evidence. |
| 3 — foundations | Implemented for preview | Finish cross-language conformance evidence and production remote-host validation. |
| 4 — visual system/primitives | Implemented for preview | Capture and approve all five-theme, state, viewport, keyboard, focus, and accessibility comparisons against Slint. |
| 5 — hard widgets | Functionally ported; unqualified | Qualify rendering, selection, disposal, IME, scale, touch, accessibility alternatives, memory, and performance. |
| 6 — shell/inbox | Functionally ported; unqualified | Qualify realistic local/remote desktop and installed-PWA workflows. |
| 7 — chat/composer | Functionally ported; unqualified | Pass reconnect, duplicate-mutation, Unicode/IME, anchoring, keyboard, touch, visual, and memory gates. |
| 8 — inspections | Functionally ported; unqualified | Qualify review/checkpoint behavior and the desktop and capability-gated PWA workflows. |
| 9 — management/settings | Functionally ported; unqualified | Attach visual, desktop, and PWA evidence to every ledger item and complete live-service failure matrices. |
| 10 — soak | Not started | Run the dual-frontend functional, visual, accessibility, security, memory, performance, update, and recovery soak. |
| 11 — promote | Reversible Wry default authorized by ADR 0027 | Complete the independent desktop qualification and rollout evidence while keeping Slint rollback; PWA promotion remains separately blocked. |
| 12 — retire/evaluate | Deferred | Retire Slint only after a successful default-release soak; evaluate post-PWA mobile options from measured usage. |

### Qualification-preview runbook

From the repository root, build and verify both frontend artifacts:

```sh
cd web/app-ui
npm ci
npm run generate:protocol
npm run typecheck
npm test
npm run build
npm run build:pwa
npm run verify:build-modes
```

The normal Wry desktop is now the default. It embeds exactly one local server
when `TROUVE_SERVER_URL` is absent, or connects to the selected server when the
variable is present. With Vite already running, launch the normal app with:

```sh
TROUVE_APP_UI_DEV_URL=http://127.0.0.1:5173 \
  cargo run -p trouve-app
```

Desktop qualification reuses exactly one already-running `trouve-server`.
Obtain its base URL from the server/desktop startup log, or start a standalone
server intentionally, then set `TROUVE_SERVER_URL` to that base URL. Preview
hosts fail fast when the variable is missing or the server does not answer
`/v1/info`; they never call `bind_local`, embed a second engine, or open the
default database. Never start a second server against a data directory already
owned by the desktop app or another server.

Test the true Servo embedder first from the repository root. It is a separate
nested Cargo workspace because the pinned Servo nightly's
rusqlite/libsqlite3-sys native link version cannot coexist in the root resolver
graph; see ADRs 0024 and 0025. Pass the
desktop Vite artifact and an explicit compatible protocol server URL, then run
the nested manifest with its lockfile:

```sh
TROUVE_SERVER_URL=http://127.0.0.1:7433 \
TROUVE_APP_UI_DIST=/absolute/path/to/trouve/web/app-ui/dist/desktop \
  cargo run \
    --manifest-path crates/trouve-servo-embed-preview/Cargo.toml \
    --locked
```

The harness links Servo into the process, paints one `WebView` over the entire
native client area, and includes no servoshell address bar or tabs. It enables
the pinned experimental preference set required by the Lit layout, including
CSS Grid. Servo's storage and desktop-host preferences use retained temporary
directories. The harness cannot link or start `trouve-server`, verifies the
explicit server's protocol version before opening the window, and reaches it
only through the hardened loopback gateway. Root `cargo test --workspace` and
other root workspace commands intentionally exclude this harness; qualification
and CI must invoke its manifest separately.

Direct embedding is now implemented, but native Wayland and X11/XWayland must
still be tested independently along with every other supported target. A
successful window on one backend is not a platform qualification pass.

Use the explicit Wry/system-webview comparison host without opening the
default database:

```sh
TROUVE_SERVER_URL=http://127.0.0.1:7433 \
TROUVE_APP_UI_DIST=/absolute/path/to/trouve/web/app-ui/dist/desktop \
  cargo run -p trouve-app --features web-preview --bin trouve-web-preview
```

For development, both hosts follow ADR 0026's shared source policy. A debug or
qualification process snapshots `TROUVE_APP_UI_DIST` at startup without
embedding it into the Rust binary. To use HMR, run `npm run dev` from
`web/app-ui`, omit
`TROUVE_APP_UI_DIST`, and set
`TROUVE_APP_UI_DEV_URL=http://127.0.0.1:5173` on the preview process. The
desktop gateway stays the webview origin: it proxies Vite assets while keeping
native-capability and `/v1` routes local to the gateway.

The `trouve` binary is the Wry product host; `trouve-slint` is its explicit
rollback. `dist/pwa` is a
separate deployable artifact and must not be passed to either desktop build.
Both qualification hosts reject invalid desktop assets and an older,
newer-major, or malformed server protocol before opening the preview. Build a
current server and restart its single DB-owning
process when the existing process is too old; never launch a second server over
the same data directory.
Do not expose the PWA publicly until the HTTPS, authentication, origin,
service-worker, revocation, and deployment gates in sections 11, 14, and 15
have passed.

## 1. Current implementation baseline

The source analysis described a large migration, and the current workspace
confirms that assessment:

| Area | Current size |
| --- | ---: |
| trouve-app Slint UI | 10,365 lines |
| trouve-app Rust/controller/rendering | 17,195 lines |
| Generic Slint widget UI | 1,216 lines |
| Generic Slint widget Rust support | 2,637 lines |
| Total directly affected UI code | 31,413 lines |

The migration is a state-boundary redesign, not a syntax conversion.

The main implementation anchors are:

- crates/trouve-app/ui/app.slint, which declares the main UI surface and the
  “models in, callbacks out” boundary.
- crates/trouve-app/src/main.rs, which handles desktop startup, the native
  window, clipboard images, external navigation, callbacks, and quit behavior.
- crates/trouve-app/src/controller.rs, which mixes orchestration, protocol
  consumption, projections, presentation state, and native behaviors.
- crates/trouve-app/src/render.rs, which contains presentation semantics that
  must be preserved rather than treated as disposable drawing code.
- crates/trouve-app/src/ui.rs, which bridges Rust state into Slint models and
  callbacks.
- crates/trouve-client-core/src/viewmodel.rs, which is the best existing
  starting point for frontend-neutral event projection.
- docs/design/ux-screen-map.md, which remains the canonical intended-screen
  inventory.
- The four generic widget crates: trouve-slint-code-view,
  trouve-slint-diff-view, trouve-slint-markdown, and
  trouve-slint-terminal.

Several architectural facts must remain unchanged:

- Clients communicate with the harness through HTTP and SSE only.
- The desktop app may embed trouve-server using trouve_server::bind_local, but
  it must not import engine internals.
- Durable UI-visible state flows through the persisted, cursor-addressed event
  log.
- Terminal byte streams remain explicitly ephemeral.
- File, shell, git, and MCP effects continue to pass through ToolExecutor.
- Sessions retain ownership of their worktrees.
- Modes remain data rather than Rust control-flow variants.

These boundaries are established by ADR 0008, ADR 0019, and the root
repository instructions.

## 2. Visual and interaction continuity

Visual parity is a hard acceptance criterion. The migration must preserve:

- The existing theme families and their semantic color relationships.
- Background, panel, surface, border, text, muted-text, selection, status,
  warning, error, success, accent, diff-addition, and diff-deletion colors.
- Typography scale, weight hierarchy, line height, density, spacing rhythm,
  corner treatment, separators, and elevation.
- The three-column desktop shell, relative panel proportions, collapsible
  regions, screen hierarchy, cards, badges, and status placement.
- Information architecture, workflows, keyboard behavior, focus restoration,
  scroll anchoring, disclosure behavior, and selection rules.
- Existing distinctions among inactive, selected, hovered, focused, disabled,
  loading, disconnected, stale, warning, failure, approval, question, and
  success states.
- The overall Trouve visual identity across Servo, the system-webview fallback,
  ordinary browsers, and the PWA.

Controlled variation is acceptable when caused by the chosen web control
library or platform conventions. Examples include WebAwesome tab, menu, select,
dialog, and form-control chrome. Such controls must still use Trouve semantic
tokens, match the surrounding density and hierarchy, and preserve the same
workflow. Intentional deviations must be recorded in the parity ledger with a
reason and approval; they must not appear incidentally during implementation.

Pixel-for-pixel equality is not required where browser font rasterization,
native scrollbar behavior, or platform accessibility settings differ. The
required outcome is close visual correspondence in layout, color, density,
hierarchy, component dimensions, and core experience.

The migration must not bundle unrelated visual redesigns. Deliberate redesign
proposals should be reviewed separately after parity is achieved.

### Visual baseline and review artifacts

Before component implementation:

- Capture reference screenshots from the Slint frontend for every major screen,
  theme, connectivity state, permission state, dialog, popover, empty/loading/
  error state, and representative content density.
- Capture standard desktop viewport sizes and all supported operating-system
  theme modes.
- Record panel widths, split defaults and limits, row heights, padding, gaps,
  border radii, type scale, status colors, and animation timings.
- Extract the semantic intent of crates/trouve-app/ui/theme.slint and
  crates/trouve-app/src/theme.rs into a versioned parity specification.
- Capture focus order, hover/pressed/selected states, scroll positions, and
  keyboard-driven state transitions that screenshots alone cannot represent.

During implementation:

- Build side-by-side Slint and Lit component-gallery cases.
- Map the current theme into Trouve-owned semantic CSS custom properties before
  mapping those properties into WebAwesome.
- Use screenshot regression tests at fixed viewports and themes.
- Review browser-rendering differences rather than blindly accepting or
  rejecting pixel diffs.
- Require explicit UX sign-off for every intentional visual deviation.

Before promotion:

- Run full-screen visual comparisons for all 21 surfaces.
- Verify themes and status semantics on every shipping desktop engine.
- Verify that responsive PWA adaptations retain the same visual identity even
  when navigation and panel arrangement change for mobile.
- Close all unexplained visual-regression differences.

## 3. Target architecture

Use an app-owned loopback gateway between the desktop web UI and either the
embedded or remote Trouve server:

```text
Lit application
  screens/components → normalized store → services
                     │ same-origin HTTP/SSE
                     ▼
trouve-desktop-host loopback gateway
  /                  bundled static UI
  /v1/*              streaming HTTP/SSE proxy
  /desktop/v1/*      narrow typed native API
                     │
           ┌─────────┴──────────┐
           ▼                    ▼
  embedded bind_local       validated OS APIs
  or remote server          picker/clipboard/etc.
           │
  trouve-server/core/providers
```

The gateway belongs to the desktop application rather than trouve-server
because:

- The headless server should not acquire desktop asset or native-host
  responsibilities.
- The same gateway can proxy to an embedded server or TROUVE_SERVER_URL.
- The browser receives one same-origin surface.
- Native functions remain completely separate from the harness protocol.
- The host can apply strict origin, navigation, cookie, and content-security
  controls.

The gateway is a transport and native-capability boundary. It must not become a
second agent-state channel.

The shipping desktop architecture may continue to own one embedded server
through `bind_local`. Qualification previews have a stricter safety rule:
The embedded Servo qualification harness and Wry preview are clients of an
explicitly selected external server only. They must never implicitly embed a
server or open the default database. The Servo harness additionally isolates
its engine storage and gateway preferences in temporary directories. Two live
engines over one SQLite database would contend for the WAL
writer while maintaining separate in-memory event broadcasts, schedulers,
turn state, and worktree serialization, so a larger SQLite timeout is not an
acceptable substitute for this rule.

### Mobile PWA architecture

The initial mobile delivery is a PWA built from the same
@trouve-ai/app-ui source:

```text
Installed mobile PWA
  Lit screens + responsive/mobile navigation
  web capability adapter
                     │ same-origin HTTPS preferred
                     ▼
PWA web host / reverse proxy
  /                  hashed frontend assets
  /manifest.webmanifest
  /v1/*              HTTP/SSE proxy
                     │ authenticated HTTP/SSE
                     ▼
             remote trouve-server
```

Mobile rules:

- The PWA never embeds trouve-server or imports trouve-core.
- Prefer an HTTPS same-origin deployment that proxies /v1/*; do not rely on
  permissive CORS.
- Use a capability adapter so screens can distinguish desktop-host,
  browser/PWA, and unavailable operations.
- Provide responsive navigation, safe-area handling, touch targets, virtual
  keyboard handling, and touch terminal modifiers as first-class requirements.
- Retain the desktop frontend’s visual identity through the same semantic
  tokens, typography, component language, and state colors. Change panel
  arrangement only where mobile constraints require it.
- The PWA service worker may cache immutable hashed application assets and an
  offline shell. It must never cache protocol responses, SSE data, prompt or
  repository content, credentials, or native-bridge responses.
- The desktop-host build must not register the PWA service worker; the desktop
  gateway uses its own immutable packaged-asset lifecycle.
- PWA installation is not a claim of full native parity. Each unsupported
  capability must be represented explicitly rather than failing silently.
- After measured PWA adoption, workflow gaps, memory, accessibility,
  notification reliability, and background behavior are available, evaluate
  other options such as a native mobile shell, an embedded system webview, or
  a future Servo mobile embedder. That evaluation is a later ADR, not part of
  initial delivery.

### Proposed packages

Once this plan is approved:

- Add web/app-ui with package name @trouve-ai/app-ui.
- Add crates/trouve-desktop-host.
- Keep trouve-app as the main desktop application and binary.
- Introduce a feature-gated web-UI preview path in trouve-app; do not create a
  second desktop product package.
- Add the PWA manifest, icons, service-worker entry, and deployment
  configuration to @trouve-ai/app-ui or a build-mode-specific subdirectory.
  Do not create a separately versioned mobile package unless deployment later
  requires one.
- Add shared Node packages only after two real consumers exist. Do not
  pre-emptively extract abstractions from the Preact review UI.

Both new packages must inherit the root workspace version. Use the repository
package-creation, version synchronization, and release workflows when
implementation starts.

## 4. Architectural decision records and design documents

The first implementation change should be an ADR, before production code.

### ADR 0023

Create docs/adr/0023-lit-web-frontend-and-webview-host.md after the direction is
approved. It should record:

- Lit and TypeScript as the application frontend.
- @lit/context as the context mechanism for stable services and scoped store
  objects.
- @lit-labs/signals behind a Trouve-owned adapter as the fine-grained
  reactivity mechanism, explicitly acknowledging its experimental status and
  retaining an exit strategy.
- Visual and interaction parity with the existing Slint frontend as a hard
  requirement, with controlled widget-chrome variation allowed.
- WebAwesome Free for general controls.
- CodeMirror, MergeView, xterm.js, and the Markdown pipeline.
- The app-owned same-origin desktop gateway.
- Continued use of bind_local and protocol-only access.
- Servo as a gated candidate rather than a guaranteed default.
- The maintained system-webview fallback.
- The native bridge boundary.
- The PWA as the initial mobile delivery mechanism.
- Remote protocol-only access and capability degradation for the PWA.
- A later evidence-based evaluation of native mobile or embedded alternatives.
- The complete promotion gates.
- The point at which Slint may be retired.

Then:

- Mark ADR 0005 as superseded without rewriting its historical content.
- Keep ADR 0006 accepted throughout dual-front-end development and
  distribution.
- Supersede ADR 0006 only when no shipped artifact contains Slint or its
  required attribution.
- Update the ADR index.
- Add an architecture invariant stating that the desktop bridge is for native
  capabilities and nonsecret preferences only; durable agent state never flows
  through it.
- Update the UX screen map’s “web later” language and document PWA-first mobile
  navigation.
- Add design documents for frontend architecture, native bridge/threat model,
  projection conformance, engine qualification, PWA capabilities, memory
  budgets, visual parity, and the UI parity ledger.

## 5. Engine decision: Servo and system webview

### Servo remains worth qualifying

Servo offers:

- A Rust-controlled desktop lifecycle.
- No Electron Node.js runtime.
- A potentially smaller and simpler process model than Electron.
- The same web frontend across Servo, system webviews, ordinary browsers, and
  the mobile PWA.
- A path to tighter Rust integration without allowing frontend access to
  engine internals.

However, the embedding project is still young. Servo’s library distribution
began at 0.1.0 in 2026, its embedding overview remains explicitly work in
progress, and its published support model includes frequent breaking releases
with periodic LTS lines. See the
[Servo 0.1.0 announcement](https://servo.org/blog/2026/04/13/servo-0.1.0-release/),
[embedding overview](https://book.servo.org/embedding/overview.html), and
[LTS policy](https://book.servo.org/embedding/lts-release.html).

Current blockers and risks include:

- Accessibility-tree actions are not supported by the current embedder API.
- Embedding lifecycle, IME, accessibility, downloads, focus, drag-and-drop,
  and crash behavior need product-level validation.
- Published Servo downloads do not demonstrate every Trouve release target,
  particularly Linux ARM64 and Windows ARM64. See
  [Servo downloads](https://servo.org/download/).
- WebAwesome officially tests current mainstream browsers, not Servo. Every
  component behavior used by Trouve must be tested independently. See
  [WebAwesome browser support](https://webawesome.com/docs/resources/browser-support).
- Exact-origin, cookie, and navigation behavior must be tested against the
  pinned Servo build. See the
  [Servo April 2026 update](https://servo.org/blog/2026/05/31/april-in-servo/).
- A same-process renderer can expose the host to renderer crashes or memory
  exhaustion unless recovery is designed explicitly.

### Current Servo-first qualification harness

The first executable engine check is
`crates/trouve-servo-embed-preview`, a chrome-free in-process embedder pinned
to an exact Servo nightly revision. It creates a native window and rendering
context, drives one direct Servo `WebView`, and loads the same packaged Lit
assets through the hardened loopback gateway used by the Wry preview. The exact
engine revision and required experimental web-platform preferences make initial
results reproducible and prevent a CSS-Grid-disabled run from being mistaken
for representative layout evidence.

ADR 0024 keeps this disposable harness in an excluded nested Cargo workspace
with its own lockfile because the pinned Servo nightly and the product server
require incompatible `libsqlite3-sys` link versions. ADR 0025 requires an exact
nightly revision instead of a moving branch. This also makes the safety
boundary structural: the harness cannot link or start `trouve-server`, requires
an explicit compatible `TROUVE_SERVER_URL`, and uses isolated temporary engine
and gateway storage.

The harness exercises the real embedding API, full-client-area rendering,
resize/DPI, focus, keyboard, IME, pointer, wheel, touch, theme, animation
pumping, origin-restricted navigation, and clean process shutdown. It remains
qualification-only. Accessibility actions, the production native-capability
adapter, clipboard and dialog behavior, drag/drop, downloads, DevTools,
crash/OOM recovery, renderer recreation, packaging, memory/performance budgets,
visual parity, and six-platform support remain open gates.

The pinned nightly supports keyboard-driven selection in editable controls,
and the adapter preserves the Shift and command/control modifiers needed for
that path. Servo's
[ordinary document selection issue](https://github.com/servo/servo/issues/38124)
remains open for mouse/touch interaction, so drag selection is still an engine
qualification failure.

### System-webview qualification

Use Wry as the staged default and retain its explicit comparison path after the
initial Servo run. Wry must still be qualified even if Servo's first rendering
pass looks viable.
Wry maps to WebKitGTK on Linux, WKWebView on macOS, and WebView2 on Windows,
but Linux runtime dependencies, Wayland integration, accessibility, and
cross-compilation still require validation for Trouve’s six desktop targets.
See the [Wry documentation](https://docs.rs/wry/latest/wry/).

### Engine decision outcomes

Phase 2 must end in one of these outcomes:

1. Servo passes every gate: make it the preferred desktop engine.
2. Servo fails, but the system webview passes: ship the Lit desktop UI on the
   system webview and continue tracking Servo.
3. Neither passes: stop the desktop frontend migration and retain Slint.
4. If product policy forbids a system-webview fallback, failure of the Servo
   accessibility or packaging gate ends the desktop migration.

The system-webview fallback is an engine adapter, not a separate frontend. The
mobile PWA is independently qualified and does not depend on this decision.

## 6. Lit versus Preact

Choose Lit for the main application.

### Why Lit fits

- WebAwesome is based on custom elements and Lit.
- The existing application maps naturally to reusable leaf widgets with
  explicit inputs and typed events.
- Code, diff, Markdown, terminal, question, approval, and tool-card components
  benefit from framework-neutral custom elements.
- @lit/context can express stable application, workspace, session, thread,
  terminal, host-capability, and deployment scopes.
- The frontend remains usable outside a particular virtual-DOM runtime.
- A component gallery can be built incrementally while Slint remains usable.

### Lit constraints

- Do not make every trivial element a custom element.
- Use light DOM for screens and layout-heavy compositions.
- Use Shadow DOM for reusable product widgets needing encapsulation.
- Document CSS properties, parts, slots, keyboard behavior, and events.
- Use @lit/context as dependency and scope injection, not the entire state
  manager.
- Use typed, bubbling, composed CustomEvents rather than recreating the current
  large property/callback bridge.
- Use @lit-labs/signals only through an owned module such as
  state/reactivity.ts. Lit describes signal integration as experimental and
  notes template limitations. See
  [Lit signals documentation](https://lit.dev/docs/data/signals/).
- Pin @lit-labs/signals, verify that only one signal-polyfill implementation is
  installed, and retain the ability to replace the library.
- Do not let signal-aware list rendering bypass keyed identity or
  virtualization.
- Use @lit/task for bounded request/response loading, not SSE streams. See
  [Lit task documentation](https://lit.dev/docs/data/task/).
- Use stable service/store objects through @lit/context. See
  [Lit context documentation](https://lit.dev/docs/data/context/).

### Preact’s role

Keep web/review-ui as Preact. Share only generated protocol DTOs, pure
formatters, semantic design tokens, and reusable event-log fixtures. Do not
copy its monolithic structure into the desktop/PWA frontend. Reconsider Preact
only if actual sharing becomes more valuable than custom-element fit.

## 7. Frontend structure and state model

Proposed structure:

```text
web/app-ui/
  src/
    app/
    services/
    state/
    contexts/
    router/
    capabilities/
    components/
    screens/
    workers/
    styles/
    generated/
    pwa/
    test/
```

### Services and generated types

Define stable interfaces for protocol/capability discovery, HTTP requests,
durable event SSE, session-summary SSE, terminal SSE, reconnect/backoff,
cancellation, native-host capabilities, PWA/browser capabilities, and safe
diagnostics. Runtime-validate every API/SSE payload before it enters state.

Generate TypeScript types and clients from the canonical OpenAPI snapshot:

- Pin the generator and configuration.
- Commit output if required for deterministic/offline builds.
- Add a CI drift check.
- Use Ajv 2020 or an equivalent validator at HTTP/SSE ingress.
- Preserve compatible unknown-event handling while logging safe diagnostics.
- Never maintain a second hand-written wire schema.

### Store

Normalize workspaces, sessions, threads, events, message projections,
approvals, questions, queue, todos, pull requests, automations, providers,
model catalog/availability, MCP servers, and terminals by stable ID.

Keep one canonical copy of source data. Do not retain raw events, parsed
structures, highlighted spans, rendered HTML, and live DOM for the same content
at once.

### Contexts with @lit/context

Define explicit keys and stable interfaces:

- AppServicesContext for protocol, logging, router, and deployment services.
- AppStoreContext for normalized state and selectors.
- HostCapabilitiesContext for desktop-host versus PWA/browser capabilities.
- WorkspaceContext for selected workspace scope.
- SessionContext for selected session scope.
- ThreadContext for selected thread/cursor scope.
- TerminalContext for one ephemeral terminal manager scope.

Context values stay stable and expose selectors/methods instead of being
replaced for every event. Providers live at clear layout boundaries, and
consumers handle missing context in gallery/test isolation.

### Reactivity with @lit-labs/signals

Create state/reactivity.ts as the only direct importer:

- Signals represent selected slices, revision counters, or small live values.
- Never allocate a signal per historical message, token, line, terminal cell,
  or field.
- Preserve keyed object identity across unrelated state changes.
- Unsubscribe components automatically on disconnect.
- Expose test hooks for active subscription counts.
- Keep the selector/context interfaces independent of the signal library.

### Streaming and workers

Use explicit controllers for cursor resume, idempotency, duplicates,
reconnect/backoff, subscription policy, abort/disposal, and terminal offsets.
Do not model EventSource through @lit/task.

Use lazy workers for Markdown/sanitization, syntax highlighting, diff
preparation, fuzzy matching, and expensive search. Terminate idle workers and
clear caches.

### Local and persisted state

Keep disclosures, dialogs, unsaved forms, completion selection, copy/hover, and
drag state local.

On desktop, persist only typed nonsecret preferences through the host API
because the local origin uses an ephemeral port. In the PWA, browser storage is
limited to explicitly approved nonsecret device preferences and the selected
authenticated session mechanism. Never store prompts, repository contents,
provider secrets, raw event history, or native credentials for offline replay.

## 8. Protocol and projection work

The current desktop controller follows active and background thread histories
to derive attention, outcomes, and notifications. Repeating that in a browser
would create too many EventSource connections and retain too much history; it
would be especially fragile in a suspended mobile PWA.

### Session-summary projection

Add a server-scoped, cursor-addressed projection:

- SessionAttention: none, approval, question, or both.
- SessionOutcome: idle, running, succeeded, or failed.
- SessionSummary: session/workspace IDs, active/archived state, attention,
  outcome, latest relevant thread ID, latest durable cursor, and timestamp.
- A snapshot containing summaries and a cursor.
- A durable SessionSummaryUpdated event.
- A compact durable SessionNotification edge for completed/failed turns and
  approval/question requests, including the source thread and optional bounded
  native-equivalent detail.
- A browser-consumable endpoint such as GET /v1/session-summaries.
- One resume-capable SSE stream after the snapshot cursor.

Requirements:

- Derive summaries transactionally from durable events.
- Do not miss transitions between snapshot and stream.
- Persist updates or tie them atomically to the source cursor.
- Keep unread client-local by comparing seen and latest cursors.
- Let desktop and PWA clients consume notification edges without following
  every inactive thread; keep delivery preferences, focus suppression, sound,
  and activation client-owned.
- Let the desktop host observe summaries for sleep.
- Let the PWA rebuild after foregrounding without all thread histories or
  continuous background execution.

After the breaking 3.0 semantic-router change on `main`, the summary projection
is the additive 3.1 change and exact compact notification edges are the
additive 3.2 change, independently of the
workspace product version. Follow the protocol-change workflow, update
trouve-protocol/server/client tests, and regenerate OpenAPI deliberately:

```sh
TROUVE_UPDATE_OPENAPI=1 cargo test -p trouve-server openapi
cargo test -p trouve-server
```

### Active history and terminal

Active threads now bootstrap from the newest bounded folded-view page and open
SSE strictly after the exact cursor returned with that page. Older folded
items load backward in contiguous 256-item pages as the reader approaches the
top; prepends retain stable absolute identities and preserve the reader's
anchor. The explicit accessibility mode incrementally loads every remaining
page before exposing the complete nonvirtual history. The durable event log
remains authoritative, and reconnect replay begins only after the installed
snapshot/live cursor.

Terminal creation/input/resize/close remain HTTP; bytes remain dedicated
ephemeral SSE; every terminal keeps independent parser/grid/offset state;
bytes never enter the durable reducer; reconnect does not duplicate output;
renderer disposal does not close the PTY.

### Projection conformance

Do not compile all of trouve-client-core to Wasm initially. Instead:

1. Extract frontend-neutral projection rules and fixtures.
2. Serialize event sequences and expected normalized snapshots.
3. Implement the TypeScript reducer against them.
4. Run identical cases through Rust and TypeScript in CI.
5. Cover duplicates, cursor gaps, reconnects, partial streaming,
   approvals/questions, tools, queue, and compatible unknown events.

Rust remains the semantic reference without becoming a browser runtime.

## 9. Component and tooling choices

### Controls and product components

Use self-hosted WebAwesome Free for ordinary controls. Use only the MIT Free
package unless Pro licensing is separately approved. See
[WebAwesome usage](https://webawesome.com/docs/usage/) and
[license](https://webawesome.com/license).

Own Trouve semantics: workspace/session rows, aggregate status, connectivity,
chat cards, tools, approvals, questions, queue, composer, code shell,
multi-file diff, terminal manager, PR cards, model download cards, and MCP
editor/logs.

### Specialized widgets

- CodeMirror 6 for read-only files; lazy language support and worker
  highlighting.
- MergeView prototype for unified/split diffs, with a purpose-built virtualized
  fallback if scale, accessibility, touch, or memory fails.
- xterm.js with Fit, Search, Unicode, WebLinks, and accessibility support.
  WebGL remains off until disposal tests pass. Mobile gets a touch modifier row
  and explicit platform-limit messaging.
- A micromark/unified-style Markdown pipeline with explicit extensions,
  sanitization, safe links, worker highlighting, incremental streaming, and
  unmounted collapsed output.
- A small owned virtualizer supporting fixed/variable heights, stable IDs,
  ResizeObserver correction, follow-tail, anchored restoration, heavyweight
  unmounting, and accessible nonvirtual fallbacks.

### Styling, responsive behavior, and tools

Create Trouve-owned semantic CSS tokens by extracting the current Slint theme,
then map those tokens to WebAwesome. Do not begin from WebAwesome defaults and
approximate the application afterward. Support system theme, forced colors,
reduced motion, large fonts, compact/touch navigation, safe areas,
virtual-keyboard resizing, portrait/landscape, and mobile browser chrome.

Use Vite, strict TypeScript, Vitest, Playwright, axe-core, screenshot
regression tests, side-by-side parity galleries, and host smoke suites. Do not
add SSR, hydration, or a Node runtime. A narrowly scoped service worker is
allowed only in the hosted PWA build.

## 10. Complete 21-surface migration ledger

Every item needs functional, keyboard, accessibility, visual, failure,
lifecycle, desktop, and mobile-PWA acceptance. PWA-unavailable behavior must
be explicitly capability-gated. Every desktop surface must retain its current
layout, density, visual hierarchy, theme semantics, and workflow unless a
specific deviation is approved.

1. **Shell and inbox — medium.** CSS Grid desktop columns/splitters;
   collapsible groups, reorder/archive, statuses, rename/archive/delete,
   picking, replacement screens, recovery, focus, quit, and persistence.
   Match current panel proportions, row density, selection, badges, and status
   placement. Mobile uses compact route navigation, back-stack semantics, safe
   areas, touch reorder alternatives, and selection recovery after restart.

2. **Session/thread management — medium.** Branch/remote, mode/model/thinking,
   permissions, defaults, attachments, inheritance, health/availability,
   fuzzy search, warnings, offline blocking, validation, and recovery. Build
   an owned virtualized health combobox for pointer and touch while preserving
   current information order and status color meanings.

3. **Chat — high.** Streaming Markdown, attachments, thinking, every tool
   state, raw/formatted output, duration/exit, file actions, inline diffs,
   approvals, questions, usage, links/copy, disclosures, anchored scroll,
   follow-tail, reduced motion, occlusion, and bounded DOM. Match current card
   hierarchy, spacing, metadata prominence, status colors, and disclosure
   defaults. Add narrow, touch-selection, virtual-keyboard,
   foreground-resume, and mobile-memory cases.

4. **Composer/completion/queue/attachments — medium-high.** Autogrow, slash and
   file completion, image paste/file input, send/cancel, pause/restart, queue
   edit/reorder/delete/send-now, and controls. Preserve current placement,
   density, enabled/disabled cues, and keyboard flow. Use one tested DOM UTF-16
   to protocol UTF-8 conversion. Test IME, dead keys, virtual keyboard, focus,
   touch, and attachment permission/cancel.

5. **Diff — high.** Unified/split, multi-file, line numbers, selection/copy,
   syntax, expansion, undo/redo, PR navigation, large patches, disposal, and
   accessible text. Preserve current addition/deletion colors, line density,
   headers, and action placement. Narrow/mobile layouts use the existing
   single-file unified contract; side-by-side remains desktop-only.

6. **Files/code — medium.** Tree navigation, lazy load, external open, range
   reveal, select/copy/search, gutter, language, binary/large fallbacks,
   virtualization, and themes. Match current source colors, gutter hierarchy,
   selection, and panel composition. PWA actions follow browser capabilities
   and never imply arbitrary local filesystem access.

7. **Terminal — high.** Multiple tabs, lifecycle, independent IDs/parser/grid/
   offsets, input/resize/keys, selection, clipboard confirmation, mouse/wheel/
   search/links, Unicode/IME, touch modifiers, exit, and duplicate-free resume.
   Preserve the current terminal container, tab/status placement, palette, and
   focus behavior. Benchmark one/five desktop terminals and a bounded mobile
   workload. Dispose inactive renderers without closing PTYs.

8. **Todos/plan — low.** Semantic status/progress covering empty, stale,
   current, completed/cancelled, streaming updates, long text, ownership, and
   compact mobile presentation. Preserve status symbols, ordering, density,
   and color semantics.

9. **Session PR — low-medium.** Eligibility, draft/create/open, branch/remote,
   progress/failure/retry, validated external navigation, and aggregate badges.
   Preserve current placement and hierarchy. PWA uses user-initiated HTTPS
   navigation.

10. **PR dashboard — medium.** Full-screen Lit view with grouped virtual cards,
    filters, pagination/refresh, review jobs/artifacts, repository/provider
    distinctions, all async states, responsive cards, and route restoration.
    Preserve the current Slint dashboard’s information hierarchy and theme.
    Use the Preact UI only as an API/fixture reference.

11. **Automations — low-medium.** List/detail/forms for create/edit/delete/
    enable/run, schedule, selection, validation, history, concurrency, failure,
    offline, confirmation, and usable touch schedule editing. Preserve current
    screen hierarchy and visual status language.

12. **General/appearance — low UI, medium native.** Theme, semantic colors,
    contrast, scaling, motion, layout defaults, preview, narrow layout,
    persistence, restart indicators. Existing themes remain first-class and
    visually matched. Hide/explain desktop-only window/sleep settings in PWA
    mode.

13. **Notifications — medium-high.** Capabilities/permissions, test, toggles,
    focus suppression, click routing, attention, quiet/offline, persistence.
    Preserve current grouping and status presentation. Desktop remains
    host-owned. PWA reliability is capability-tested and never overstated.

14. **Providers/onboarding — medium.** First run, login/device auth, provider/
    CLI health, cancellation, expiry/recovery, subscription, model overlays,
    defaults. Preserve current onboarding progression and visual trust cues.
    Secrets never enter persistence, logs, signals, DOM attributes, or
    displayed raw errors.

15. **Modes/models — low-medium.** Modes remain data. Support defaults,
    inheritance, availability, unsupported combinations, search, reset, and
    refresh on desktop/mobile. Preserve current option density, health cues,
    and inheritance presentation.

16. **Local models — medium.** Search, installed/available, progress, cancel/
    retry/delete, disk errors, concurrency, updates, cleanup. Match current
    progress/status colors and card hierarchy. PWA explains that models live on
    the remote server host, not the phone.

17. **Git/worktrees — low UI.** Identity/defaults, worktree policy/status,
    dirty/conflict/error, remote/default branch, cleanup, confirmation. Match
    current forms and warning hierarchy. All effects remain protocol operations
    in session-owned worktrees.

18. **MCP — medium.** Scoped editor, effective configuration, environment,
    validation, enable/disable, masking, health, restart/reconnect, tools,
    virtual logs, copy, large output, disposal, and bounded mobile long lines.
    Preserve current scope/status/log hierarchy and secret presentation.

19. **Integrations — medium.** Connect/disconnect/re-auth, multiple accounts,
    scopes, health, defaults, unavailable capability, OAuth navigation,
    callback cancellation, confirmation, validated desktop URLs, secure PWA
    redirect origins. Preserve current grouping and trust/status language.

20. **About/licensing — low technical, medium compliance.** Product/workspace/
    protocol/engine/deployment/frontend versions, links, diagnostics, notices,
    offline content, conditional Slint attribution, npm inventory, and PWA
    build/service-worker revision. Match the existing app’s visual treatment.

21. **Desktop integration and web capabilities — high.** Typed desktop bridge
    for picker, clipboard images, files/HTTPS, notifications, attention, sleep,
    window/focus/visibility/occlusion, quit, and crashes. Test schema/origin/
    path/scheme, gestures, DPI, IME, drag/drop, downloads, remote mode,
    shutdown, and recovery. Ensure native dialogs/actions feel integrated with
    the existing UX. PWA uses a separate capability adapter and never an
    unrestricted browser/server escape hatch.

Retain all four Slint widgets and examples as fixture sources until their
replacements pass. Remove them only if no consumer remains.

## 11. Native bridge, PWA capabilities, and security

### Desktop bridge

Version/schema the bridge separately from trouve-protocol. Allow only explicit
directory/file selection, selected clipboard image, validated local file/HTTPS
open, notifications, attention, sleep, typed preferences, focus/visibility/
occlusion, geometry, quit, and lifecycle/navigation events.

Never allow arbitrary filesystem, shell, URL scheme, Rust invocation, git/MCP/
tool operations, durable agent state, or unrestricted logging.

Expose a typed capability snapshot through HostCapabilitiesContext. Components
render from capabilities rather than user-agent parsing and show useful
unsupported states.

### Desktop gateway security

- Bind loopback on an ephemeral port.
- Create a random per-launch bootstrap/CSRF credential.
- Prefer HttpOnly SameSite=Strict cookies plus a validated mutation header.
- Validate exact Host, Origin, and applicable Referer.
- Fix the upstream in the host; never accept a browser-selected proxy target.
- Stream SSE without buffering/cursor changes.
- Strip unsafe forwarding/hop-by-hop headers.
- Serve only hashed packaged assets.
- Apply restrictive CSP with no unsafe-eval.
- Block unexpected navigation, subresources, downloads, and schemes.
- Package all dependencies/fonts/workers/icons/languages locally.
- Do not register the PWA service worker in desktop builds.
- Do not persist agent content/secrets in browser storage.
- Validate bridge schemas and redact diagnostics.

Remote desktop mode preserves TLS/authentication; it does not use permissive
CORS.

### PWA hosting and service-worker security

- Host over HTTPS outside local development.
- Prefer same-origin API proxying and HttpOnly authenticated sessions.
- Pin allowed API origins when separation is unavoidable.
- Scope the worker to the PWA application path.
- Precache only the versioned shell and immutable static assets.
- Use network-only behavior for /v1/*, SSE, auth, OAuth, and all user/repository
  content.
- Never put tokens, secrets, prompts, event logs, terminals, source, or diffs in
  Cache Storage or IndexedDB.
- Surface controlled update/reload and never mix incompatible assets.
- Validate manifest scope/start URL/icons/display/link capture.
- Threat-model shared/lost devices, app-switcher exposure, revocation, and
  logout cache clearing.
- Treat suspension as normal and rebuild from summaries/cursors on foreground.

Desktop notifications/sleep use a native summary observer. PWA web
notifications are used only where capability gates pass; the design never
depends on a continuously running page/SSE connection for guaranteed
background delivery. Reliable push, if later required, is a separate
authenticated server/protocol/security decision.

## 12. Phased implementation sequence

Parallel workstreams are engine/desktop host, protocol/state, UI/components,
and quality/mobile/packaging. Critical path is engine accessibility/platform
support, then chat anchoring, large diffs, terminal lifecycle, and native
integration.

The functional implementation work described by phases 3–9 is complete at
this checkpoint. Their exit clauses intentionally remain open because they
require measured qualification evidence rather than source implementation.
Phases 0–2, 10, and 11 therefore still block promotion even though the
frontend screens and callbacks are ported.

The 2026-08-04 exhaustive implementation audit closed the remaining
repository-automation gaps: route-scoped context consumers, a lazy bounded
content worker, real-browser Playwright/axe and visual regression suites,
desktop/PWA bundle budgets, npm and Rust dependency notices/SBOMs, and an
explicit CI job for the excluded Servo workspace. See the implementation
audit for requirement-level traceability and the external evidence that still
prevents promotion.

### Phase 0 — baseline

Freeze event/visual fixtures; build the 21-surface desktop/mobile ledger;
measure startup/memory/input/event-to-paint/scroll/widgets/package/crash;
define workloads; classify controller/render/UI responsibilities; capture
shortcuts, focus, scroll, notification, sleep, quit, and responsive behavior.

Capture the full Slint visual baseline described in section 2, including
screenshots, theme tokens, geometry, density, state styles, focus behavior, and
standard viewports.

**Exit:** no required behavior or visual rule is tribal knowledge.

### Phase 1 — decisions and gates

Accept ADR 0023; update docs/invariants; freeze boundaries; explicitly record
@lit/context and the @lit-labs/signals adapter; approve functional, visual,
memory, performance, accessibility, security, platform, and PWA gates; define
engine rollback; record PWA-first mobile and the later-options evaluation
trigger.

**Exit:** failure rules, visual-parity rules, and mobile scope are agreed.

### Phase 2 — desktop engine qualification

Run the pinned, chrome-free, in-process Servo nightly qualification harness
first, then the Wry comparison host. Test recreation, resize/DPI,
focus/IME/dead keys,
clipboard, drag/drop, pickers/downloads/navigation, custom elements/Shadow DOM/
forms/observers, Lit context/signals/tasks, WebAwesome, all hard widgets,
workers, EventSource/cookies, AT actions, DevTools, crash/OOM recovery, assets,
visual consistency, and all six targets.

**Exit:** Servo passes or is demoted; the fallback passes or desktop migration
stops. A successful embedded smoke test does not satisfy this exit. Current
missing AT actions prevent Servo promotion.

### Phase 3 — foundations

- Scaffold @trouve-ai/app-ui and trouve-desktop-host.
- Add desktop gateway and mock native host.
- Add PWA build, manifest, icons, capability adapter, scoped service worker,
  and local test host.
- Generate/validate protocol types and add drift CI.
- Add session summaries.
- Extract client-core fixtures and TypeScript reducer conformance.
- Implement request, durable SSE, summary SSE, and terminal SSE clients.
- Build the router, @lit/context providers, owned @lit-labs/signals adapter,
  deployment capabilities, and engine adapter.
- Add feature-gated desktop and PWA previews without changing desktop default.

**Exit:** both blank shells connect, bootstrap, resume, validate, and pass
projection fixtures without trouve-core.

### Phase 4 — visual system, primitives, and gallery

Extract the existing Slint theme into semantic CSS tokens before mapping them
to WebAwesome. Deliver typography, density, geometry, focus/motion/forced
colors, layouts, navigation, forms/overlays, typed events, router/focus,
virtualization/anchors, responsive navigation, safe areas, touch, and all
desktop/mobile/async/accessibility states.

Build side-by-side Slint and Lit gallery references and screenshot tests for
every migrated primitive.

**Exit:** screens assemble from qualified primitives and the visual system
matches the approved Slint baseline.

### Phase 5 — hard widgets

Implement streaming Markdown, CodeMirror, diff, and xterm in that order. Use
Slint fixtures. Test desktop engines and mobile browsers for visual parity,
disposal, touch, IME, scale, accessibility alternatives, and foreground resume.

**Exit:** each passes or has a selected fallback; no hard widget introduces an
unapproved visual or UX redesign.

### Phase 6 — read-only shell/inbox

Deliver desktop shell, mobile route navigation, workspace/session summaries,
archive/grouping, reconnect, read-only threads, and safe persisted state.

**Exit:** packaged desktop preview and installed PWA browse a realistic server
without mutations, and desktop shell layout/theme/density match the baseline.

### Phase 7 — chat/composer

Deliver read-only chat, then composer, completions, attachments, queue,
send/cancel, approvals/questions, and model/mode/permissions.

**Exit:** conformance, visual hierarchy, Unicode/IME, mobile keyboard, history
memory, anchoring, reconnect, duplicate-mutation, keyboard, and touch gates
pass.

### Phase 8 — inspections

Deliver todos, files, code, diff, session PR, and terminal.

**Exit:** a complete agent workflow works on desktop, visually matches the
current experience, and the PWA subset is coherent and honestly
capability-gated.

### Phase 9 — management/settings

Deliver PR dashboard, automations, all settings, About, desktop lifecycle, and
PWA install/update/session management.

**Exit:** every ledger item has functional and visual desktop/PWA evidence or a
documented gated limitation.

### Phase 10 — soak

Run Slint and web desktop side by side; dogfood installed PWA; replay logs;
compare projections; run full-screen screenshot comparisons, component-gallery
comparisons, interaction, AT, memory, crash, reconnect, offline,
background/foreground, update, local/remote, installer, and deployment tests.

Require explicit UX sign-off for intentional visual deviations. Close all
unexplained visual differences before promotion.

**Exit:** no severity-one parity, visual, security, growth, platform, or PWA
data-handling failure remains.

### Phase 11 — promote and qualify

ADR 0027 authorizes Wry as a staged default while the remaining engine,
functional, visual, packaging, and soak evidence is completed. Keep Slint
rollback, diagnostics, and rollback reproduction current throughout that
rollout. Publish PWA only when HTTPS/auth/
service-worker/update/mobile AT/background-resume and visual-identity gates
independently pass. They may ship in different releases.

### Phase 12 — retire Slint and evaluate mobile options

After a successful desktop default-release soak, remove Slint and unused
widgets/dependencies/assets, update ADR 0006, remove rollback, and validate in a
separate release.

After meaningful PWA usage, report adoption, unavailable capabilities,
notification/background limits, code/diff/terminal usability, memory, battery,
accessibility, distribution, and maintenance. Use that evidence for a new ADR
deciding whether to retain PWA-only mobile, augment it, or pursue a native/
embedded alternative.

## 13. Memory plan and Electron concern

Planning estimates, not measured promises:

| Workload | Slint estimate | Servo/Lit estimate |
| --- | ---: | ---: |
| Empty shell | 120–220 MiB | 220–400 MiB |
| Normal session | 180–350 MiB | 350–650 MiB |
| Large chat/diff | 300–550 MiB | 550–950 MiB |
| Heavy plus 3–5 terminals | 400–750 MiB | 700 MiB–1.2 GiB |

Expected increase is roughly 50–150 MiB idle, 100–250 MiB normally, and
300–500 MiB heavy. Lit versus Preact is probably low tens of MiB; DOM, editors,
terminals, caches, duplicate projections, and leaks dominate. Each substantial
xterm renderer can add roughly 10–30 MiB.

The design excludes Electron’s Node runtime, desktop service worker, SSR/
hydration, and bundled Chromium utility fleet. Unvirtualized DOM or leaks can
still cause multigigabyte growth.

### Desktop gates

- Idle at most 400 MiB.
- Typical at most 650 MiB.
- Heavy at most 1.0 GiB.
- 3–5-terminal stress at most 1.2 GiB.
- At most about +150 MiB idle and +300 MiB typical versus Slint.
- No staircase after 50 cycles.
- Final settled within greater of 50 MiB or 10% of initial, slope below
  2 MiB/cycle.
- Closing terminal/code/diff releases at least 80% of marginal allocation
  within 30 seconds after cleanup/diagnostic collection.

### PWA gates

- No full-history/background-thread retention while hidden.
- Bounded DOM/caches on representative mobile conversations.
- No staircase through foreground/background and route cycles.
- More aggressive terminal renderer/scrollback caps than desktop.
- Explicit static-asset service-worker cache budget.
- Cache size independent of messages/source/diff/terminal data.
- Correct summary/cursor recovery after low-memory reload.

### Workloads, metrics, and controls

Measure empty shell, populated inbox, typical session, 10,000-message chat,
10 MiB source, 20,000-line unified/split diff, one/five/closed terminals,
settings, and 50 cycles. Use release builds and one external-server snapshot.
Collect process-tree RSS/PSS, JS heap, engine/GPU breakdown, peaks/settled,
marginal terminal cost, and per-message/character growth. On mobile record
memory-pressure reloads, battery/thermal behavior, and recovery.

Virtualize large surfaces; unmount collapsed output; keep one canonical
representation; bound LRU caches; lazy-load widgets/workers/settings; cap
scrollback; keep only active/recent terminal renderers; dispose streams,
observers, editors, WebGL, object URLs, workers, timers, and overlays; avoid
whole-chat reparse; add lifecycle counters. Fix failures before adding screens.

## 14. Testing

### Unit and conformance

Cover schema/capability/version validation, snapshot-stream races, cursors,
duplicates/order, reconnect, reducer idempotency, summaries, chat/tools/queue/
approvals/questions, terminal offsets, Markdown sanitation, URLs/paths, UTF
conversion, contrast, redaction, bridge/capability validation, PWA cache
routing, and disposal.

### Components, visual regression, and end-to-end

Gallery states include default, empty, loading, disconnected, stale, denied,
failed, retrying, long, narrow, portrait, landscape, safe area, virtual
keyboard, large font, high contrast, forced colors, reduced motion, keyboard,
touch, and screen reader.

Maintain:

- Fixed-viewport reference screenshots for Slint and Lit.
- Per-theme component and full-screen visual snapshots.
- Tolerances/masks only for known font rasterization, caret, cursor, and native
  scrollbar differences.
- Manual review of significant diffs.
- An approved-deviation registry with rationale and owner.
- Checks that CSS/WebAwesome dependency updates do not silently change visual
  density, colors, or component dimensions.

Automate all session/thread/chat/approval/question/queue/attachment/completion/
file/diff/terminal/PR/automation/provider/model/MCP/integration/notification/
persistence/embedded/remote/crash/quit workflows.

PWA cases additionally cover installation, authenticated remote connection,
manifest scope, update/stale recovery, offline shell, proof of no cached API
content, foreground/background cursor recovery, mobile navigation, keyboard,
orientation, safe areas, touch selection, permission denial, logout/revocation,
and approved-state removal.

### Accessibility, performance, and security

Require axe without serious/critical findings; keyboard/focus/semantics; forced
colors; 200% text; reduced motion; IME; accessible diff and terminal. Test NVDA,
desktop VoiceOver, Orca, iOS PWA VoiceOver, and Android TalkBack. Servo passes
only when AT activates controls.

Set budgets for startup, SSE-to-paint, input, scroll, anchoring, code/diff open,
terminal throughput, bundle/package, recovery, and unexpected network fetches.
Retain prior Slint workloads and add memory cases.

Test hostile origins/hosts, loopback access, credentials, SSE cookies, path
traversal, schemes, malformed/oversized bridge data, CSP, subresources, upstream
confusion, secrets in DOM/logs/storage, PWA cache poisoning, worker scope,
logout/revocation, and update integrity.

## 15. CI, packaging, release, and licensing

### CI

Use Node 24. Add npm install, protocol drift, strict TypeScript, format/lint,
Vitest, production Vite, bundle budget, Playwright, axe, visual snapshots,
approved visual-deviation checks, dependency/license/SBOM, PWA manifest/
service-worker validation, mobile viewports, and proof API/SSE data is never
cached.

Add Rust bridge/gateway/SSE, system-webview smoke, CSP/offline asset, and
renderer recovery jobs. Run the Servo smoke and dependency checks explicitly
through the excluded nested workspace manifest and lockfile. Expensive memory,
AT, mobile-device, full visual, and engine suites may be
nightly/path-triggered, but promotion requires current passes.

### Desktop bundle

Produce a hashed static Vite bundle with no Node runtime, CDN, SSR, hydration,
or service worker. Build once, upload dist, feed all six release application
builds through `TROUVE_APP_UI_DIST`, and embed under a bundled-web-ui feature.
Runtime directories and Vite proxying are debug/qualification inputs only and
cannot replace embedded release assets. Offline Cargo tests must not require
Node. Adjacent packaged assets are a documented fallback.

### PWA deployment

Produce a separate deterministic PWA output from the same revision:

- Hashed assets, versioned manifest/icons, and allowlisted service worker.
- No API/SSE/user-data cache rules.
- Metadata tying frontend, supported protocol range, and source revision.
- Atomic rollout/rollback.
- Security headers and HTTPS checks.
- Automated post-deploy install/update/offline-shell smoke.

### Platforms, versions, and licenses

Validate Linux x86_64/ARM64 GNU, macOS x86_64/ARM64, and Windows x86_64/ARM64.
Define a PWA matrix for supported iOS/iPadOS Safari and Android Chrome-class
browsers plus responsive desktop browser smoke.

Root workspace version remains the product version. Frontend, lockfile, host,
manifest, PWA metadata, and artifacts use it. Run scripts/sync_versions.py,
keep protocol compatibility separate, choose SemVer at release time, and use
the release workflow for version/automation changes. The Servo qualification
harness is a Cargo-membership and lockfile exception, not a product-version
exception: its nested workspace version and internal Trouve pins remain
synchronized to the root under ADR 0024.

Before promotion:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo metadata --offline --no-deps
cargo metadata \
  --manifest-path crates/trouve-servo-embed-preview/Cargo.toml \
  --locked --no-deps
python3 scripts/sync_versions.py
```

Also run frontend, OpenAPI, packaging, engine, PWA, mobile, visual, and offline
suites.

Generate npm/Rust notices; include WebAwesome Free MIT notice; do not use Pro
without a decision; retain AboutSlint while Slint ships; record engine/webview
and PWA asset obligations; define a safe source-map policy.

## 16. Gains, losses, and cost

### Gains

- Mature code/diff/Markdown/terminal/form/overlay primitives.
- Faster iteration, browser profiling, galleries, visual tests, CSS themes,
  responsive design, and larger contributor pool.
- Better text selection, clipboard, IME, forms, and widget UX.
- One frontend for Servo, system webviews, browsers, and installable PWA.
- A protocol-only initial mobile client without immediately choosing a second
  native stack.
- A narrow auditable native boundary and less bespoke widget maintenance.

### Losses and obligations

- Loss of the Rust/Slint compile-time UI model.
- TypeScript/Node build tooling and npm supply-chain maintenance.
- Browser-engine/version, service-worker, and mobile-device testing.
- Higher baseline memory and potentially larger artifacts.
- More renderer lifecycle, CSP, origin, storage, navigation, and update work.
- Reliance on browser accessibility/PWA capabilities.
- Mobile background, notification, file, and terminal limitations.
- Temporary dual implementation.
- Versioned native bridge, generated protocol, and PWA deployment pipeline.
- Continuing visual-parity maintenance while both frontends coexist.
- Slint obligations until cleanup.

### Effort

This is a multi-quarter, multi-release program with dozens of changes.

| Work | Size |
| --- | --- |
| Baseline, visual capture, and ADRs | Small–medium |
| Servo/system-webview qualification | Large |
| Gateway/security/native bridge | Large |
| Protocol/projection foundations | Large |
| PWA hosting/security/mobile capabilities | Large |
| Theme extraction, design system, and parity gallery | Large |
| Markdown/code/diff/terminal | Extra large |
| Shell/inbox | Medium |
| Chat/composer/queue/questions | Extra large |
| Inspection workflow | Extra large |
| Settings/PR/automations | Extra large |
| Desktop/PWA visual and functional soak | Large |
| Promotion/rollback | Medium |
| Slint retirement | Medium |
| Later mobile-options evaluation | Small research phase; implementation TBD |

## 17. Go/no-go and definition of done

Proceed if a system-webview fallback is acceptable, long-term web/mobile reuse
is desired, the dual period can be funded, functional and visual parity plus
memory are enforced, projection work is first-class, and accessibility/platform
gates remain hard.

Do not proceed if Servo is mandatory despite evidence, WebAwesome is expected
to solve the hard widgets, Slint must be deleted before soak, the migration is
used as an implicit redesign, the PWA is expected to provide unsupported native
behavior, or parallel paths cannot be maintained.

Desktop qualification and Slint retirement require:

- All 21 desktop surfaces pass functional and visual parity.
- Themes, semantic colors, layout, density, hierarchy, and core UX remain
  recognizably the same as Slint.
- Every intentional visual deviation is documented and approved.
- The web UI uses only HTTP/SSE and the typed native bridge.
- Session summaries eliminate background full-history subscriptions.
- Rust/TypeScript projections conform.
- Accessibility passes on the selected engine.
- Memory budgets and 50-cycle leak gates pass.
- All six offline artifacts pass.
- Security and embedded/remote operation pass.
- Extended dual-front-end soak and visual sign-off complete.
- First-release rollback remains available.

Initial mobile delivery requires:

- Installability on the supported mobile matrix.
- Responsive/touch/keyboard/accessibility acceptance for the supported subset.
- The same Trouve visual identity, themes, semantic colors, typography, and
  component language, with layout changes only where mobile requires them.
- Reviewed HTTPS authentication, logout/revocation, manifest, worker scope,
  update, and rollback.
- No API, SSE, prompt, source, diff, terminal, or secret data in offline caches.
- Durable summary/cursor recovery after foregrounding.
- Explicit unavailable capabilities and useful alternatives where possible.
- Mobile memory/lifecycle gates.
- Deployment monitoring for availability/version mismatch without user content.

Slint removal occurs only in a later cleanup release. Native mobile,
embedded-mobile-webview, and other alternatives are evaluated later from PWA
evidence rather than chosen speculatively.

In short: proceed with Lit, explicitly use @lit/context and a contained
@lit-labs/signals adapter, preserve the existing visual and interaction
experience, build the gateway/projection foundations and hard widgets, qualify
Servo without depending on it, and deliver the first mobile client as a
capability-aware PWA before deciding whether another mobile stack is justified.
