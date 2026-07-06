//! External agent backends: vendor coding agents (Codex, Cursor, Claude
//! Code) driven through their sanctioned CLI/JSON interfaces, running inside
//! trouve's session worktrees.
//!
//! Unlike a [`trouve-providers`] `Provider` (raw model inference inside
//! trouve's own agent loop), an [`AgentBackend`] owns the whole turn: the
//! vendor harness plans, calls its own tools, and edits files. Trouve
//! translates its event stream into the trouve protocol and bridges its
//! approval requests through the engine's permission layer. Subscription
//! auth stays inside the vendor binary — we never touch vendor OAuth tokens.

pub mod claude;
pub mod codex;
pub mod cursor;
mod login;

use std::path::PathBuf;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use trouve_protocol::{ModelInfo, Usage};

pub use login::spawn_login;

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
    pub thread_id: String,
    /// Session worktree the vendor agent operates in.
    pub worktree: PathBuf,
    /// Vendor-side session id from a previous turn on this thread, if any.
    pub session: Option<String>,
    /// Bare model name (provider prefix already stripped); empty = default.
    pub model: String,
    pub prompt: String,
    /// Trouve mode prompt, appended to the vendor's own system prompt where
    /// the vendor protocol allows.
    pub instructions: Option<String>,
    pub permission: BackendPermission,
    /// When set, the vendor agent runs with its built-in tools disabled and
    /// trouve's ToolExecutor bridged in over MCP (Claude Code only, v1).
    pub mcp_bridge: Option<McpBridgeConfig>,
}

/// Stdio MCP server the vendor agent should launch to reach trouve (the
/// `trouve mcp-bridge` subcommand, pointed at this engine's internal
/// endpoints via env vars). Always used for approval prompting in Ask mode;
/// optionally also replaces the vendor's built-in tools with trouve's.
#[derive(Debug, Clone)]
pub struct McpBridgeConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// When true the bridge serves trouve's ToolExecutor tools and the
    /// vendor's built-ins are disabled; when false it only serves the
    /// approval-prompt gate.
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
    Completed {
        usage: Usage,
    },
}

impl std::fmt::Debug for BackendEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionStarted { session_id } => {
                write!(f, "SessionStarted({session_id})")
            }
            Self::TextDelta(t) => write!(f, "TextDelta({t:?})"),
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
            Self::Completed { usage } => write!(f, "Completed({usage:?})"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
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

/// Cheap, filesystem-based health check (no subprocesses) so provider
/// listings stay fast. Best effort: "has_credentials" means "looks logged
/// in", verified for real on the first turn.
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
    pub done: BoxFuture<'static, Result<(), BackendError>>,
}

#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    /// Stable identifier used as the prefix of model ids ("codex/gpt-5.4").
    fn id(&self) -> &str;

    /// Static model snapshot: instant and offline-safe, used when the
    /// vendor can't be asked (not installed, not logged in, query failed).
    fn models(&self) -> Vec<ModelInfo>;

    /// Models as reported by the vendor right now (authoritative: vendors
    /// evolve their catalogs faster than we ship). Implementations should
    /// cache; the default falls back to the static snapshot.
    async fn list_models(&self) -> Vec<ModelInfo> {
        self.models()
    }

    fn status(&self) -> BackendStatus;

    /// Start the vendor's own login flow (spawns the vendor CLI).
    async fn start_login(&self) -> Result<BackendLogin, BackendError>;

    /// Run one agent turn in the worktree, streaming translated events.
    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError>;
}

/// Locate a binary on PATH (absolute/relative paths pass through).
pub(crate) fn binary_on_path(command: &str) -> bool {
    if command.contains('/') {
        return std::path::Path::new(command).exists();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

/// Spawn a task producing events into a channel and expose it as a stream.
pub(crate) fn async_stream<F, Fut>(
    f: F,
) -> impl futures::Stream<Item = Result<BackendEvent, BackendError>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Result<BackendEvent, BackendError>>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(f(tx));
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Simple options-schema for backend models: vendors own the knobs.
pub(crate) fn empty_schema() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
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
