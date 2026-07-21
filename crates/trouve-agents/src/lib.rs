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

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// When set, the vendor agent runs with its built-in tools disabled and
    /// trouve's ToolExecutor bridged in over MCP (Claude Code only, v1).
    pub mcp_bridge: Option<McpBridgeConfig>,
    /// User-configured MCP servers (user/workspace/worktree scopes, already
    /// merged and env-expanded by the engine) to mount alongside the bridge.
    pub mcp_servers: Vec<McpServerLaunch>,
}

/// One prompt attachment, resolved to a stored file the backend process can
/// read (the server and vendor CLIs share a filesystem).
#[derive(Debug, Clone)]
pub struct TurnAttachment {
    /// Display name from the upload ("screenshot.png").
    pub name: String,
    /// MIME type ("image/png").
    pub mime: String,
    /// Absolute path of the stored bytes.
    pub path: PathBuf,
}

impl TurnAttachment {
    /// The file's bytes as standard base64, for protocols that embed image
    /// data instead of referencing paths.
    pub fn read_base64(&self) -> std::io::Result<String> {
        use base64::Engine as _;
        let bytes = std::fs::read(&self.path)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
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
/// used for approval prompting in Ask mode; optionally also replaces the
/// vendor's built-in tools with trouve's.
#[derive(Debug, Clone)]
pub struct McpBridgeConfig {
    /// Full endpoint URL, thread-scoped, with the tool/approval surface
    /// selected via query parameters.
    pub url: String,
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
    /// Reasoning ("thinking") text, where the vendor harness exposes it.
    ThinkingDelta(String),
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
            Self::ThinkingDelta(t) => write!(f, "ThinkingDelta({t:?})"),
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

    /// Live subscription usage (plan, metered allowance windows). Codex
    /// answers via its app-server, Claude Code via a stream-json `get_usage`
    /// control request, and Cursor via the dashboard's undocumented usage
    /// RPC (using the CLI's stored login). `None` means the vendor shares
    /// nothing at all.
    async fn subscription_health(&self) -> Option<trouve_protocol::SubscriptionHealth> {
        None
    }

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
        self.data.notify_waiters();
    }

    fn close_output(&self) {
        let mut state = self.state.lock().unwrap();
        state.output_closed = true;
        state.queue.clear();
        state.bytes = 0;
        drop(state);
        self.space.notify_waiters();
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
    pub(crate) async fn send(&self, item: Result<BackendEvent, BackendError>) -> Result<(), ()> {
        let mut item = Some(item);
        let mut counted = false;
        loop {
            // Register before inspecting state so a concurrent dequeue cannot
            // race between the capacity check and waiting for its wakeup.
            let space = self.buffer.space.notified();
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
        (_, incoming) => BackendMerge::Separate(incoming),
    }
}

fn backend_event_window(event: &Result<BackendEvent, BackendError>) -> Option<Duration> {
    match event {
        Ok(BackendEvent::TextDelta(text) | BackendEvent::ThinkingDelta(text))
            if text.len() < COALESCED_CHUNK_MAX_BYTES =>
        {
            Some(TEXT_COALESCE_WINDOW)
        }
        Ok(BackendEvent::ToolOutput { chunk, .. }) if chunk.len() < COALESCED_CHUNK_MAX_BYTES => {
            Some(TOOL_OUTPUT_COALESCE_WINDOW)
        }
        _ => None,
    }
}

fn backend_event_size(event: &Result<BackendEvent, BackendError>) -> usize {
    match event {
        Ok(BackendEvent::SessionStarted { session_id }) => session_id.len(),
        Ok(BackendEvent::TextDelta(text) | BackendEvent::ThinkingDelta(text)) => text.len(),
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
        Ok(BackendEvent::Completed { .. }) => std::mem::size_of::<Usage>(),
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
                buffer.space.notify_waiters();
                if tx.send(item).await.is_err() {
                    buffer.close_output();
                    break;
                }
            }
            Action::Wait => notified.await,
            Action::WaitUntil(deadline) => {
                tokio::select! {
                    _ = notified => {}
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
    let (tx, mut rx) = tokio::sync::mpsc::channel(BACKEND_STREAM_CAPACITY);
    tokio::spawn(pump_backend_events(buffer, tx));
    tokio::spawn(f(sender));
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx))
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
            let mut completed = false;
            while let Some(event) = stream.next().await {
                match event.unwrap() {
                    BackendEvent::ToolOutput { call_id, chunk } => {
                        assert_eq!(call_id, format!("call-{stream_id}"));
                        output.push_str(&chunk);
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    BackendEvent::Completed { .. } => completed = true,
                    other => panic!("unexpected event: {other:?}"),
                }
            }
            assert!(completed);
            assert_eq!(output, "x".repeat(DELTAS * DELTA_BYTES));
        });
        futures::future::join_all(consumers).await;
    }
}
