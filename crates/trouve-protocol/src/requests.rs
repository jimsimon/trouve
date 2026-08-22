//! Request/response bodies for the command endpoints.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApprovalDecision, CallId, SessionId, ThreadId, WorkspaceId};

/// A scalar value accepted by a model's advertised options schema.
///
/// Protocol request/response structs retain `serde_json::Value` internally so
/// arbitrary-precision JSON number tokens survive deserialization. Their
/// OpenAPI fields use this type to advertise the narrower wire contract that
/// the engine already enforces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum ModelOptionValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

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

/// Whether a persona is intended for general interaction or specialized review.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PersonaGroup {
    #[default]
    General,
    Reviewer,
}

/// A data-driven agent persona: prompt + tool policy + model/permission defaults.
/// Adding a persona is configuration, not code (AGENTS.md invariant 6).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentPersona {
    /// Stable identifier, e.g. "code", "plan", "review".
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub group: PersonaGroup,
    /// Appended to the base system prompt.
    pub system_prompt: String,
    /// Tool names this persona may use; empty means all registered tools.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// When true the persona can never mutate the worktree regardless of the
    /// thread's permission policy (e.g. plan/question personas).
    #[serde(default)]
    pub read_only: bool,
    /// Permission policy for threads started with this persona. None falls back to
    /// the global default permission mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_permission_mode: Option<PermissionMode>,
    /// Preferred model for threads started with this persona ("provider/model").
    /// None falls back to the global default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Preferred thinking setting for threads started with this persona. The value
    /// is a model-advertised enum token (for example "medium" or "high") or
    /// a decimal token budget for fixed-thinking models.
    /// None falls back to the global default thinking level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
}

/// A persona plus where it came from, for the settings UI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PersonaInfo {
    pub persona: AgentPersona,
    /// "builtin" (untouched), "customized" (builtin with a user override
    /// file), "custom" (user-added), or "workspace" (defined in the
    /// workspace's .agents/personas — file-managed, read-only in settings).
    pub origin: String,
}

/// Create or update a user-level persona (`<config>/personas/<id>.toml`). Saving
/// under a built-in id customizes that built-in.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpsertPersonaRequest {
    pub display_name: String,
    #[serde(default)]
    pub group: PersonaGroup,
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

// --- session naming settings --------------------------------------------

/// When the dedicated session-title model should occupy memory.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TitleModelLoadBehavior {
    /// Keep the model ready when the server detects comfortable memory
    /// headroom; otherwise load it for each naming request.
    #[default]
    Auto,
    /// Load at server startup and keep the model resident.
    Always,
    /// Load for naming requests and release it after an idle period.
    OnDemand,
    /// Never load the model; use the built-in naming heuristics.
    Off,
}

/// Compute resources the dedicated session-title model may use.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TitleModelResourcePolicy {
    /// Choose GPU acceleration when it will not contend with a running local
    /// coding model; otherwise use CPU and system RAM.
    Adaptive,
    /// Allow llama.cpp to place the model across GPU, CPU, and system RAM.
    GpuCpuRam,
    /// Require all model layers to fit on a detected GPU.
    GpuOnly,
    /// Keep all model computation off the GPU. This preserves the behavior
    /// used before resource selection was exposed.
    #[default]
    CpuRamOnly,
}

/// Runtime status for the managed session-title model.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TitleModelStatus {
    /// `not_installed`, `installing`, `stopped`, `loading`, `ready`, or
    /// `error`.
    pub state: String,
    /// Human-readable context for the settings screen.
    #[serde(default)]
    pub detail: String,
    pub runtime_installed: bool,
    pub model_downloaded: bool,
    /// Empty, `runtime`, or `model`.
    #[serde(default)]
    pub install_stage: String,
    #[serde(default)]
    pub install_bytes: u64,
    #[serde(default)]
    pub install_total: u64,
}

/// Global session-naming settings shown under Settings → Sessions & Chat.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GitWorktreeSettings {
    /// Whether new session branches include a slug derived from the session
    /// title. False uses the compact `trouve/<short-id>` form.
    #[serde(default)]
    pub derive_branch_name_from_session_title: bool,
    pub title_model_load_behavior: TitleModelLoadBehavior,
    #[serde(default)]
    pub title_model_resource_policy: TitleModelResourcePolicy,
    pub title_model: TitleModelStatus,
}

/// Update the Session Naming section under Settings → Sessions & Chat.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetGitWorktreeSettingsRequest {
    /// Omitted by older clients to preserve the current branch-naming mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive_branch_name_from_session_title: Option<bool>,
    pub title_model_load_behavior: TitleModelLoadBehavior,
    #[serde(default)]
    pub title_model_resource_policy: TitleModelResourcePolicy,
}

/// Ask the server to derive a concise title for a new session.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenerateSessionTitleRequest {
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GeneratedSessionTitle {
    pub title: String,
    /// `model` or `heuristic`.
    pub source: String,
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

/// Workspace presentation returned by the list and registration endpoints.
/// Separate checkouts and linked worktrees share repository_key when they
/// resolve to the same configured remote or local Git common directory.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceListItem {
    pub id: WorkspaceId,
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_name: Option<String>,
}

// --- sessions ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionRequest {
    pub workspace_id: WorkspaceId,
    /// Stable client-generated key for retrying this create operation without
    /// creating a second session if the original response is lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(min_length = 1, max_length = 128, pattern = "^[A-Za-z0-9._-]+$")]
    pub idempotency_key: Option<String>,
    /// Human-readable title. When title-derived branch naming is enabled,
    /// this is also used to derive the branch slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Base ref the session branch is created from (default: workspace HEAD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// Optional ref used to create the session branch while `base_ref`
    /// remains the comparison base. Automated PR reviews use this to check
    /// out the exact head SHA and still expose the base-to-head diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_ref: Option<String>,
    /// Fetch the base branch's configured upstream and start from its latest
    /// remote commit. Refs without an upstream are used as-is.
    #[serde(default = "default_true")]
    pub fetch_latest: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    /// Branch dedicated to this session. New sessions default to
    /// `trouve/<short-id>`; users may opt into `trouve/<slug>-<short-id>`.
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

/// A new session and its initial thread, both created from an existing
/// turn checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ForkCheckpointResponse {
    pub session: Session,
    pub thread: Thread,
}

/// Partial session update (rename / archive). Omitted fields are unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Apply the new title only while the persisted title still has this value.
    /// Used by asynchronous title generation so later manual renames win.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

// --- threads -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateThreadRequest {
    pub session_id: SessionId,
    /// Concise user-visible title for navigation surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Agent persona id (default: "code").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Provider/model identifier, e.g. "openai/gpt-4.1".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model-specific options validated against the model's options schema.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    #[schema(value_type = std::collections::BTreeMap<String, ModelOptionValue>)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

/// Progress state for one item in a thread's current todo list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// One user-visible transition in a todo's lifecycle. `Skipped` is derived
/// when an unfinished todo disappears from a replacement snapshot; it is not
/// a current-list status because skipped items are no longer in that list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadTodoState {
    Started,
    Completed,
    Cancelled,
    Skipped,
}

/// One stable item in a thread's current todo list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Thread {
    pub id: ThreadId,
    pub session_id: SessionId,
    /// Direct parent when this thread was spawned by another thread. The
    /// relation is optional so user-created threads and older servers remain
    /// compatible; clients can use it to render nested collaborator trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    /// Concise user-visible title. Older threads may not have one; clients
    /// fall back to the session title or mode/model metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub mode: String,
    pub model: String,
    /// Current values for the model's options (thinking level, etc.);
    /// clients render controls from the model's `options_schema`.
    #[serde(default)]
    #[schema(value_type = std::collections::BTreeMap<String, ModelOptionValue>)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub permission_mode: PermissionMode,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// True when an agent spawned this thread (spawn_thread/spawn_session
    /// tools) rather than the user; clients mark such threads visually.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spawned: bool,
    /// Current todo snapshot for this thread. Tool-call events retain the
    /// history of how the list changed; this field is the initial-load view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<TodoItem>,
}

/// One folded, renderable row in a thread snapshot. Raw streaming fragments
/// remain in the event log; snapshots expose their current semantic form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadViewItem {
    User {
        turn: u64,
        content: String,
        attachments: Vec<Attachment>,
        /// Server-dispatched attach turn for vendor-autonomous agent
        /// activity; render as background activity, not as user input.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        background: bool,
    },
    Steered {
        turn: u64,
        content: String,
        attachments: Vec<Attachment>,
    },
    /// A separately navigable child agent transcript spawned from this parent
    /// turn. Children in read-only modes are transcript-only; other child
    /// threads can accept follow-up prompts after their initial turn.
    Subagent {
        turn: u64,
        thread_id: ThreadId,
        session_id: SessionId,
        prompt: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<CallId>,
    },
    Assistant {
        turn: u64,
        content: String,
        complete: bool,
    },
    /// User-facing progress authored by the agent harness rather than model
    /// reasoning or final answer text.
    Progress {
        turn: u64,
        content: String,
        complete: bool,
    },
    Thinking {
        turn: u64,
        content: String,
        complete: bool,
    },
    /// A context-window compaction boundary in the transcript. Unlike a
    /// tool call, this is engine lifecycle state and remains visible after
    /// completion so clients can show where earlier context was summarized.
    Compaction {
        turn: u64,
        state: ThreadCompactionState,
    },
    /// A durable todo lifecycle boundary derived from successive
    /// `thread.todos_updated` snapshots while a turn is running.
    TodoUpdate {
        turn: u64,
        todo_id: String,
        content: String,
        state: ThreadTodoState,
    },
    ToolCall {
        call_id: String,
        tool: String,
        args: serde_json::Value,
        /// Completed historical rows may contain only bounded presentation
        /// arguments. Fetch `/v1/threads/{thread}/tools/{call_id}` before
        /// rendering expanded/raw detail when this flag is true.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        details_deferred: bool,
        status: ThreadToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        /// Executor-only duration when the completion event carries a
        /// monotonic measurement; otherwise the compatible fallback derived
        /// from durable tool event timestamps.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    TurnStatus {
        turn: u64,
        state: ThreadTurnState,
    },
    Questions {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        questions: Vec<crate::Question>,
        /// True after `question.resolved`; `answers` remains absent when the
        /// user skipped.
        #[serde(default)]
        resolved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<Vec<crate::QuestionAnswer>>,
    },
}

/// Full arguments and terminal result for one materialized historical tool
/// call. Live-tail tool calls continue to carry these fields inline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ThreadToolDetails {
    pub call_id: String,
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadToolStatus {
    AwaitingApproval,
    Running,
    Ok,
    Error,
    Denied,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ThreadTurnState {
    /// The durable turn shell exists, but shared/provider scheduler capacity
    /// has not yet been acquired.
    WaitingForCapacity,
    Running,
    Completed {
        usage: crate::Usage,
        /// Checkpoint created after this turn. Older retained snapshots may
        /// omit it, in which case checkpoint actions are unavailable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<crate::CheckpointId>,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ThreadCompactionState {
    Running,
    Completed {
        messages_compacted: u64,
    },
    /// A started compaction did not report completion before normal turn
    /// output or a terminal turn event arrived.
    Failed,
}

/// Folded current thread state and one transcript item page at the cursor
/// returned in `x-trouve-event-cursor`. Clients seed their view from this
/// response and subscribe to the thread event stream after that cursor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ThreadViewSnapshot {
    /// Zero-based index of `items[0]` in the complete folded transcript.
    #[serde(default)]
    pub item_offset: u64,
    /// Number of folded items in the complete transcript at this snapshot.
    #[serde(default)]
    pub total_items: u64,
    /// Whether another page exists before `item_offset`.
    #[serde(default)]
    pub has_older: bool,
    pub items: Vec<ThreadViewItem>,
    #[serde(default)]
    pub pending_approvals: Vec<String>,
    #[serde(default)]
    pub pending_questions: Vec<String>,
    /// Aggregate usage for the most recently completed turn. Running-turn
    /// usage is reported separately in `active_usage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_usage: Option<crate::Usage>,
    /// Cumulative usage for the active turn. Billing counters sum its model
    /// requests while context fields describe the latest request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_usage: Option<crate::Usage>,
    #[serde(default)]
    pub compacting: bool,
    #[serde(default)]
    pub turn_running: bool,
    #[serde(default)]
    pub thinking: bool,
    /// Current transient activity for the running turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_phase: Option<crate::TurnPhase>,
    #[serde(default)]
    pub turn_models: std::collections::BTreeMap<u64, String>,
    #[serde(default)]
    pub turn_thinking_levels: std::collections::BTreeMap<u64, String>,
    /// Per-turn native steering capability. False/absent is authoritative;
    /// clients must not infer capability from provider or model names.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub turn_steerable: std::collections::BTreeMap<u64, bool>,
    #[serde(default)]
    pub turn_started_at: std::collections::BTreeMap<u64, chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub turn_duration_ms: std::collections::BTreeMap<u64, u64>,
    #[serde(default)]
    pub commands: Vec<crate::CommandInfo>,
    #[serde(default)]
    pub queue: Vec<crate::QueuedPrompt>,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ThreadViewQuery {
    /// Exclusive folded-item offset for backward pagination. Omit for the
    /// newest page.
    #[serde(default)]
    pub before: Option<u64>,
    /// Maximum item count; the server applies a safe upper bound.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Expand the page backward to the beginning of its oldest turn. This can
    /// return more than `limit` items, but prevents a paged turn from changing
    /// shape when its preceding history is loaded.
    #[serde(default)]
    pub turn_aligned: Option<bool>,
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
    #[schema(value_type = Option<std::collections::BTreeMap<String, ModelOptionValue>>)]
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

/// Add user guidance to the turn currently running on a thread. The backend
/// must advertise steering support for that exact turn.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SteerTurnRequest {
    pub content: String,
    /// Steering accepts the same attachment inputs as an ordinary prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentUpload>,
}

/// A steering message accepted by the active vendor turn. Durable display
/// state follows as `turn.steered` on the thread event stream.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SteerAccepted {
    pub thread_id: ThreadId,
    pub turn: u64,
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
    /// The newly accepted durable queue row when this prompt remains queued.
    /// Clients can use its stable id immediately instead of waiting for the
    /// matching `thread.queue_updated` event before enabling queue mutations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_prompt: Option<QueuedPrompt>,
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
    /// Server-dispatched attach prompt for vendor-autonomous agent
    /// activity. Trusted dispatch metadata: never inferred from `content`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// Attachments uploaded with the prompt (already stored server-side).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateQueuedPromptRequest {
    pub content: String,
    /// Existing queued attachment ids to keep, in display order. Omit this
    /// field to preserve every existing attachment (the behavior of clients
    /// predating protocol 2.14); send an empty list to remove all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_attachment_ids: Option<Vec<String>>,
    /// New files to append after the retained attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentUpload>,
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
    /// Owning thread for this vendor-local call id.
    pub thread_id: ThreadId,
    pub call_id: CallId,
    pub decision: ApprovalDecision,
}

// --- questions -------------------------------------------------------------

/// Answers for a pending `question.requested`. `answers: null` skips the
/// questions (the agent is told the user declined to answer).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveQuestionRequest {
    /// Owning thread for this vendor-local request id.
    pub thread_id: ThreadId,
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

/// Bounded metadata for one path changed against a session's base ref.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionDiffFileSummary {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

/// Lightweight changed-file manifest for a session. File patch content is
/// intentionally excluded and loaded only after the user selects a path.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionDiffSummary {
    pub files: Vec<SessionDiffFileSummary>,
    pub additions: u64,
    pub deletions: u64,
}

/// A bounded unified patch for exactly one selected session-relative path.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionFileDiff {
    pub path: String,
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
// A session may own multiple interactive shells, each spawned in its worktree.
// Output is an ephemeral byte stream (SSE of base64 chunks addressed by
// byte offset), like the diff/files endpoints — not part of the event log.

/// Initial dimensions for a newly created terminal. The singular compatibility
/// endpoint ignores these values when it re-attaches to a live terminal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenTerminalRequest {
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

/// Absolute byte offset at which a terminal output subscription begins.
///
/// Sent as JSON in the named, id-less `replay-start` SSE event before any
/// replayed or live base64 output chunks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TerminalReplayStart {
    pub offset: u64,
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
    /// GitHub page for the check run, when the provider exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrReview {
    pub reviewer: String,
    /// approved / changes_requested / commented / …
    pub state: String,
}

/// A review produced by trouve's first-party review service. The marker is
/// joined from durable job/finding records rather than inferred from an
/// untrusted comment author or body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FirstPartyCodeReview {
    pub job_id: String,
    pub bot_login: String,
    pub status: String,
    pub summary: String,
    pub prompt_for_agents: String,
    pub review_url: String,
    #[serde(default)]
    pub findings: Vec<CodeReviewFinding>,
    #[serde(default)]
    pub themes: Vec<CodeReviewTheme>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrInfo {
    /// GitHub instance that owns this PR.
    #[serde(default)]
    pub host: String,
    /// Repository in `owner/name` form.
    #[serde(default)]
    pub repository: String,
    /// Matching local workspace, when one is registered.
    #[serde(default)]
    pub workspace_id: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub base: String,
    pub head: String,
    /// Exact pull-request head commit. Older servers omitted this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub checks: Vec<CheckRun>,
    pub reviews: Vec<PrReview>,
    /// Latest successfully published trouve review for this exact PR head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trouve_review: Option<FirstPartyCodeReview>,
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
    /// Whether GitHub can merge this PR cleanly (false = merge/rebase
    /// conflicts). None while unknown: list endpoints omit it and GitHub
    /// computes it lazily even on single-PR reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeable: Option<bool>,
    /// GitHub's detailed merge state (`clean`, `blocked`, `behind`, ...).
    /// `clean` means the PR is mergeable and all required checks and reviews
    /// permit merging. None when unavailable or still unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_state_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Pull requests relevant to the authenticated account on one GitHub host,
/// spanning every repository visible to that account. Includes open PRs the
/// account authored, was asked to review, or participated in, plus recently
/// merged or closed relevant PRs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GithubPrList {
    /// Login of the authenticated GitHub user ("" when unknown) — clients
    /// use it to spot PRs where that user's review was requested.
    #[serde(default)]
    pub viewer: String,
    /// GitHub instance this slice came from.
    pub host: String,
    pub prs: Vec<PrInfo>,
}

/// Controls an account-level GitHub pull-request refresh.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct RefreshGithubPrsQuery {
    /// Bypass the server freshness window for an explicit user action. A
    /// concurrent refresh that completed after this request began is still
    /// reused rather than immediately repeated.
    #[serde(default)]
    pub force: bool,
}

/// Latest persisted account-level PR replacement for one GitHub host.
/// The event cursor and timestamp let clients order this bootstrap state
/// against newer SSE events without replaying the retained server history.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GithubPrHostProjection {
    pub cursor: u64,
    pub refreshed_at: chrono::DateTime<chrono::Utc>,
    pub pull_requests: GithubPrList,
}

/// Pull requests already associated with one session using durable branch or
/// `session.pr_opened` evidence. This is a local projection of the persisted
/// account snapshots and never performs a GitHub request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionPrProjection {
    pub session_id: SessionId,
    pub prs: Vec<PrInfo>,
}

/// Durable server-owned state that is not part of the session-summary
/// snapshot. Clients fetch this once during bootstrap and can then resume the
/// server event stream at the session-summary cursor instead of cursor zero.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServerProjection {
    pub github_pull_requests: Vec<GithubPrHostProjection>,
    pub session_pull_requests: Vec<SessionPrProjection>,
    pub git_worktree_settings: GitWorktreeSettings,
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

/// A GitHub account, bot, or team shown in pull-request collaboration UI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrActor {
    /// GraphQL node id. It is opaque to clients and only sent back in typed
    /// pull-request actions.
    pub id: String,
    /// User/bot login or team slug.
    pub login: String,
    /// Human-readable name when GitHub exposes one.
    #[serde(default)]
    pub name: String,
    /// user / bot / team / mannequin / unknown
    pub kind: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrLabel {
    pub id: String,
    pub name: String,
    /// Six-digit GitHub label color without a leading `#`.
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrMilestone {
    pub id: String,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrReactionSummary {
    /// GitHub reaction content (`THUMBS_UP`, `HEART`, ...).
    pub content: String,
    pub count: u64,
    #[serde(default)]
    pub viewer_has_reacted: bool,
}

/// A top-level PR conversation comment or one comment in a review thread.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrComment {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_id: Option<u64>,
    pub body: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PrActor>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_edited_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub viewer_can_update: bool,
    #[serde(default)]
    pub viewer_can_delete: bool,
    #[serde(default)]
    pub viewer_did_author: bool,
    #[serde(default)]
    pub reactions: Vec<PrReactionSummary>,
    /// Review-comment-only context.
    #[serde(default)]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default)]
    pub diff_hunk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrReviewDetail {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PrActor>,
    pub state: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub commit_oid: String,
    #[serde(default)]
    pub viewer_can_update: bool,
    #[serde(default)]
    pub viewer_can_delete: bool,
    #[serde(default)]
    pub viewer_did_author: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrReviewThread {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    #[serde(default)]
    pub diff_side: String,
    #[serde(default)]
    pub is_outdated: bool,
    #[serde(default)]
    pub is_resolved: bool,
    #[serde(default)]
    pub viewer_can_reply: bool,
    #[serde(default)]
    pub viewer_can_resolve: bool,
    #[serde(default)]
    pub viewer_can_unresolve: bool,
    #[serde(default)]
    pub comments: Vec<PrComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrCommit {
    pub oid: String,
    pub abbreviated_oid: String,
    pub message_headline: String,
    #[serde(default)]
    pub message_body: String,
    pub committed_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PrActor>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub change_type: String,
    #[serde(default)]
    pub viewer_viewed_state: String,
}

/// Lazily fetched before/after content for one file in a selected pull
/// request. Large and binary blobs retain their metadata without crossing the
/// protocol as unbounded text.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrFileDiff {
    pub path: String,
    pub change_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_bytes: Option<u64>,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub notice: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrMergeQueueEntry {
    pub id: String,
    pub position: u64,
    pub state: String,
    pub enqueued_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_time_to_merge: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrMergeQueueStatus {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<PrMergeQueueEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrAutoMerge {
    pub method: String,
    pub enabled_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_by: Option<PrActor>,
    #[serde(default)]
    pub commit_title: String,
    #[serde(default)]
    pub commit_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrStackEntry {
    pub position: u64,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub base: String,
    pub head: String,
    #[serde(default)]
    pub review_decision: String,
    #[serde(default)]
    pub merge_state_status: String,
}

/// GitHub's native pull-request stack, when the host supports the current
/// GraphQL stack fields and this PR belongs to one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrStack {
    pub id: String,
    pub number: u64,
    pub size: u64,
    pub base: String,
    #[serde(default)]
    pub entries: Vec<PrStackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrCapabilities {
    #[serde(default)]
    pub can_update: bool,
    #[serde(default)]
    pub can_close: bool,
    #[serde(default)]
    pub can_reopen: bool,
    #[serde(default)]
    pub can_assign: bool,
    #[serde(default)]
    pub can_label: bool,
    #[serde(default)]
    pub can_merge_as_admin: bool,
    #[serde(default)]
    pub can_update_branch: bool,
    #[serde(default)]
    pub can_enable_auto_merge: bool,
    #[serde(default)]
    pub can_disable_auto_merge: bool,
    #[serde(default)]
    pub did_author: bool,
}

/// Full collaboration state for one selected PR. Account/session summary
/// projections remain compact; this is fetched lazily by the PR pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrDetailSection {
    /// PR metadata, capabilities, checks, and sidebar choices.
    Overview,
    /// Description, issue comments, reviews, and review threads.
    Conversation,
    /// Commit history.
    Commits,
    /// Changed-file metadata. Individual file content remains separately lazy.
    Files,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrDetail {
    pub info: PrInfo,
    /// Immutable base commit used with `info.head_sha` to load known files
    /// without re-listing the pull request's changed-file connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    /// Pull-request GraphQL node id used by typed server-side mutations.
    pub id: String,
    pub viewer: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub reactions: Vec<PrReactionSummary>,
    /// subscribed / unsubscribed / ignored
    #[serde(default)]
    pub viewer_subscription: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub commit_count: u64,
    #[serde(default)]
    pub review_decision: String,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub active_lock_reason: String,
    #[serde(default)]
    pub maintainer_can_modify: bool,
    pub capabilities: PrCapabilities,
    /// Merge methods enabled by repository settings (`merge`, `squash`,
    /// `rebase`).
    #[serde(default)]
    pub merge_methods: Vec<String>,
    #[serde(default)]
    pub default_merge_method: String,
    #[serde(default)]
    pub auto_merge_allowed: bool,
    #[serde(default)]
    pub labels: Vec<PrLabel>,
    #[serde(default)]
    pub available_labels: Vec<PrLabel>,
    #[serde(default)]
    pub assignees: Vec<PrActor>,
    #[serde(default)]
    pub assignable_users: Vec<PrActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<PrMilestone>,
    #[serde(default)]
    pub available_milestones: Vec<PrMilestone>,
    #[serde(default)]
    pub review_requests: Vec<PrActor>,
    #[serde(default)]
    pub reviews: Vec<PrReviewDetail>,
    #[serde(default)]
    pub comments: Vec<PrComment>,
    #[serde(default)]
    pub review_threads: Vec<PrReviewThread>,
    #[serde(default)]
    pub commits: Vec<PrCommit>,
    #[serde(default)]
    pub files: Vec<PrFile>,
    pub merge_queue: PrMergeQueueStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<PrAutoMerge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<PrStack>,
    /// True only when a safety cap prevented an unbounded GitHub connection
    /// from being returned in full.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrCommentKind {
    Issue,
    Review,
}

/// Typed PR-page actions. The server resolves every opaque target against the
/// selected session PR before contacting GitHub; OAuth tokens and arbitrary
/// GitHub API access never cross into the frontend.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PrActionRequest {
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maintainer_can_modify: Option<bool>,
    },
    SetState {
        /// draft / ready / close / reopen
        state: String,
    },
    RequestReviewers {
        #[serde(default)]
        users: Vec<String>,
        #[serde(default)]
        bots: Vec<String>,
        #[serde(default)]
        teams: Vec<String>,
        /// Replace the current request set instead of adding to it.
        #[serde(default)]
        replace: bool,
    },
    SubmitReview {
        /// approve / request_changes / comment
        event: String,
        #[serde(default)]
        body: String,
    },
    UpdateReview {
        id: String,
        body: String,
    },
    DeleteReview {
        id: String,
    },
    DismissReview {
        id: String,
        message: String,
    },
    AddComment {
        body: String,
    },
    UpdateComment {
        id: String,
        kind: PrCommentKind,
        body: String,
    },
    DeleteComment {
        id: String,
        kind: PrCommentKind,
    },
    ReplyReviewThread {
        thread_id: String,
        body: String,
    },
    ResolveReviewThread {
        thread_id: String,
        resolved: bool,
    },
    AddReviewThread {
        body: String,
        path: String,
        line: u64,
        /// left / right
        side: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_line: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_side: Option<String>,
    },
    SetFileViewed {
        path: String,
        viewed: bool,
    },
    UpdateBranch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_head_sha: Option<String>,
    },
    Merge {
        /// merge / squash / rebase
        method: String,
        #[serde(default)]
        commit_title: String,
        #[serde(default)]
        commit_message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_head_sha: Option<String>,
    },
    SetAutoMerge {
        enabled: bool,
        #[serde(default)]
        method: String,
        #[serde(default)]
        commit_title: String,
        #[serde(default)]
        commit_message: String,
    },
    SetMergeQueue {
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_head_sha: Option<String>,
    },
    SetLabels {
        #[serde(default)]
        label_ids: Vec<String>,
    },
    SetAssignees {
        #[serde(default)]
        assignee_ids: Vec<String>,
    },
    SetMilestone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        milestone_id: Option<String>,
    },
    SetLock {
        locked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    SetSubscription {
        /// subscribed / unsubscribed / ignored
        state: String,
    },
    AddReaction {
        subject_id: String,
        content: String,
    },
    RemoveReaction {
        subject_id: String,
        content: String,
    },
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

/// Subscription usage for one configured provider.
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
    /// Whether this definition participates in the effective MCP config.
    /// Older servers omit the field, which clients interpret as enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// "ok" / "error" / "unknown" (unknown when listing skipped the probe) /
    /// "untrusted" (a repo-scoped server that is never auto-run) /
    /// "disabled" (this definition is persistently disabled).
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
    /// Preserve a disabled definition while editing or importing it. Omitted
    /// requests retain the historical behavior of creating an enabled server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Persistently enable or disable an existing user-managed MCP definition.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetMcpServerEnabledRequest {
    /// "user" or "workspace".
    pub scope: String,
    /// Required for workspace scope: whose `.agents/.mcp.json` to edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub enabled: bool,
}

/// Recent stderr and lifecycle lines for one MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpLogs {
    pub lines: Vec<String>,
}

// --- integrations ----------------------------------------------------------

/// Whether the GitHub OAuth integration is authenticated.
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
    /// "oauth", or "" when unconfigured.
    pub source: String,
    /// A device-flow OAuth app client id is configured for this host.
    pub oauth_available: bool,
    /// Enterprise hosts can be removed; github.com cannot.
    pub removable: bool,
}

/// Register a self-hosted GitHub Enterprise instance
/// (`POST /v1/integrations/github/hosts`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddGithubHostRequest {
    /// Hostname only, e.g. "github.example.com".
    pub host: String,
    /// Client id of an OAuth app on that instance (device flow enabled).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
}

// --- automated code review -------------------------------------------------

/// Whether an installed repository participates in automated code review.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewMode {
    #[default]
    Off,
    /// Review only when a trusted collaborator comments `@trouve-ai review`
    /// on the pull request or explicitly requests the App bot as a reviewer.
    Manual,
    /// Review every new non-draft head SHA and manual comment/reviewer requests.
    Automatic,
}

/// Public GitHub App state. Private keys and webhook secrets are never
/// returned by the protocol.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct GithubAppStatus {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slug: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_login: String,
    #[serde(default)]
    pub webhook_configured: bool,
    /// Whether the installation token reports `checks: write`. Polling-only
    /// deployments still create and update Check Runs when this is true.
    #[serde(default)]
    pub checks_write_configured: bool,
    /// Whether `check_run` delivery is selected in the GitHub App. This is
    /// optional unless interactive Re-run actions are desired.
    #[serde(default)]
    pub check_run_webhook_configured: bool,
    #[serde(default)]
    pub installation_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_poll_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_remaining: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_reset_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Configure the GitHub App used for code reviews. The server validates the
/// private key against GitHub before replacing any stored credentials.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigureGithubAppRequest {
    pub app_id: u64,
    /// PEM-encoded RSA private key downloaded from the GitHub App settings.
    pub private_key_pem: String,
    /// Secret used to verify `X-Hub-Signature-256`. Empty disables webhooks
    /// and leaves reconciliation polling as the only trigger source.
    #[serde(default)]
    pub webhook_secret: String,
}

/// Configuration for one focused reviewer. Built-in profiles are shipped by
/// trouve; every profile may choose model and thinking defaults.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ReviewerProfile {
    pub id: String,
    pub name: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Preferred thinking level or fixed token budget for this reviewer. None
    /// inherits the review mode's default, then the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
    #[serde(default)]
    pub built_in: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerPromptMode {
    #[default]
    Inherit,
    Append,
    Replace,
}

/// How a repository chooses reviewer personas for each diff batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewRoutingMode {
    /// Run exactly the repository's manually selected `reviewer_ids`.
    #[serde(alias = "core")]
    Manual,
    /// Always run the baseline and `included_reviewer_ids`, then optionally
    /// add relevant personas through semantic routing.
    #[default]
    #[serde(alias = "auto")]
    Additive,
    /// Let the semantic router select from the complete persona catalog, with
    /// no baseline or manually selected core set.
    #[serde(alias = "thorough")]
    Automatic,
}

/// Repository-specific changes layered over a reusable reviewer profile.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ReviewerOverride {
    pub reviewer_id: String,
    /// Provider-qualified model. Absent means inherit the profile, which in
    /// turn may inherit the repository/default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Preferred thinking level or fixed token budget. Absent means inherit
    /// the reviewer profile, which in turn inherits the review mode default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub prompt_mode: ReviewerPromptMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
}

/// One repository visible to a GitHub App installation plus trouve's local
/// review policy for it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewRepository {
    pub installation_id: u64,
    pub repository: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub mode: CodeReviewMode,
    /// Provider-qualified model used by the coordinator and inherited by
    /// reviewers without an override. Required while reviews are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Preferred thinking level or fixed token budget for the final
    /// coordinator/editor. Absent inherits the review mode's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_thinking_level: Option<String>,
    /// Provider-qualified model used by semantic persona triage. Absent
    /// inherits `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_model: Option<String>,
    /// Preferred thinking level or fixed token budget for semantic persona
    /// triage. Absent inherits the review mode's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_thinking_level: Option<String>,
    /// Provider-qualified model used by the per-round implementation
    /// analyst. Absent inherits `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_model: Option<String>,
    /// Preferred thinking level or fixed token budget for the implementation
    /// analyst. Absent inherits the review mode's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_thinking_level: Option<String>,
    /// Extra repository-specific review instructions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// Ordered reviewer profiles run for each revision.
    #[serde(default)]
    pub reviewer_ids: Vec<String>,
    /// Persona-selection policy. Manual uses `reviewer_ids`; Additive and
    /// Automatic consider the complete reviewer catalog.
    #[serde(default)]
    pub routing_mode: CodeReviewRoutingMode,
    /// Whether Additive mode may run one tool-free semantic router pass per
    /// diff batch. Automatic mode always runs it as the sole selector.
    #[serde(default)]
    pub semantic_routing: bool,
    /// Personas that Additive mode always runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included_reviewer_ids: Vec<String>,
    /// Legacy forced exclusions retained for backward compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_reviewer_ids: Vec<String>,
    /// Per-reviewer repository overrides. Entries may be retained while a
    /// reviewer is disabled so re-enabling it restores its configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewer_overrides: Vec<ReviewerOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateCodeReviewRepositoryRequest {
    pub installation_id: u64,
    pub repository: String,
    pub mode: CodeReviewMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_thinking_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_thinking_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_thinking_level: Option<String>,
    #[serde(default)]
    pub prompt: String,
    /// Omitted by older clients to preserve the current/default selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_ids: Option<Vec<String>>,
    /// Omitted by older clients to preserve the current/default routing mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_mode: Option<CodeReviewRoutingMode>,
    /// Omitted by older clients to preserve the current/default semantic
    /// routing choice. Forced to `true` when `routing_mode` is Automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_routing: Option<bool>,
    /// Omitted by older clients to preserve existing forced inclusions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_reviewer_ids: Option<Vec<String>>,
    /// Omitted by older clients to preserve existing forced exclusions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_reviewer_ids: Option<Vec<String>>,
    /// Omitted by older clients to preserve existing reviewer overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_overrides: Option<Vec<ReviewerOverride>>,
}

/// Whether a job reviews only changes since the last successfully published
/// head, or the entire pull-request branch against its GitHub base.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewJobScope {
    #[default]
    Incremental,
    Full,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewProgress {
    pub completed_reviewers: u64,
    pub total_reviewers: u64,
    /// Integer percentage in the inclusive range 0..=100.
    pub percent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewTaskRole {
    Router,
    /// Per-round implementation analysis over the full-branch diff, derived
    /// fresh each round and consumed only by the coordinator.
    Analyst,
    Reviewer,
    Coordinator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewRoutingSource {
    Core,
    Baseline,
    /// Retained for decoding routing snapshots created before protocol 3.0.
    Deterministic,
    Semantic,
    Included,
    Thorough,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewRoutingReason {
    pub source: CodeReviewRoutingSource,
    pub detail: String,
}

/// Durable explanation of why one reviewer did or did not run for one batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewRoutingDecision {
    pub batch_index: u64,
    pub reviewer_id: String,
    pub reviewer_name: String,
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<CodeReviewRoutingReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewOutputStream {
    Assistant,
    Thinking,
    Tool,
}

/// Current lifecycle stage for an active review task, or the last observed
/// stage when a task fails or is cancelled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewTaskLifecycleStage {
    #[default]
    Queued,
    WaitingForCapacity,
    StartingModel,
    RunningModel,
    RunningTool,
    RepairingOutput,
    Completed,
}

/// Small, durable progress snapshot emitted while a review task is running.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewTaskProgress {
    pub lifecycle_stage: CodeReviewTaskLifecycleStage,
    pub provider_wait_ms: u64,
    pub model_elapsed_ms: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub tool_call_count: u64,
    #[serde(default)]
    #[schema(required = true)]
    pub model_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_progress_at: chrono::DateTime<chrono::Utc>,
}

/// One durable router, reviewer, or coordinator execution. Tasks survive
/// cleanup of their implementation sessions and threads.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewTask {
    pub id: String,
    pub job_id: String,
    pub role: CodeReviewTaskRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reviewer_name: String,
    #[serde(default)]
    pub batch_index: u64,
    #[serde(default)]
    pub batch_count: u64,
    /// `queued`, `running`, `succeeded`, `failed`, `cancelled`,
    /// `not_applicable`, or `superseded`.
    pub status: String,
    /// Current stage while active; last observed stage after failure or
    /// cancellation.
    #[serde(default)]
    pub lifecycle_stage: CodeReviewTaskLifecycleStage,
    /// The provider-qualified model actually used by the created thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_output: String,
    #[serde(default)]
    pub candidate_issue_count: u64,
    #[serde(default)]
    pub confirmed_issue_count: u64,
    /// Time spent waiting for shared/provider model capacity.
    #[serde(default)]
    pub provider_wait_ms: u64,
    /// Wall time from model dispatch through tool iterations and completion.
    #[serde(default)]
    pub model_elapsed_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub tool_call_count: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Current elapsed time for active tasks and final elapsed time for
    /// terminal tasks, as measured by the server.
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// Reviewer-level rollup across one or more diff batches.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewPersonaResult {
    pub reviewer_id: String,
    pub reviewer_name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default)]
    pub completed_batches: u64,
    #[serde(default)]
    pub total_batches: u64,
    #[serde(default)]
    pub candidate_issue_count: u64,
    #[serde(default)]
    pub confirmed_issue_count: u64,
    #[serde(default)]
    pub provider_wait_ms: u64,
    #[serde(default)]
    pub model_elapsed_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub tool_call_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// A persona/candidate that contributed to a confirmed finding.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewFindingSource {
    pub reviewer_id: String,
    pub reviewer_name: String,
    pub candidate_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
}

/// A reviewer candidate that the final editor chose not to publish.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewCandidateRejection {
    pub candidate_id: String,
    pub task_id: String,
    pub reviewer_id: String,
    pub reviewer_name: String,
    pub path: String,
    pub line: u64,
    pub side: String,
    pub severity: String,
    /// Strength of the evidence for the candidate, independently of impact.
    /// `high`, `medium`, or `low`; legacy records default to `medium`.
    #[serde(default = "default_code_review_confidence")]
    pub confidence: String,
    /// Concise, generated one-line summary of the candidate issue.
    pub title: String,
    pub body: String,
    pub reason: String,
}

/// A reviewer candidate the final editor neither retained nor substantively
/// rejected. This represents incomplete coordinator work, not a negative
/// decision about the candidate.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewUnadjudicatedCandidate {
    pub candidate_id: String,
    pub task_id: String,
    pub reviewer_id: String,
    pub reviewer_name: String,
    pub path: String,
    pub line: u64,
    pub side: String,
    pub severity: String,
    /// Strength of the reviewer evidence, independently of impact.
    /// `high`, `medium`, or `low`; legacy records default to `medium`.
    #[serde(default = "default_code_review_confidence")]
    pub confidence: String,
    /// Concise, generated one-line summary of the candidate issue.
    pub title: String,
    pub body: String,
}

/// One step of the causal chain from changed code to a finding's anchor,
/// quoted by the coordinator and mechanically verified against the reviewed
/// revision. A finding anchored outside the diff can only block the review
/// when its waypoints verify and at least one lies on a changed line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewCausalWaypoint {
    pub path: String,
    pub line: u64,
    /// Verbatim source line at `path:line`, from the head revision.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote: String,
}

/// Concrete evidence that makes a confirmed finding independently verifiable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewFindingEvidence {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preconditions: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub execution_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub consequence: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub introduction: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub regression_test: String,
    /// Verbatim source line at the finding's anchor, quoted by the
    /// coordinator while verifying the finding. Mechanically matched against
    /// the reviewed revision; empty when the anchor was never verified.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub anchor_quote: String,
    /// Server-derived verdict for `anchor_quote`: `matched`, `mismatched`,
    /// or `unchecked`. Model-provided values are overwritten.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub anchor_match: String,
    /// The coordinator's grade of how much of `execution_path` it verified
    /// against the repository: `verified`, `partial`, or `unverified`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub execution_path_verification: String,
    /// The refuting guard, caller, or test the coordinator searched for to
    /// disprove the finding, and what it found. Empty when no refutation was
    /// attempted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub counterexample_search: String,
    /// Coordinator's causation claim for the finding: `introduced` when this
    /// change caused the issue, `pre_existing` when the issue predates it and
    /// is surfaced for awareness only. Empty on legacy records.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub change_causation: String,
    /// The causal chain from changed code to the finding's anchor, required
    /// to verify an `introduced` claim whose anchor is outside the diff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causal_waypoints: Vec<CodeReviewCausalWaypoint>,
    /// Server-derived scope verdict: `verified` when the causation claim is
    /// mechanically corroborated (anchor on a changed line, or verified
    /// waypoints reaching one), `unverified` otherwise. Only scope-verified
    /// findings block the review. Model-provided values are overwritten;
    /// empty legacy records block as before.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub change_scope: String,
}

/// How a finding relates to earlier review rounds on the pull request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewFindingOrigin {
    #[default]
    NewChange,
    Recurrence,
    FixRegression,
    PreviouslyMissed,
}

/// How a durable root-cause theme appeared in one review round.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewThemeObservationKind {
    #[default]
    New,
    Continuation,
    Recurrence,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewThemeObservation {
    pub job_id: String,
    pub head_sha: String,
    pub kind: CodeReviewThemeObservationKind,
    pub finding_ids: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A root cause tracked across all review rounds for one pull request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewTheme {
    pub id: String,
    pub repository: String,
    pub pull_number: u64,
    pub root_cause: String,
    pub recommendation: String,
    /// `pending` while the producing review is unpublished, `open` while at
    /// least one authoritative linked finding is open, otherwise `resolved`.
    pub status: String,
    pub first_seen_head: String,
    pub last_seen_head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolved_head: String,
    #[serde(default)]
    pub recurrence_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finding_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<CodeReviewThemeObservation>,
}

/// The outcome of attempting to publish a finding as an inline GitHub comment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewFindingPublicationStatus {
    /// Publication has not completed or its legacy outcome is unknown.
    #[default]
    Pending,
    /// GitHub accepted the inline comment. Its URL may still be unavailable.
    Published,
    /// The finding had no valid path/line pair for an inline comment.
    NotEligible,
    /// The finding was retained internally but did not meet the automatic
    /// publication threshold for its severity and confidence.
    SuppressedByPolicy,
    /// The symptom is retained in Trouve but represented on GitHub by the
    /// primary comment for its shared root-cause theme.
    GroupedByTheme,
    /// GitHub did not publish the inline comment.
    Failed,
}

/// A confirmed issue produced by the coordinator. Findings on commentable
/// diff lines are published as inline GitHub review comments; findings whose
/// strongest valid anchor is unchanged code are published in the review body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewFinding {
    pub id: String,
    pub job_id: String,
    pub path: String,
    pub line: u64,
    pub side: String,
    /// The finding is anchored to a head-revision line that GitHub cannot
    /// represent as an inline pull-request diff comment.
    #[serde(default)]
    pub outside_diff: bool,
    pub severity: String,
    /// Strength of the evidence for the issue, independently of impact.
    /// `high`, `medium`, or `low`; legacy records default to `medium`.
    #[serde(default = "default_code_review_confidence")]
    pub confidence: String,
    /// Concise, generated one-line summary of the issue.
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_for_agents: String,
    /// `open`, `fixed`, or `dismissed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<CodeReviewFindingSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_comment_id: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub github_comment_url: String,
    #[serde(default)]
    pub github_publication_status: CodeReviewFindingPublicationStatus,
    #[serde(default)]
    pub evidence: CodeReviewFindingEvidence,
    #[serde(default)]
    pub origin: CodeReviewFindingOrigin,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub theme_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Immutable PR head on which this finding was first observed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub observed_head: String,
    /// Immutable PR head whose review demonstrated that the finding was fixed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolved_head: String,
    /// Review job that demonstrated the fix, for exact fix-diff reconstruction.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolved_by_job_id: String,
}

fn default_code_review_confidence() -> String {
    "medium".into()
}

/// A durable execution of one model review against one immutable PR head.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewJob {
    pub id: String,
    pub installation_id: u64,
    pub repository: String,
    pub pull_number: u64,
    pub pull_title: String,
    pub pull_url: String,
    pub head_sha: String,
    /// Commit used as the left side of this review's diff. For incremental
    /// jobs this is normally the last successfully published head.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub review_base_sha: String,
    /// Immutable commit from the last successfully published review. This is
    /// the incremental watermark even when history rewriting makes the
    /// effective `review_base_sha` fall back to the pull request merge base.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub review_watermark_sha: String,
    /// Whether this round's diff spanned the entire branch, recorded at the
    /// moment the diff base was resolved. `review_base_sha` can be refined to
    /// the pull-request merge base during execution, so comparing it against
    /// `base_ref` misclassifies full first reviews whenever the base branch
    /// advanced past the branch point; this flag is authoritative. None on
    /// rounds that predate it (clients fall back to the sha comparison).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_full_branch: Option<bool>,
    pub base_ref: String,
    pub head_ref: String,
    #[serde(default)]
    pub scope: CodeReviewJobScope,
    /// `automatic`, `manual`, or `retry`.
    pub trigger: String,
    /// `queued`, `running`, `succeeded`, `failed`, `cancelled`, or `stale`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Thinking level snapshotted for the final coordinator/editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_thinking_level: Option<String>,
    /// Model snapshotted for semantic persona triage. Absent inherits
    /// `model`; legacy jobs may omit both and are rejected before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_model: Option<String>,
    /// Thinking level snapshotted for semantic persona triage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_thinking_level: Option<String>,
    /// Model snapshotted for the per-round implementation analyst. Absent
    /// inherits `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_model: Option<String>,
    /// Thinking level snapshotted for the implementation analyst.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_thinking_level: Option<String>,
    /// Reviewer profiles are snapshotted internally; their stable ids are
    /// exposed here for history and diagnostics. Additive/Automatic jobs
    /// snapshot the candidate catalog; routing decisions record which
    /// personas ran.
    #[serde(default)]
    pub reviewer_ids: Vec<String>,
    #[serde(default)]
    pub routing_mode: CodeReviewRoutingMode,
    /// Snapshotted Additive semantic-routing choice. Automatic jobs route
    /// semantically regardless of a legacy `false` value.
    #[serde(default)]
    pub semantic_routing: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included_reviewer_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_reviewer_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub review_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lifecycle_comment_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_run_id: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub check_run_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub check_sync_error: String,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub progress: CodeReviewProgress,
    #[serde(default)]
    pub candidate_issue_count: u64,
    #[serde(default)]
    pub issue_count: u64,
    #[serde(default)]
    pub fixed_issue_count: u64,
    /// Blocking confirmed findings (high severity, or medium severity with
    /// at least medium confidence) that remained open across the pull
    /// request after this review was published. Only these gate the check
    /// run. Absent while publication is pending and for legacy jobs that
    /// predate this snapshot. Consumers must treat absence on a succeeded
    /// job as unknown, never as a clean review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_issue_count: Option<u64>,
    /// Advisory findings (low severity, or medium severity with low
    /// confidence) still open across the pull request: durable engineering
    /// debt recorded in trouve, never posted to GitHub and never
    /// merge-blocking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_open_issue_count: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub pending_elapsed_ms: u64,
    #[serde(default)]
    pub running_elapsed_ms: u64,
    #[serde(default)]
    pub preparation_elapsed_ms: u64,
    #[serde(default)]
    pub reviewer_elapsed_ms: u64,
    #[serde(default)]
    pub coordinator_elapsed_ms: u64,
    #[serde(default)]
    pub publication_elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewJobDetail {
    pub job: CodeReviewJob,
    /// Latest persisted job event included by or predating this snapshot.
    /// Clients can resume the job event stream after this cursor instead of
    /// replaying the complete retained output history.
    #[serde(default)]
    pub event_cursor: u64,
    #[serde(default)]
    pub tasks: Vec<CodeReviewTask>,
    #[serde(default)]
    pub personas: Vec<CodeReviewPersonaResult>,
    #[serde(default)]
    pub findings: Vec<CodeReviewFinding>,
    #[serde(default)]
    pub themes: Vec<CodeReviewTheme>,
    #[serde(default)]
    pub candidate_rejections: Vec<CodeReviewCandidateRejection>,
    /// Candidates left without a final-editor decision after the bounded
    /// repair attempt. Their presence means the review is incomplete.
    #[serde(default)]
    pub unadjudicated_candidates: Vec<CodeReviewUnadjudicatedCandidate>,
    #[serde(default)]
    pub routing_decisions: Vec<CodeReviewRoutingDecision>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_for_agents: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewJobList {
    pub jobs: Vec<CodeReviewJob>,
}

/// Manual review request. Full scope always compares the current head with
/// the pull request's GitHub base; incremental scope uses the saved watermark
/// when it remains valid.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RequestCodeReviewRequest {
    pub installation_id: u64,
    pub repository: String,
    pub pull_number: u64,
    #[serde(default)]
    pub scope: CodeReviewJobScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeReviewStatsRange {
    Hour,
    #[default]
    Day,
    Week,
    Month,
    Year,
    All,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewStatusCounts {
    pub queued: u64,
    pub running: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub stale: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewDurationStats {
    pub samples: u64,
    pub average_ms: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub maximum_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewStatsBucket {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub status: CodeReviewStatusCounts,
    #[serde(default)]
    pub issue_count: u64,
    #[serde(default)]
    pub pending_average_ms: u64,
    #[serde(default)]
    pub running_average_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewPersonaModelStats {
    pub reviewer_id: String,
    pub reviewer_name: String,
    pub model: String,
    pub task_count: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    #[serde(default)]
    pub not_applicable: u64,
    pub candidate_issue_count: u64,
    pub confirmed_issue_count: u64,
    #[serde(default)]
    pub duration: CodeReviewDurationStats,
    #[serde(default)]
    pub provider_wait_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub model_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub tool_call_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewRepositoryStats {
    pub repository: String,
    #[serde(default)]
    pub status: CodeReviewStatusCounts,
    pub issue_count: u64,
    #[serde(default)]
    pub pending_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub running_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub preparation_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub reviewer_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub coordinator_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub publication_duration: CodeReviewDurationStats,
}

/// Signals that measure repeated review work rather than raw issue volume.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewChurnStats {
    #[serde(default)]
    pub recurrence_issue_count: u64,
    #[serde(default)]
    pub fix_regression_issue_count: u64,
    #[serde(default)]
    pub previously_missed_issue_count: u64,
    #[serde(default)]
    pub grouped_issue_count: u64,
    #[serde(default)]
    pub external_duplicate_count: u64,
    #[serde(default)]
    pub insufficient_evidence_rejection_count: u64,
    #[serde(default)]
    pub pull_request_count: u64,
    #[serde(default)]
    pub clean_pull_request_count: u64,
    #[serde(default)]
    pub average_rounds_to_clean: f64,
    #[serde(default)]
    pub max_rounds_to_clean: u64,
}

/// Auto-resolve worker backlog for finding threads.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewCollapseBacklog {
    /// Findings marked for thread resolution that the worker has not
    /// completed yet.
    pub pending: u64,
    /// Age of the oldest pending entry, from when its finding was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_pending_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewStats {
    pub range: CodeReviewStatsRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// Current active work plus terminal outcomes in the selected range.
    #[serde(default)]
    pub status: CodeReviewStatusCounts,
    #[serde(default)]
    pub pending_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub running_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub preparation_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub reviewer_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub coordinator_duration: CodeReviewDurationStats,
    #[serde(default)]
    pub publication_duration: CodeReviewDurationStats,
    pub issue_count: u64,
    #[serde(default)]
    pub churn: CodeReviewChurnStats,
    /// Fixed or dismissed findings whose GitHub threads still await the
    /// auto-resolve worker. A growing or aging backlog means resolved
    /// findings look unresolved on GitHub and thread-driven flows lag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_collapse_backlog: Option<CodeReviewCollapseBacklog>,
    #[serde(default)]
    pub buckets: Vec<CodeReviewStatsBucket>,
    #[serde(default)]
    pub personas: Vec<CodeReviewPersonaModelStats>,
    #[serde(default)]
    pub repositories: Vec<CodeReviewRepositoryStats>,
}

/// Historical default for simultaneously running automated review jobs.
pub const DEFAULT_MAX_PARALLEL_REVIEWS: u32 = 2;
/// Hard safety ceiling for simultaneously running automated review jobs.
pub const MAX_PARALLEL_REVIEWS: u32 = 32;

const fn default_max_parallel_reviews() -> u32 {
    DEFAULT_MAX_PARALLEL_REVIEWS
}

/// Persisted execution settings for automated code reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewSettings {
    /// Maximum number of review jobs that may execute concurrently.
    #[serde(default = "default_max_parallel_reviews")]
    #[schema(minimum = 1, maximum = 32, default = 2)]
    pub max_parallel_reviews: u32,
    /// Whole-job deadline, including preparation, reviewers, final editing,
    /// and publication.
    #[schema(minimum = 1)]
    pub total_timeout_seconds: u64,
    /// Deadline for one reviewer persona batch.
    #[schema(minimum = 1)]
    pub reviewer_timeout_seconds: u64,
    /// Deadline for the final review editor.
    #[schema(minimum = 1)]
    pub coordinator_timeout_seconds: u64,
}

/// Replace the persisted automated code-review execution settings
/// (`PUT /v1/config/code-review`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetCodeReviewSettingsRequest {
    /// Maximum number of review jobs that may execute concurrently. Omission
    /// preserves the current value for compatibility with older clients;
    /// values above 32 are accepted and normalized to 32 in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1)]
    pub max_parallel_reviews: Option<u32>,
    /// Whole-job deadline, including preparation, reviewers, final editing,
    /// and publication.
    #[schema(minimum = 1)]
    pub total_timeout_seconds: u64,
    /// Deadline for one reviewer persona batch.
    #[schema(minimum = 1)]
    pub reviewer_timeout_seconds: u64,
    /// Deadline for the final review editor.
    #[schema(minimum = 1)]
    pub coordinator_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeReviewDashboard {
    pub app: GithubAppStatus,
    pub reviewers: Vec<ReviewerProfile>,
    pub repositories: Vec<CodeReviewRepository>,
    pub jobs: Vec<CodeReviewJob>,
    /// Job ids for which the server will accept a scoped retry of the latest
    /// failed or cancelled final-editor attempt while retaining reviewer output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub final_editor_retryable_job_ids: Vec<String>,
}

// --- branches --------------------------------------------------------------

/// Local branches of a workspace repository, for base-ref selection.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BranchList {
    pub branches: Vec<String>,
    /// The branch or commit HEAD currently points at.
    pub head: String,
    /// The default branch advertised by the repository's origin remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

// --- provider configuration -------------------------------------------------

/// A configured provider, with secrets elided.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderInfo {
    /// Stable identifier, e.g. "openai" or "openrouter".
    pub id: String,
    /// Provider transport family.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Non-secret values used to expand the provider's endpoint and request
    /// templates. Secret values are intentionally never returned.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub settings: std::collections::BTreeMap<String, String>,
    /// Whether credentials are configured or delegated to a cloud credential
    /// chain. Native cloud credentials are validated on first request.
    pub has_credentials: bool,
    /// "api-key", "oauth", "cli", "aws", "gcp", or "none" — drives which
    /// credential UI to show.
    pub auth: String,
    /// Presentation/billing category: "subscription", "api", or "local".
    /// This is independent of `auth`: a subscription such as Kimi Code can
    /// still authenticate with an API key.
    #[serde(default = "default_provider_category")]
    pub category: String,
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
    /// Transport family, such as "openai-compat", "anthropic",
    /// "azure-openai", "amazon-bedrock", "google-vertex", or
    /// "google-vertex-anthropic".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Non-secret `${NAME}` template values.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub settings: std::collections::BTreeMap<String, String>,
    /// Named secrets to store. Omitted entries retain their existing value;
    /// these values are write-only and never appear in provider responses.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub secret_values: std::collections::BTreeMap<String, String>,
    /// Additional HTTP header templates for compatible transports. Values
    /// may reference `${API_KEY}`, settings, named secrets, or
    /// catalog-declared environment-backed settings.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Additional query-parameter templates, with the same expansion rules
    /// as `headers`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub query_params: std::collections::BTreeMap<String, String>,
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

/// Atomically replace the global defaults used by new threads
/// (`PUT /v1/config/defaults`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetGlobalDefaultsRequest {
    /// Provider-qualified id, e.g. "openai/gpt-4.1-mini".
    pub model: String,
    /// Global thinking level for the selected model. None clears the default
    /// so the model chooses its own setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
    pub permission_mode: PermissionMode,
}

/// Set the global default permission mode
/// (`PUT /v1/config/default-permission-mode`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetDefaultPermissionModeRequest {
    pub permission_mode: PermissionMode,
}

/// One configuration field advertised by a well-known provider preset.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderConfigField {
    /// Placeholder name used in templates and in `UpsertProviderRequest`.
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Conventional environment-variable fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default)]
    pub required: bool,
    /// Secret fields are written to the secret store and never returned.
    #[serde(default)]
    pub secret: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
}

/// A well-known provider preset: clients offer these for one-click setup
/// instead of hand-typed base URLs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KnownProvider {
    /// Suggested provider id, e.g. "openrouter".
    pub id: String,
    pub display_name: String,
    /// Provider transport family.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Conventional environment variable holding the API key, when one
    /// exists (empty for keyless local providers like Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Provider-specific setup fields rendered by clients. Their values fill
    /// placeholders in `base_url`, headers, and query parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_fields: Vec<ProviderConfigField>,
    /// Safe templates only; no credential values are returned here.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub query_params: std::collections::BTreeMap<String, String>,
    /// How the provider authenticates: "api-key", "oauth" (subscription
    /// login), "cli" (the vendor's own CLI holds subscription auth), "aws"
    /// or "gcp" (ambient cloud credential chains), or "none" (keyless local
    /// endpoints).
    pub auth: String,
    /// Presentation/billing category: "subscription", "api", or "local".
    #[serde(default = "default_provider_category")]
    pub category: String,
    /// Uses an undocumented vendor endpoint that may break or be restricted
    /// at any time; clients should display a warning.
    #[serde(default)]
    pub experimental: bool,
}

fn default_provider_category() -> String {
    "api".into()
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

/// Browser callback URL/code pasted into a CLI login running on another host.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CompleteLoginRequest {
    pub callback_url: String,
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
/// workspace, a thread with the configured persona/model, and sends the
/// prompt — exactly as if the user had typed it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub workspace_id: WorkspaceId,
    /// Agent persona for the runs (None = the default persona).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Model for the runs (None = the persona's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Thinking level for the runs (None = the selected model/persona/global
    /// default). The engine maps this canonical value to the model's
    /// advertised option key when the turn starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// Model-specific values selected from the model's `options_schema`.
    /// `thinking_level` remains as a compatibility shorthand; values in this
    /// object take precedence when both select the same model capability.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    #[schema(value_type = std::collections::BTreeMap<String, ModelOptionValue>)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
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
    /// Thinking level for each fresh automation thread. Omitted by older
    /// clients preserves normal model/mode/global inheritance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// Model-specific values selected from the model's `options_schema`.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    #[schema(value_type = std::collections::BTreeMap<String, ModelOptionValue>)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
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
pub struct ModelUsageSummary {
    pub model: String,
    pub turns: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UsageSummary {
    pub turns: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelUsageSummary>,
}

// --- errors --------------------------------------------------------------

/// Uniform error body for non-2xx responses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_fetch_latest_defaults_to_true() {
        let request: CreateSessionRequest =
            serde_json::from_value(serde_json::json!({ "workspace_id": "ws_test" })).unwrap();

        assert!(request.fetch_latest);
        assert!(request.checkout_ref.is_none());
    }

    #[test]
    fn title_resources_default_to_the_historical_cpu_mode() {
        let request: SetGitWorktreeSettingsRequest = serde_json::from_value(serde_json::json!({
            "title_model_load_behavior": "auto"
        }))
        .unwrap();

        assert_eq!(
            request.title_model_resource_policy,
            TitleModelResourcePolicy::CpuRamOnly
        );
        assert_eq!(request.derive_branch_name_from_session_title, None);

        let historical: GitWorktreeSettings = serde_json::from_value(serde_json::json!({
            "title_model_load_behavior": "auto",
            "title_model": {
                "state": "not_installed",
                "runtime_installed": false,
                "model_downloaded": false
            }
        }))
        .unwrap();
        assert!(!historical.derive_branch_name_from_session_title);
    }

    #[test]
    fn thread_view_preserves_skipped_question_resolution() {
        let item = ThreadViewItem::Questions {
            request_id: "q1".into(),
            title: None,
            questions: Vec::new(),
            resolved: true,
            answers: None,
        };

        let round_trip: ThreadViewItem =
            serde_json::from_value(serde_json::to_value(&item).unwrap()).unwrap();
        assert_eq!(round_trip, item);
    }

    #[test]
    fn persona_selection_modes_accept_legacy_names_and_serialize_canonically() {
        for (legacy, mode, canonical) in [
            ("core", CodeReviewRoutingMode::Manual, "manual"),
            ("auto", CodeReviewRoutingMode::Additive, "additive"),
            ("thorough", CodeReviewRoutingMode::Automatic, "automatic"),
        ] {
            assert_eq!(
                serde_json::from_value::<CodeReviewRoutingMode>(serde_json::json!(legacy)).unwrap(),
                mode
            );
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::json!(canonical)
            );
        }
    }

    #[test]
    fn code_review_task_progress_serializes_a_model_clock_reset_as_null() {
        let progress = CodeReviewTaskProgress {
            lifecycle_stage: CodeReviewTaskLifecycleStage::RepairingOutput,
            provider_wait_ms: 0,
            model_elapsed_ms: 42,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            tool_call_count: 0,
            model_started_at: None,
            last_progress_at: chrono::Utc::now(),
        };

        let value = serde_json::to_value(progress).unwrap();
        assert_eq!(
            value.get("model_started_at"),
            Some(&serde_json::Value::Null)
        );
    }
}
