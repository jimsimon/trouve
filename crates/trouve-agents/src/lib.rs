//! External agent backends: vendor coding agents (Codex, Cursor, Claude
//! Code) driven through their sanctioned CLI/JSON interfaces, running inside
//! trouve's session worktrees.
//!
//! Unlike a `trouve_providers::Provider` (raw model inference inside
//! trouve's own agent loop), an [`AgentBackend`] owns the whole turn: the
//! vendor harness plans, calls its own tools, and edits files. Trouve
//! translates its event stream into the trouve protocol and bridges its
//! approval requests through the engine's permission layer. Subscription
//! auth stays inside the vendor binary — we never touch vendor OAuth tokens.

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod install;
mod login;
pub mod process_env;
mod route;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use trouve_protocol::{ModelInfo, Usage};

pub use login::{spawn_claude_login, spawn_codex_login, spawn_login};

/// Permission posture for a backend turn, folded down from trouve's
/// permission mode + agent mode (read-only) for the thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendPermission {
    /// The turn must not mutate the worktree (plan/review modes).
    ReadOnly,
    /// Mutations need approval; the backend surfaces them as
    /// [`BackendEvent::ApprovalNeeded`] where its protocol supports it.
    Ask,
    /// Run everything without prompting.
    Yolo,
}

/// Everything a backend needs to run one turn.
#[derive(Debug)]
pub struct BackendTurn {
    /// Cooperative cancellation for every phase of this vendor turn. An
    /// adapter must not finish its stream after observing cancellation until
    /// any vendor request/process cleanup that protects a replacement turn is
    /// complete.
    pub cancel: tokio_util::sync::CancellationToken,
    pub thread_id: String,
    /// Session worktree the vendor agent operates in.
    pub worktree: PathBuf,
    /// Vendor-side session id from a previous turn on this thread, if any.
    pub session: Option<String>,
    /// Bare model name (provider prefix already stripped); empty = default.
    pub model: String,
    /// Values for the model's options schema (thinking level, fast, ...).
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub prompt: String,
    /// Image attachments riding with the prompt, already stored as files by
    /// the engine. Sent as native image inputs where the vendor protocol
    /// supports them (non-image uploads are referenced by path inside
    /// `prompt` instead — the engine handles that).
    pub attachments: Vec<TurnAttachment>,
    /// Trouve mode prompt, appended to the vendor's own system prompt where
    /// the vendor protocol allows.
    pub instructions: Option<String>,
    pub permission: BackendPermission,
    /// Request a tool-free turn. The engine omits mounted MCP tools and
    /// rejects reported tool use; adapters also disable vendor built-ins
    /// where their protocol supports it.
    pub tool_free: bool,
    /// When set, the vendor agent runs with its built-in tools disabled and
    /// trouve's ToolExecutor bridged in over MCP (Claude Code only, v1).
    pub mcp_bridge: Option<McpBridgeConfig>,
    /// User-configured MCP servers (user/workspace/worktree scopes, already
    /// merged and env-expanded by the engine) to mount alongside the bridge.
    pub mcp_servers: Vec<McpServerLaunch>,
}

/// Additional user input for the vendor turn currently running in a resumed
/// backend session. The engine serializes this with durable transcript output
/// before acknowledging the steering request.
#[derive(Debug)]
pub struct BackendSteer {
    /// Cancels an in-flight steering request with its owning turn.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Vendor-side thread/session id that owns the active turn.
    pub session: String,
    pub prompt: String,
    /// Image attachments resolved to local files; non-image attachments are
    /// already represented by paths in `prompt`.
    pub attachments: Vec<TurnAttachment>,
}

/// One prompt attachment whose bytes were verified and copied through the
/// engine's trusted filesystem boundary before entering vendor code.
#[derive(Debug, Clone)]
pub struct TurnAttachment {
    /// Display name from the upload ("screenshot.png").
    pub name: String,
    /// MIME type ("image/png").
    pub mime: String,
    /// Owned bytes for protocols that embed image data.
    pub bytes: Arc<[u8]>,
    /// Opaque, worktree-local path for vendors that require a local-image
    /// filename. This never names the durable attachment store.
    pub local_path: Option<std::path::PathBuf>,
}

impl TurnAttachment {
    /// The file's bytes as standard base64, for protocols that embed image
    /// data instead of referencing paths.
    pub fn base64(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&self.bytes)
    }
}

/// One user-configured stdio MCP server, ready to launch (env `${VAR}`
/// references already expanded).
#[derive(Debug, Clone)]
pub struct McpServerLaunch {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Streamable-HTTP MCP server the vendor agent connects to in order to
/// reach trouve (the engine's internal per-thread MCP endpoint). Always
/// used for approval prompting in Ask mode; normally also supplies every
/// mutation-capable tool so the engine can enforce worktree serialization.
#[derive(Debug, Clone)]
pub struct McpBridgeConfig {
    /// Full endpoint URL, thread-scoped, with the tool/approval surface
    /// selected via query parameters.
    pub url: String,
    /// When true the bridge serves trouve's ToolExecutor tools and vendor
    /// mutations are disabled or sandbox-confined; when false it only serves
    /// the approval-prompt gate.
    pub bridge_tools: bool,
    /// Vendor built-in tools to disable while the bridge supplies tools.
    pub disallowed_tools: Vec<String>,
}

/// One event from a backend turn, in trouve-shaped vocabulary.
pub enum BackendEvent {
    /// The vendor allocated (or rotated) its session id; persist it so the
    /// next turn resumes the same conversation.
    SessionStarted {
        session_id: String,
    },
    TextDelta(String),
    /// User-facing progress authored by the vendor harness.
    ProgressDelta(String),
    /// The vendor harness explicitly closed its current progress item.
    ProgressCompleted,
    /// Reasoning ("thinking") text, where the vendor harness exposes it.
    ThinkingDelta(String),
    /// The vendor harness explicitly closed its current thinking item.
    ThinkingCompleted,
    ToolStarted {
        call_id: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolOutput {
        call_id: String,
        chunk: String,
    },
    ToolCompleted {
        call_id: String,
        ok: bool,
        result: serde_json::Value,
    },
    /// The vendor harness paused for approval. Send `true` to allow.
    ApprovalNeeded {
        call_id: String,
        tool: String,
        args: serde_json::Value,
        responder: tokio::sync::oneshot::Sender<bool>,
    },
    /// The vendor harness asked the user questions and blocked its turn on
    /// the answers. Send `None` when the user skips.
    QuestionsNeeded {
        request_id: String,
        title: Option<String>,
        questions: Vec<trouve_protocol::Question>,
        responder: tokio::sync::oneshot::Sender<Option<Vec<trouve_protocol::QuestionAnswer>>>,
    },
    /// The vendor harness announced the slash commands / skills it accepts
    /// in prompts (cursor sends these per session; claude lists them at
    /// init). Replaces any earlier list.
    CommandsUpdated {
        commands: Vec<trouve_protocol::CommandInfo>,
    },
    /// The vendor harness replaced its current plan. Unlike a tool call,
    /// this is durable thread state and should not render as transcript
    /// activity; the core publishes the canonical todo snapshot separately.
    TodosUpdated {
        todos: Vec<trouve_protocol::TodoItem>,
    },
    /// Usage for the most recently completed model request while the vendor
    /// turn is still running. Final turn aggregates arrive in `Completed`.
    UsageUpdated {
        usage: Usage,
    },
    /// The vendor harness began compacting its own conversation context.
    CompactionStarted,
    /// The vendor harness finished compacting its own conversation context.
    CompactionCompleted,
    /// The vendor harness finished a compaction item unsuccessfully.
    CompactionFailed,
    /// A vendor-native collaborator became part of this turn. The session
    /// ids are vendor thread ids: core maps them onto durable trouve threads
    /// and persists the mapping so the collaborator can be resumed directly.
    CollaboratorStarted {
        session_id: String,
        parent_session_id: String,
        /// Provider-owned human-readable collaborator name, when the harness
        /// exposes one. Core preserves this ahead of prompt-derived naming.
        name: Option<String>,
        prompt: Option<String>,
        model: Option<String>,
        thinking_level: Option<String>,
        /// Whether the harness describes this child as transcript-only or as
        /// an interactive worker. Unknown roles inherit their parent's mode.
        access: BackendCollaboratorAccess,
    },
    /// One event produced by a vendor-native collaborator. Keeping the child
    /// vocabulary separate prevents a nested collaborator from accidentally
    /// completing or mutating its parent's turn.
    CollaboratorEvent {
        session_id: String,
        turn_id: Option<String>,
        event: BackendCollaboratorEvent,
    },
    Completed {
        usage: Usage,
    },
}

/// Interaction contract advertised for a vendor-native collaborator.
///
/// This is intentionally separate from provider-specific role names. Core
/// maps it onto the workspace's data-driven modes when materializing the
/// durable child thread.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BackendCollaboratorAccess {
    #[default]
    Inherit,
    ReadOnly,
    Interactive,
}

/// Stream events scoped to one vendor-native collaborator.
pub enum BackendCollaboratorEvent {
    TurnStarted,
    UserMessage(String),
    TextDelta(String),
    ProgressDelta(String),
    ProgressCompleted,
    ThinkingDelta(String),
    ThinkingCompleted,
    ToolStarted {
        call_id: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolOutput {
        call_id: String,
        chunk: String,
    },
    ToolCompleted {
        call_id: String,
        ok: bool,
        result: serde_json::Value,
    },
    ApprovalNeeded {
        call_id: String,
        tool: String,
        args: serde_json::Value,
        responder: tokio::sync::oneshot::Sender<bool>,
    },
    TodosUpdated {
        todos: Vec<trouve_protocol::TodoItem>,
    },
    UsageUpdated {
        usage: Usage,
    },
    CompactionStarted,
    CompactionCompleted,
    CompactionFailed,
    Completed {
        usage: Usage,
    },
    Failed {
        error: String,
    },
}

impl std::fmt::Debug for BackendCollaboratorEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TurnStarted => f.write_str("TurnStarted"),
            Self::UserMessage(text) => write!(f, "UserMessage({text:?})"),
            Self::TextDelta(text) => write!(f, "TextDelta({text:?})"),
            Self::ProgressDelta(text) => write!(f, "ProgressDelta({text:?})"),
            Self::ProgressCompleted => f.write_str("ProgressCompleted"),
            Self::ThinkingDelta(text) => write!(f, "ThinkingDelta({text:?})"),
            Self::ThinkingCompleted => f.write_str("ThinkingCompleted"),
            Self::ToolStarted { call_id, tool, .. } => {
                write!(f, "ToolStarted({call_id}, {tool})")
            }
            Self::ToolOutput { call_id, .. } => write!(f, "ToolOutput({call_id})"),
            Self::ToolCompleted { call_id, ok, .. } => {
                write!(f, "ToolCompleted({call_id}, ok={ok})")
            }
            Self::ApprovalNeeded { call_id, tool, .. } => {
                write!(f, "ApprovalNeeded({call_id}, {tool})")
            }
            Self::TodosUpdated { todos } => write!(f, "TodosUpdated({} todos)", todos.len()),
            Self::UsageUpdated { usage } => write!(f, "UsageUpdated({usage:?})"),
            Self::CompactionStarted => f.write_str("CompactionStarted"),
            Self::CompactionCompleted => f.write_str("CompactionCompleted"),
            Self::CompactionFailed => f.write_str("CompactionFailed"),
            Self::Completed { usage } => write!(f, "Completed({usage:?})"),
            Self::Failed { error } => write!(f, "Failed({error:?})"),
        }
    }
}

impl std::fmt::Debug for BackendEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionStarted { session_id } => {
                write!(f, "SessionStarted({session_id})")
            }
            Self::TextDelta(t) => write!(f, "TextDelta({t:?})"),
            Self::ProgressDelta(t) => write!(f, "ProgressDelta({t:?})"),
            Self::ProgressCompleted => f.write_str("ProgressCompleted"),
            Self::ThinkingDelta(t) => write!(f, "ThinkingDelta({t:?})"),
            Self::ThinkingCompleted => f.write_str("ThinkingCompleted"),
            Self::ToolStarted { call_id, tool, .. } => {
                write!(f, "ToolStarted({call_id}, {tool})")
            }
            Self::ToolOutput { call_id, .. } => write!(f, "ToolOutput({call_id})"),
            Self::ToolCompleted { call_id, ok, .. } => {
                write!(f, "ToolCompleted({call_id}, ok={ok})")
            }
            Self::QuestionsNeeded {
                request_id,
                questions,
                ..
            } => {
                write!(f, "QuestionsNeeded({request_id}, {} qs)", questions.len())
            }
            Self::ApprovalNeeded { call_id, tool, .. } => {
                write!(f, "ApprovalNeeded({call_id}, {tool})")
            }
            Self::CommandsUpdated { commands } => {
                write!(f, "CommandsUpdated({} commands)", commands.len())
            }
            Self::TodosUpdated { todos } => {
                write!(f, "TodosUpdated({} todos)", todos.len())
            }
            Self::UsageUpdated { usage } => write!(f, "UsageUpdated({usage:?})"),
            Self::CompactionStarted => f.write_str("CompactionStarted"),
            Self::CompactionCompleted => f.write_str("CompactionCompleted"),
            Self::CompactionFailed => f.write_str("CompactionFailed"),
            Self::CollaboratorStarted { session_id, .. } => {
                write!(f, "CollaboratorStarted({session_id})")
            }
            Self::CollaboratorEvent {
                session_id, event, ..
            } => {
                write!(f, "CollaboratorEvent({session_id}, {event:?})")
            }
            Self::Completed { usage } => write!(f, "Completed({usage:?})"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("turn cancelled")]
    Cancelled,
    #[error("{0} is not installed (or not on PATH)")]
    NotInstalled(String),
    #[error("not logged in: {0}")]
    Auth(String),
    #[error("backend protocol error: {0}")]
    Protocol(String),
    #[error("backend io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type BackendEventStream = BoxStream<'static, Result<BackendEvent, BackendError>>;

/// Best-effort provider health. Implementations should keep this fast;
/// `has_credentials` means the vendor currently reports a usable login.
#[derive(Debug, Clone, Default)]
pub struct BackendStatus {
    pub installed: bool,
    pub has_credentials: bool,
}

/// A vendor login flow in progress. `done` resolves when the vendor CLI
/// exits (successfully or not).
pub struct BackendLogin {
    /// URL the user must open (also opened by most vendor CLIs themselves).
    pub verification_url: Option<String>,
    pub user_code: Option<String>,
    /// Sends a browser authentication code or callback URL back to an
    /// interactive vendor CLI.
    pub callback_sender: Option<tokio::sync::mpsc::Sender<String>>,
    pub done: BoxFuture<'static, Result<(), BackendError>>,
}

#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    /// Stable identifier used as the prefix of model ids ("codex/gpt-5.4").
    fn id(&self) -> &str;

    /// Canonical model metadata snapshot: instant and offline-safe, used when
    /// the vendor cannot report current availability.
    fn models(&self) -> Vec<ModelInfo>;

    /// Models available for this backend. Catalog-covered backends use their
    /// canonical static roster; explicit adapters may add newly released or
    /// account-specific models discovered through a vendor CLI. The default
    /// returns the canonical snapshot.
    async fn list_models(&self) -> Vec<ModelInfo> {
        self.models()
    }

    fn status(&self) -> BackendStatus;

    /// Whether the backend can guarantee that a requested tool-free turn
    /// exposes no vendor-native tools. Backends that return false still run
    /// such turns without mounted MCP tools and under read-only permissions,
    /// but the engine must tolerate vendor-native read/search activity.
    fn supports_tool_free_turns(&self) -> bool {
        false
    }

    /// Live subscription usage (plan, metered allowance windows). Codex
    /// answers via its app-server, Claude Code via a stream-json `get_usage`
    /// control request, and Cursor via the dashboard's undocumented usage
    /// RPC (using the CLI's stored login). `None` means the vendor shares
    /// nothing at all.
    async fn subscription_health(&self) -> Option<trouve_protocol::SubscriptionHealth> {
        None
    }

    /// Whether this backend can append user input to an active turn without
    /// cancelling it or starting another turn.
    fn supports_steering(&self) -> bool {
        false
    }

    /// Append user guidance to the active turn in `steer.session`.
    async fn steer_turn(&self, _steer: BackendSteer) -> Result<(), BackendError> {
        Err(BackendError::Protocol(format!(
            "{} does not support steering active turns",
            self.id()
        )))
    }

    /// Start the vendor's own login flow (spawns the vendor CLI).
    async fn start_login(&self) -> Result<BackendLogin, BackendError>;

    /// Run one agent turn in the worktree, streaming translated events.
    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError>;
}

/// Locate a binary on PATH (absolute/relative paths pass through).
pub(crate) fn binary_on_path(command: &str) -> bool {
    process_env::find_executable(command).is_some()
}

const BACKEND_STREAM_CAPACITY: usize = 64;
const BACKEND_BUFFER_MAX_ITEMS: usize = 1024;
const BACKEND_BUFFER_MAX_BYTES: usize = 4 * 1024 * 1024;
const COALESCED_CHUNK_MAX_BYTES: usize = 64 * 1024;
const TEXT_COALESCE_WINDOW: Duration = Duration::from_millis(16);
const TOOL_OUTPUT_COALESCE_WINDOW: Duration = Duration::from_millis(50);

/// Provider-neutral sender for vendor backend events. Delta boundaries are a
/// transport detail, so adjacent text, thinking, and same-call tool-output
/// fragments are combined before they reach core. Control events retain their
/// exact order and backpressure behind earlier deltas instead of being lost.
pub(crate) struct BackendEventSender {
    buffer: Arc<BackendEventBuffer>,
}

struct BufferedBackendEvent {
    item: Result<BackendEvent, BackendError>,
    bytes: usize,
    ready_at: Instant,
}

#[derive(Default)]
struct BackendBufferStats {
    input_events: u64,
    emitted_events: u64,
    coalesced_events: u64,
    waits: u64,
    peak_items: usize,
    peak_bytes: usize,
}

#[derive(Default)]
struct BackendEventBufferState {
    queue: VecDeque<BufferedBackendEvent>,
    bytes: usize,
    input_closed: bool,
    output_closed: bool,
    stats: BackendBufferStats,
}

struct BackendEventBuffer {
    state: Mutex<BackendEventBufferState>,
    data: tokio::sync::Notify,
    space: tokio::sync::Notify,
}

enum BackendEnqueue {
    Sent,
    Closed,
    Wait,
}

impl BackendEventBuffer {
    fn new() -> Self {
        Self {
            state: Mutex::new(BackendEventBufferState::default()),
            data: tokio::sync::Notify::new(),
            space: tokio::sync::Notify::new(),
        }
    }

    fn close_input(&self) {
        self.state.lock().unwrap().input_closed = true;
        self.data.notify_one();
    }

    fn close_output(&self) {
        let mut state = self.state.lock().unwrap();
        state.output_closed = true;
        state.queue.clear();
        state.bytes = 0;
        drop(state);
        self.space.notify_one();
    }

    fn try_enqueue(
        &self,
        item: &mut Option<Result<BackendEvent, BackendError>>,
        counted: &mut bool,
    ) -> BackendEnqueue {
        let mut state = self.state.lock().unwrap();
        if state.output_closed {
            return BackendEnqueue::Closed;
        }
        if !*counted {
            state.stats.input_events += 1;
            *counted = true;
        }

        let pending = item.as_ref().expect("event remains while enqueueing");
        let bytes = backend_event_size(pending);
        let window = backend_event_window(pending);
        let has_byte_capacity = state.bytes.saturating_add(bytes) <= BACKEND_BUFFER_MAX_BYTES;

        // Merging does not consume another item slot, so permit it even when
        // the count limit is reached. The byte check is conservative for
        // same-call tool output because it counts the repeated call id.
        if has_byte_capacity && let Some(back) = state.queue.back_mut() {
            let incoming = item.take().expect("event remains while enqueueing");
            match merge_backend_event(&mut back.item, incoming) {
                BackendMerge::Merged(added) => {
                    back.bytes += added;
                    state.bytes += added;
                    state.stats.coalesced_events += 1;
                    state.stats.peak_bytes = state.stats.peak_bytes.max(state.bytes);
                    return BackendEnqueue::Sent;
                }
                BackendMerge::Separate(incoming) => *item = Some(incoming),
            }
        }

        let has_item_capacity = state.queue.len() < BACKEND_BUFFER_MAX_ITEMS;
        let single_oversize = state.queue.is_empty() && bytes > BACKEND_BUFFER_MAX_BYTES;
        if (has_item_capacity && has_byte_capacity) || single_oversize {
            state.queue.push_back(BufferedBackendEvent {
                item: item.take().expect("event remains while enqueueing"),
                bytes,
                ready_at: Instant::now() + window.unwrap_or(Duration::ZERO),
            });
            state.bytes += bytes;
            state.stats.peak_items = state.stats.peak_items.max(state.queue.len());
            state.stats.peak_bytes = state.stats.peak_bytes.max(state.bytes);
            return BackendEnqueue::Sent;
        }

        state.stats.waits += 1;
        if state.stats.waits == 1 || state.stats.waits.is_power_of_two() {
            let limit = match (has_item_capacity, has_byte_capacity) {
                (false, false) => "items+bytes",
                (false, true) => "items",
                (true, false) => "bytes",
                (true, true) => unreachable!("capacity branch returned above"),
            };
            tracing::warn!(
                limit,
                buffered_items = state.queue.len(),
                buffered_bytes = state.bytes,
                max_items = BACKEND_BUFFER_MAX_ITEMS,
                max_bytes = BACKEND_BUFFER_MAX_BYTES,
                waits = state.stats.waits,
                "backend event coalescer applying backpressure"
            );
        }
        BackendEnqueue::Wait
    }
}

impl BackendEventSender {
    /// Wait until the consumer drops the exposed backend event stream.
    pub(crate) async fn closed(&self) {
        loop {
            // Register before checking state so close_output cannot notify
            // between the check and this waiter becoming visible.
            let closed = self.buffer.space.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if self.buffer.state.lock().unwrap().output_closed {
                return;
            }
            closed.await;
        }
    }

    pub(crate) async fn send(&self, item: Result<BackendEvent, BackendError>) -> Result<(), ()> {
        let mut item = Some(item);
        let mut counted = false;
        loop {
            // Register before inspecting state so a concurrent dequeue cannot
            // race between the capacity check and waiting for its wakeup.
            let space = self.buffer.space.notified();
            tokio::pin!(space);
            space.as_mut().enable();
            match self.buffer.try_enqueue(&mut item, &mut counted) {
                BackendEnqueue::Sent => {
                    self.buffer.data.notify_one();
                    return Ok(());
                }
                BackendEnqueue::Closed => return Err(()),
                BackendEnqueue::Wait => space.await,
            }
        }
    }
}

impl Drop for BackendEventSender {
    fn drop(&mut self) {
        self.buffer.close_input();
    }
}

struct BackendEventStreamGuard {
    buffer: Arc<BackendEventBuffer>,
}

impl Drop for BackendEventStreamGuard {
    fn drop(&mut self) {
        self.buffer.close_output();
    }
}

enum BackendMerge {
    Merged(usize),
    Separate(Result<BackendEvent, BackendError>),
}

fn merge_backend_event(
    existing: &mut Result<BackendEvent, BackendError>,
    incoming: Result<BackendEvent, BackendError>,
) -> BackendMerge {
    match (&mut *existing, incoming) {
        (Ok(BackendEvent::TextDelta(current)), Ok(BackendEvent::TextDelta(next)))
            if current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (Ok(BackendEvent::ProgressDelta(current)), Ok(BackendEvent::ProgressDelta(next)))
            if current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (Ok(BackendEvent::ThinkingDelta(current)), Ok(BackendEvent::ThinkingDelta(next)))
            if current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (
            Ok(BackendEvent::ToolOutput {
                call_id: current_id,
                chunk: current,
            }),
            Ok(BackendEvent::ToolOutput {
                call_id: next_id,
                chunk: next,
            }),
        ) if current_id == &next_id
            && current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (
            Ok(BackendEvent::CollaboratorEvent {
                session_id: current_session,
                turn_id: current_turn,
                event: BackendCollaboratorEvent::TextDelta(current),
            }),
            Ok(BackendEvent::CollaboratorEvent {
                session_id: next_session,
                turn_id: next_turn,
                event: BackendCollaboratorEvent::TextDelta(next),
            }),
        ) if current_session == &next_session
            && current_turn == &next_turn
            && current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (
            Ok(BackendEvent::CollaboratorEvent {
                session_id: current_session,
                turn_id: current_turn,
                event: BackendCollaboratorEvent::ProgressDelta(current),
            }),
            Ok(BackendEvent::CollaboratorEvent {
                session_id: next_session,
                turn_id: next_turn,
                event: BackendCollaboratorEvent::ProgressDelta(next),
            }),
        ) if current_session == &next_session
            && current_turn == &next_turn
            && current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (
            Ok(BackendEvent::CollaboratorEvent {
                session_id: current_session,
                turn_id: current_turn,
                event: BackendCollaboratorEvent::ThinkingDelta(current),
            }),
            Ok(BackendEvent::CollaboratorEvent {
                session_id: next_session,
                turn_id: next_turn,
                event: BackendCollaboratorEvent::ThinkingDelta(next),
            }),
        ) if current_session == &next_session
            && current_turn == &next_turn
            && current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (
            Ok(BackendEvent::CollaboratorEvent {
                session_id: current_session,
                turn_id: current_turn,
                event:
                    BackendCollaboratorEvent::ToolOutput {
                        call_id: current_id,
                        chunk: current,
                    },
            }),
            Ok(BackendEvent::CollaboratorEvent {
                session_id: next_session,
                turn_id: next_turn,
                event:
                    BackendCollaboratorEvent::ToolOutput {
                        call_id: next_id,
                        chunk: next,
                    },
            }),
        ) if current_session == &next_session
            && current_turn == &next_turn
            && current_id == &next_id
            && current.len().saturating_add(next.len()) <= COALESCED_CHUNK_MAX_BYTES =>
        {
            let added = next.len();
            current.push_str(&next);
            BackendMerge::Merged(added)
        }
        (_, incoming) => BackendMerge::Separate(incoming),
    }
}

fn backend_event_window(event: &Result<BackendEvent, BackendError>) -> Option<Duration> {
    match event {
        Ok(
            BackendEvent::TextDelta(text)
            | BackendEvent::ProgressDelta(text)
            | BackendEvent::ThinkingDelta(text),
        ) if text.len() < COALESCED_CHUNK_MAX_BYTES => Some(TEXT_COALESCE_WINDOW),
        Ok(BackendEvent::ToolOutput { chunk, .. }) if chunk.len() < COALESCED_CHUNK_MAX_BYTES => {
            Some(TOOL_OUTPUT_COALESCE_WINDOW)
        }
        Ok(BackendEvent::CollaboratorEvent {
            event:
                BackendCollaboratorEvent::TextDelta(text)
                | BackendCollaboratorEvent::ProgressDelta(text)
                | BackendCollaboratorEvent::ThinkingDelta(text),
            ..
        }) if text.len() < COALESCED_CHUNK_MAX_BYTES => Some(TEXT_COALESCE_WINDOW),
        Ok(BackendEvent::CollaboratorEvent {
            event: BackendCollaboratorEvent::ToolOutput { chunk, .. },
            ..
        }) if chunk.len() < COALESCED_CHUNK_MAX_BYTES => Some(TOOL_OUTPUT_COALESCE_WINDOW),
        _ => None,
    }
}

fn backend_collaborator_event_size(event: &BackendCollaboratorEvent) -> usize {
    match event {
        BackendCollaboratorEvent::TurnStarted => 0,
        BackendCollaboratorEvent::UserMessage(text)
        | BackendCollaboratorEvent::TextDelta(text)
        | BackendCollaboratorEvent::ProgressDelta(text)
        | BackendCollaboratorEvent::ThinkingDelta(text) => text.len(),
        BackendCollaboratorEvent::ProgressCompleted
        | BackendCollaboratorEvent::ThinkingCompleted
        | BackendCollaboratorEvent::CompactionStarted
        | BackendCollaboratorEvent::CompactionCompleted
        | BackendCollaboratorEvent::CompactionFailed => 0,
        BackendCollaboratorEvent::ToolStarted {
            call_id,
            tool,
            args,
        } => call_id.len() + tool.len() + args.to_string().len(),
        BackendCollaboratorEvent::ToolOutput { call_id, chunk } => call_id.len() + chunk.len(),
        BackendCollaboratorEvent::ToolCompleted {
            call_id, result, ..
        } => call_id.len() + result.to_string().len(),
        BackendCollaboratorEvent::ApprovalNeeded {
            call_id,
            tool,
            args,
            ..
        } => call_id.len() + tool.len() + args.to_string().len(),
        BackendCollaboratorEvent::TodosUpdated { todos } => {
            serde_json::to_string(todos).map_or(0, |json| json.len())
        }
        BackendCollaboratorEvent::UsageUpdated { .. }
        | BackendCollaboratorEvent::Completed { .. } => std::mem::size_of::<Usage>(),
        BackendCollaboratorEvent::Failed { error } => error.len(),
    }
}

fn backend_event_size(event: &Result<BackendEvent, BackendError>) -> usize {
    match event {
        Ok(BackendEvent::SessionStarted { session_id }) => session_id.len(),
        Ok(
            BackendEvent::TextDelta(text)
            | BackendEvent::ProgressDelta(text)
            | BackendEvent::ThinkingDelta(text),
        ) => text.len(),
        Ok(BackendEvent::ToolStarted {
            call_id,
            tool,
            args,
        }) => call_id.len() + tool.len() + args.to_string().len(),
        Ok(BackendEvent::ToolOutput { call_id, chunk }) => call_id.len() + chunk.len(),
        Ok(BackendEvent::ToolCompleted {
            call_id, result, ..
        }) => call_id.len() + result.to_string().len(),
        Ok(BackendEvent::ApprovalNeeded {
            call_id,
            tool,
            args,
            ..
        }) => call_id.len() + tool.len() + args.to_string().len(),
        Ok(BackendEvent::QuestionsNeeded {
            request_id,
            title,
            questions,
            ..
        }) => {
            request_id.len()
                + title.as_ref().map_or(0, String::len)
                + serde_json::to_string(questions).map_or(0, |json| json.len())
        }
        Ok(BackendEvent::CommandsUpdated { commands }) => {
            serde_json::to_string(commands).map_or(0, |json| json.len())
        }
        Ok(BackendEvent::TodosUpdated { todos }) => {
            serde_json::to_string(todos).map_or(0, |json| json.len())
        }
        Ok(BackendEvent::UsageUpdated { .. } | BackendEvent::Completed { .. }) => {
            std::mem::size_of::<Usage>()
        }
        Ok(BackendEvent::CollaboratorStarted {
            session_id,
            parent_session_id,
            name,
            prompt,
            model,
            thinking_level,
            access: _,
        }) => {
            session_id.len()
                + parent_session_id.len()
                + name.as_ref().map_or(0, String::len)
                + prompt.as_ref().map_or(0, String::len)
                + model.as_ref().map_or(0, String::len)
                + thinking_level.as_ref().map_or(0, String::len)
        }
        Ok(BackendEvent::CollaboratorEvent {
            session_id,
            turn_id,
            event,
        }) => {
            session_id.len()
                + turn_id.as_ref().map_or(0, String::len)
                + backend_collaborator_event_size(event)
        }
        Ok(
            BackendEvent::ProgressCompleted
            | BackendEvent::ThinkingCompleted
            | BackendEvent::CompactionStarted
            | BackendEvent::CompactionCompleted
            | BackendEvent::CompactionFailed,
        ) => 0,
        Err(error) => error.to_string().len(),
    }
}

async fn pump_backend_events(
    buffer: Arc<BackendEventBuffer>,
    tx: tokio::sync::mpsc::Sender<Result<BackendEvent, BackendError>>,
) {
    enum Action {
        Send(Result<BackendEvent, BackendError>),
        Wait,
        WaitUntil(Instant),
        Done,
    }

    loop {
        let notified = buffer.data.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let action = {
            let mut state = buffer.state.lock().unwrap();
            let now = Instant::now();
            match state.queue.front() {
                Some(front) if state.queue.len() > 1 || front.ready_at <= now => {
                    let event = state.queue.pop_front().expect("front exists");
                    state.bytes = state.bytes.saturating_sub(event.bytes);
                    state.stats.emitted_events += 1;
                    Action::Send(event.item)
                }
                Some(front) => Action::WaitUntil(front.ready_at),
                None if state.input_closed => Action::Done,
                None => Action::Wait,
            }
        };

        match action {
            Action::Send(item) => {
                buffer.space.notify_one();
                if tx.send(item).await.is_err() {
                    buffer.close_output();
                    break;
                }
            }
            Action::Wait => notified.as_mut().await,
            Action::WaitUntil(deadline) => {
                tokio::select! {
                    _ = notified.as_mut() => {}
                    _ = tokio::time::sleep_until(deadline.into()) => {}
                }
            }
            Action::Done => break,
        }
    }

    let state = buffer.state.lock().unwrap();
    tracing::debug!(
        input_events = state.stats.input_events,
        emitted_events = state.stats.emitted_events,
        coalesced_events = state.stats.coalesced_events,
        peak_items = state.stats.peak_items,
        peak_bytes = state.stats.peak_bytes,
        waits = state.stats.waits,
        "backend event stream drained"
    );
}

/// Spawn a task producing events and expose a provider-neutral coalesced
/// stream. The intermediate buffer is count-and-byte-bounded, retains all
/// events, and combines only transport-fragment deltas whose concatenation is
/// semantically identical. One indivisible event larger than the byte budget
/// is admitted only while the queue is otherwise empty.
pub(crate) fn async_stream<F, Fut>(
    f: F,
) -> impl futures::Stream<Item = Result<BackendEvent, BackendError>>
where
    F: FnOnce(BackendEventSender) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let buffer = Arc::new(BackendEventBuffer::new());
    let sender = BackendEventSender {
        buffer: Arc::clone(&buffer),
    };
    let stream_guard = BackendEventStreamGuard {
        buffer: Arc::clone(&buffer),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(BACKEND_STREAM_CAPACITY);
    tokio::spawn(pump_backend_events(buffer, tx));
    tokio::spawn(f(sender));
    futures::stream::poll_fn(move |cx| {
        let _keep_guard_until_stream_drop = &stream_guard;
        rx.poll_recv(cx)
    })
}

/// Simple options-schema for backend models: vendors own the knobs.
pub(crate) fn empty_schema() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// "resets in 2h 10m" from a unix timestamp (seconds; tolerates millis).
pub(crate) fn format_reset(at: i64) -> String {
    let at = if at > 100_000_000_000 { at / 1000 } else { at };
    let now = chrono::Utc::now().timestamp();
    let secs = at - now;
    if secs <= 0 {
        return "resets shortly".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("resets in {days}d {hours}h")
    } else if hours > 0 {
        format!("resets in {hours}h {mins}m")
    } else {
        format!("resets in {}m", mins.max(1))
    }
}

/// Build a ModelInfo for a backend model.
pub(crate) fn model(backend_id: &str, name: &str, display: &str, context_window: u64) -> ModelInfo {
    ModelInfo {
        id: format!("{backend_id}/{name}"),
        display_name: display.into(),
        context_window,
        supports_tools: true,
        // Subscription-billed: no per-token prices.
        input_price_per_mtok: None,
        output_price_per_mtok: None,
        options_schema: empty_schema(),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;

    #[tokio::test]
    async fn coalesces_delta_kinds_without_reordering_controls() {
        let stream = async_stream(|tx| async move {
            for event in [
                BackendEvent::TextDelta("a".into()),
                BackendEvent::TextDelta("b".into()),
                BackendEvent::ThinkingDelta("c".into()),
                BackendEvent::ThinkingDelta("d".into()),
                BackendEvent::ToolOutput {
                    call_id: "one".into(),
                    chunk: "e".into(),
                },
                BackendEvent::ToolOutput {
                    call_id: "one".into(),
                    chunk: "f".into(),
                },
                BackendEvent::ToolOutput {
                    call_id: "two".into(),
                    chunk: "g".into(),
                },
                BackendEvent::Completed {
                    usage: Usage::default(),
                },
            ] {
                tx.send(Ok(event)).await.unwrap();
            }
        });
        futures::pin_mut!(stream);

        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        assert_eq!(events.len(), 5);
        assert!(matches!(&events[0], BackendEvent::TextDelta(text) if text == "ab"));
        assert!(matches!(&events[1], BackendEvent::ThinkingDelta(text) if text == "cd"));
        assert!(matches!(
            &events[2],
            BackendEvent::ToolOutput { call_id, chunk } if call_id == "one" && chunk == "ef"
        ));
        assert!(matches!(
            &events[3],
            BackendEvent::ToolOutput { call_id, chunk } if call_id == "two" && chunk == "g"
        ));
        assert!(matches!(&events[4], BackendEvent::Completed { .. }));
    }

    #[tokio::test]
    async fn sender_observes_when_exposed_stream_is_dropped() {
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let stream = async_stream(move |tx| async move {
            tx.closed().await;
            let _ = observed_tx.send(());
        });

        drop(stream);

        tokio::time::timeout(Duration::from_secs(1), observed_rx)
            .await
            .expect("sender should observe the dropped stream")
            .expect("observer task should report closure");
    }

    #[tokio::test]
    async fn concurrent_slow_consumers_preserve_large_delta_bursts() {
        const STREAMS: usize = 5;
        const DELTAS: usize = 10_000;
        const DELTA_BYTES: usize = 1024;
        let consumers = (0..STREAMS).map(|stream_id| async move {
            let stream = async_stream(move |tx| async move {
                let chunk = "x".repeat(DELTA_BYTES);
                let call_id = format!("call-{stream_id}");
                for _ in 0..DELTAS {
                    tx.send(Ok(BackendEvent::ToolOutput {
                        call_id: call_id.clone(),
                        chunk: chunk.clone(),
                    }))
                    .await
                    .unwrap();
                }
                tx.send(Ok(BackendEvent::Completed {
                    usage: Usage::default(),
                }))
                .await
                .unwrap();
            });
            futures::pin_mut!(stream);
            let mut output = String::new();
            let mut output_events = 0;
            let mut completed = false;
            // Let the producer meet a slow consumer long enough to exercise
            // both buffering and coalescing without delaying every event.
            tokio::time::sleep(Duration::from_millis(10)).await;
            while let Some(event) = stream.next().await {
                match event.unwrap() {
                    BackendEvent::ToolOutput { call_id, chunk } => {
                        assert_eq!(call_id, format!("call-{stream_id}"));
                        output.push_str(&chunk);
                        output_events += 1;
                    }
                    BackendEvent::Completed { .. } => completed = true,
                    other => panic!("unexpected event: {other:?}"),
                }
            }
            assert!(completed);
            assert_eq!(output, "x".repeat(DELTAS * DELTA_BYTES));
            assert!(
                output_events < DELTAS / 4,
                "expected coalescing to reduce {DELTAS} deltas, got {output_events} events"
            );
        });
        futures::future::join_all(consumers).await;
    }
}
