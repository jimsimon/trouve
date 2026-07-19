//! Request/response bodies for the command endpoints.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApprovalDecision, CallId, SessionId, ThreadId, WorkspaceId};

/// How tool calls are gated in a thread. See ADR 0004.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Every mutating tool call requires explicit approval.
    #[default]
    Ask,
    /// Pre-approved commands/paths run without prompts; the rest ask.
    AllowList,
    /// Everything runs. Unsafe; clients must flag it loudly.
    Yolo,
}

/// A data-driven agent mode: prompt + tool policy + model/permission defaults.
/// Adding a mode is configuration, not code (AGENTS.md invariant 6).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentMode {
    /// Stable identifier, e.g. "code", "plan", "review".
    pub id: String,
    pub display_name: String,
    /// Appended to the base system prompt.
    pub system_prompt: String,
    /// Tool names this mode may use; empty means all registered tools.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// When true the mode can never mutate the worktree regardless of the
    /// thread's permission mode (e.g. plan/question modes).
    #[serde(default)]
    pub read_only: bool,
    /// Permission mode for threads started in this mode. None falls back to
    /// the global default permission mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_permission_mode: Option<PermissionMode>,
    /// Preferred model for threads started in this mode ("provider/model").
    /// None falls back to the global default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Preferred thinking level for threads started in this mode. The value
    /// is a model-advertised enum token (for example "medium" or "high").
    /// None falls back to the global default thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
}

/// A mode plus where it came from, for the settings UI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModeInfo {
    pub mode: AgentMode,
    /// "builtin" (untouched), "customized" (builtin with a user override
    /// file), "custom" (user-added), or "workspace" (defined in the
    /// workspace's .agents/modes — file-managed, read-only in settings).
    pub origin: String,
}

/// Create or update a user-level mode (`<config>/modes/<id>.toml`). Saving
/// under a built-in id customizes that built-in.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpsertModeRequest {
    pub display_name: String,
    pub system_prompt: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub read_only: bool,
    /// None uses the global default permission mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// None uses the global default thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
}

// --- server info ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
    /// Whether the server can currently reach the internet (see the
    /// `server.connectivity_changed` event). Absent on older servers, which
    /// never report offline.
    #[serde(default = "default_true")]
    pub online: bool,
}

// --- workspaces ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegisterWorkspaceRequest {
    /// Absolute path to a git repository root.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub path: String,
}

// --- sessions ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionRequest {
    pub workspace_id: WorkspaceId,
    /// Human-readable title; also used to derive the branch slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Base ref the session branch is created from (default: workspace HEAD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    /// Branch dedicated to this session (`trouve/<slug>`).
    pub branch: String,
    /// Absolute path of the session worktree.
    pub worktree_path: String,
    pub base_ref: String,
    /// Archived sessions are hidden from default listings but keep their
    /// worktree and history.
    #[serde(default)]
    pub archived: bool,
    /// One of the session's threads is actively processing prompts right
    /// now. Live updates ride the server-scope `session.activity` event.
    #[serde(default)]
    pub active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Partial session update (rename / archive). Omitted fields are unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

// --- threads -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateThreadRequest {
    pub session_id: SessionId,
    /// Agent mode id (default: "code").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Provider/model identifier, e.g. "openai/gpt-4.1".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model-specific options validated against the model's options schema.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Thread {
    pub id: ThreadId,
    pub session_id: SessionId,
    pub mode: String,
    pub model: String,
    /// Current values for the model's options (thinking level, etc.);
    /// clients render controls from the model's `options_schema`.
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub permission_mode: PermissionMode,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// True when an agent spawned this thread (spawn_thread/spawn_session
    /// tools) rather than the user; clients mark such threads visually.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spawned: bool,
}

/// Partial thread update between turns (mode/model switching). Rejected with
/// a conflict while a turn is running. Omitted fields are unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateThreadRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Replaces the thread's model options when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_options: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

// --- turns ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SendMessageRequest {
    pub content: String,
    /// Files riding along with the prompt (screenshots, logs, …); bytes are
    /// base64 in the request, stored server-side, and referenced by id from
    /// then on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentUpload>,
}

/// One file uploaded with a prompt.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AttachmentUpload {
    /// Display name ("screenshot.png"); the server keeps it for rendering
    /// and derives the stored file's extension from it.
    pub name: String,
    /// MIME type ("image/png"). `image/*` attachments are passed to agents
    /// as native image inputs; anything else is referenced by path.
    pub mime: String,
    /// Base64-encoded contents (standard alphabet, padded).
    pub data: String,
}

/// A stored prompt attachment. Bytes are served at
/// `GET /v1/attachments/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size_bytes: u64,
}

/// Accepted-for-processing response; progress arrives on the event stream.
/// When the thread already has a turn running the prompt is queued instead:
/// `queued` is true and `turn` is 0 (the turn number is assigned when the
/// prompt is dispatched).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnAccepted {
    pub thread_id: ThreadId,
    pub turn: u64,
    #[serde(default)]
    pub queued: bool,
}

// --- queued prompts --------------------------------------------------------

/// A prompt waiting its turn. Queued prompts persist on disk and run in
/// `position` order once the thread is idle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QueuedPrompt {
    pub id: String,
    pub thread_id: ThreadId,
    pub position: u64,
    pub content: String,
    /// Attachments uploaded with the prompt (already stored server-side).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateQueuedPromptRequest {
    pub content: String,
}

/// Full desired order for a thread's queue (every queued prompt id, first
/// to run first).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReorderQueueRequest {
    pub ids: Vec<String>,
}

// --- approvals -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveApprovalRequest {
    pub call_id: CallId,
    pub decision: ApprovalDecision,
}

// --- questions -------------------------------------------------------------

/// Answers for a pending `question.requested`. `answers: null` skips the
/// questions (the agent is told the user declined to answer).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveQuestionRequest {
    pub request_id: CallId,
    #[serde(default)]
    pub answers: Option<Vec<crate::QuestionAnswer>>,
}

// --- worktree inspection ---------------------------------------------------

/// The session's unified diff against its base ref.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionDiff {
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FileContent {
    pub path: String,
    pub content: String,
}

// --- integrated terminal -----------------------------------------------------
//
// A session has at most one interactive shell, spawned in its worktree.
// Output is an ephemeral byte stream (SSE of base64 chunks addressed by
// byte offset), like the diff/files endpoints — not part of the event log.

/// Open (or re-attach to) the session's terminal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenTerminalRequest {
    /// Initial grid size; ignored when re-attaching to a live terminal.
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TerminalInfo {
    pub id: String,
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
    /// True once the shell process has exited (the stream is complete).
    pub exited: bool,
}

/// Keyboard/paste bytes for the PTY, base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TerminalInputRequest {
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TerminalResizeRequest {
    pub cols: u16,
    pub rows: u16,
}

// --- GitHub PRs ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckRun {
    pub name: String,
    /// queued / in_progress / completed
    pub status: String,
    /// success / failure / … (None while running)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrReview {
    pub reviewer: String,
    /// approved / changes_requested / commented / …
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub base: String,
    pub head: String,
    pub checks: Vec<CheckRun>,
    pub reviews: Vec<PrReview>,
    /// PR author's login.
    #[serde(default)]
    pub author: String,
    /// Logins with an outstanding review request.
    #[serde(default)]
    pub requested_reviewers: Vec<String>,
    /// Issue + review comments combined.
    #[serde(default)]
    pub comments: u64,
    /// When the newest comment (of either kind) was posted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_comment_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Every dashboard-relevant PR of a workspace's origin repo: all open PRs
/// plus those merged in the last day.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkspacePrList {
    /// Login of the authenticated GitHub user ("" when unknown) — clients
    /// use it to spot PRs where that user's review was requested.
    #[serde(default)]
    pub viewer: String,
    pub prs: Vec<PrInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatePrRequest {
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Base branch (default: the session's base ref without `origin/`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergePrRequest {
    /// merge / squash / rebase (default: merge)
    #[serde(default)]
    pub method: Option<String>,
}

// --- subscription health -----------------------------------------------------

/// One metered rate-limit window of a vendor subscription (e.g. Codex's
/// 5-hour and weekly buckets).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionWindow {
    /// "5h window", "Weekly", …
    pub label: String,
    pub used_percent: i64,
    /// Pre-rendered reset note ("resets in 2h 10m"), "" when unknown.
    pub resets: String,
}

/// Subscription usage for one configured agent-backend provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionHealth {
    pub provider_id: String,
    /// "ok" (windows below), "unavailable" (vendor query failed / not
    /// logged in), or "unsupported" (vendor doesn't share the data).
    pub status: String,
    /// Plan name as reported ("plus", "pro", …); "" when unknown.
    pub plan: String,
    pub windows: Vec<SubscriptionWindow>,
    /// Credits summary ("credits: 42.50", "unlimited credits"); "" if n/a.
    pub credits: String,
    /// Human explanation for unavailable/unsupported; "" when ok.
    pub note: String,
}

// --- MCP servers -------------------------------------------------------------

/// One user-managed MCP server (from `mcp.json` in the trouve config dir or
/// `.agents/.mcp.json` in a workspace). First-party servers trouve injects
/// itself (the Claude approval bridge) never appear here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServerInfo {
    pub name: String,
    /// "user" (config dir) or "workspace" (.agents/.mcp.json).
    pub scope: String,
    /// For workspace scope: which workspace's config this entry lives in.
    /// Empty for user scope.
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Values may be `${VAR}` references resolved at spawn time.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// "ok" / "error" / "unknown" (unknown when listing skipped the probe) /
    /// "untrusted" (a repo-scoped server that is never auto-run).
    pub health: String,
    /// "5 tools" when healthy, the failure reason when not, "" for unknown.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpsertMcpServerRequest {
    /// "user" or "workspace".
    pub scope: String,
    /// Required for workspace scope: whose `.agents/.mcp.json` to edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

/// Recent stderr and lifecycle lines for one MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpLogs {
    pub lines: Vec<String>,
}

// --- integrations ----------------------------------------------------------

/// Whether the GitHub integration can authenticate, and where the token
/// came from ("environment", "oauth", "settings", "gh-cli", or "" when
/// unconfigured).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GithubIntegration {
    /// github.com's state (mirrors `hosts[0]`; kept for older clients).
    pub configured: bool,
    pub source: String,
    /// Whether "Sign in with GitHub" (OAuth device flow) is available —
    /// i.e. a GitHub OAuth app client id is configured or built in.
    #[serde(default)]
    pub oauth_available: bool,
    /// Every known host: github.com first, then the configured GitHub
    /// Enterprise hosts in config order.
    #[serde(default)]
    pub hosts: Vec<GithubHostIntegration>,
}

/// Auth state of one GitHub host (github.com or a GitHub Enterprise
/// instance).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GithubHostIntegration {
    /// "github.com" or the enterprise hostname ("github.example.com").
    pub host: String,
    pub configured: bool,
    /// "environment", "oauth", "settings", "gh-cli", or "" when
    /// unconfigured.
    pub source: String,
    /// A device-flow OAuth app client id is configured for this host.
    pub oauth_available: bool,
    /// Enterprise hosts can be removed; github.com cannot.
    pub removable: bool,
}

/// Store (or, with an empty token, remove) the GitHub personal access
/// token in the server's secret store.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetGithubTokenRequest {
    pub token: String,
    /// Which host the token is for; empty/absent means github.com.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
}

/// Register a self-hosted GitHub Enterprise instance
/// (`POST /v1/integrations/github/hosts`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddGithubHostRequest {
    /// Hostname only, e.g. "github.example.com".
    pub host: String,
    /// Client id of an OAuth app on that instance (device flow enabled);
    /// enables "Sign in" for the host. A PAT works without one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
}

// --- branches --------------------------------------------------------------

/// Local branches of a workspace repository, for base-ref selection.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BranchList {
    pub branches: Vec<String>,
    /// The branch HEAD currently points at (default selection).
    pub head: String,
}

// --- provider configuration -------------------------------------------------

/// A configured provider, with secrets elided.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderInfo {
    /// Stable identifier, e.g. "openai" or "openrouter".
    pub id: String,
    /// "openai-compat" or "anthropic".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Whether usable credentials were resolved (key, env, OAuth, or a
    /// logged-in vendor CLI).
    pub has_credentials: bool,
    /// "api-key", "oauth", "cli" (vendor CLI holds the subscription auth),
    /// or "none" — drives which credential UI to show.
    pub auth: String,
    /// Uses an undocumented vendor endpoint that may break or be restricted
    /// at any time; clients should display a warning.
    #[serde(default)]
    pub experimental: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProvidersResponse {
    pub providers: Vec<ProviderInfo>,
    /// Default model for new threads, e.g. "openai/gpt-4.1-mini".
    pub default_model: String,
    /// Global thinking level for new threads. None leaves the selected
    /// model at its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
    /// Global default permission mode for new threads, used by modes without
    /// a default of their own. Absent on older servers means Ask.
    #[serde(default)]
    pub default_permission_mode: PermissionMode,
}

/// Create or update a provider. The API key (when given) goes to the secret
/// store, never to the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpsertProviderRequest {
    /// "openai-compat" or "anthropic".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetDefaultModelRequest {
    /// Provider-qualified id, e.g. "openai/gpt-4.1-mini".
    pub model: String,
    /// Global thinking level for the selected model. Omitted when the model
    /// has no thinking knob, preserving the existing global setting for
    /// models that do support it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
}

/// Set the global default permission mode
/// (`PUT /v1/config/default-permission-mode`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetDefaultPermissionModeRequest {
    pub permission_mode: PermissionMode,
}

/// A well-known provider preset: clients offer these for one-click setup
/// instead of hand-typed base URLs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KnownProvider {
    /// Suggested provider id, e.g. "openrouter".
    pub id: String,
    pub display_name: String,
    /// Wire protocol: "openai-compat" or "anthropic".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Conventional environment variable holding the API key, when one
    /// exists (empty for keyless local providers like Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// How the provider authenticates: "api-key", "oauth" (subscription
    /// login), "cli" (the vendor's own CLI holds subscription auth), or
    /// "none" (keyless local endpoints).
    pub auth: String,
    /// Uses an undocumented vendor endpoint that may break or be restricted
    /// at any time; clients should display a warning.
    #[serde(default)]
    pub experimental: bool,
}

/// Response to starting an OAuth login for a provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LoginStarted {
    /// URL the user must open in a browser to approve access.
    pub verification_url: String,
    /// Code the user must enter at the verification URL (device flow only;
    /// PKCE flows encode everything in the URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
}

/// Current state of a provider's OAuth login attempt.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LoginStatus {
    /// "none" (no login running), "pending", "success", or "failed".
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// --- vendor CLIs ------------------------------------------------------------

/// A vendor CLI trouve can download and manage (cursor-agent, claude,
/// codex), with its current install state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CliInfo {
    /// Stable id, also the binary name: "cursor-agent", "claude", "codex".
    pub id: String,
    pub display_name: String,
    /// Provider kinds served by this CLI (e.g. ["cursor-cli"]).
    pub kinds: Vec<String>,
    /// Version of the binary trouve would run, when one was resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Where that binary comes from: "managed" (trouve-installed),
    /// "path" (system install), or "none".
    pub source: String,
    /// Absolute path of the resolved binary, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Newest version the vendor serves (None when the check failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    pub update_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CliList {
    pub clis: Vec<CliInfo>,
}

/// A GPU the local-models hardware probe found.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LocalGpu {
    pub name: String,
    /// Dedicated VRAM in bytes (system RAM for unified-memory machines).
    pub vram_bytes: u64,
}

/// One local model (curated catalog entry or user-added GGUF) with its
/// download and hardware-fit state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LocalModelInfo {
    #[allow(rustdoc::invalid_html_tags)]
    /// Stable id; runs as model "local/<id>".
    pub id: String,
    pub display_name: String,
    /// HuggingFace repo the GGUF comes from (e.g. "Qwen/…-GGUF").
    pub repo: String,
    /// GGUF filename inside the repo.
    pub file: String,
    pub size_bytes: u64,
    /// Human parameter count ("7B", "30B MoE").
    pub params: String,
    /// Context window trouve serves the model with.
    pub context_window: u64,
    /// Hardware fit: "gpu" (fits in VRAM), "cpu" (fits in RAM, slower),
    /// or "too-large".
    pub fit: String,
    /// One-line description shown in settings.
    #[serde(default)]
    pub notes: String,
    /// True when the GGUF is on disk and ready to run.
    pub downloaded: bool,
    /// "none" / "pending" / "failed" (success shows as downloaded).
    pub download_status: String,
    /// Downloaded bytes so far (pending only).
    #[serde(default)]
    pub download_bytes: u64,
    #[serde(default)]
    pub download_error: String,
    /// User-added entry (can be removed entirely).
    pub custom: bool,
}

/// Local inference status: hardware, the llama.cpp runtime install, the
/// running server, and every known model.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LocalStatus {
    /// Whether local models are enabled (the "local" provider is
    /// registered). Toggled with `PUT /v1/local/enabled`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub ram_bytes: u64,
    pub gpus: Vec<LocalGpu>,
    /// Whether the llama.cpp runtime (llama-server) is installed.
    pub runtime_installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    /// True when the runtime is a trouve-managed install (updatable and
    /// uninstallable through the API), false for PATH/system builds.
    #[serde(default)]
    pub runtime_managed: bool,
    /// Newest llama.cpp build the vendor serves (None when the check
    /// failed, e.g. offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_latest_version: Option<String>,
    /// True when a managed install is older than `runtime_latest_version`.
    #[serde(default)]
    pub runtime_update_available: bool,
    /// Model id currently loaded in (or loading into) llama-server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_model: Option<String>,
    /// Sidecar state: "stopped", "starting" (model loading), or "running".
    #[serde(default)]
    pub server_status: String,
    pub models: Vec<LocalModelInfo>,
}

fn default_true() -> bool {
    true
}

/// Turn local models on or off (`PUT /v1/local/enabled`). Disabling stops
/// the llama-server sidecar and unregisters the "local" provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetLocalEnabledRequest {
    pub enabled: bool,
}

/// Add a custom GGUF from a HuggingFace repo to the local model list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddLocalModelRequest {
    /// HuggingFace repo id, e.g. "unsloth/Qwen3.6-27B-GGUF".
    pub repo: String,
    /// GGUF filename inside the repo.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// One single-file GGUF inside a search result's repo
/// (`GET /v1/local/search`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LocalSearchFile {
    /// Path inside the repo, ready for [`AddLocalModelRequest::file`].
    pub file: String,
    pub size_bytes: u64,
    /// Quantization tag parsed from the filename ("Q4_K_M"; may be empty).
    pub quant: String,
    /// Hardware fit on this machine: "gpu", "cpu", or "too-large".
    pub fit: String,
    /// Already in the local model list (catalog or previously added).
    pub added: bool,
}

/// One HuggingFace repo matching a local-model search, with its
/// single-file GGUFs (smallest first) and a recommended pick for this
/// machine's hardware.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LocalSearchResult {
    /// Repo id ("Qwen/Qwen2.5-Coder-7B-Instruct-GGUF").
    pub repo: String,
    pub downloads: u64,
    pub likes: u64,
    pub files: Vec<LocalSearchFile>,
    /// Index into `files` of the best pick for this hardware.
    pub recommended: u32,
}

// --- automations -----------------------------------------------------------

/// When an automation fires. Times are the server's local time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AutomationSchedule {
    /// "hourly", "daily", or "weekly".
    pub kind: String,
    /// Hourly: minute of the hour (0-59).
    #[serde(default)]
    pub minute: u8,
    /// Daily/weekly: time of day as "HH:MM" (24h).
    #[serde(default)]
    pub time: String,
    /// Weekly: days it fires (0 = Monday … 6 = Sunday); at least one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<u8>,
}

/// A scheduled prompt. Each run creates a fresh session (worktree) in the
/// workspace, a thread with the configured mode/model, and sends the
/// prompt — exactly as if the user had typed it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub workspace_id: WorkspaceId,
    /// Agent mode for the runs (None = the default mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Model for the runs (None = the mode's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Permission policy applied only to sessions created by this automation.
    /// Defaults to Ask; Yolo is an explicit unattended-execution opt-in.
    #[serde(default)]
    pub permission_mode: PermissionMode,
    pub schedule: AutomationSchedule,
    pub enabled: bool,
    /// Next fire time (RFC3339), when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    /// Last fire time (RFC3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// Session created by the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<String>,
    /// Why the last run failed ("" = it didn't).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_error: String,
    pub created_at: String,
}

/// Create or update an automation (`POST /v1/automations`,
/// `PUT /v1/automations/{id}`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpsertAutomationRequest {
    pub name: String,
    pub prompt: String,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Permission policy for each fresh automation session. Omitted by older
    /// clients means Ask.
    #[serde(default)]
    pub permission_mode: PermissionMode,
    pub schedule: AutomationSchedule,
    pub enabled: bool,
}

/// A pre-canned automation for a common development task
/// (`GET /v1/automations/templates`). Clients use these to pre-fill the
/// create form; the user still picks the workspace and can edit anything.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutomationTemplate {
    pub id: String,
    pub name: String,
    /// One-line summary shown in template pickers.
    pub description: String,
    pub prompt: String,
    /// Suggested schedule (editable like the rest).
    pub schedule: AutomationSchedule,
}

/// State of a CLI install started with `POST /v1/clis/{id}/install`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CliInstallStatus {
    /// "none" (nothing running), "pending", "success", or "failed".
    pub status: String,
    /// Version being (or just) installed, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Bytes downloaded so far (pending only).
    #[serde(default)]
    pub received_bytes: u64,
    /// Expected total from Content-Length; 0 when unknown.
    #[serde(default)]
    pub total_bytes: u64,
}

// --- models --------------------------------------------------------------

/// A model a configured provider can run, with enough metadata for clients
/// to render selection and options UIs generically.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelInfo {
    /// Provider-qualified id, e.g. "openai/gpt-4.1-mini".
    pub id: String,
    pub display_name: String,
    pub context_window: u64,
    pub supports_tools: bool,
    /// USD per million input tokens (None = unknown; cost reporting skips it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price_per_mtok: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price_per_mtok: Option<f64>,
    /// JSON Schema for the model's options object (thinking level, etc.).
    /// Clients render these controls from the schema, not from hardcoded
    /// per-model knowledge.
    pub options_schema: serde_json::Value,
}

/// Aggregated usage for a thread or session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UsageSummary {
    pub turns: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cost_usd: f64,
}

// --- errors --------------------------------------------------------------

/// Uniform error body for non-2xx responses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}
