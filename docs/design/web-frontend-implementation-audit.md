# Web frontend implementation audit

**Audit date:** 2026-08-04

**Plan:** [Web frontend migration implementation plan](web-frontend-migration-plan.md)

**Surface ledger:** [Web frontend parity and qualification ledger](web-frontend-parity-ledger.md)

**Source audit:** [Rust/Slint to TypeScript frontend source-parity audit](web-frontend-source-parity-audit.md)

## Verdict

The repository-side functional port described by phases 3–9 is implemented.
All 21 surfaces have a Lit implementation or an explicit capability gate, all
134 Slint `AppWindow` callbacks have a source-checked Lit disposition, and the
desktop and PWA artifacts share the same protocol-only application. The audit
closed previously missing repository work for scoped contexts, dedicated
content workers, real-browser accessibility and visual regression, bundle
budgets, dependency notices/SBOMs, and explicit Servo nested-workspace CI.
The subsequent source-by-source pass accounted for all 50 retained Rust/Slint
frontend files and closed state-lifecycle, replay, timeout, process-cleanup,
terminal, streaming, and oversized-document virtualization gaps that a
screen/callback inventory alone could not expose.

The migration project as a whole is **not promotion-complete**. Phases 0, 2,
10, 11, and 12 deliberately require evidence that cannot be manufactured from
one Linux development host: paired Slint/Lit reviews, assistive-technology and
device matrices, all release targets, signed packages, production PWA
authentication/HTTPS/deployment, measured memory/performance workloads,
failure/renderer recovery, soak, promotion, and post-release retirement. Slint
therefore remains the default and rollback frontend.

One repository-visible decision also remains unapproved: the implementation
uses self-hosted WebAwesome Free for selected ordinary/brand controls while
retaining many semantic native controls to reproduce Slint's compact density.
That is safe and functional, but it is broader native-control use than the
plan's default WebAwesome wording. It must either receive an approved-deviation
entry or be migrated control-by-control after engine and visual evidence. This
audit does not silently approve it.

## Audit method

The audit treated the saved plan, not the earlier progress prose, as the source
of requirements. Each plan section was traced to implementation, tests, CI,
packaging, or a named external evidence gate. Claims were checked against:

- the retained Slint source, theme definitions, and callback surface;
- the Lit application, generated protocol/host clients, state projection,
  native bridge, Wry host, Servo embedder, and PWA artifact;
- Vitest, cross-language fixtures, Playwright, axe, and visual baselines;
- Cargo/npm lockfiles, bundle outputs, license inventories, and SBOM jobs; and
- the parity ledger's 21 surfaces, enhancement register, deviation register,
  and promotion evidence matrix.

“Implemented” below means executable repository work exists. “Automated gate”
means ordinary or qualification CI rejects drift. “External qualification”
means implementation exists but promotion still requires reviewable hardware,
service, deployment, or longitudinal evidence.

## Plan-section traceability

| Plan section | Repository implementation | Automated evidence | Status / remaining gate |
| --- | --- | --- | --- |
| Outcome | Shared Lit frontend, protocol-only desktop/PWA clients, Wry fallback, pinned direct Servo embedder, PWA-first mobile strategy, retained Slint rollback. | Build-mode checks and host/protocol boundary tests. | Implemented; promotion remains gated. |
| 1. Current baseline | Slint and Rust surfaces remain present while the Lit port is feature-gated. | Callback manifest extracts all 134 Slint callbacks. | Functional inventory closed; measured baseline evidence remains. |
| 2. Visual continuity | Slint-derived generated palettes, semantic tokens, compact geometry, responsive shell, five-theme gallery, fixed browser snapshots, forced-colors and 200% text cases. | Theme generator drift test, visual-contract tests, Playwright snapshots, axe. | Lit baselines exist; paired Slint/Lit approval matrix remains external. |
| 3. Target architecture | `web/app-ui`, `trouve-desktop-host`, Wry preview, isolated Servo preview, HTTP/SSE state path, typed native boundary, separate desktop/PWA builds. | Cargo boundary tests, protocol/host schema tests, build-mode verifier. | Implemented. |
| 4. ADRs/design | ADRs 0023–0026 record web stack/host, Servo isolation, the exact nightly pin, and asset-source policy; plan and ledger remain living records. | Version/ADR-linked source invariants. | Implemented. |
| 5. Engine decision | Chrome-free in-process Servo `WebView` pinned to revision `35672cc3d4beb768489f5218e73bee7aff0ddb01`; Wry system-webview fallback; both require one explicit server and cannot open the default DB. | Root metadata gate plus path/nightly nested-workspace `cargo test`; Wry feature check. | Embedders implemented; six-platform, AT-action, recovery, packaging, visual, memory, and performance qualification remains external. |
| 6. Lit versus Preact | Lit 3 is the only application component runtime; no Preact/React application layer, SSR, hydration, or Node renderer. | Dependency inventory and bundle inspection. | Implemented. |
| 7. State model | Normalized application store, stable service/store/capability contexts, route-scoped workspace/session/thread contexts, terminal scope, contained signals adapter, cursor controllers, local UI state, typed preference persistence. | State, context boundary, subscription-count, ingress, resume, and preference tests. | Implemented. |
| 7. Workers | One lazy bounded content worker handles Markdown/sanitization, source highlighting, diff preparation, composer/palette fuzzy matching; pure direct fallbacks preserve behavior; idle termination releases it. | Worker lifecycle/fallback tests, worker TypeScript build, emitted-worker bundle budget, browser worker smoke through the gallery. | Implemented. |
| 8. Protocol/projections | Generated OpenAPI clients, precompiled CSP-safe runtime validators, atomic session summaries/cursor resume, bounded folded thread snapshots with exact cursor handoff and lazy backward pages, server reconnect replay, normalized PR/session/thread/todo projections, ephemeral PTY stream. | Schema drift, validator sync/CSP tests, Rust/TS shared event/snapshot projection fixtures, snapshot-generation and ingress race/recovery tests, browser PR projection test. | Implemented for current protocol; live remote-host/security qualification remains. |
| 9. Components/tooling | CodeMirror/MergeView, xterm, sanitized Markdown, owned virtualizer, self-hosted WebAwesome Free dependency, Vite, strict TypeScript, Vitest, Playwright, axe. | Unit, browser, visual, bundle, and license gates. | Specialized widgets implemented. Native-versus-WebAwesome ordinary-control decision requires approval before promotion. |
| 10. Twenty-one surfaces | All 21 rows in the parity ledger have a functional implementation; surface 21 remains capability/promotion gated. | Callback manifest plus surface/model/component tests. | Functional port closed; qualification evidence remains per row. |
| 11. Desktop bridge | Versioned bridge v8: preferences, system fonts, directory/file pickers, clipboard image, safe file/HTTPS open, notifications, attention, sleep inhibition, lifecycle/window/close/quit. Hardened loopback asset/API/SSE gateway carries no durable side channel. | Rust host/gateway/OpenAPI tests and generated host validators. | Implemented; OS/security/recovery/packaging matrix remains. |
| 11. PWA security | Separate installable artifact, manifest/icons/version metadata, allowlisted static service worker, no API/SSE/user-data caching, capability adapters, wake lock, pull-to-refresh, update and install controllers. | Cache-policy, manifest/build-mode, PWA build, browser responsive tests. | Artifact implemented; public deployment is blocked on reviewed auth, HTTPS, origins, CSRF/revocation, headers, rollout, and real devices. |
| 12. Phases 3–9 | Foundations, visual primitives, hard widgets, shell, chat/composer, inspections, management, and settings are ported. | 21-surface ledger and frontend/Rust test suites. | Repository implementation complete; evidence qualification is separate. |
| 12. Phases 0–2, 10–12 | Baseline and engine harness infrastructure exists; rollback is retained. | Qualification workflow scaffolding. | External evidence, soak, promotion, retirement, and post-PWA mobile evaluation remain by design. |
| 13. Memory plan | Bounded normalized projections, capped thread views, virtualized history/tree/list paths, bounded output/attachments/diffs/files, lazy routes/widgets/worker, idle worker termination, explicit disposal, no Electron runtime. | Unit bounds/disposal tests and JS/CSS bundle budgets. | Controls implemented; workload measurements on release hardware remain. |
| 14. Unit/conformance | Protocol mapping, reducers, races, idempotency, security policies, projections, widgets, preferences, PWA caching, and callback parity have deterministic tests. | Vitest and Cargo suites. | Implemented. |
| 14. Browser/visual/a11y | Desktop/mobile Chromium visual snapshots for all five themes and hard widgets; full PR dashboard snapshot; Chromium/Firefox/WebKit projects; axe serious/critical gate; keyboard, selection, forced colors, reduced motion, 200% text. | Playwright in CI. | Automated baseline implemented; manual AT/IME/touch/device and paired Slint reviews remain. |
| 14. Performance/security | Bundle budgets, CSP-safe generated validators, safe URL/file policies, bounded inputs/outputs, host CSRF/origin boundary, no API cache. | Unit/Rust gates and bundle checker. | Source gates implemented; measured latency/memory, penetration scenarios, and production deployment review remain. |
| 15. CI | Node 24 install, generated-schema drift, source format/policy lint, strict TypeScript, Vitest, desktop/PWA production builds, budgets, Playwright/axe/snapshots, Wry check, Servo metadata plus path/nightly nested tests. | `lint.yml` and `web-frontend-qualification.yml`. | Implemented. Expensive platform/AT/device suites remain promotion jobs outside this host. |
| 15. Desktop bundle | Hashed local static assets, no CDN/Node/SSR/hydration/service worker, release-only embedding, and one shared source selector for packaged assets, runtime `TROUVE_APP_UI_DIST` snapshots, or loopback `TROUVE_APP_UI_DEV_URL` Vite proxying in Wry and Servo. Native and `/v1` routes retain precedence in every mode. | Desktop build-mode verifier, desktop-host source/gateway tests, Wry and Servo feature checks, and Vite desktop-mode checks. | Implemented; unbundled sources remain debug/qualification-only and signed six-target artifacts remain release qualification. |
| 15. PWA deployment | Deterministic PWA artifact and metadata exist. | PWA build/cache tests. | Hosting/auth/HTTPS/atomic rollout/post-deploy smoke require deployment infrastructure and security approval. |
| 15. Versions/licenses | Root version synchronization includes first-party Cargo/Node/Servo artifacts; generated npm and Rust dependency inventories; WebAwesome Free/Slint policy recorded; CycloneDX npm and Rust SBOMs uploaded by CI. | Version sync and notice/license allowlist checks. | Implemented for repository artifacts; final packaged-notice inspection remains promotion evidence. |
| 16. Gains/losses/cost | Architectural consequences are reflected in the implementation and rollback strategy. | Not an executable requirement. | Recorded. |
| 17. Go/no-go | Slint remains default; neither desktop engine nor public PWA is promoted; rollback stays intact. | Feature gates and conservative evidence ledger. | Correctly blocked until the definition-of-done evidence is current. |

## Defects and gaps closed by this audit

### Rust/Slint source-level follow-up

The exhaustive [source-parity audit](web-frontend-source-parity-audit.md)
compares all retained native frontend, host, client-view-model, Servo, and
generic-widget sources with their TypeScript/JavaScript counterparts. Its
inventory and the exact 21-event Rust/TypeScript reducer mapping are executable
tests. The follow-up closed replay coalescing, live settings/connectivity and
automation reactions, title generation timeout/fallback, client-local unread
state, running/queued tail attention, process reaping, large code/diff
virtualization, old-history batching, sleep retry, and deep terminal/streaming
behavior. The final pass also closed the last lossy platform translation:
protocol 2.4 now carries exact durable completion/failure/approval/question
notification edges, optional compact failure/question detail, and repeated
attention requests without background per-thread streams. Remaining engine
translations are recorded there rather than hidden as implicit differences.

### Scoped context consumption

The route scopes were originally declared and provided but no real component
consumed them. The shell now provides stable workspace/session/thread scope at
the route boundary, and terminal, MCP, PR, inspection, new-thread, todo, and
terminal-view components consume the appropriate context with explicit
property fallbacks for isolated tests/gallery rendering. This removes repeated
ID plumbing without turning context into application state.

### Dedicated content processing

The service worker was the only worker. The application now has one owned,
lazy content worker for every CPU-heavy category required by the plan. The
audit also caught a runtime-only failure: Vite selected a DOM-dependent
character-reference decoder inside the worker. Vite now aliases the package's
DOM-free entry, so the worker remains alive in a real browser instead of
silently falling back on every Markdown request.

### Browser accessibility and visual regression

There was no Playwright, axe, or screenshot infrastructure. The new suite
exercises desktop/mobile Chromium, desktop Firefox, desktop WebKit, and mobile
WebKit projects; records all five Lit themes, hard widgets, full gallery states,
forced colors/large text, and the PR dashboard; checks keyboard focus and
selectable text; and fails on serious/critical axe findings. Its first run
found keyboard-inaccessible CodeMirror scroll regions and forced-color token
leakage. Expanding the route baselines also found a 2.26:1 primary-action
contrast failure in Modes & Models. All three product defects were fixed
rather than excluded from axe.

### Shared pull-request state

The browser suite replays a durable `github.pull_requests_updated` event across
the snapshot cursor boundary and verifies that the same normalized projection
drives the session badge, session PR pane, and account dashboard. This covers
the cold-start regression that previously left the dashboard empty or made the
integration state disagree between surfaces. A second browser case forces an
unconfigured GitHub account and verifies that the session call to action lands
on an Integrations screen reporting the same unconfigured state.

### Chat rendering and turn lifecycle

The focused chat parity review found several behaviors that were present only
superficially. Native `details` elements hid activity and tool bodies without
unmounting them; tool cards had no raw/formatted switch or execution metadata;
user prompts were plain text instead of Markdown; processing state floated
outside the active Agent card; and real folded event order could place a
terminal status before its prompt or leave an empty completed row. The renderer
now builds the same prompt/Agent/activity hierarchy as Slint, conditionally
mounts every disclosure body, supports raw/formatted assistant and tool copy,
formats inline diffs/todos/file targets/duration/exit state, and keeps
starting/thinking/tool/cancelling/compacting messages in the keyed virtual
stream.

The review also found several state races. An accepted message or cancellation
could complete over HTTP before its durable event reached the browser, making
the controls briefly offer the wrong action. More seriously, opening the same
thread twice advanced `ThreadIngress`' generation while reusing callbacks that
captured the old generation, causing all later events to be discarded. Turn
controls now model both request and acknowledgement gaps explicitly
(`Sending…`, `Queueing…`, `Starting…`, queue, `Stopping…`, and `Send next`),
and a same-thread reopen reconnects from the retained cursor. The active
thread stream also reconnects on foreground/online recovery. Every async chat
mutation is scoped to its originating thread generation so an old response
cannot mutate or focus a newly selected thread.

Regression tests exercise the true event-fold order and the start → queue →
cancel → send-after-cancel sequence. A 400+ unit browser fixture additionally
proves keyed bounded default DOM, complete on-demand accessible history,
collapsed heavyweight unmounting, tail-only live-log announcements, invalid
bookmark recovery, reduced-motion streaming output, and stable ResizeObserver
correction without an observer feedback loop.

### Bundle and lazy-loading gates

Management routes are dynamically imported, worker output is separately
emitted, and deterministic budgets cap the app entry, worker, largest chunk,
total JavaScript, and CSS for both desktop and PWA builds. Budget failure is a
build failure, not an informational report.

### Supply-chain and Servo CI

The npm and Rust lock graphs now generate reviewed license inventories. Exact
license-expression allowlists reject unreviewed changes, WebAwesome Pro is
prohibited, Slint attribution is retained, and CI emits CycloneDX SBOMs. The
excluded Servo manifest/lock is checked explicitly in ordinary CI and compiled
and tested in a path-triggered/weekly qualification workflow with documented
Linux dependencies.

## Validation performed for this audit

The closure pass was validated from a clean npm lockfile install and the root
Cargo workspace on 2026-08-04:

- strict application and worker TypeScript checks passed;
- source formatting hygiene and CSP/runtime/dependency policy lint passed;
- generated protocol schemas and CSP-safe validators regenerated cleanly and
  their drift checks passed;
- all 93 Vitest files passed (487 tests), including the 134-callback parity
  manifest, the 50-file native source inventory, the exact 21-event
  Rust/TypeScript reducer comparison, and the cross-language pull-request/session
  projection fixtures;
- desktop and PWA production builds passed their budgets: desktop entry
  821,904 bytes, PWA entry 812,381 bytes, content worker 335,033 bytes, and
  total JavaScript 2,791,353/2,781,830 bytes respectively, within the
  3,000,000-byte budget in each artifact; the artifact-boundary
  verifier also confirms that neither production distribution contains source
  maps and that desktop contains no PWA service worker or release metadata;
- 20 Chromium visual baselines exist, including General, Modes & Models, and
  Automations full-screen references; the combined desktop/mobile Chromium
  run passed all 20 applicable browser tests with four project-inapplicable
  cases skipped. The desktop and mobile terminal gallery references were
  visually inspected and updated for the native-matching 100×28 initial grid;
- axe found no serious or critical findings in the exercised gallery and
  shared pull-request dashboard states;
- npm and Rust notice checks passed; deterministic CycloneDX 1.6 SBOMs contain
  260 locked npm components and 1,020 Cargo components;
- root `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo doc --no-deps` with warnings denied, and `cargo test --workspace`
  passed; network/model tests remained ignored under the repository's explicit
  offline-safe policy;
- the packaged Wry preview check, desktop-host tests, root/nested metadata,
  version/theme generators, and their Python unit tests passed.

The local host cannot complete the configured WebKit browser projects because
Playwright's WebKit fallback requires Ubuntu libraries unavailable on this
EndeavourOS machine. The nested Servo compilation likewise reaches ANGLE but
cannot locate `libclang.so` on this host. CI now installs the supported Ubuntu
Playwright and Servo dependency sets and runs those checks there; neither local
environment limitation is recorded as a promotion pass.

## Remaining external evidence, not hidden implementation work

| Gate | Why it cannot be completed by this repository-only audit | Required closure evidence |
| --- | --- | --- |
| Paired visual parity | Lit screenshots alone cannot approve equivalence to live Slint on every state/platform. | Deterministic paired captures, diffs, reviewer/date, approved deviations. |
| Servo and Wry promotion | One Linux launch does not prove AT actions, all display backends, lifecycle, recovery, packaging, or six targets. | Current engine matrix with raw artifacts and reviewer. |
| Manual accessibility | axe cannot operate NVDA, VoiceOver, Orca, TalkBack, or validate every reading/activation path. | Supported AT/OS/browser/device matrix. |
| IME/touch/device behavior | Emulated viewports do not reproduce native keyboards, touch selection, permissions, safe areas, backgrounding, or memory pressure. | Physical supported-device runs. |
| Performance/memory | Source bounds and bundle size are not startup, SSE-to-paint, throughput, leak, or tail-latency measurements. | Repeated release-build workloads on defined hardware with budgets. |
| Production PWA security | No public origin, identity provider, TLS termination, revocation policy, or deployment authority is in scope. | Approved threat model/config, authenticated HTTPS deployment, headers/origin/CSRF/revocation tests, rollout/rollback smoke. |
| Six-target packaging | The current host cannot build, sign, install, and inspect every Linux/macOS/Windows architecture. | Release artifacts and install/smoke evidence for the supported matrix. |
| Dual-frontend soak | Soak is longitudinal release evidence, not a code-generation task. | Current functional/visual/a11y/security/performance/recovery soak results. |
| Promotion/retirement | Changing the default and deleting Slint are product/release decisions gated on all evidence above. | Explicit go decision, proven rollback, successful default-release soak. |

## Promotion-safe conclusion

No unimplemented relevant behavior is known after the screen, callback, and
50-file source closure passes. Repository automation now covers the concrete
gaps found by these audits and rejects an unaudited retained frontend source.
The correct next state is therefore not to delete Slint or claim the entire
migration done; it is to execute and attach the external evidence matrix,
resolve the ordinary-control deviation, and promote desktop and PWA paths only
when their independent gates pass.
