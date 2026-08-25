//! Wire types for the trouve harness protocol.
//!
//! This crate is the single source of truth for everything that crosses the
//! client/server boundary: request/response bodies, the event envelope, and
//! the OpenAPI schema derived from them. It contains **no logic** — see
//! `AGENTS.md` invariant 5.

pub mod events;
pub mod requests;

pub use events::*;
pub use requests::*;

/// Protocol version, independent of crate versions. Bump the minor for
/// additive changes and the major for breaking ones; the OpenAPI snapshot
/// test in `trouve-server` pins the serialized schema to this value.
// 0.2: added modes/diff/files inspection endpoints, GitHub PR endpoints,
// and the session.pr_opened event (all additive).
// 0.3: added provider configuration endpoints, session rename/archive
// (PATCH + session.updated), thread mode/model updates (PATCH +
// thread.updated), workspace branch listing, and context compaction
// events (all additive).
// 0.5: added the interactive question flow — question.requested /
// question.resolved events and POST /v1/questions (additive).
// 0.7: queued prompts — thread.queue_updated event, /v1/threads/{id}/queue
// endpoints, and the `queued` flag on TurnAccepted (all additive).
// 0.8: integrated terminal — POST /v1/sessions/{id}/terminal plus
// /v1/terminals/{id} input/resize/kill/output endpoints (all additive).
// 0.9: install lifecycle — byte progress on CliInstallStatus, cancel
// (DELETE …/install, DELETE …/download) and uninstall (DELETE /v1/clis/{id})
// endpoints, local enable toggle (PUT /v1/local/enabled + LocalStatus
// fields), and POST /v1/local/server/restart (all additive).
// 0.10: prompt attachments — SendMessageRequest.attachments (base64
// uploads), Attachment metadata on user.message events and QueuedPrompt,
// and GET /v1/attachments/{id} serving the stored bytes (all additive).
// 0.11: local model search — GET /v1/local/search?q= returns HuggingFace
// GGUF repos with per-file hardware-fit guidance (additive).
// 0.12: automations — scheduled prompts (CRUD under /v1/automations, run-now
// endpoint, automation.fired server event); each run creates a session and
// sends the prompt (all additive).
// 0.13: GitHub OAuth sign-in — GithubIntegration gains oauth_available and
// new token sources ("oauth", "gh-cli"); POST /v1/providers/github/login
// starts the device flow (all additive).
// 0.14: session activity — Session.active flag and the session.activity
// server event for live "processing a prompt" indicators (all additive).
// 0.15: automation templates — GET /v1/automations/templates returns
// pre-canned automations for common development tasks (additive).
// 0.16: GitHub Enterprise — GithubIntegration.hosts (per-host auth state),
// per-host auth state, POST/DELETE /v1/integrations/github/hosts for
// self-hosted instances, and provider-login ids "github:<host>" (additive).
// 0.17: turn cancellation — POST /v1/threads/{id}/cancel interrupts the
// running turn, and the turn.cancelled event reports it (additive).
// 0.18: per-automation permission_mode; omitted requests default to Ask,
// while Yolo enables explicit unattended execution for that automation.
// 0.19: connectivity — the server.connectivity_changed event and
// ServerInfo.online report internet reachability; while offline
// GET /v1/models lists only models that run without internet (additive).
// 0.20: global default permission mode — AgentPersona.default_permission_mode
// is now optional (absent = global default), GET /v1/providers reports
// default_permission_mode, and PUT /v1/config/default-permission-mode sets it.
// 0.21: global and per-mode default thinking levels — additive fields on
// AgentPersona, UpsertPersonaRequest, ProvidersResponse, and SetDefaultModelRequest.
// 0.22: PR dashboard — PrInfo gains review and comment metadata;
// workspace.pull_requests_updated persists each workspace snapshot, and
// POST /v1/workspaces/{id}/prs/refresh triggers a refresh without returning
// UI state directly.
// 0.23: CreateSessionRequest.fetch_latest chooses whether a session starts
// from the selected local ref or its freshly fetched upstream (additive;
// omitted requests default to fetching).
// 0.24: ProviderInfo.category and KnownProvider.category classify model
// sources as subscription, API, or local independently from authentication
// (additive).
// 0.25: thread-owned todo snapshots — Thread.todos provides initial state
// and thread.todos_updated replaces it on the event stream (additive).
// 0.26: PrInfo gains an optional `mergeable` flag — additive; drives the
// dashboard's merge-conflict pill and its needs-attention grouping.
// 1.0: GitHub becomes OAuth-only and the PR dashboard becomes an
// account-centric, multi-instance feed (breaking route/event replacement),
// and DELETE /v1/workspaces/{id} closes a workspace without deleting its
// sessions while workspace.closed records the state change.
// 1.1: GitHub App-backed code review configuration, repositories, durable
// jobs, code_review.updated events, PR head SHAs, and separate session
// checkout refs (all additive).
// 1.2: reusable built-in/custom reviewer profiles, per-repository reviewer
// selection, and profile snapshots on durable review jobs (all additive).
// 1.3: per-repository reviewer model overrides and inherit/append/replace
// prompt policies (additive).
// 1.4: PrInfo gains GitHub's optional `merge_state_status` so clients can
// distinguish PRs that are ready to merge from open-but-blocked PRs.
// 1.5: reviewer personas gain an optional default thinking level, and
// built-in reviewer model/thinking defaults can be customized (additive).
// 1.6: POST /v1/providers/{id}/login/callback forwards a failed browser
// callback URL to an interactive vendor CLI login (additive).
// 1.7: durable code-review task/output streams, progress and elapsed timing,
// cancellation/retry/full-review actions, incremental review metadata,
// published finding attribution, GitHub Check Run state, filtered history,
// review statistics, and first-party desktop review actions (all additive).
// 1.8: review tasks/persona/model statistics expose provider-capacity wait,
// model/tool elapsed time, token/cache usage, tool-call count, and
// not-applicable persona batches (all additive).
// 1.9: code-review job details expose durable final-editor rejection reasons
// for reviewer candidates that were not selected (additive).
// 1.10: data-driven provider configuration fields, safe endpoint/header/query
// templates, named write-only secrets, and native transport kinds (additive).
// 1.11: code-review job details can omit large retained task content, which
// is available on a new job-scoped task endpoint for lazy loading, and expose
// the snapshot's event cursor so clients can skip redundant history (additive).
// 1.12: failed code-review personas can be retried independently while
// retaining successful reviewer task outputs (additive).
// 1.13: session-naming settings persist the title-model load behavior;
// title-model status/install endpoints and POST /v1/session-title provide
// synchronous model-assisted naming with a deterministic fallback, while
// settings.git_worktrees_updated carries lifecycle snapshots (additive).
// 1.14: cancelling a missing title-model installation returns Not Found,
// matching the managed CLI installation lifecycle (additive).
// 1.15: session-naming settings responses include the corresponding server
// event cursor so clients can order snapshots against SSE replay (additive).
// 1.16: code-review repositories and jobs expose Core/Auto/Thorough persona
// routing, semantic-routing and include/exclude controls, durable per-batch
// routing decisions and their job-scoped event, and router tasks (additive).
// 1.17: code-review repository/job snapshots expose semantic-router model and
// thinking settings; enabled policies require an explicit review model so
// unattended review never relies on the engine's built-in model (additive).
// 1.19: code-review repositories and jobs expose coordinator thinking,
// repository reviewer overrides can select a thinking setting, and canonical
// settings accept fixed token budgets advertised by older models (additive).
// 1.20: session-naming settings expose a persisted session-title compute
// resource policy spanning adaptive, mixed GPU/CPU, GPU-only, and CPU-only
// placement (additive).
// 1.21: the code-review dashboard response includes the server event cursor
// for its snapshot so clients can resume SSE without replaying retained
// code-review update history (additive).
// 1.22: persisted automated code-review total, reviewer, and final-editor
// deadlines; GET/PUT /v1/config/code-review; and the
// settings.code_review_updated replacement-snapshot event (additive).
// 1.23: automated code-review timeout schemas advertise their existing
// positive-seconds constraint (additive).
// 2.0: persona routing becomes persona selection with Manual/Additive/
// Automatic policies; Automatic replaces Thorough's run-everything semantics
// with fully routed selection (breaking).
// 2.1: code-review execution settings expose a persisted, live-updatable
// maximum number of concurrently running review jobs (additive).
// 2.2: code-review concurrency requests remain positive and unbounded on the
// wire, while values above 32 are normalized to the response maximum of 32.
// 2.3: folded thread-view snapshots expose their event cursor so clients can
// open long chats without replaying retained history from cursor zero.
// 2.4: thread-view snapshots expose bounded folded-item pages and backward
// pagination metadata so clients never render the complete transcript eagerly.
// 2.5: oversized session diffs return a stable machine-readable error code.
// 2.6: code-review tasks expose their durable current/last lifecycle stage,
// model start and last-progress timestamps, and incremental task-progress
// events so in-flight and timed-out metrics remain observable (additive).
// 2.7: task-progress snapshots always carry the nullable model-start field so
// clients can clear a previous turn's live timing anchor (additive).
// 2.8: code-review findings expose their durable GitHub inline-publication
// outcome (additive).
// 3.0: Dynamic persona selection delegates solely to the semantic router;
// Automatic always enables it, while Additive retains only its baseline and
// configured inclusions before optional semantic additions (breaking).
// 3.1: transactionally derived session summaries, snapshot endpoint,
// session.summary_updated durable server events, and session.recovered
// restart reconciliation (additive).
// 3.2: transactionally derived session.notification edges preserve the native
// background completion/failure/approval/question notification category and
// optional compact detail without per-thread background streams (additive).
// 3.3: folded tool-call items expose server-measured execution duration so
// clients do not depend on incomplete provider result metadata (additive).
// 3.4: folded thread snapshots retain context-compaction boundaries and
// their running/completed/failed lifecycle as top-level transcript items
// (additive).
// 3.5: automations persist an optional thinking level and apply it to every
// fresh thread they create (additive).
// 3.6: turn usage exposes the most recent request's authoritative context
// size and a durable live replacement event so clients do not infer context
// utilization from aggregate or provider-specific billing counters
// (additive).
// 3.7: queued-prompt edits can retain/remove existing stored attachments
// and append new attachment uploads without re-uploading unchanged files
// (additive).
// 3.8: GET /v1/server-projection bootstraps account PR snapshots, durable
// session-to-PR associations, and session-naming settings at a server cursor
// so clients no longer replay the complete server event log on startup
// (additive).
// 3.9: failed provider-owned context compactions emit an explicit terminal
// lifecycle edge so clients can clear compaction state immediately (additive).
// 3.10: POST /v1/queue/{id}/dispatch atomically prioritizes one queued prompt,
// interrupts an active turn, and resumes with that selected prompt (additive).
// 3.11: turn.started and folded thread snapshots expose the effective
// thinking level selected for each turn (additive).
// 3.12: completed folded turns retain their checkpoint id, and checkpoint-
// targeted restore/fork endpoints make turn-boundary actions explicit
// (additive).
// 3.13: session-naming settings can opt new sessions into title-derived
// branch names; compact short-id branch names are otherwise the default
// (additive).
// 3.14: assistant.thinking_completed preserves provider-owned thinking-item
// boundaries even when no ordinary output event immediately follows
// (additive).
// 3.15: selected session pull requests expose lazy full-page collaboration
// detail and typed actions for conversation, reviews, metadata, state,
// merging, merge queues, auto-merge, and native PR stacks (additive).
// 3.16: PR collaboration actions gain bot review requests, pending-review
// management, review dismissal, and per-file viewed state (additive).
// 3.17: selected PR files expose lazy, bounded before/after diff content so
// large pull requests never require downloading one aggregate patch.
// 3.18: session diff metadata and selected-file patches can be loaded
// independently, so large worktrees no longer cross the protocol as one diff.
// 3.19: selected PR detail can be requested by tab section, and exposes the
// immutable base SHA so cached file lists can load content without another
// changed-files query (additive).
// 3.20: account PR refreshes accept an optional force flag so automatic
// clients can share a server-side freshness window without weakening the
// explicit user refresh action (additive).
// 3.21: steerable turns advertise their capability, POST
// /v1/threads/{id}/steer adds input to an active vendor turn, and the durable
// turn.steered event/folded item preserves that input in the turn rail
// (additive).
// 3.22: tool.completed optionally carries monotonic executor-only duration,
// allowing clients to distinguish actual tool work from event-log queueing,
// persistence, scheduling, and post-processing latency (additive).
// 3.23: historical tool calls can defer their complete arguments/results to
// a lazy detail endpoint while thread snapshots retain bounded presentation
// data (additive).
// 3.24: folded thread history includes durable TODO lifecycle entries so
// clients can render started, completed, cancelled, and skipped TODO rail
// nodes while retaining the latest TODO snapshot (additive).
// 3.25: folded turn state distinguishes a started turn waiting for scheduler
// capacity from one actively running its provider (additive).
// 3.26: parent turns expose durable, linked subagent transcript nodes with an
// optional originating tool-call id (additive).
// 3.27: threads expose an optional durable navigation title and creation can
// seed it without a follow-up mutation (additive).
// 3.28: compact durable per-thread status snapshots and replacement events
// keep every conversation tab's activity/attention outcome live (additive).
// 3.29: GET /v1/threads/{id}/subagents exposes every durable child transcript
// independently of paginated parent chat history (additive).
// 3.30: ThreadStatus exposes optional latest-turn start/completion timestamps
// so compact background-thread lists can show live and terminal durations.
// 3.31: MCP server projections expose persistent enablement and a narrow
// settings mutation can enable or disable an existing definition (additive).
// 3.32: folded history pages can opt into expanding backward to a complete
// turn boundary so prepending history never mutates the oldest already-rendered
// turn (additive).
// 3.33: queued turn acceptance can include the newly persisted prompt row so
// clients can mutate its durable id without waiting for event-stream delivery
// (additive).
// 3.34: GET /v1/models/refresh resolves live account and vendor-CLI model
// availability separately from the instant static GET /v1/models snapshot
// (additive).
// 3.35: GET /v1/threads/{id}/subagents accepts recursive=true so parent
// overviews can include active nested collaborator descendants (additive).
// 3.36: thread projections include their optional direct parent id so clients
// can render durable collaborator hierarchies without reconstructing them
// from paged transcript events (additive).
// 4.0: acknowledge that the 3.25/3.26 closed-enum additions were breaking for
// generated clients. Clients now require an exact protocol version instead of
// assuming that every newer same-major schema is forward-compatible.
// 5.0: terminal output streams announce their absolute replay start with a
// named, id-less `replay-start` SSE event before replay and live output;
// approval and question resolution require the owning thread to prevent
// vendor-local id collisions and delayed-response ambiguity.
// 5.1: GitHub endpoints report OAuth credentials that need renewed scopes as
// an explicit authentication-required error instead of an internal failure.
// 5.2: assistant.progress / assistant.progress_completed events and folded
// Progress thread items distinguish authored agent updates from reasoning.
// 5.3: account GitHub projections include recently closed, unmerged pull
// requests so session navigators can render their terminal PR state.
// 6.0: interactive and code-review personas become one Persona catalog;
// AgentPersona/PersonaInfo and /v1/personas replace the former split names.
// 6.1: code-review findings and candidate rejections expose confidence
// alongside severity, plus an explicit publication-policy suppression outcome
// for confirmed findings retained internally but not posted (additive).
// 7.0: code-review findings and rejected candidates require a separately
// generated one-line title instead of deriving presentation from body text
// (breaking).
// 7.1: PUT /v1/config/defaults atomically replaces the global model,
// thinking level, and permission mode used by new threads (additive).
// 7.2: code-review jobs expose the immutable incremental watermark separately
// from the effective diff base selected after ancestry checks (additive).
// 7.3: personas declare whether they are general or code-review personas.
// 7.4: repository review updates document retryable 409 conflicts (additive).
// 7.5: thread-view snapshots identify active-turn cumulative usage separately
// from the latest valid usage measurement (additive).
// 7.6: thread-view snapshot last_usage is the last completed turn, while
// active_usage exclusively carries cumulative usage for a running turn.
// 7.7: code-review findings expose structured evidence, revision origin,
// durable root-cause theme membership, and immutable resolution provenance;
// review details, PR projections, and statistics expose recurrence and churn
// controls (additive).
// 7.8: running turns expose an additive phase event/snapshot field, and
// session title updates accept an expected persisted title for atomic
// background upgrades (additive).
// 7.9: session creation accepts an additive idempotency key so clients can
// safely retry when a committed response is lost.
// 7.10: code-review findings identify verified RIGHT-side anchors outside the
// pull-request diff so clients can distinguish review-level comments.
// 7.11: a failed or cancelled final review editor can be retried independently,
// retaining successful reviewer task outputs (additive).
// 7.12: code-review jobs expose the server-authoritative final-editor retry
// capability derived from their latest durable task attempts (additive).
// 7.13: workspace branch listings expose the origin remote's default branch
// for session base selection (additive).
// 7.14: code-review details expose reviewer candidates left unresolved after
// final-editor repair so clients can represent incomplete reviews (additive).
// 7.15: code-review jobs expose the PR-wide open-finding count captured after
// publication so a clean incremental round cannot hide older findings.
// 7.16: code-review jobs expose the server-derived per-PR fix-churn signal so
// clients and the check run can distinguish a settled clean state from a
// single clean round inside a fix-churn loop; jobs and repositories carry the
// implementation-analyst model/thinking configuration and tasks gain the
// `analyst` role; findings gate in two tiers — `open_issue_count` counts only
// blocking findings while `advisory_open_issue_count` tracks recorded debt
// that no longer posts to GitHub or blocks the check.
pub const PROTOCOL_VERSION: &str = "7.16";
pub const EVENT_CURSOR_HEADER: &str = "x-trouve-event-cursor";
pub const ERROR_CODE_SESSION_DIFF_TOO_LARGE: &str = "session_diff_too_large";
pub const ERROR_CODE_GITHUB_REAUTHENTICATION_REQUIRED: &str = "github_reauthentication_required";

pub type WorkspaceId = String;
pub type SessionId = String;
pub type ThreadId = String;
pub type CallId = String;
pub type CheckpointId = String;
