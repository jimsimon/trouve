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

/// A data-driven agent mode: prompt + tool policy + default permissions.
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
    #[serde(default)]
    pub default_permission_mode: PermissionMode,
}

// --- server info ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
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
}

/// Accepted-for-processing response; progress arrives on the event stream.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnAccepted {
    pub thread_id: ThreadId,
    pub turn: u64,
}

// --- approvals -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveApprovalRequest {
    pub call_id: CallId,
    pub decision: ApprovalDecision,
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
