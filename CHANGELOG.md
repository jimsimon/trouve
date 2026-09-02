# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Full-branch review on every head**: automatic, manual, and retried reviews
  now inspect the Git merge-base-to-head branch diff. A successfully published
  clean round completes the Check Run immediately while durable finding,
  dismissal, root-cause, rejection, external-thread, and carried-anchor history
  continues to inform later rounds.
- **Client/server compatibility**: protocol compatibility advances to 8.0.
  Upgrade the desktop, PWA, review dashboard, and `trouve-server` together. A
  derived compatibility-pending and exhausted states keep clean pre-8.0
  partial results neutral while the server performs a bounded full-branch
  migration review and surface when a manual retry is required.

### Removed

- **Incremental review coverage state**: review requests no longer select a
  scope, and jobs no longer expose review watermarks or full-coverage flags.
  New Check Runs no longer expose a separate full-review action, and new jobs
  no longer need a coverage-confirmation round. Existing pre-8.0 `full_review`
  actions remain accepted and request the pull request's current head;
  `@trouve-ai review full` remains an alias for the standard command.

## [4.7.0] - 2026-09-01

This release keeps pull requests and model reasoning connected to the sessions
that produced them, while improving recovery from missed events and transport
shutdown races.

### Added

- **Durable pull-request associations**: pull request URLs mentioned in chat
  are recorded against their session, normalized across repositories, and
  shown alongside session-created pull requests without granting mention-only
  entries mutation authority. Existing transcripts are recovered lazily and
  paged for large histories.
- **Continuous provider reasoning**: assistant thinking now retains
  provider-owned identity across interleaved tool calls, delayed deltas, and
  rebuilt thread snapshots instead of being split or attached to the wrong
  lifecycle.

### Changed

- **Client/server compatibility**: protocol compatibility advances to 7.29 for
  durable pull-request mention events and identity-aware reasoning lifecycles.
  Upgrade the desktop or PWA client and `trouve-server` together.
- **Session-governed private web access**: approved private-address fetches now
  follow the session's Ask, allow-list, or Yolo policy while retaining scheme,
  redirect, resolution, and connection-pinning safeguards.

### Fixed

- **Reliable review commands**: polling can recover newly missed resolve and
  unresolve commands exactly once without replaying historical commands or
  confusing webhook receipts with polling progress.
- **Clean Codex completion and process shutdown**: observed root completion
  remains authoritative when transport EOF races collaborator collection, and
  inert Linux zombies owned by another reaper no longer quarantine a completed
  app-server process tree.

## [4.6.0] - 2026-08-31

This release adds provider-native control over model options and live turns,
removes fixed engine turn ceilings, and improves usage and review workflows.

### Added

- **Live boundary steering**: follow-up instructions and image attachments can
  steer active direct-API and Claude Code turns. Guidance is queued through
  response and tool boundaries with durable ordering, bounded admission, and
  the same-turn transcript preserved.
- **Schema-driven model options**: New Session, New Thread, live chat, and
  Automations now render provider-declared choices, booleans, text, and exact
  numeric settings without provider-specific controls. Selections are
  validated, persisted, inherited, and rechecked before automated runs.
- **Detailed usage views**: the session usage panel separates provider,
  active-thread, and session totals into accessible keyboard-navigable tabs.

### Changed

- **Provider-governed turn admission**: ordinary desktop and spawned-agent
  turns no longer use fixed engine concurrency caps. Providers govern capacity,
  shared throttling cooldowns remain in effect, and review-job concurrency
  stays independently bounded. The retired `TROUVE_TURN_CONCURRENCY`,
  `TROUVE_BACKGROUND_TURN_CONCURRENCY`,
  `TROUVE_PROVIDER_TURN_CONCURRENCY`, and
  `TROUVE_PROVIDER_BACKGROUND_TURN_CONCURRENCY` settings are now ignored
  with a startup warning and should be removed from operator configuration.
- **Clearer model and persona configuration**: semantic persona selection is
  grouped separately from repository instructions, and model-specific option
  contracts remain stable across catalog refreshes and inherited defaults.
- **Client/server compatibility**: protocol compatibility advances to 7.27.
  Upgrade the desktop or PWA client and `trouve-server` together.

### Fixed

- **More reliable automated reviews**: review collection accepts large
  changesets within the existing byte and model-token bounds, decimal-minute
  timeout inputs no longer fail browser step validation, and opaque GitHub 422
  verdict rejections retry safely as comment reviews without losing blockers.
- **Stable session controls and usage**: new sessions retain the resolved
  repository default branch after asynchronous option loading, while usage
  refreshes preserve the last successful data and surface partial failures.

## [4.5.0] - 2026-08-30

This release makes large and long-lived workspaces easier to navigate, exposes
model-level usage, and grounds automated review decisions in evidence from the
revision being reviewed.

### Added

- **Organized workspace navigation**: workspace sessions can be grouped by
  repository, filtered by name, date, and branch, and collapsed with preferences
  that persist across reloads. Repository identity is cached and refreshed
  without repeatedly invoking Git.
- **Per-model usage visibility**: the workspace sidebar now shows provider
  subscription, API, and local-model usage with session- and thread-level
  breakdowns by model.
- **Explicit review commands**: maintainers can request a full-branch pass with
  `@trouve-ai review full` and resolve or reopen threadless findings with
  attributed, reason-bearing `resolve` and `unresolve` commands.

### Changed

- **Evidence-grounded review gates**: finding confidence and change causation
  are derived from mechanically verified anchors, execution paths, attempted
  refutations, and causal waypoints. Only findings verified as caused by the
  reviewed change block merging; severe pre-existing issues remain visible as
  non-gating observations.
- **Broader, convergent reviews**: arbitrary file- and changed-line count
  cutoffs no longer reject large reviews, while existing byte and model-token
  budgets still bound work. Carried blockers can be resolved against verified
  source at the current head even after their fixing commit leaves the
  incremental window.
- **Responsive thread reconciliation**: resolved and reopened GitHub review
  threads prioritize their pull request through a deduplicated, retry-bounded
  webhook dispatcher. Operators should subscribe the GitHub App to the
  `Pull request review thread` event; polling remains the fallback.
- **Higher-quality session titles**: local title generation uses a constrained
  two-to-five-word grammar, preserves both ends of long prompts, and upgrades
  legacy Q4 installations to the Qwen3 1.7B Q5 naming model.
- **Client/server compatibility**: protocol compatibility advances to 7.22.
  Upgrade the desktop or PWA client and `trouve-server` together.

### Fixed

- **Bounded review publication recovery**: definitively missing or superseded
  GitHub reviews now reach a safe terminal state instead of retrying forever;
  current rounds can be reposted only after confirmed absence, and cosmetic
  thread-collapse retries are abandoned after a bounded attempt window.
- **Stable workspace and CI behavior**: collapsed workspace session lists are
  honored, stale usage responses cannot repopulate the wrong scope, and
  benchmark confirmation compares fresh candidate and base measurements to
  avoid shared-runner contention false positives.

## [4.4.0] - 2026-08-27

This release surfaces agent-initiated Claude activity as durable turns and
makes automated review output and prompt sizing more useful across model fleets.

### Added

- **Agent-initiated Claude turns**: Claude Code monitors and scheduled
  wake-ups now appear as labeled background turns instead of blocking an unread
  output pipe or leaking stale events into the next interactive turn. Continuous
  routing, provider-reload recovery, and bounded process cleanup keep autonomous
  activity attached to the correct thread throughout the process lifetime.

### Changed

- **Focused pull-request review output**: GitHub review surfaces now publish
  only merge-blocking findings, distinguish new issues from carried-forward
  blockers, and render remediation context as readable, injection-contained
  prose. Advisory findings remain durable and visible in the trouve dashboard.
- **Model-aware review prompt budgets**: reviewer batches, coordinator history,
  and diff context now scale from the smallest configured model context window.
  The resolved basis is persisted per job so retries remain deterministic even
  when provider metadata changes or is temporarily unavailable.
- **Client/server compatibility**: protocol compatibility advances to 7.19 for
  the trusted background-turn marker. Upgrade the desktop or PWA client and
  `trouve-server` together.

## [4.3.0] - 2026-08-26

This release makes automated reviews converge on merge-blocking defects while
preserving advisory engineering debt and giving maintainers direct control over
finding dismissal.

### Added

- **Whole-change review context**: each review round can run a configurable
  Change analyst over the full branch diff, while the final editor receives the
  pull-request description as untrusted claimed intent and the independent
  analysis as observed implementation evidence.
- **Maintainer dismissal controls**: resolving a finding thread now dismisses
  the finding immediately, and findings without diff threads expose equivalent
  task-list controls in the lifecycle comment. Restoring either control reopens
  the finding and its linked root-cause themes.

### Changed

- **Blocking and advisory review gates**: only high-severity findings and
  sufficiently confident medium-severity findings block merging or publish to
  GitHub. Lower-severity findings remain durable and visible in trouve without
  holding the check red.
- **Full-branch confirmation before success**: a review reports success only
  when no blocking findings remain and the newest published round covers the
  entire branch. Clean incremental rounds remain pending until the existing
  full-coverage recheck confirms the result.
- **Broader review routing and lifecycle analysis**: user-facing changes are
  routed per file instead of by a batch's dominant content, and reviewers must
  trace writers of persisted state and verify re-execution assumptions across
  startup and migration paths.
- **Client/server compatibility**: protocol compatibility advances to 7.18.
  Upgrade the desktop or PWA client and `trouve-server` together.

### Fixed

- **Reliable review reconciliation**: fixed and dismissed findings no longer
  wait on GitHub thread bookkeeping before full-coverage confirmation, while
  lifecycle checkbox edits converge transactionally from durable state and
  survive reordered, replayed, or missed webhook deliveries.
- **Visible thread-cleanup backlog**: review statistics now report pending
  thread collapses and the oldest pending age, making a stalled auto-resolution
  worker visible from the dashboard.

## [4.2.0] - 2026-08-24

This release improves automated review throughput and restores semantic search
to Cursor-backed reviews.

### Changed

- **Faster parallel reviews**: planned semantic-router and reviewer batches now
  enter the shared scheduler together, while a separate short-lived lane bounds
  durable setup bursts without capping active model turns. The obsolete
  `TROUVE_CODE_REVIEW_TASK_CONCURRENCY` override has been removed; operators
  should use the global or provider turn limits and
  `TROUVE_CODE_REVIEW_JOB_CONCURRENCY` when narrowing review capacity.

### Fixed

- **Cursor semantic search**: Cursor ACP sessions again mount trouve's
  supplemental HTTP MCP bridge, allowing automated reviews to use semantic
  search within their tool budgets. Sessions reload when bridge credentials or
  MCP settings rotate, while Cursor's native tools remain read-only confined.

## [4.1.2] - 2026-08-23

### Changed

- **Clearer review outcomes**: successful automated reviews with open findings
  now use a warning-style “needs attention” status instead of looking like
  failed review runs, while unavailable open-finding counts remain explicitly
  marked as unknown.

### Fixed

- **Automated review tool budgets**: repeated ACP lifecycle updates for one
  logical Cursor tool call are charged only once, and the bounded reviewer
  allowance accommodates synchronized release manifests without prematurely
  terminating valid supply-chain reviews.

## [4.1.1] - 2026-08-23

### Fixed

- **Confined Cursor code reviews**: automated reviews can again use Cursor's
  vendor-native read and search tools under ACP Ask mode's read-only
  confinement, while external MCP servers remain withheld and backends without
  an enforceable confinement boundary still fail closed.

## [4.1.0] - 2026-08-23

This release adds faster ways to find and understand work, broadens attachment
and code-search workflows, and strengthens automated review safety and
reliability under long-running workloads.

### Added

- **Transcript search and richer activity**: each thread now has bounded,
  reload-safe chat search, while live status distinguishes model waits, tool
  work, and other agent phases. Pull request views retain recently closed
  work and show PR and session status together.
- **Attachment galleries and video handoff**: image attachments open in a
  gallery, and desktop video attachments can be handed to the system player
  through a bounded, cancellation-safe native cache.
- **Expanded code search**: search supports additional web, configuration,
  markup, and template grammars; callers can select code, docs, config, or all
  content per request; local Model2Vec layouts are supported; and
  `trouve-search clear orphans` safely removes stores for deleted repositories.
- **Visible release identity**: the desktop About surface and server/search
  version commands now report the shared workspace release version.

### Changed

- **Evidence-grounded reviews**: automated review routing and adjudication use
  stronger dependency, API, performance, and prior-rejection evidence.
  Outside-diff and previously unresolved findings remain visible, and
  incomplete coordinator decisions cannot be mistaken for a clean review.
- **Responsive long-running sessions**: shared and bounded search indexes,
  idle MCP cleanup, completed Codex thread cleanup, lower initial response
  latency, and runtime thread prioritization reduce retained memory and keep
  the desktop responsive under CPU contention.
- **Client/server compatibility**: protocol compatibility advances to 7.15.
  Upgrade the desktop or PWA client and `trouve-server` together.

### Fixed

- **Review retry and publication recovery**: final-editor and coordinator
  retries reuse successful reviewer work, publication failures retain the
  blocking verdict, and retry/cancellation recovery avoids stale or duplicate
  outcomes.
- **New-session lifecycle**: navigation safely dismisses setup without losing
  retryable state, creation retries are idempotent after uncertain responses,
  repository defaults stay synchronized, and feature checkouts no longer
  replace the repository's default base branch.
- **Model and title defaults**: inherited thinking budgets resolve correctly,
  generated titles recover from reasoning wrappers and reject truncated
  output, and the frontend protocol guard remains synchronized with the
  server.
- **Search cache recovery**: Hugging Face model downloads coordinate across
  processes, repair corrupt or undersized cached weights, bound lock waits,
  and never modify invalid local model directories.

### Security

- **Prompt-injection-resistant reviews**: automated reviews enforce an
  authoritative tool allowlist and reserved tool budgets, fail closed for
  unconfined vendor backends, and redact complete credential fragments from
  review context.

## [4.0.0] - 2026-08-20

This release unifies interactive modes and review profiles as reusable
personas, gives code review durable evidence and root-cause history, and makes
long-running agent and review workflows more reliable.

### Added

- **Durable review intelligence**: code reviews retain finding, root-cause,
  resolution, recurrence, regression, and prior-fix history across revisions;
  related symptoms are grouped while remaining individually traceable, and
  churn metrics and evidence are visible in the review interfaces.
- **Actionable review verdicts**: completed reviews approve pull requests with
  no confirmed findings or request changes when findings remain, while
  resolved and reopened threads trigger bounded, revision-safe rechecks.
- **Close-the-loop workflow**: the new repository skill drives a session pull
  request through CI, review feedback, approvals, and mergeability checks to a
  verified Ready to merge handoff.
- **Cumulative turn usage**: live token and cost counters now accumulate across
  all model requests in a turn, survive reconnects, and preserve the last
  completed measurement after cancellation or failure.

### Changed

- **Breaking persona API and configuration**: interactive modes and reviewer
  profiles now share one persona catalog across settings, sessions, threads,
  and code review. API clients must upgrade with the server to protocol 7.7
  and use the persona endpoints and types; custom persona configuration now
  lives under `personas/` or `.agents/personas/`.
- **Deterministic session defaults**: provider, model, thinking, permission,
  and persona defaults come from the authoritative static catalog instead of
  refresh timing. Cursor's documented models are available offline, while
  live discovery can still add account-specific choices.
- **Verified pull request association**: connector-created pull requests are
  associated with sessions only after durable repository, branch, and exact
  head-commit verification, with bounded recovery outside the turn lifecycle.
- **More resilient releases**: crate publication now orders first-party
  dependencies correctly and uses Cargo-backed, retry-safe visibility checks.

### Fixed

- **Steering and cancellation**: text-only steering no longer waits behind the
  session mutation lane, and cancelled steers still run normal backend cleanup
  for streams, collaborators, approvals, and partial output.
- **Concurrent persistence**: SQLite read-modify-write transactions now avoid
  stale snapshot upgrades, preventing intermittent database-locked failures
  during event logging and code-review scheduling without claiming the writer
  slot for idle polls.
- **Review publication recovery**: publication attempts, resolved-thread
  reconciliation, retries, and crash recovery preserve one authoritative
  verdict without duplicate or stale GitHub reviews.

## [3.8.0] - 2026-08-18

This release moves the shipping desktop application to the shared Lit/Wry
frontend, expands native agent collaboration and inspection workflows, and
makes code reviews more selective and resilient.

### Added

- **Shared desktop and PWA frontend**: the Lit application now powers the Wry
  desktop client and browser installation, with responsive session, chat,
  terminal, settings, automation, and pull request workflows behind the same
  protocol boundary.
- **Deeper agent workflows**: recursive subagents have visible, navigable
  transcripts; active Codex turns can be steered; and durable TODOs, turn
  metadata, checkpoint actions, hashline edits, and scoped external file reads
  give agents richer tools without bypassing the permission boundary.
- **Review intelligence and capacity controls**: review roles and agent modes
  share one configurable persona catalog, while confidence-aware publication,
  generated finding titles, cross-finding themes, and live parallel-review
  limits make automated reviews easier to tune and act on.
- **Reproducible search benchmarks**: the repository now includes comparison
  tooling and documented results for trouve search, grep, and ripgrep.

### Changed

- **Exact client/server compatibility**: generated clients now require an
  exact protocol-version match and reject incompatible servers during
  bootstrap. Upgrade desktop or PWA clients and `trouve-server` together.
- **One shipping desktop stack**: the former Slint frontend and reusable Slint
  widgets are retired; the typed desktop host owns native capabilities and a
  single embedded server while durable state continues through HTTP and SSE.
- **Scalable session history**: materialized, pageable thread views, bounded
  diffs, coalesced replay, and attention summaries keep large and concurrent
  sessions responsive without replaying or rendering their complete history.
- **More reliable incremental reviews**: reviews preserve unchanged findings
  across rebases, fall back safely when history cannot be matched, reduce task
  fan-out, skip automatic runs for draft pull requests, and reconcile
  publication and resolved-thread state durably.
- **Consistent session setup**: repository and global mode, model, and
  permission defaults now resolve consistently for new sessions and threads,
  with clearer Modes & Models settings and improved generated session names.

### Fixed

- **Agent routing and cancellation**: Codex child requests stay attached to
  the correct parent turn across startup, replacement, interruption, and
  transport teardown, while late prompts and transient activity remain
  visible in the frontend.
- **GitHub review authentication**: long-running reviews refresh installation
  credentials before publication and surface missing permissions as an
  actionable reauthentication request.
- **Large-session resource bounds**: ignored files no longer inflate
  checkpoints, pathological diffs are rejected before transfer, transcript
  paging preserves scroll position, and desktop notification and worker
  lifetimes remain bounded.
- **Review and daemon recovery**: rewritten review histories, empty summaries,
  generated-marker retries, daemon-directory startup, and immediate search
  fallback recover without losing durable state or hanging the workflow.

### Security

- **Confined side effects and process launches**: vendor mutations remain
  behind `ToolExecutor`, external reads are limited to registered roots, and
  trouve-owned child processes share one synchronized launch boundary to
  prevent cross-process sentinel and cleanup races.

## [3.7.0] - 2026-07-31

### Added

- **Configurable session-naming resources**: desktop settings now control
  CPU/GPU placement for the managed local naming model, coordinate resources
  with the coding sidecar, and validate GPU-only configurations before use.
- **Explicit review persona selection**: repository review policies now offer
  Manual, Additive, and Automatic persona modes, making fixed selections,
  always-included personas, and fully automatic routing distinct.

### Changed

- **Higher-quality session titles**: the managed title model, prompting,
  startup bounds, and output validation produce concise titles more reliably.
- **Responsive review dashboards**: cursor-based event resumption, coalesced
  refreshes, bounded live output, and SSE-driven updates prevent long-running
  reviews from accumulating unbounded browser rendering work.

### Fixed

- **Bounded code review tasks**: configurable reviewer and coordinator
  deadlines cancel stalled provider sessions and surface them as task
  failures, while proxy discovery and streamed output remain robust.

## [3.6.0] - 2026-07-30

### Added

- **Multiple interactive terminals**: sessions can open, rename, switch
  between, and close multiple terminal tabs, with scrollback search,
  selection and copy, bracketed paste, IME-friendly input, terminal mouse
  tracking, cursor modes, and visual bell support.
- **Complete review model controls**: the review dashboard now configures
  coordinator, semantic router, and per-persona models and thinking settings,
  including adaptive levels and model-specific token budgets. Repository
  policies can also be disabled without discarding their preconfigured
  settings.

### Changed

- **Consistent model metadata**: models.dev is now the canonical source for
  provider identity, model metadata, and option schemas across the desktop and
  review dashboard. Live provider and CLI discovery overlays account
  availability, while a refreshable cache and bundled snapshot preserve
  offline setup and selection.
- **Clearer agent activity**: Codex commentary is displayed in the thinking
  stream separately from final answers, with raw reasoning retained and
  duplicate summaries suppressed.
- **More recognizable inspection tabs**: inspection tabs now include icons for
  faster navigation.

## [3.5.0] - 2026-07-28

### Added

- **Catalog-backed model providers**: provider setup and model discovery now
  combine live account availability with a refreshable catalog and offline
  snapshot, including native Azure OpenAI, Amazon Bedrock, Vertex Gemini, and
  Anthropic-on-Vertex transports plus provider-specific fields and secrets.
- **Model-assisted session naming**: an optional managed local model can
  generate concise session and branch names before worktree creation, with
  configurable preload policies and deterministic fallback when the model is
  disabled, unavailable, or resource-constrained.
- **Automatic review persona routing**: code review can run a fixed Core set,
  the complete Thorough catalog, or an Auto baseline augmented by path signals
  and optional semantic triage. Routing choices, reasons, router output, model,
  and thinking level are durable and inspectable for each job.
- **Desktop workflow conveniences**: active sessions can prevent system sleep,
  text inputs have standard context menus, and the New Session screen no
  longer asks for an absolute workspace registration path.

### Changed

- **Responsive long-running interfaces**: chat history and review transcripts
  load incrementally, review details no longer wait on live model discovery,
  and desktop and web activity animations reduce work while hidden or idle.
- **Targeted review retries**: failed or cancelled reviewer persona batches can
  be retried without rerunning successful personas.
- **Lossless burst handling**: provider deltas are coalesced and event-log
  writes are batched without changing persisted ordering, while bounded
  per-turn routes keep one overloaded Codex or Cursor turn from stalling
  unrelated sessions.

### Fixed

- **Cancellation and queue recovery**: replacement Codex turns wait for
  predecessor interruption, stale routes cannot consume new events, and a
  prompt submitted during cancellation explicitly resumes dispatch instead of
  remaining stranded.
- **Clipboard pasting**: copied text takes precedence when a rich clipboard
  source also advertises image data.

## [3.4.2] - 2026-07-26

### Added

- **Review decision transparency**: completed review details now retain and
  display why the final editor rejected individual candidate findings, and
  the review activity view makes persona batches, coordinator attempts,
  metrics, prompts, and live output directly inspectable.

### Changed

- **Faster unattended reviews**: review mode now defaults to low thinking
  unless a reviewer persona explicitly requests another level, while
  documented provider and global concurrency controls make deployment
  throttling easier to tune.

### Fixed

- **Automated review orchestration and reporting**: batched reviewer tasks,
  JSON-repair attempts, final-editor selection, candidate accounting,
  statistics, progress summaries, and tool-free finalization now remain
  consistent across retries, failures, cancellation, and concurrent work.

## [3.4.1] - 2026-07-25

### Fixed

- **GitHub App review reconciliation**: polling now refreshes Checks permission
  and `check_run` webhook health after restarts and later configuration
  changes, while JWT signing initializes its cryptography provider so
  repository reconciliation no longer risks panicking before it begins.

## [3.4.0] - 2026-07-25

### Added

- **Expanded code review workflows**: review jobs now expose durable live
  progress, reviewer output, elapsed time, findings, and coverage; operators
  can filter, cancel, retry, and request either incremental or full-branch
  reviews from the redesigned dashboard.
- **GitHub review lifecycle integration**: reviews publish Check Run progress
  and pull request status comments, reconcile fixed findings with review
  threads, and support signed webhook actions for reruns and full reviews.
  GitHub Apps must grant `checks:write`; subscribe to `check_run` events to
  enable the GitHub action buttons.
- **Review remediation and statistics**: the desktop pull request view offers
  per-finding **Fix** and summary **Fix all** actions, while the dashboard
  reports repository, reviewer, model, queue, runtime, token, cache, and issue
  history.

### Changed

- **Review throughput and context reuse**: read-shared sessions,
  provider-aware prioritized capacity, adaptive backoff, coalesced progress,
  diff reuse, persona routing, batched context, and Anthropic prompt caching
  improve concurrent review efficiency.
- **Bounded event replay**: persisted SSE history is replayed in fixed-size
  pages, stale pull request dashboard snapshots are coalesced, and unchanged
  snapshots are no longer persisted, reducing startup memory and redundant UI
  work without deleting existing event history.

### Fixed

- **Review lifecycle reliability**: cancellation, retry, branch selection,
  concurrent detail loading, CLI polling updates, cache bounds, and credential
  handling now remain correct across overlapping review and dashboard work.
- **Pull request dashboard refreshes**: per-host publication locking prevents
  stale state from resurfacing when a GitHub host is removed and re-added.
- **Review dashboard builds**: Vite's client declarations keep stylesheet
  imports valid under TypeScript 7.

## [3.3.3] - 2026-07-24

### Fixed

- **Claude Code subscription sign-in**: authentication now strips terminal
  hyperlink control sequences, accepts either the browser callback URL or its
  displayed authorization code, and verifies the actual CLI session before
  reporting readiness. Desktop users can complete the code-based flow inline
  without changing the existing browser callback flows for other providers.

## [3.3.2] - 2026-07-24

### Changed

- **End-to-end release publishing**: the repository release workflow now
  prepares and validates the synchronized release, merges it through a checked
  pull request, and verifies the tag, GitHub release, assets, and downstream
  publishing jobs.

### Fixed

- **Checkpoints without a configured Git identity**: session checkpoints now
  use a dedicated internal identity, so creating or updating a session no
  longer depends on global `user.name` and `user.email` Git settings.

## [3.3.1] - 2026-07-24

### Fixed

- **Reliable pull request comment review requests**: the review service now
  polls trusted issue comments when webhook delivery is unavailable, persists
  comment claims so commands cannot trigger duplicate reviews, and revalidates
  recurring GitHub reads through a bounded ETag cache.

## [3.3.0] - 2026-07-24

### Added

- **Pull request comment review requests**: repository owners, members, and
  collaborators can comment `@trouve-ai review` on a pull request to request
  an on-demand review in either manual or automatic mode, including while the
  pull request is a draft. Comment triggers are persisted and deduplicated
  across webhook retries and service restarts.

### Fixed

- **Remote Codex and Claude Code sign-in**: Codex now uses device
  authentication, while Claude Code runs its subscription login in a PTY and
  accepts a validated browser callback pasted through the review dashboard.
  Both vendor CLI login flows now work when the browser and trouve server do
  not share the same localhost.

## [3.2.0] - 2026-07-23

### Added

- **Managed subscription CLIs in code review**: the hosted review dashboard
  can install, update, cancel, retry, and remove trouve-managed vendor CLIs,
  so Claude, Codex, and Cursor subscription sign-in no longer depends on a
  binary already being available on `PATH`. Direct Codex provider login also
  resolves the managed binary.
- **Review model and thinking defaults**: the dashboard now exposes the
  system-wide review model and thinking defaults and persists model and
  thinking defaults for every reviewer persona, including built-ins. Durable
  jobs snapshot persona thinking settings so queued and webhook-triggered
  reviews use the configuration selected when they were created.

### Changed

- **Faster, hardened release builds**: app, server, and search artifacts now
  compile together per target; server images reuse the static musl artifacts;
  release caches are shared with trusted main builds; and platform npm
  packages publish concurrently. Release workflows pin reviewed actions,
  avoid persisted checkout credentials, and restrict cache writes to main.

## [3.1.0] - 2026-07-23

### Added

- **Downloadable application and server binaries**: GitHub releases now ship
  prebuilt `trouve` desktop application and `trouve-server` archives for
  supported Linux, macOS, and Windows targets alongside the existing
  `trouve-search` assets and SHA-256 checksums.

### Changed

- **Review dashboard setup**: provider settings now show credential state,
  guide subscription CLI sign-in, and offer presets for API providers.
  Repository policies are easier to manage with search, mode filters,
  pagination, collapsible details, and clearer per-reviewer overrides.
- **Pull request merge readiness**: session PR icons use GitHub's detailed
  merge state and semantic colors, distinguishing merge-ready pull requests
  from open pull requests that are blocked, behind, or still being evaluated.
- **Review deployment access**: the single-user review dashboard and `/v1`
  API no longer use a shared bearer token. Keep them on a trusted private
  network or VPN, or add authentication and TLS at the reverse proxy before
  exposing them; GitHub webhooks and internal provider bridges retain their
  dedicated authentication.
- **Container publishing**: AMD64 and ARM64 images are built on matching
  native runners before being joined under the existing multi-platform
  version and commit tags.

### Fixed

- **Queued prompt previews**: multiline queued prompts now render a clipped
  one-line teaser without bleeding into the surrounding chat, while the full
  prompt remains available when editing the queue entry.

### Security

- **Review dashboard rendering**: server-provided labels, identifiers,
  messages, and repository data are escaped before rendering, and external
  review links are limited to safe HTTP(S) URLs.

## [3.0.0] - 2026-07-22

This is the first release of the trouve AI coding harness and its GitHub
App-backed code review service, deployable on your own infrastructure. It
grows trouve from a code-search tool into a protocol-first agent platform with
a native desktop client, while keeping `trouve-search` available as the same
standalone CLI, library, MCP server, and agent plugin. The major version also
establishes one lockstep version for every first-party artifact and includes
the breaking removal of remote git URL indexing.

### Added

- **trouve AI coding harness**: a Rust agent engine, HTTP + SSE server, shared
  client layer, and native Slint desktop app. Sessions own isolated git
  worktrees; threads share the session worktree while retaining durable
  per-thread conversations, queues, modes, models, and todo state. Per-turn
  hidden-ref checkpoints provide session undo and redo. The desktop app embeds
  the server in-process but continues to use the authenticated loopback
  protocol boundary.
- **Agent and model integrations**: run Claude Code, Codex, and Cursor through
  their native protocols with managed CLI installs, login flows, live model
  discovery, and persistent or resumable vendor sessions. Direct API
  providers and OpenAI-compatible endpoints are supported alongside managed
  local `llama.cpp` models, with mid-thread model changes, configurable
  thinking, context and fast-mode controls, and subscription-health views for
  Claude, Codex, Cursor, and Kimi.
- **Coding tools and delegation**: agents can read and edit files, apply
  patches, inspect diffs, search code and transcripts, glob, fetch web pages,
  run foreground or background shell jobs, maintain todos, and recover from
  compacted context. Parent agents can delegate work to child threads or
  fully isolated child sessions, then collect their output. Side effects pass
  through Ask, Allow list, or Yolo permission gates; local execution is not
  OS-sandboxed, and Yolo deliberately skips approval prompts.
- **Native desktop workflow**: streaming chat and reasoning, Markdown tables
  and syntax highlighting, file and diff inspection, an interactive PTY
  terminal, file and image attachments, `@` file mentions, `/skill`
  completion, editable queued prompts, desktop notifications, workspace
  reordering, and restored window, session, and scroll state. Modes, model
  defaults, permission policies, providers, integrations, MCP servers, and
  vendor CLIs are configurable in Settings.
- **Automations**: schedule scoped agent prompts, start them on demand, pause
  or resume them, choose their model and permission mode, and create common
  workflows from built-in templates. Runs create normal durable sessions and
  record their outcomes.
- **GitHub pull request workspace**: OAuth sign-in for GitHub.com and
  self-hosted GitHub Enterprise instances, an account-wide PR dashboard with
  actionable review/check/merge groups, project filters, session association,
  and PR actions. Shared GraphQL-backed snapshots refresh every 30 seconds and
  feed the dashboard, session status, and per-session PR panel without
  repeatedly fetching unchanged details.
- **GitHub App code review service**: a separately authenticated GitHub App can
  review selected repositories in manual or automatic mode. Signed webhooks
  provide a fast path while durable polling reconciles missed events; every
  job is deduplicated, runs read-only in an isolated session at the exact PR
  head, and is cancelled or marked stale when the revision or effective
  policy changes.
- **Focused, verified reviews**: built-in reviewer profiles cover
  correctness, security, reliability, performance, concurrency, API
  compatibility, data integrity, testing, maintainability, dependencies,
  accessibility, and operations. Repositories can select reviewers and
  override their prompts or models, while reusable custom profiles add
  project-specific expertise. A final editor pass verifies findings against
  the repository and commentable diff lines before publishing inline comments
  and a summary under the App's bot identity.
- **Code review operations**: a standalone web dashboard configures the
  GitHub App, providers, models, reviewers, and repository policies and shows
  durable job history and GitHub rate limits. Docker Compose deployment,
  backup and upgrade guidance, and multi-architecture `trouve-server` and
  `trouve-review-ui` images are included.
- **Shared search daemon**: on Unix, concurrent `trouve-search` MCP sessions
  with matching configuration now share one background embedding model and
  in-memory index cache. The daemon starts on demand, exits after 15 idle
  minutes, and falls back to in-process serving if it cannot be reached;
  `TROUVE_DAEMON=0` opts out. Windows keeps the existing in-process behavior.
- **Offline and reconnect handling**: the server reports internet reachability
  and filters remote models while offline, leaving local models available.
  The desktop app gates unavailable actions, explains connectivity state,
  reconnects and resynchronizes automatically, and announces recovery.
- **Reusable Slint widgets**: independently usable `trouve-slint-*` crates
  provide code, diff, streaming Markdown, and terminal views without exposing
  trouve protocol types in their public APIs.
- **Global default permissions**: Settings → Modes & Models gains a "Global
  default permissions" picker (Ask / Allow list / Yolo) that applies to new
  threads whose mode doesn't set its own permission mode. Per-mode default
  permissions now default to "Global default" and can still be overridden
  per mode in the mode editor; existing modes that already set an explicit
  permission default keep that behavior, while new modes (or modes without an
  explicit default) inherit the global setting. Server side: a mode's
  `default_permission_mode` is now optional (absent = global default), the
  global value persists in `config.toml` and is settable via
  `PUT /v1/config/default-permission-mode`, and `GET /v1/providers` reports
  it alongside the default model.

### Changed

- **Cargo workspace and release tags**: the repository is now a monorepo for
  `trouve-search`, the harness, and reusable UI crates. All Cargo crates, Node
  packages, plugins, internal package pins, lockfile records, containers, and
  release artifacts now share root `[workspace.package].version`; repository
  releases use `vX.Y.Z` tags. The workspace uses Rust edition 2024 and requires
  Rust 1.92 to build.
- **GitHub authentication**: account PR discovery now uses OAuth exclusively
  and unifies data from GitHub.com and configured Enterprise instances. The
  review service deliberately uses separate GitHub App installation tokens,
  so its repository access and rate limits remain isolated from desktop OAuth.
- **Search model parity with Semble v0.5**: the default embedding model is now
  `minishlab/potion-code-16M-v2`. Model-keyed stores and snapshots keep
  existing v1 cache data isolated.

### Fixed

- **Concurrent event ingestion**: event-log appends are batched through a
  dedicated writer thread, preserving commit-before-publish and cursor order
  while preventing high-volume streaming from overflowing vendor-agent event
  routes across concurrent sessions.
- **Agent turn reliability**: fixed Codex approval responses, approvals that
  arrive before their tool card, waiter cleanup after app-server exits, Git
  writes in mutable modes, completed reasoning summaries, and subscription
  limit reporting. Tool activity and reasoning now remain visible without
  duplicate or retired replay events.
- **Desktop state and input handling**: stabilized session switching and chat
  scroll restoration, kept prompt drafts, queues, and todos scoped to their
  thread, preserved queued editor text during stream updates, fixed deferred
  quit and opener cleanup, and made session activity indicators consistent.
- **Wayland image paste**: clipboard images copied by Spectacle and similar
  tools are accepted when they are exposed through Wayland's data-control
  protocol.
- **Corrupt search caches**: malformed per-file entries and structurally
  inconsistent mmap snapshots are rejected instead of reaching query/patch
  code. Incomplete Hugging Face model caches are invalidated and downloaded
  once more, while local model directories remain untouched.
- **Hybrid search edge cases**: zero-weight candidates are removed at
  `alpha = 0` or `alpha = 1`, matching current Semble, and an empty filter
  selector now returns no dense results instead of panicking.
- **Screen artifacts in the desktop app**: the app now prefers Slint's Skia
  renderer over the default FemtoVG renderer, whose glyph atlas corrupts on
  some Linux drivers — flashing garbage across the window while typing or
  whenever a repaint hits (e.g. a desktop notification appearing). If Skia
  can't initialize, the app falls back to the previous renderer, and an
  explicit `SLINT_BACKEND` still overrides the choice.

### Removed

- **Remote git URL support**: trouve no longer clones repositories on the
  user's behalf. The CLI, MCP server, and library (`TrouveIndex::from_git`,
  the `clone_cache` module) reject or omit git URLs; clone the repository
  yourself and pass the local directory path. The `<cache>/clones` directory,
  its eviction logic, and the `TROUVE_CLONE_TTL` / `TROUVE_CLONE_TIMEOUT`
  (and deprecated `SEMBLE_CLONE_TIMEOUT`) environment variables are gone.
  Local indexing is unaffected.

## [2.0.0] - 2026-07-05

Major bump: the crate, CLI binary, and npm packages are renamed, which breaks
existing installs and MCP configurations pointing at the `trouve` binary or
the `trouve-plugin` npm package. See [INSTALL.md](INSTALL.md) to migrate.

### Changed

- **Rename for the `@trouve-ai` npm org**: the crates.io crate and CLI binary
  are now `trouve-search` (the bare `trouve` name is reserved for future
  products). **`@trouve-ai/search-core`** ships the native binary and MCP
  launcher, with per-platform binaries installed via `@trouve-ai/search-*`
  optional dependencies — `npm i -g @trouve-ai/search-core` needs no separate
  install step, and MCP configs default to `npx -y @trouve-ai/search-core`.
  **`@trouve-ai/search-plugin`** replaces `trouve-plugin` and now carries the
  whole plugin surface: the OpenCode/Kilo native tools plus the Claude Code
  and Codex bundles (MCP config, workflow skill, sub-agent, session hook)
  formerly in `plugins/trouve` — one directory (`npm/search-plugin`), two
  install channels (npm registry and git marketplace).
- `npm/` is now an npm workspace (`search-core` + `search-plugin`, one shared
  lockfile), and `@trouve-ai/search-core` is plain ESM JavaScript with a type
  declaration, so it runs under Node 18+ (`npx`) and Bun alike.
- Dependency updates: hf-hub 0.5, tokenizers 0.23, safetensors 0.8.

### Added

- [NAME.md](NAME.md): where the name *trouve* comes from and how to pronounce
  it.

## [1.1.0] - 2026-07-04

### Added

- **Persistent clone cache**: remote git URLs are cloned once into
  `<cache>/clones` and refreshed with a cheap `git fetch` at most once per
  freshness window (`TROUVE_CLONE_TTL` seconds, default 300) instead of
  re-cloned per query. Clones are lock-protected against concurrent trouve
  processes, evicted after a week idle, and removed by `trouve clear index`.
  A stale clone is served (with a warning) when the remote is unreachable.
- The MCP server now re-validates git URLs after the same cooldown as local
  paths — a revalidation is now a TTL-gated fetch plus an incremental
  rebuild, not a re-clone.
- **`.trouveignore` files**: exclude files from indexing without git-ignoring
  them, replacing upstream's `.sembleignore` (same syntax, same per-directory
  inheritance; `.trouveignore` wins where patterns conflict).
- **`.semble/` directories** are now skipped during walks, alongside
  `.trouve/`, matching upstream's default ignore list.
- **Native OpenCode custom tools**: a standalone custom-tool file
  (`src/agents/opencode-tool.ts`, copied to
  `~/.config/opencode/tools/trouve.ts`) exposes `trouve_search` and
  `trouve_find_related` as native OpenCode tools. An alternative to an MCP
  entry: it avoids the MCP transport, needs no JSON config edits, and
  defaults `repo` to the session worktree.
- **[INSTALL.md](INSTALL.md)**: step-by-step manual setup for every
  integration route — plugins, the OpenCode native tool file, and MCP
  server entries (config file, key, and snippet for 14 agents), plus
  optional `trouve-search` sub-agent files.
- **23 new tree-sitter grammars** (~50 languages total): CMake, D, Dart, Elm,
  ERB/EJS embedded templates, Erlang, Fortran, Gleam, GraphQL, Groovy,
  HCL/Terraform, Julia, Make, Nix, Objective-C, Perl, PowerShell, Protocol
  Buffers, R, Solidity, SQL, Svelte, and XML (incl. DTD). Files in these
  languages now get syntax-aware chunk boundaries instead of the line-based
  fallback.
- **Unified agent plugin** (`plugins/trouve`): one package serving four
  harnesses. As the npm package `trouve-plugin` it exposes `trouve_search`
  and `trouve_find_related` as native tools in OpenCode and Kilo Code,
  backed by a single persistent `trouve` server process per session
  (preserving the in-process index cache, including for remote git URLs).
  The same directory carries the Claude Code plugin bundle (MCP server +
  `trouve-search` sub-agent + workflow skill, installed via the marketplace
  catalog at `.claude-plugin/marketplace.json`) and the Codex plugin bundle
  (MCP server + skill, via `.agents/plugins/marketplace.json`). All
  manifests pass their official validators and ship at the crate version.
- **Session-start index warming**: the OpenCode/Kilo plugin builds or
  refreshes the project index in the background when it loads and
  (throttled) on every `session.idle` event, so the first search never pays
  the index build and later searches absorb the agent's own edits
  (`"warm": false` disables). The Claude Code bundle ships an equivalent
  `SessionStart` hook running `trouve stats` in the background.
- **Version sync tooling**: `scripts/sync_versions.py` keeps every published
  artifact (npm plugin packages, Claude Code and Codex plugin manifests) on
  the exact crate version from `Cargo.toml`, and lint CI fails when anything
  drifts (`--check`). The release workflow refuses tags that don't match the
  crate version and publishes all npm plugin packages at the crate version
  alongside the crates.io publish (skipped until `NPM_TOKEN` is configured).
- **Model-backed end-to-end tests**: `TROUVE_E2E=1 cargo test -- --ignored`
  (already documented in the README and run by CI) now actually runs a small
  e2e suite against the real default model — cold index, semantic and
  identifier queries, `find_related`, and a warm rebuild that recomputes
  nothing.

### Changed

- MSRV raised from 1.87 to 1.89 (std file locking for the clone cache).

### Fixed

- **`.trouveignore` now works in git repositories**: ignore rules were only
  consulted by the directory walker (non-git roots); git repositories build
  their manifest from `git ls-files`/`git status` and skipped them entirely.
  Rules are now applied on top of the git listing — before any hashing I/O —
  for tracked and untracked files alike.
- **MCP protocol violations**: tool failures were returned with
  `isError: false` (clients treated them as successful output) and a
  malformed request with an id but no method got no response at all, hanging
  the client; failures now set `isError: true` and malformed requests get a
  `-32600 Invalid Request` error. `top_k: 0` is rejected as the schema
  advertises, and `max_snippet_lines: null` now means the documented default
  instead of being an undocumented full-chunk escape hatch.
- **Git manifest correctness**: tracked symlinks were keyed by the blob OID
  of the link *target path* while indexing read the target file's content,
  serving stale chunks whenever the target changed; symlinks are now skipped
  like the walker already did. Merge-conflicted (unmerged) paths are treated
  as dirty and indexed from the working tree instead of an arbitrary
  conflict stage.
- **Snapshot compatibility checks**: snapshots now record the store format
  version and chunk length, and the incremental patch path rejects
  mismatches instead of silently splicing rows chunked under different rules
  (snapshot format bumped to v4; old snapshots are rebuilt automatically).
  `save()` also verifies a pre-existing snapshot file's embedded hash and
  rewrites partial or foreign files instead of trusting them forever.
- **Model loading robustness**: corrupt or mismatched model artifacts
  (out-of-range mapping entries, undersized embedding tables, token-id gaps)
  are rejected with a clear error at load time instead of panicking
  mid-index, and a tokenizer failure on one text now embeds it as the zero
  vector with a one-time warning instead of aborting the whole build.
- **Accurate cache statistics**: `files_from_store` no longer counts rows
  spliced zero-copy from a previous snapshot (reported separately as
  `files_from_snapshot`), and `trouve stats` now emits the documented
  `cache_hit_rate`.

### Removed

- **`trouve install` / `trouve uninstall`**: the interactive installer is
  gone. Every integration it configured is now documented as a manual (and
  easily reversible) step in [INSTALL.md](INSTALL.md): plugins for
  OpenCode/Kilo/Claude Code/Codex, the OpenCode native tool file, one MCP
  config entry per agent, and optional sub-agent files. Editing user
  configs programmatically was the installer's main risk (JSONC files had
  to be skipped, TOML edits could clobber user changes); a documented
  one-line config entry per agent is simpler and safer.

### Deprecated

- **`.sembleignore` files**: still honoured, but log a warning and will be
  removed in a future release. Rename to `.trouveignore`.
- **`SEMBLE_CACHE_LOCATION`, `SEMBLE_MODEL_NAME`, `SEMBLE_CLONE_TIMEOUT`**:
  now honoured as fallbacks when the `TROUVE_*` equivalent is unset, but log
  a warning and will be removed in a future release. Use
  `TROUVE_CACHE_LOCATION`, `TROUVE_MODEL_NAME`, and `TROUVE_CLONE_TIMEOUT`.

## [1.0.0] - 2026-07-03

First stable release. trouve is a Rust port of
[MinishLab/semble](https://github.com/MinishLab/semble) — fast, accurate code
search for agents — rebuilt around an incremental, branch- and worktree-aware
index. Retrieval behaviour matches upstream (mean NDCG@10 within 0.0002 on the
upstream annotated benchmark, identical chunk boundaries and BM25 scores).

### Added

- **Faithful retrieval port**: tree-sitter chunking for ~28 languages with
  line-based fallback, `potion-code-16M` model2vec embeddings, BM25 (Lucene
  variant) with identifier tokenization and path enrichment, RRF hybrid
  fusion, and upstream's code-tuned reranking heuristics.
- **Content-addressed chunk store**: per-file artifacts (chunks, embedding
  rows, BM25 token lists) keyed by content hash — git blob OIDs for clean
  files (no file reads), BLAKE3 for dirty/untracked files. Editing one file
  re-embeds one file.
- **Branch- and worktree-aware caching**: one store per repository (keyed by
  the git common directory), shared across all branches and worktrees.
- **Memory-mapped snapshots**: warm queries load embeddings and BM25 postings
  zero-copy; incremental builds patch the newest snapshot so cost is
  proportional to the edit, not the repository.
- **Bounded cache**: snapshot pruning (4 newest kept) plus a daily
  mark-and-sweep GC that deletes store entries unreferenced by any kept
  snapshot, with a one-hour grace period for concurrent builds.
- **In-house model2vec engine**: memory-mapped embedding table, byte-level
  WordPiece fast path with a sharded word memo for ASCII text, bit-identical
  output to `model2vec-rs` per text. Embeddings are batch-independent (no
  `[PAD]` pooling).
- **Fully parallel pipeline**: hashing, parsing, chunking, tokenizing,
  embedding, and BM25 construction run across all cores via rayon, with flat
  token arenas to minimise allocation traffic.
- **CLI**: `search`, `find-related`, `stats`, `savings`, `clear`, `install`,
  `uninstall`; bare `trouve` starts an MCP stdio server with `search` and
  `find_related` tools and an in-process LRU index cache.
- **Agent installer**: MCP server config, instruction blocks, and dedicated
  sub-agents across 14 coding agents (Claude Code, Cursor, Codex, Gemini,
  OpenCode, and more).
- **Test and parity suite**: offline unit/integration tests against a
  deterministic toy model, embedding parity tests against `model2vec-rs`,
  property tests, and a parity harness verifying chunk boundaries, tokens,
  BM25 scores, and search results against the upstream Python implementation.
- **Release automation**: binaries for Linux (glibc and static musl, x64 and
  arm64), macOS (x64 and arm64), and Windows (x64 and arm64) built from
  semantic tags, with SHA-256 checksums.

### Performance

Measured on kubernetes/kubernetes (30,563 tracked files) vs upstream Python
semble ([BENCHMARKS.md](BENCHMARKS.md)):

- Cold index + query: 3.3 s vs ~3 min (54x)
- Incremental reindex (1 file touched): 0.86 s vs ~3 min (212x)
- Warm query: 0.55 s vs 7.2 s (13x)

[4.7.0]: https://github.com/jimsimon/trouve/compare/v4.6.0...v4.7.0
[4.6.0]: https://github.com/jimsimon/trouve/compare/v4.5.0...v4.6.0
[4.5.0]: https://github.com/jimsimon/trouve/compare/v4.4.0...v4.5.0
[4.4.0]: https://github.com/jimsimon/trouve/compare/v4.3.0...v4.4.0
[4.3.0]: https://github.com/jimsimon/trouve/compare/v4.2.0...v4.3.0
[4.2.0]: https://github.com/jimsimon/trouve/compare/v4.1.2...v4.2.0
[4.1.2]: https://github.com/jimsimon/trouve/compare/v4.1.1...v4.1.2
[4.1.1]: https://github.com/jimsimon/trouve/compare/v4.1.0...v4.1.1
[4.1.0]: https://github.com/jimsimon/trouve/compare/v4.0.0...v4.1.0
[4.0.0]: https://github.com/jimsimon/trouve/compare/v3.8.0...v4.0.0
[3.8.0]: https://github.com/jimsimon/trouve/compare/v3.7.0...v3.8.0
[3.7.0]: https://github.com/jimsimon/trouve/compare/v3.6.0...v3.7.0
[3.6.0]: https://github.com/jimsimon/trouve/compare/v3.5.0...v3.6.0
[3.5.0]: https://github.com/jimsimon/trouve/compare/v3.4.2...v3.5.0
[3.4.2]: https://github.com/jimsimon/trouve/compare/v3.4.1...v3.4.2
[3.4.1]: https://github.com/jimsimon/trouve/compare/v3.4.0...v3.4.1
[3.4.0]: https://github.com/jimsimon/trouve/compare/v3.3.3...v3.4.0
[3.3.3]: https://github.com/jimsimon/trouve/compare/v3.3.2...v3.3.3
[3.3.2]: https://github.com/jimsimon/trouve/compare/v3.3.1...v3.3.2
[3.3.1]: https://github.com/jimsimon/trouve/compare/v3.3.0...v3.3.1
[3.3.0]: https://github.com/jimsimon/trouve/compare/v3.2.0...v3.3.0
[3.2.0]: https://github.com/jimsimon/trouve/compare/v3.1.0...v3.2.0
[3.1.0]: https://github.com/jimsimon/trouve/releases/tag/v3.1.0
[3.0.0]: https://github.com/jimsimon/trouve/releases/tag/v3.0.0
[2.0.0]: https://github.com/jimsimon/trouve/releases/tag/v2.0.0
[1.1.0]: https://github.com/jimsimon/trouve/releases/tag/v1.1.0
[1.0.0]: https://github.com/jimsimon/trouve/releases/tag/v1.0.0
