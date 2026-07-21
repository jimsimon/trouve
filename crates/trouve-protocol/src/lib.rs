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
// 0.20: global default permission mode — AgentMode.default_permission_mode
// is now optional (absent = global default), GET /v1/providers reports
// default_permission_mode, and PUT /v1/config/default-permission-mode sets it.
// 0.21: global and per-mode default thinking levels — additive fields on
// AgentMode, UpsertModeRequest, ProvidersResponse, and SetDefaultModelRequest.
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
// 1.1: automations persist model_options and apply them to every run
// (additive; omitted values preserve the model's defaults).
pub const PROTOCOL_VERSION: &str = "1.1";

pub type WorkspaceId = String;
pub type SessionId = String;
pub type ThreadId = String;
pub type CallId = String;
pub type CheckpointId = String;
