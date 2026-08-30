//! Claude Code backend, driving the `claude` CLI in print mode.
//!
//! One persistent `claude -p --input-format stream-json` process is kept per
//! trouve thread (see the internal process pool): turns after the first skip the CLI's cold
//! start, the transcript re-read, and the MCP bridge re-handshake. The pool
//! is bounded (LRU cap + idle reaping); killing a process loses nothing
//! because Claude Code persists the transcript and `--resume` restores it.
//! Claude Code rotates its session id on every resume, so we re-persist the
//! id from each turn's `system/init` / `result` events.
//!
//! Permission mapping: `Yolo` → auto-approval through trouve's gate,
//! `ReadOnly` → disallowed mutating built-ins + trouve's approval gate,
//! `Ask` → the trouve MCP bridge's `approval_prompt` tool via
//! `--permission-prompt-tool`, so headless print mode routes permission
//! requests to trouve's approval flow instead of failing them.
//!
//! Login uses `claude auth login` in a small PTY. The client pastes the
//! authentication code shown by Claude's browser flow back through trouve.
//!
//! Subscription usage (the data behind the TUI's `/usage` dialog) is read
//! through the same stream-json surface: a short-lived print-mode process
//! answers a `get_usage` control request with the plan and its metered
//! rate-limit windows. No user message is sent, so no model turn runs.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Mutex, mpsc};
use trouve_protocol::{ModelInfo, Usage};

use crate::process_env::{ProcessTreeChild, spawn_process_tree};
use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventStream, BackendLogin, BackendPermission,
    BackendStatus, BackendSteer, BackendTurn, async_stream, binary_on_path, format_reset,
    spawn_claude_login,
};

/// Most live processes kept at once; the least recently used is evicted.
const POOL_CAP: usize = 3;
/// Idle time after which a pooled process is reaped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How often the reaper scans the pool.
const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Steering accepted before the lazy stream sends its initial prompt. The
/// engine normally polls that stream immediately, but a continuously-ready
/// steering producer must not grow process memory without bound.
const PENDING_STEER_CAP: usize = 8;

pub struct ClaudeBackend {
    id: String,
    command: String,
    pool: Arc<Pool>,
    /// Signals one thread id per vendor-autonomous turn the router observes.
    background_turns: mpsc::Sender<String>,
    background_turns_rx: std::sync::Mutex<Option<mpsc::Receiver<String>>>,
    /// Serialized one-shot usage process. A failed cleanup remains here so a
    /// later poll cannot spawn over an unproven prior process tree.
    usage_process: Mutex<Option<ProcessTreeChild>>,
    #[cfg(test)]
    injected_usage_cleanup_failure: std::sync::atomic::AtomicBool,
    catalog: Arc<trouve_providers::models_dev::ModelsDevCatalog>,
}

impl ClaudeBackend {
    pub fn new(id: impl Into<String>, command: Option<String>) -> Self {
        let (background_turns, background_turns_rx) = mpsc::channel(64);
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "claude".into()),
            pool: Arc::new(Pool::default()),
            background_turns,
            background_turns_rx: std::sync::Mutex::new(Some(background_turns_rx)),
            usage_process: Mutex::new(None),
            #[cfg(test)]
            injected_usage_cleanup_failure: std::sync::atomic::AtomicBool::new(false),
            catalog: Arc::new(trouve_providers::models_dev::ModelsDevCatalog::embedded()),
        }
    }

    pub fn with_catalog(
        mut self,
        catalog: Arc<trouve_providers::models_dev::ModelsDevCatalog>,
    ) -> Self {
        self.catalog = catalog;
        self
    }
}

fn auth_status_is_logged_in(output: &[u8]) -> bool {
    serde_json::from_slice::<Value>(output)
        .ok()
        .and_then(|status| status.get("loggedIn")?.as_bool())
        .unwrap_or(false)
}

fn claude_is_logged_in(command: &str) -> bool {
    let mut command = crate::process_env::std_command(command);
    command
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    trouve_process::spawn(&mut command)
        .and_then(std::process::Child::wait_with_output)
        .map(|output| output.status.success() && auth_status_is_logged_in(&output.stdout))
        .unwrap_or(false)
}

/// Live `claude` processes keyed by trouve thread id.
#[derive(Default)]
struct Pool {
    procs: Mutex<HashMap<String, Arc<ClaudeProc>>>,
    reaper_started: std::sync::atomic::AtomicBool,
}

impl Pool {
    /// Quarantine a process, prove its complete process tree has exited, and
    /// only then make its key available for replacement.
    async fn terminate_and_remove(
        &self,
        thread_id: &str,
        proc_: &Arc<ClaudeProc>,
    ) -> Result<std::process::ExitStatus, BackendError> {
        let mut procs = self.procs.lock().await;
        proc_.quarantine();
        let status = proc_.terminate().await?;
        if procs
            .get(thread_id)
            .is_some_and(|current| Arc::ptr_eq(current, proc_))
        {
            procs.remove(thread_id);
        }
        Ok(status)
    }

    /// Kill processes idle past the timeout, skipping any with a turn in
    /// flight (their line receiver is locked).
    async fn reap_idle(&self) {
        let mut procs = self.procs.lock().await;
        let mut dead = Vec::new();
        for (id, p) in procs.iter() {
            if Arc::strong_count(p) != 1 || p.router.is_busy() || p.router.has_pending_background()
            {
                continue; // turn in flight or buffered awaiting attach
            }
            if p.last_used.lock().unwrap().elapsed() > IDLE_TIMEOUT {
                dead.push(id.clone());
            }
        }
        for id in dead {
            let Some(p) = procs.get(&id).cloned() else {
                continue;
            };
            p.quarantine();
            match p.terminate().await {
                Ok(_) => {
                    if procs.get(&id).is_some_and(|entry| Arc::ptr_eq(entry, &p)) {
                        procs.remove(&id);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        thread_id = %id,
                        "claude: retaining idle process after unacknowledged cleanup: {error}"
                    );
                }
            }
        }
    }

    /// Evict the least recently used idle process while over capacity.
    async fn enforce_cap(procs: &mut HashMap<String, Arc<ClaudeProc>>) {
        while procs.len() >= POOL_CAP {
            let lru = procs
                .iter()
                .filter(|(_, p)| {
                    p.is_reusable()
                        && Arc::strong_count(p) == 1
                        && !p.router.is_busy()
                        && !p.router.has_pending_background()
                })
                .min_by_key(|(_, p)| *p.last_used.lock().unwrap())
                .map(|(id, _)| id.clone());
            let Some(id) = lru else { break }; // all busy: allow overflow
            let Some(p) = procs.get(&id).cloned() else {
                continue;
            };
            p.quarantine();
            match p.terminate().await {
                Ok(_) => {
                    if procs.get(&id).is_some_and(|entry| Arc::ptr_eq(entry, &p)) {
                        procs.remove(&id);
                    }
                }
                Err(error) => {
                    // Keep this key quarantined, but let an unrelated thread
                    // overflow the soft cap rather than losing availability.
                    tracing::warn!(
                        thread_id = %id,
                        "claude: retaining LRU process after unacknowledged cleanup: {error}"
                    );
                    break;
                }
            }
        }
    }
}

/// Bytes of vendor-autonomous ("background") turn output buffered while no
/// attach consumer is connected. Overflow drops the oldest lines (logged);
/// the vendor transcript remains authoritative on disk.
const BACKGROUND_BUFFER_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Outcome of registering a turn consumer with the stdout router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouterRegistration {
    /// The consumer will receive lines (live or drained from the buffer).
    Streaming,
    /// An attach consumer claimed a vendor-autonomous turn that is still
    /// running, so Claude can accept steering for it.
    StreamingLive,
    /// Attach-only registration found no background turn to attach to.
    NothingPending,
}

#[derive(Default)]
struct RouterState {
    /// Consumer for the trouve turn currently entitled to stdout lines.
    turn: Option<mpsc::Sender<String>>,
    /// The registered consumer is an attach turn: it must receive exactly
    /// one buffered/live background turn, ending at its `result` line.
    turn_is_attach: bool,
    /// A non-attach consumer registered but its prompt has not been written
    /// to the vendor yet, so no arriving line can belong to it.
    prompt_pending: bool,
    /// The process is inside a vendor-autonomous turn whose `result` has not
    /// arrived yet.
    background_in_flight: bool,
    /// The stdout pump has read a result belonging to the registered turn,
    /// but the router has not attributed that line yet. Steering must close
    /// at receipt rather than after this channel handoff.
    terminal_pending: bool,
    /// Complete, in-order lines of vendor-autonomous turns awaiting an
    /// attach consumer. May span multiple turns; `result` lines delimit.
    background: std::collections::VecDeque<String>,
    background_bytes: usize,
    dropped_background_lines: u64,
}

/// Owns a process's stdout for its whole life and routes each line to the
/// correct consumer. This guarantees two properties the old
/// turn-locks-the-receiver design could not: Claude Code never blocks on an
/// unread stdout pipe between trouve turns, and events from a
/// vendor-autonomous turn (e.g. a Monitor wake-up inside Claude Code) can
/// never leak into the next trouve-initiated turn — attribution only
/// switches at `result` boundaries.
struct StdoutRouter {
    state: std::sync::Mutex<RouterState>,
    /// Serializes steering writes with terminal-result attribution. A steer
    /// that wins this boundary belongs to the active turn; one that loses it
    /// observes the cleared router consumer and fails closed.
    turn_boundary: Mutex<()>,
    /// Wakes the router loop when a consumer registers.
    notify: tokio::sync::Notify,
    /// Announces a pending vendor-autonomous turn to the engine. Invoked
    /// whenever a turn begins with no consumer, and re-invoked whenever a
    /// buffered turn loses or outlives its attach consumer, so every
    /// buffered turn is eventually announced even if an earlier signal or
    /// attach was lost.
    signal: Box<dyn Fn() + Send + Sync>,
}

/// Owns an eager router registration until its returned stream is dropped.
/// Its weak sender preserves exact-channel cleanup without keeping the
/// receiver open after the router releases its sender at a turn boundary.
struct RouterRegistrationGuard {
    router: Arc<StdoutRouter>,
    sender: mpsc::WeakSender<String>,
}

impl Drop for RouterRegistrationGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.upgrade() {
            self.router.consumer_lost(&sender, None);
        }
    }
}

impl StdoutRouter {
    fn new(signal: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: std::sync::Mutex::new(RouterState::default()),
            turn_boundary: Mutex::new(()),
            notify: tokio::sync::Notify::new(),
            signal: Box::new(signal),
        }
    }

    /// True while stdout lines are attributed to any in-flight turn.
    fn is_busy(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.turn.is_some() || state.background_in_flight
    }

    /// True while completed autonomous turns sit buffered awaiting an attach
    /// consumer. Pool reaping and cap eviction must treat such processes as
    /// live: recycling one discards its buffered turns before the engine's
    /// attach can drain them.
    fn has_pending_background(&self) -> bool {
        !self.state.lock().unwrap().background.is_empty()
    }

    /// Called immediately before a steering record write while holding
    /// `turn_boundary`. The stdout pump may close terminal admission without
    /// that boundary so it can never stop draining the vendor pipe.
    fn can_accept_steer(&self, attach_turn: bool) -> bool {
        let state = self.state.lock().unwrap();
        state.turn.is_some()
            && state.turn_is_attach == attach_turn
            && (!attach_turn || state.background_in_flight)
            && !state.terminal_pending
    }

    /// Close steering admission as soon as stdout yields a terminal record,
    /// before that record can wait in the pump-to-router channel.
    fn line_received(&self, line: &str) {
        if !line_is_result(line) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.terminal_pending = state.turn.is_some()
            && if state.turn_is_attach {
                state.background_in_flight
            } else {
                !state.prompt_pending && !state.background_in_flight
            };
    }

    /// The registered non-attach turn's prompt reached the vendor; lines
    /// arriving from now on may belong to it.
    fn prompt_delivered(&self) {
        self.state.lock().unwrap().prompt_pending = false;
    }

    /// Install the consumer for a trouve-initiated turn. A non-attach turn
    /// starts receiving lines only after its prompt is delivered and any
    /// in-flight background turn reaches its `result`; an attach turn
    /// receives exactly one background turn (buffered and/or live) and
    /// reports `NothingPending` when there is none.
    fn register(
        &self,
        sender: mpsc::Sender<String>,
        attach: bool,
    ) -> Result<RouterRegistration, BackendError> {
        let mut state = self.state.lock().unwrap();
        if state.turn.as_ref().is_some_and(|turn| turn.is_closed()) {
            // A cancelled turn's consumer can linger until its next send
            // fails; replace it eagerly so registration never deadlocks. A
            // dead attach consumer may leave its turn buffered: re-announce.
            let was_attach = state.turn_is_attach;
            state.turn = None;
            state.turn_is_attach = false;
            if was_attach && (!state.background.is_empty() || state.background_in_flight) {
                (self.signal)();
            }
        }
        if state.turn.is_some() {
            return Err(BackendError::Protocol(
                "claude: a turn is already consuming this process".into(),
            ));
        }
        if attach && state.background.is_empty() && !state.background_in_flight {
            return Ok(RouterRegistration::NothingPending);
        }
        if state.dropped_background_lines > 0 {
            tracing::warn!(
                dropped = state.dropped_background_lines,
                "claude: background turn output was dropped before a consumer attached"
            );
            state.dropped_background_lines = 0;
        }
        let streaming_live = attach && state.background_in_flight;
        state.turn = Some(sender);
        state.turn_is_attach = attach;
        state.prompt_pending = !attach;
        drop(state);
        self.notify.notify_one();
        Ok(if streaming_live {
            RouterRegistration::StreamingLive
        } else {
            RouterRegistration::Streaming
        })
    }

    /// Register an eagerly claimed consumer whose ownership is released even
    /// when the lazy event stream is dropped before its first poll.
    fn register_owned(
        self: &Arc<Self>,
        sender: mpsc::Sender<String>,
        attach: bool,
    ) -> Result<(RouterRegistration, Option<RouterRegistrationGuard>), BackendError> {
        let weak_sender = sender.downgrade();
        let registration = self.register(sender, attach)?;
        let guard = matches!(
            registration,
            RouterRegistration::Streaming | RouterRegistration::StreamingLive
        )
        .then(|| RouterRegistrationGuard {
            router: self.clone(),
            sender: weak_sender,
        });
        Ok((registration, guard))
    }

    fn buffer_background(state: &mut RouterState, line: String) {
        state.background_bytes += line.len();
        state.background.push_back(line);
        while state.background_bytes > BACKGROUND_BUFFER_MAX_BYTES {
            let Some(dropped) = state.background.pop_front() else {
                break;
            };
            state.background_bytes -= dropped.len();
            state.dropped_background_lines += 1;
        }
    }

    /// Uninstall a failed consumer; when it was an attach consumer, put the
    /// undelivered line back and re-announce the still-pending turn.
    fn consumer_lost(&self, sender: &mpsc::Sender<String>, undelivered: Option<String>) {
        let mut state = self.state.lock().unwrap();
        let current = state
            .turn
            .as_ref()
            .is_some_and(|turn| turn.same_channel(sender));
        if !current {
            return;
        }
        let was_attach = state.turn_is_attach;
        state.turn = None;
        state.turn_is_attach = false;
        if was_attach {
            if let Some(line) = undelivered {
                state.background_bytes += line.len();
                state.background.push_front(line);
            }
            if !state.background.is_empty() || state.background_in_flight {
                (self.signal)();
            }
        }
    }

    /// Run the routing loop until the stdout pump closes.
    async fn run(self: Arc<Self>, mut lines: mpsc::Receiver<String>) {
        loop {
            // Drain buffered background lines to an attach consumer first;
            // this must not require fresh stdout activity.
            loop {
                let (sender, line, last_of_turn) = {
                    let mut state = self.state.lock().unwrap();
                    let Some(sender) = state.turn.clone() else {
                        break;
                    };
                    if !state.turn_is_attach {
                        break;
                    }
                    let Some(line) = state.background.pop_front() else {
                        break;
                    };
                    state.background_bytes -= line.len();
                    let last = line_is_result(&line);
                    if last {
                        state.turn = None;
                        state.turn_is_attach = false;
                        if !state.background.is_empty() {
                            // Another complete buffered turn awaits its own
                            // attach consumer; its original signal may have
                            // been consumed by this one.
                            (self.signal)();
                        }
                    }
                    (sender, line, last)
                };
                if sender.send(line.clone()).await.is_err() {
                    if last_of_turn {
                        // Consumer death and turn completion coincided; the
                        // turn is consumed either way.
                        break;
                    }
                    self.consumer_lost(&sender, Some(line));
                    break;
                }
                if last_of_turn {
                    break;
                }
            }

            let line = tokio::select! {
                line = lines.recv() => match line {
                    Some(line) => line,
                    None => break,
                },
                _ = self.notify.notified() => continue,
            };
            let is_result = line_is_result(&line);
            let _turn_boundary = if is_result {
                Some(self.turn_boundary.lock().await)
            } else {
                None
            };
            // Decide the destination under the lock, send outside it.
            let (destination, attach_completed) = {
                let mut state = self.state.lock().unwrap();
                if is_result {
                    state.terminal_pending = false;
                }
                let turn = state.turn.clone();
                let is_attach = state.turn_is_attach;
                let prompt_pending = state.prompt_pending;
                let in_flight = state.background_in_flight;
                match (turn, in_flight) {
                    // An attach consumer takes the in-flight background
                    // turn's lines directly and ends at its boundary.
                    (Some(sender), true) if is_attach => {
                        if is_result {
                            state.background_in_flight = false;
                            state.turn = None;
                            state.turn_is_attach = false;
                        }
                        (Some(sender), is_result)
                    }
                    // A background turn in flight always owns the line, even
                    // when a non-attach turn is already registered: that
                    // turn's prompt is queued vendor-side and its events
                    // begin only after this boundary.
                    (_, true) => {
                        if is_result {
                            state.background_in_flight = false;
                        }
                        Self::buffer_background(&mut state, line.clone());
                        (None, false)
                    }
                    (Some(sender), false) if is_attach => {
                        // Live continuation of a partially buffered turn.
                        if is_result {
                            state.turn = None;
                            state.turn_is_attach = false;
                        }
                        (Some(sender), is_result)
                    }
                    (Some(sender), false) if !prompt_pending => {
                        if is_result {
                            state.turn = None;
                        }
                        (Some(sender), false)
                    }
                    // Either no consumer, or a non-attach consumer whose
                    // prompt has not reached the vendor: this line starts a
                    // new vendor-autonomous turn.
                    (_, false) => {
                        state.background_in_flight = !is_result;
                        Self::buffer_background(&mut state, line.clone());
                        (self.signal)();
                        (None, false)
                    }
                }
            };
            if let Some(sender) = destination {
                if sender.send(line.clone()).await.is_err() {
                    self.consumer_lost(&sender, Some(line));
                } else if attach_completed {
                    let state = self.state.lock().unwrap();
                    if !state.background.is_empty() {
                        (self.signal)();
                    }
                }
            }
        }
        let mut state = self.state.lock().unwrap();
        state.turn = None;
        state.turn_is_attach = false;
        state.background_in_flight = false;
    }
}

fn line_is_result(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|kind| kind == "result")
        })
        .unwrap_or(false)
}

/// One persistent `claude` process serving one trouve thread.
struct ClaudeProc {
    input: Mutex<ClaudeInputState>,
    /// Routes stdout lines to the active consumer; owns the receiver for
    /// the process's whole life.
    router: Arc<StdoutRouter>,
    child: Mutex<ProcessTreeChild>,
    /// False as soon as any path decides this transport must be recycled.
    /// The pool retains a false entry until full-tree cleanup is acknowledged.
    reusable: std::sync::atomic::AtomicBool,
    /// Explicit turn readiness. This is set before `run_turn` returns its
    /// lazy stream, so steering can be accepted at the advertised boundary.
    active_turn: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    injected_terminate_failure: std::sync::atomic::AtomicBool,
    /// Claude reads user MCP credentials from this owner-only file. Keeping
    /// the handle alive for the child lifetime also removes it on drop.
    _mcp_config: Option<tempfile::NamedTempFile>,
    /// Spawn-time configuration; a differing turn forces a respawn.
    config_fp: String,
    /// Vendor session id the process is holding, updated from its events.
    /// A turn arriving with a different id (e.g. after undo) respawns.
    session: std::sync::Mutex<Option<String>>,
    last_used: std::sync::Mutex<Instant>,
    /// Rolling stderr tail for error reporting.
    stderr_tail: Arc<std::sync::Mutex<String>>,
}

struct ClaudeInputState {
    stdin: ChildStdin,
    prompt_sent: bool,
    attach_turn: bool,
    pending_steers: Vec<Value>,
}

struct ClaudeTurnGuard {
    proc_: Arc<ClaudeProc>,
}

impl Drop for ClaudeTurnGuard {
    fn drop(&mut self) {
        self.proc_
            .active_turn
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl ClaudeProc {
    async fn begin_turn(
        self: &Arc<Self>,
        prompt_already_sent: bool,
    ) -> Result<ClaudeTurnGuard, BackendError> {
        let mut input = self.input.lock().await;
        self.active_turn
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| {
                BackendError::Protocol("claude process already has an active turn".into())
            })?;
        input.prompt_sent = prompt_already_sent;
        input.attach_turn = prompt_already_sent;
        input.pending_steers.clear();
        Ok(ClaudeTurnGuard {
            proc_: self.clone(),
        })
    }

    fn quarantine(&self) {
        self.reusable
            .store(false, std::sync::atomic::Ordering::Release);
    }

    fn is_reusable(&self) -> bool {
        self.reusable.load(std::sync::atomic::Ordering::Acquire)
    }

    async fn terminate(&self) -> Result<std::process::ExitStatus, BackendError> {
        self.quarantine();
        #[cfg(test)]
        if self
            .injected_terminate_failure
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(BackendError::Protocol(
                "injected Claude process-tree cleanup failure".into(),
            ));
        }

        let mut child = self.child.lock().await;
        if let Some(status) = child.try_wait_tree().map_err(BackendError::Io)? {
            return Ok(status);
        }
        child.terminate_and_reap().await.map_err(BackendError::Io)
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }
}

/// Spawn-time configuration that must match for a process to be reused.
fn config_fingerprint(turn: &BackendTurn) -> String {
    let bridge = turn
        .mcp_bridge
        .as_ref()
        .map(|b| format!("{}|{}|{:?}", b.url, b.bridge_tools, b.disallowed_tools));
    let servers: Vec<String> = turn
        .mcp_servers
        .iter()
        .map(|s| format!("{}|{}|{:?}|{:?}", s.name, s.command, s.args, s.env))
        .collect();
    format!(
        "{:?}|{}|{:?}|{:?}|{:?}|{}|{:?}|{:?}",
        turn.worktree,
        turn.model,
        Value::Object(turn.model_options.clone()),
        turn.instructions,
        turn.permission,
        turn.tool_free,
        bridge,
        servers,
    )
}

/// Apply the model's reasoning control using Claude Code's current interface.
/// Adaptive-thinking models use `--effort`; older fixed-budget models retain
/// the environment variables supported by the CLI. The `thinking_level`
/// fallback keeps pre-schema-split threads working after an upgrade.
fn configure_thinking(
    cmd: &mut Command,
    turn: &BackendTurn,
    catalog: &trouve_providers::models_dev::ModelsDevCatalog,
) {
    let effort_capable = catalog
        .model(
            "anthropic",
            "claude-code",
            &turn.model,
            trouve_providers::models_dev::OptionsDialect::ClaudeCli,
        )
        .is_some_and(|model| model.options_schema.pointer("/properties/effort").is_some());
    let effort = turn
        .model_options
        .get("effort")
        .and_then(Value::as_str)
        .or_else(|| {
            if effort_capable {
                turn.model_options
                    .get("thinking_level")
                    .and_then(Value::as_str)
            } else {
                None
            }
        })
        .filter(|level| *level != "off");
    if let Some(effort) = effort {
        cmd.args(["--effort", effort]);
        return;
    }
    if effort_capable {
        return;
    }

    if let Some(budget) = turn
        .model_options
        .get("thinking_budget_tokens")
        .and_then(Value::as_u64)
    {
        cmd.env("MAX_THINKING_TOKENS", budget.to_string());
        return;
    }

    match turn
        .model_options
        .get("thinking_level")
        .and_then(Value::as_str)
    {
        Some("off") => {
            cmd.env("CLAUDE_CODE_DISABLE_THINKING", "1");
        }
        Some(level) => {
            if let Some(budget) = trouve_providers::catalog::thinking_budget_tokens(level) {
                cmd.env("MAX_THINKING_TOKENS", budget.to_string());
            }
        }
        None => {}
    }
}

#[async_trait::async_trait]
impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // The same catalog as the per-use Anthropic API provider, so both
        // surface the same metadata. Claude Code accepts full model ids; the
        // subscription bills nothing per token, so only pricing is dropped.
        self.catalog
            .provider_models(
                "anthropic",
                &self.id,
                trouve_providers::models_dev::OptionsDialect::ClaudeCli,
            )
            .into_iter()
            .map(|mut m| {
                m.input_price_per_mtok = None;
                m.output_price_per_mtok = None;
                m
            })
            .collect()
    }

    fn status(&self) -> BackendStatus {
        let installed = binary_on_path(&self.command);
        BackendStatus {
            installed,
            has_credentials: installed && claude_is_logged_in(&self.command),
        }
    }

    fn supports_tool_free_turns(&self) -> bool {
        true
    }

    fn supports_steering(&self) -> bool {
        true
    }

    async fn steer_turn(&self, steer: BackendSteer) -> Result<(), BackendError> {
        let proc_ = {
            let procs = self.pool.procs.lock().await;
            procs
                .values()
                .find(|proc_| {
                    proc_.is_reusable()
                        && proc_.session.lock().unwrap().as_deref() == Some(&steer.session)
                })
                .cloned()
        }
        .ok_or_else(|| {
            BackendError::Protocol(format!(
                "claude steer: no live process owns session {}",
                steer.session
            ))
        })?;
        let mut content = Vec::with_capacity(1 + steer.attachments.len());
        if !steer.prompt.is_empty() {
            content.push(json!({ "type": "text", "text": steer.prompt }));
        }
        for attachment in steer.attachments {
            let data = attachment.base64();
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": attachment.mime,
                    "data": data,
                }
            }));
        }
        let message = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content,
            }
        });
        let mut input = tokio::select! {
            biased;
            _ = steer.cancel.cancelled() => return Err(BackendError::Cancelled),
            input = proc_.input.lock() => input,
        };
        if !proc_.active_turn.load(std::sync::atomic::Ordering::Acquire) {
            return Err(BackendError::Protocol(format!(
                "claude steer: session {} has no active turn",
                steer.session
            )));
        }
        if !input.prompt_sent {
            if input.pending_steers.len() >= PENDING_STEER_CAP {
                return Err(BackendError::Protocol(format!(
                    "claude steer: session {} pending steering queue is full",
                    steer.session
                )));
            }
            input.pending_steers.push(message);
            return Ok(());
        }
        let _turn_boundary = tokio::select! {
            biased;
            _ = steer.cancel.cancelled() => return Err(BackendError::Cancelled),
            boundary = proc_.router.turn_boundary.lock() => boundary,
        };
        if !proc_.router.can_accept_steer(input.attach_turn) {
            return Err(BackendError::Protocol(format!(
                "claude steer: session {} has no active turn",
                steer.session
            )));
        }
        // Build one complete record before the final terminal-state check.
        // Once this write starts, finish it even if request cancellation or a
        // terminal stdout record arrives; a partial JSON line would corrupt
        // the persistent stream-json transport for every later turn.
        let mut record = message.to_string().into_bytes();
        record.push(b'\n');
        if !proc_.router.can_accept_steer(input.attach_turn) {
            return Err(BackendError::Protocol(format!(
                "claude steer: session {} has no active turn",
                steer.session
            )));
        }
        input
            .stdin
            .write_all(&record)
            .await
            .map_err(BackendError::Io)?;
        input.stdin.flush().await.map_err(BackendError::Io)
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        spawn_claude_login(&self.command).await
    }

    async fn subscription_health(&self) -> Option<trouve_protocol::SubscriptionHealth> {
        Some(match self.query_usage().await {
            Ok(payload) => parse_usage_health(&self.id, &payload),
            Err(e) => trouve_protocol::SubscriptionHealth {
                provider_id: self.id.clone(),
                status: "unavailable".into(),
                plan: String::new(),
                windows: Vec::new(),
                credits: String::new(),
                note: format!("could not read usage from the Claude CLI: {e}"),
            },
        })
    }

    fn take_background_turn_signals(&self) -> Option<mpsc::Receiver<String>> {
        self.background_turns_rx.lock().unwrap().take()
    }

    async fn abandon_background_turns(&self, thread_id: &str) {
        let proc_ = self.pool.procs.lock().await.get(thread_id).cloned();
        if let Some(proc_) = proc_ {
            // The thread is gone, so this process can never serve another
            // turn and its output can never be attached. Terminating it
            // (rather than clearing router state at one instant) also covers
            // an autonomous turn still streaming, which would otherwise
            // refill the buffer and re-pin the pool slot with no later
            // signal guaranteed to repeat the cleanup.
            if let Err(error) = self.pool.terminate_and_remove(thread_id, &proc_).await {
                tracing::warn!(
                    %thread_id,
                    "claude: terminating the pooled process for a deleted thread failed: {error}"
                );
                // Free the slot regardless: terminate_and_remove already
                // quarantined the process before failing, and the thread can
                // never use it again. Dropping the pool's reference releases
                // the pending-background pin, and the child is spawned with
                // kill_on_drop, so it is reaped when the last reference
                // goes.
                let mut procs = self.pool.procs.lock().await;
                if procs
                    .get(thread_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &proc_))
                {
                    procs.remove(thread_id);
                }
            }
        }
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        self.start_reaper();
        let cancel = turn.cancel.clone();
        if turn.attach_background {
            let existing = self
                .pool
                .procs
                .lock()
                .await
                .get(&turn.thread_id)
                .cloned()
                .filter(|proc_| proc_.is_reusable());
            let Some(_) = existing else {
                // The process (and any background output) is gone; report an
                // empty completed turn instead of spawning a fresh CLI.
                return Ok(Box::pin(futures::stream::once(async {
                    Ok(BackendEvent::Completed {
                        usage: Usage::default(),
                    })
                })));
            };
        }
        // Process acquisition may have to clean a stale tree. Do not cancel
        // that future midway through its acknowledgement: it keeps the pool
        // key quarantined and checks cancellation before spawning.
        let proc_ = self.proc_for(&turn, &cancel).await?;
        let pool = self.pool.clone();
        let thread_id = turn.thread_id.clone();
        if cancel.is_cancelled() {
            pool.terminate_and_remove(&thread_id, &proc_).await?;
            return Err(BackendError::Cancelled);
        }
        let attach = turn.attach_background;
        // Claim an attach target before advertising native steering. The
        // router distinguishes a genuinely live vendor turn from a completed
        // turn whose output is merely buffered; writing to Claude's stdin in
        // the latter case would start unrelated background work.
        let (attached_lines, attach_registration, active_turn) = if attach {
            let (turn_tx, lines) = mpsc::channel::<String>(1024);
            let (registration, registration_guard) = proc_.router.register_owned(turn_tx, true)?;
            match registration {
                RouterRegistration::StreamingLive => (
                    Some(lines),
                    registration_guard,
                    Some(proc_.begin_turn(true).await?),
                ),
                RouterRegistration::Streaming => (Some(lines), registration_guard, None),
                RouterRegistration::NothingPending => {
                    return Ok(Box::pin(futures::stream::once(async {
                        Ok(BackendEvent::Completed {
                            usage: Usage::default(),
                        })
                    })));
                }
            }
        } else {
            (None, None, Some(proc_.begin_turn(false).await?))
        };
        let prompt = turn.prompt.clone();
        // Anthropic-style base64 image blocks, alongside the text block.
        let mut content = vec![json!({ "type": "text", "text": prompt })];
        for att in &turn.attachments {
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": att.mime,
                    "data": att.base64(),
                }
            }));
        }

        let stream = async_stream(move |tx| async move {
            let _active_turn = active_turn;
            let _attach_registration = attach_registration;
            let mut lines = if let Some(lines) = attached_lines {
                lines
            } else {
                let (turn_tx, lines) = mpsc::channel::<String>(1024);
                match proc_.router.register(turn_tx, false) {
                    Ok(RouterRegistration::Streaming) => {}
                    Ok(RouterRegistration::StreamingLive) => unreachable!(
                        "non-attach Claude registration cannot claim a live background turn"
                    ),
                    Ok(RouterRegistration::NothingPending) => {
                        unreachable!("non-attach Claude registration always creates a consumer")
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                }
                lines
            };
            proc_.touch();

            let msg = json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": content,
                }
            });
            if !attach {
                let sent = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        if let Err(error) = pool.terminate_and_remove(&thread_id, &proc_).await {
                            let _ = tx.send(Err(error)).await;
                        }
                        return;
                    }
                    _ = tx.closed() => {
                        if let Err(error) = pool.terminate_and_remove(&thread_id, &proc_).await {
                            tracing::warn!(
                                "claude: stream-drop cleanup was not acknowledged: {error}"
                            );
                        }
                        return;
                    }
                    sent = async {
                        let mut input = proc_.input.lock().await;
                        input.stdin.write_all(msg.to_string().as_bytes()).await?;
                        input.stdin.write_all(b"\n").await?;
                        for steer in std::mem::take(&mut input.pending_steers) {
                            input.stdin.write_all(steer.to_string().as_bytes()).await?;
                            input.stdin.write_all(b"\n").await?;
                        }
                        input.stdin.flush().await?;
                        input.prompt_sent = true;
                        Ok::<(), std::io::Error>(())
                    } => sent,
                };
                if let Err(e) = sent {
                    // Likely the process died between turns; keep reading —
                    // the no-result exit path below reports it (with stderr)
                    // and drops it from the pool so the next turn respawns.
                    // Delivery is still marked: a write error cannot
                    // distinguish "the vendor never saw the prompt" from
                    // "the vendor consumed the prompt and closed stdin
                    // before our flush" (EPIPE after full consumption), and
                    // withholding delivery in the second case strands the
                    // legitimate response as background output.
                    tracing::debug!("claude stdin write failed: {e}");
                }
                proc_.router.prompt_delivered();
            }

            let mut completed = false;
            loop {
                let line = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        if let Err(error) = pool.terminate_and_remove(&thread_id, &proc_).await {
                            let _ = tx.send(Err(error)).await;
                        }
                        return;
                    }
                    _ = tx.closed() => {
                        if let Err(error) = pool.terminate_and_remove(&thread_id, &proc_).await {
                            tracing::warn!(
                                "claude: stream-drop cleanup was not acknowledged: {error}"
                            );
                        }
                        return;
                    }
                    line = lines.recv() => match line {
                        Some(line) => line,
                        None => break,
                    },
                };
                let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(error) = result_error(&ev) {
                    // Error results can still carry a session_id; persist it
                    // before reporting the error so the process isn't respawned.
                    if let Some(sid) = ev["session_id"].as_str() {
                        *proc_.session.lock().unwrap() = Some(sid.to_string());
                    }
                    let _ = tx.send(Err(BackendError::Protocol(error))).await;
                    completed = true;
                    break;
                }
                let events = map_event(&ev);
                // Track the session the process is holding so the next
                // turn's reuse check compares against the current id.
                for out in &events {
                    if let BackendEvent::SessionStarted { session_id } = out {
                        *proc_.session.lock().unwrap() = Some(session_id.clone());
                    }
                }
                let is_result = ev["type"].as_str() == Some("result");
                for out in events {
                    let sent = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => false,
                        sent = tx.send(Ok(out)) => sent.is_ok(),
                    };
                    if !sent {
                        // Consumer dropped mid-turn (cancel): the CLI has no
                        // per-turn abort in this mode, so kill the process.
                        // The transcript is on disk; next turn resumes it.
                        if let Err(error) = pool.terminate_and_remove(&thread_id, &proc_).await {
                            tracing::warn!(
                                "claude: consumer-drop cleanup was not acknowledged: {error}"
                            );
                        }
                        return;
                    }
                }
                if is_result {
                    completed = true;
                    break;
                }
            }
            proc_.touch();

            if !completed {
                // Stdout closed without a result. Terminate and acknowledge
                // any surviving descendants before releasing the pool key.
                let error = match pool.terminate_and_remove(&thread_id, &proc_).await {
                    Ok(status) => BackendError::Protocol(format!(
                        "claude exited with {status:?}: {}",
                        proc_.stderr_tail.lock().unwrap().trim()
                    )),
                    Err(error) => error,
                };
                let _ = tx.send(Err(error)).await;
            }
        });
        Ok(stream.boxed())
    }
}

/// Claude reports turn-level failures as a final `result` record rather
/// than closing stdout with an error. In particular, subscription limits use
/// this path, so treating every result as successful makes the turn disappear
/// from the chat without any feedback.
fn result_error(ev: &Value) -> Option<String> {
    if ev["type"].as_str() != Some("result") {
        return None;
    }
    let subtype = ev["subtype"].as_str().unwrap_or_default();
    if ev["is_error"].as_bool() != Some(true) && !subtype.starts_with("error_") {
        return None;
    }

    ev["result"]
        .as_str()
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            ev["error"]
                .as_str()
                .filter(|message| !message.trim().is_empty())
        })
        .or_else(|| {
            ev["error"]["message"]
                .as_str()
                .filter(|message| !message.trim().is_empty())
        })
        .or_else(|| {
            ev["errors"].as_array().and_then(|errors| {
                errors.iter().find_map(|error| {
                    error
                        .as_str()
                        .or_else(|| error["message"].as_str())
                        .filter(|message| !message.trim().is_empty())
                })
            })
        })
        .map(str::to_string)
        .or_else(|| {
            Some(if subtype.is_empty() {
                "Claude turn failed".to_string()
            } else {
                format!("Claude turn failed ({subtype})")
            })
        })
}

/// How long a `get_usage` query may take end to end (CLI cold start plus
/// the CLI's own usage fetch, which retries internally).
const USAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Fixed request id for the usage control request (one per process, so no
/// collision is possible).
const USAGE_REQUEST_ID: &str = "trouve-usage";

impl ClaudeBackend {
    async fn cleanup_usage_process(
        &self,
        child: &mut ProcessTreeChild,
    ) -> Result<(), BackendError> {
        #[cfg(test)]
        if self
            .injected_usage_cleanup_failure
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(BackendError::Protocol(
                "injected Claude usage process-tree cleanup failure".into(),
            ));
        }
        if child.try_wait_tree().map_err(BackendError::Io)?.is_some() {
            return Ok(());
        }
        child
            .terminate_and_reap()
            .await
            .map(|_| ())
            .map_err(BackendError::Io)
    }

    /// Ask a short-lived print-mode process for subscription usage via the
    /// `get_usage` control request — the same data the TUI's `/usage`
    /// dialog shows (which has no headless equivalent). Returns the inner
    /// response payload (`subscription_type`, `rate_limits`, ...).
    async fn query_usage(&self) -> Result<Value, BackendError> {
        let mut usage_process = self.usage_process.lock().await;
        if let Some(stale) = usage_process.as_mut() {
            self.cleanup_usage_process(stale).await?;
            usage_process.take();
        }

        let mut command = crate::process_env::tokio_command(&self.command);
        command
            .arg("-p")
            .args(["--input-format", "stream-json"])
            .args(["--output-format", "stream-json"])
            .arg("--verbose")
            // No turn runs: skip the user's MCP servers and don't persist
            // an empty session transcript for every poll.
            .arg("--strict-mcp-config")
            .arg("--no-session-persistence")
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = spawn_process_tree(&mut command).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
            _ => BackendError::Io(e),
        })?;
        *usage_process = Some(child);
        let child = usage_process
            .as_mut()
            .expect("usage process was installed above");
        let mut stdin = child.take_stdin().expect("stdin piped");
        let stdout = child.take_stdout().expect("stdout piped");

        let query = async move {
            let request = json!({
                "type": "control_request",
                "request_id": USAGE_REQUEST_ID,
                "request": { "subtype": "get_usage" },
            });
            stdin
                .write_all(format!("{request}\n").as_bytes())
                .await
                .map_err(BackendError::Io)?;
            stdin.flush().await.map_err(BackendError::Io)?;

            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await.map_err(BackendError::Io)? {
                let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if ev["type"].as_str() != Some("control_response") {
                    continue;
                }
                let response = &ev["response"];
                if response["request_id"].as_str() != Some(USAGE_REQUEST_ID) {
                    continue;
                }
                if response["subtype"].as_str() == Some("success") {
                    return Ok(response["response"].clone());
                }
                return Err(BackendError::Protocol(format!(
                    "get_usage failed: {}",
                    response["error"].as_str().unwrap_or("unknown error")
                )));
            }
            Err(BackendError::Protocol(
                "claude exited before answering the usage query".into(),
            ))
        };
        let result = tokio::time::timeout(USAGE_TIMEOUT, query).await;
        // A usage response is not complete until the short-lived process and
        // every descendant have been terminated and reaped.
        self.cleanup_usage_process(child).await?;
        usage_process.take();
        result
            .map_err(|_| BackendError::Protocol("timed out waiting for the usage query".into()))?
    }

    fn start_reaper(&self) {
        if self
            .pool
            .reaper_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let pool = Arc::downgrade(&self.pool);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_INTERVAL).await;
                let Some(pool) = pool.upgrade() else { break };
                pool.reap_idle().await;
            }
        });
    }

    /// Fetch the pooled process for this thread, or (re)spawn one when there
    /// is none, it died, or the turn's spawn-time config / session id no
    /// longer matches.
    async fn proc_for(
        &self,
        turn: &BackendTurn,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Arc<ClaudeProc>, BackendError> {
        if cancel.is_cancelled() {
            return Err(BackendError::Cancelled);
        }
        let fp = config_fingerprint(turn);
        let mut procs = self.pool.procs.lock().await;
        if cancel.is_cancelled() {
            return Err(BackendError::Cancelled);
        }
        if let Some(p) = procs.get(&turn.thread_id).cloned() {
            let alive = if p.is_reusable() {
                matches!(p.child.lock().await.try_wait_leader(), Ok(None))
            } else {
                false
            };
            let session_matches = match (&turn.session, p.session.lock().unwrap().as_ref()) {
                (Some(want), Some(have)) => want == have,
                (None, _) => false, // explicit fresh session: start over
                (Some(_), None) => false,
            };
            if alive && p.config_fp == fp && session_matches {
                return Ok(p);
            }
            p.quarantine();
            p.terminate().await?;
            if procs
                .get(&turn.thread_id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &p))
            {
                procs.remove(&turn.thread_id);
            }
            // Cancellation may arrive during non-cancellable stale cleanup.
            // Observe it before starting a replacement process.
            if cancel.is_cancelled() {
                return Err(BackendError::Cancelled);
            }
        }

        Pool::enforce_cap(&mut procs).await;
        if cancel.is_cancelled() {
            return Err(BackendError::Cancelled);
        }
        let proc_ = Arc::new(self.spawn(turn, fp)?);
        procs.insert(turn.thread_id.clone(), proc_.clone());
        Ok(proc_)
    }

    /// Spawn a persistent `claude` process configured for this turn's
    /// thread. The prompt is NOT passed here; turns arrive over stdin.
    fn spawn(&self, turn: &BackendTurn, config_fp: String) -> Result<ClaudeProc, BackendError> {
        let mut cmd = crate::process_env::tokio_command(&self.command);
        let mut mcp_config_file = None;
        cmd.arg("-p")
            .args(["--input-format", "stream-json"])
            .args(["--output-format", "stream-json"])
            .arg("--verbose")
            // Stream text/thinking deltas live instead of whole blocks.
            .arg("--include-partial-messages")
            // Anthropic redacts thinking text by default (empty blocks with
            // only a signature); this opts back in to summarized thinking.
            .args(["--thinking-display", "summarized"])
            // Claude Code defers tool schemas behind a ToolSearch lookup by
            // default. The trouve bridge exposes only a handful of tools, so
            // load them upfront — no ToolSearch round-trip before the first
            // code search, and no failures while the bridge reconnects.
            .env("ENABLE_TOOL_SEARCH", "false")
            .current_dir(&turn.worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(session) = &turn.session {
            cmd.args(["--resume", session]);
        }
        if !turn.model.is_empty() {
            cmd.args(["--model", &turn.model]);
        }
        configure_thinking(&mut cmd, turn, &self.catalog);
        if let Some(instr) = &turn.instructions {
            cmd.args(["--append-system-prompt", instr]);
        }
        if turn.tool_free {
            cmd.args(["--tools", ""]);
        }
        // MCP config: the trouve bridge plus any user-configured servers.
        // The bridge has two roles, both optional:
        //  - approval gate: in Ask mode, Claude's permission requests go to
        //    the bridge's approval_prompt tool (trouve's approval flow)
        //    instead of failing in headless print mode;
        //  - tool bridge: Claude's built-ins stand down and trouve's
        //    ToolExecutor serves tools (approvals then gate inside trouve,
        //    so the bridged server is pre-allowed).
        // User servers ride along un-allowlisted, so their tools flow
        // through the normal permission path (approval_prompt in Ask mode).
        let mut mcp_servers = serde_json::Map::new();
        for server in &turn.mcp_servers {
            let env: serde_json::Map<String, serde_json::Value> = server
                .env
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            mcp_servers.insert(
                server.name.clone(),
                serde_json::json!({
                    "command": server.command,
                    "args": server.args,
                    "env": env,
                }),
            );
        }
        if let Some(bridge) = &turn.mcp_bridge {
            mcp_servers.insert(
                "trouve".into(),
                serde_json::json!({
                    "type": "http",
                    "url": bridge.url,
                }),
            );
        }
        if !mcp_servers.is_empty() {
            use std::io::Write as _;

            let mcp_config = serde_json::json!({ "mcpServers": mcp_servers });
            // NamedTempFile uses create-new semantics and mode 0600 on Unix,
            // avoiding both shared-/tmp disclosure and symlink clobbering.
            // The handle lives in ClaudeProc, so the credential-bearing file
            // disappears as soon as the pooled child is evicted.
            let mut file = tempfile::Builder::new()
                .prefix("trouve-mcp-")
                .suffix(".json")
                .tempfile()?;
            file.write_all(mcp_config.to_string().as_bytes())?;
            cmd.arg("--mcp-config").arg(file.path());
            cmd.arg("--strict-mcp-config");
            mcp_config_file = Some(file);
        }
        if let Some(bridge) = &turn.mcp_bridge {
            if bridge.bridge_tools {
                if !bridge.disallowed_tools.is_empty() {
                    cmd.args(["--disallowedTools", &bridge.disallowed_tools.join(",")]);
                }
                cmd.args(["--allowedTools", "mcp__trouve"]);
            } else {
                // Approvals-only: Claude keeps its built-ins, but trouve's
                // read-only semantic search tools and the interactive
                // question tool ride along on the bridge and are pre-allowed
                // (they are gated inside trouve).
                cmd.args([
                    "--allowedTools",
                    "mcp__trouve__search,mcp__trouve__find_related,mcp__trouve__ask_question",
                ]);
            }
            // Even Yolo routes through the engine: it auto-approves normal
            // calls but still enforces the session-worktree boundary.
            cmd.args(["--permission-prompt-tool", "mcp__trouve__approval_prompt"]);
        }
        match turn.permission {
            BackendPermission::Yolo => {
                // Direct backend use may have no embedded bridge. Preserve
                // Yolo semantics in that degraded case; normal engine turns
                // have a bridge and auto-approve through its path guard.
                if turn.mcp_bridge.is_none() {
                    cmd.arg("--dangerously-skip-permissions");
                }
            }
            // Read-only rides on trouve's approval gate (mutating requests
            // are denied inside trouve) rather than `--permission-mode plan`:
            // plan mode injects Claude's interactive plan workflow prompt
            // (ExitPlanMode / AskUserQuestion, unavailable headless) and
            // blocks read-only MCP tools like trouve's code search. The
            // definite mutators are additionally unavailable outright, so
            // the model doesn't waste turns on doomed requests.
            BackendPermission::ReadOnly => {
                let vendor_tools_stand_down = turn
                    .mcp_bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.bridge_tools);
                if !vendor_tools_stand_down {
                    cmd.args(["--disallowedTools", "Write,Edit,MultiEdit,NotebookEdit"]);
                }
            }
            BackendPermission::Ask => {}
        }

        let mut child = spawn_process_tree(&mut cmd).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
            _ => BackendError::Io(e),
        })?;
        let stdin = child.take_stdin().expect("stdin piped");
        let stdout = child.take_stdout().expect("stdout piped");
        let stderr = child.take_stderr().expect("stderr piped");

        let thread_id = turn.thread_id.clone();
        let signal = self.background_turns.clone();
        let router = Arc::new(StdoutRouter::new(move || {
            if signal.try_send(thread_id.clone()).is_err() {
                tracing::debug!(
                    thread_id = %thread_id,
                    "claude: dropping background-turn signal (retried on attach boundaries)"
                );
            }
        }));
        // Stdout pump: lines flow into the channel the router owns for the
        // process's whole life, so the pipe is always being read. Terminal
        // admission closes before the line enters this bounded channel.
        let (line_tx, line_rx) = mpsc::channel::<String>(256);
        let pump_router = Arc::clone(&router);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                pump_router.line_received(&line);
                if line_tx.send(line).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(Arc::clone(&router).run(line_rx));

        // Stderr pump: keep a bounded tail for error reporting.
        let stderr_tail = Arc::new(std::sync::Mutex::new(String::new()));
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut t = tail.lock().unwrap();
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 4000;
                    t.drain(..cut);
                }
            }
        });

        Ok(ClaudeProc {
            input: Mutex::new(ClaudeInputState {
                stdin,
                prompt_sent: false,
                attach_turn: false,
                pending_steers: Vec::new(),
            }),
            router,
            child: Mutex::new(child),
            reusable: std::sync::atomic::AtomicBool::new(true),
            active_turn: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            injected_terminate_failure: std::sync::atomic::AtomicBool::new(false),
            _mcp_config: mcp_config_file,
            config_fp,
            session: std::sync::Mutex::new(turn.session.clone()),
            last_used: std::sync::Mutex::new(Instant::now()),
            stderr_tail,
        })
    }
}

/// Map one Claude Code stream-json event to zero or more backend events.
fn map_event(ev: &Value) -> Vec<BackendEvent> {
    match ev["type"].as_str() {
        // Claude rotates session ids per run; always persist the latest.
        // The init event also lists the accepted slash commands (names
        // only), surfaced as prompt-box completions.
        Some("system") if ev["subtype"].as_str() == Some("init") => {
            let mut out: Vec<BackendEvent> = ev["session_id"]
                .as_str()
                .map(|sid| {
                    vec![BackendEvent::SessionStarted {
                        session_id: sid.to_string(),
                    }]
                })
                .unwrap_or_default();
            if let Some(cmds) = ev["slash_commands"].as_array() {
                out.push(BackendEvent::CommandsUpdated {
                    commands: cmds
                        .iter()
                        .filter_map(|c| c.as_str())
                        .map(|name| trouve_protocol::CommandInfo {
                            name: name.to_string(),
                            description: String::new(),
                        })
                        .collect(),
                });
            }
            out
        }
        // Live deltas (--include-partial-messages). Text and thinking stream
        // here; the complete "assistant" event that follows repeats the same
        // content as whole blocks, so those are skipped below.
        Some("stream_event") => {
            let delta = &ev["event"]["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => delta["text"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| vec![BackendEvent::TextDelta(t.to_string())])
                    .unwrap_or_default(),
                // Redacted thinking arrives as empty deltas carrying only a
                // token estimate; there is nothing to show, so drop them.
                Some("thinking_delta") => delta["thinking"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| vec![BackendEvent::ThinkingDelta(t.to_string())])
                    .unwrap_or_default(),
                _ => vec![],
            }
        }
        Some("assistant") => {
            let mut out = Vec::new();
            if let Some(blocks) = ev["message"]["content"].as_array() {
                for b in blocks {
                    // Text and thinking already streamed via stream_event
                    // deltas; only tool calls are taken from the complete
                    // message (their input JSON arrives fully assembled).
                    if b["type"].as_str() == Some("tool_use") {
                        out.push(BackendEvent::ToolStarted {
                            call_id: b["id"].as_str().unwrap_or("claude-tool").into(),
                            tool: b["name"].as_str().unwrap_or("tool").into(),
                            args: b["input"].clone(),
                        });
                    }
                }
            }
            out
        }
        // Tool results come back as user-role messages.
        Some("user") => {
            let mut out = Vec::new();
            if let Some(blocks) = ev["message"]["content"].as_array() {
                for b in blocks {
                    if b["type"].as_str() == Some("tool_result") {
                        let ok = b["is_error"].as_bool() != Some(true);
                        out.push(BackendEvent::ToolCompleted {
                            call_id: b["tool_use_id"].as_str().unwrap_or("claude-tool").into(),
                            ok,
                            result: b["content"].clone(),
                        });
                    }
                }
            }
            out
        }
        Some("result") => {
            let usage = &ev["usage"];
            let mut events = Vec::new();
            // Session id also appears on the result event; keep it fresh.
            if let Some(sid) = ev["session_id"].as_str() {
                events.push(BackendEvent::SessionStarted {
                    session_id: sid.to_string(),
                });
            }
            events.push(BackendEvent::Completed {
                usage: Usage {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                    cached_input_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
                    context_input_tokens: None,
                    // The CLI reports an estimate even on subscription
                    // plans, where nothing is billed per turn; suppress it
                    // like the other subscription backends.
                    cost_usd: None,
                    context_window: None,
                },
            });
            events
        }
        _ => vec![],
    }
}

/// Turn a `get_usage` control response payload into subscription health.
///
/// The payload mirrors the TUI's `/usage` data: `subscription_type`
/// ("pro"/"max"/"team"), `rate_limits_available`, and `rate_limits` with
/// the classic flat buckets (`five_hour`, `seven_day`, `seven_day_sonnet`,
/// `seven_day_opus` — `utilization` percent + `resets_at`) plus a newer
/// self-describing `limits` array that Anthropic is migrating to.
fn parse_usage_health(provider_id: &str, payload: &Value) -> trouve_protocol::SubscriptionHealth {
    let plan = payload["subscription_type"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let rate_limits = &payload["rate_limits"];

    let mut windows: Vec<trouve_protocol::SubscriptionWindow> = Vec::new();
    let push = |windows: &mut Vec<trouve_protocol::SubscriptionWindow>,
                label: String,
                used: &Value,
                resets: &Value| {
        let Some(pct) = used.as_f64() else { return };
        windows.push(trouve_protocol::SubscriptionWindow {
            label,
            used_percent: (pct.round() as i64).clamp(0, 100),
            resets: parse_reset_at(resets).map(format_reset).unwrap_or_default(),
        });
    };

    for (key, label) in [
        ("five_hour", "5h window"),
        ("seven_day", "Weekly (all models)"),
        ("seven_day_sonnet", "Weekly (Sonnet)"),
        ("seven_day_opus", "Weekly (Opus)"),
    ] {
        let bucket = &rate_limits[key];
        push(
            &mut windows,
            label.to_string(),
            &bucket["utilization"],
            &bucket["resets_at"],
        );
    }

    // Newer payloads carry the buckets in a self-describing `limits` array
    // (the flat keys then come back null). Add whatever the flat pass
    // didn't already cover.
    for entry in rate_limits["limits"].as_array().into_iter().flatten() {
        let label = match entry["kind"].as_str() {
            Some("session") => "5h window".to_string(),
            Some("weekly_all") => "Weekly (all models)".to_string(),
            Some("weekly_scoped") => match entry["scope"]["model"]["display_name"].as_str() {
                Some(name) => format!("Weekly ({name})"),
                None => continue,
            },
            _ => continue,
        };
        if windows.iter().any(|w| w.label.eq_ignore_ascii_case(&label)) {
            continue;
        }
        push(&mut windows, label, &entry["percent"], &entry["resets_at"]);
    }

    // Pay-per-use overage riding on top of the subscription, when enabled.
    // `used_credits` / `monthly_limit` are cents.
    let credits = rate_limits["extra_usage"]
        .as_object()
        .filter(|x| x.get("is_enabled").and_then(Value::as_bool) == Some(true))
        .map(|x| {
            let used = x.get("used_credits").and_then(Value::as_f64);
            let limit = x
                .get("monthly_limit")
                .and_then(Value::as_f64)
                .filter(|l| *l > 0.0);
            match (used, limit) {
                (Some(u), Some(l)) => {
                    format!("extra usage: ${:.2} of ${:.2}", u / 100.0, l / 100.0)
                }
                (Some(u), None) => format!("extra usage: ${:.2}", u / 100.0),
                _ => "extra usage enabled".to_string(),
            }
        })
        .unwrap_or_default();

    if windows.is_empty() {
        let note = if payload["rate_limits_available"].as_bool() == Some(true) {
            "the Claude CLI reported no usage windows".to_string()
        } else {
            "the Claude CLI reported no usage data — subscription usage needs a \
             claude.ai login (run `claude` and use /login)"
                .to_string()
        };
        return trouve_protocol::SubscriptionHealth {
            provider_id: provider_id.to_string(),
            status: "unavailable".into(),
            plan,
            windows,
            credits,
            note,
        };
    }
    trouve_protocol::SubscriptionHealth {
        provider_id: provider_id.to_string(),
        status: "ok".into(),
        plan,
        windows,
        credits,
        note: String::new(),
    }
}

/// `resets_at` arrives as RFC 3339 in the flat buckets and unix seconds in
/// the `limits` array; accept both.
fn parse_reset_at(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    chrono::DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|t| t.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn recv_line(rx: &mut mpsc::Receiver<String>) -> Option<String> {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .ok()
            .flatten()
    }

    async fn wait_for(mut condition: impl FnMut() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("condition not reached in time");
    }

    const BG_LINE: &str =
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"background"}]}}"#;
    const BG_RESULT: &str = r#"{"type":"result","subtype":"success"}"#;
    const USER_LINE: &str =
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"user"}]}}"#;

    #[tokio::test]
    async fn router_buffers_background_turns_and_signals_once_per_turn() {
        let signals = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = signals.clone();
        let router = Arc::new(StdoutRouter::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        // A background turn with no consumer buffers and signals exactly once.
        line_tx.send(BG_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 1).await;
        wait_for(|| !router.is_busy()).await;

        // A later non-attach turn receives only its own lines, never the
        // buffered background output.
        let (turn_tx, mut turn_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(turn_tx, false).unwrap(),
            RouterRegistration::Streaming
        );
        router.prompt_delivered();
        line_tx.send(USER_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        assert_eq!(recv_line(&mut turn_rx).await.as_deref(), Some(USER_LINE));
        assert_eq!(recv_line(&mut turn_rx).await.as_deref(), Some(BG_RESULT));
        assert!(
            recv_line(&mut turn_rx).await.is_none(),
            "turn stream ends at result"
        );

        // The attach turn drains exactly the buffered background turn.
        let (attach_tx, mut attach_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(attach_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_RESULT));
        assert!(recv_line(&mut attach_rx).await.is_none());

        // Nothing left to attach to.
        let (empty_tx, _empty_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(empty_tx, true).unwrap(),
            RouterRegistration::NothingPending
        );
        assert_eq!(signals.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn background_turn_in_flight_never_leaks_into_a_registered_turn() {
        let router = Arc::new(StdoutRouter::new(|| {}));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        // Background turn starts; a trouve turn registers mid-flight (its
        // prompt is queued vendor-side). This is the regression that used to
        // swallow the user turn: the buffered background `result` terminated
        // the user turn's stream before its own events arrived.
        line_tx.send(BG_LINE.to_string()).await.unwrap();
        wait_for(|| router.is_busy()).await;
        let (turn_tx, mut turn_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(turn_tx, false).unwrap(),
            RouterRegistration::Streaming
        );
        router.prompt_delivered();
        line_tx.send(BG_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        line_tx.send(USER_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();

        // The user turn sees only the lines after the background boundary.
        assert_eq!(recv_line(&mut turn_rx).await.as_deref(), Some(USER_LINE));
        assert_eq!(recv_line(&mut turn_rx).await.as_deref(), Some(BG_RESULT));

        // The background turn's lines await an attach consumer.
        let (attach_tx, mut attach_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(attach_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_RESULT));
    }

    #[tokio::test]
    async fn attach_registration_streams_a_live_background_turn_to_its_end() {
        let router = Arc::new(StdoutRouter::new(|| {}));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        line_tx.send(BG_LINE.to_string()).await.unwrap();
        wait_for(|| router.is_busy()).await;
        let (attach_tx, mut attach_rx) = mpsc::channel(16);
        let (registration, _guard) = router.register_owned(attach_tx, true).unwrap();
        assert_eq!(registration, RouterRegistration::StreamingLive);
        // Buffered prefix, then live continuation, ending at the result.
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_LINE));
        line_tx.send(USER_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(USER_LINE));
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_RESULT));
        assert!(recv_line(&mut attach_rx).await.is_none());
        wait_for(|| !router.is_busy()).await;
    }

    #[tokio::test]
    async fn terminal_receipt_closes_steering_before_router_attribution() {
        let router = Arc::new(StdoutRouter::new(|| {}));
        let (turn_tx, _turn_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(turn_tx, false).unwrap(),
            RouterRegistration::Streaming
        );
        router.prompt_delivered();
        assert!(router.can_accept_steer(false));

        let _turn_boundary = router.turn_boundary.lock().await;
        router.line_received(BG_RESULT);

        assert!(
            !router.can_accept_steer(false),
            "stdout receipt must close steering without waiting for a writer boundary"
        );
        assert!(
            router.is_busy(),
            "terminal receipt must not bypass normal router attribution"
        );
    }

    #[tokio::test]
    async fn lines_before_prompt_delivery_are_background_not_turn_output() {
        let signals = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = signals.clone();
        let router = Arc::new(StdoutRouter::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        // An autonomous turn starting between registration and the prompt
        // write must not be mistaken for the registered turn's response.
        let (turn_tx, mut turn_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(turn_tx, false).unwrap(),
            RouterRegistration::Streaming
        );
        line_tx.send(BG_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 1).await;

        router.prompt_delivered();
        line_tx.send(USER_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        assert_eq!(recv_line(&mut turn_rx).await.as_deref(), Some(USER_LINE));
        assert_eq!(recv_line(&mut turn_rx).await.as_deref(), Some(BG_RESULT));

        // The pre-delivery autonomous turn is intact for an attach consumer.
        let (attach_tx, mut attach_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(attach_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_RESULT));
    }

    #[tokio::test]
    async fn dead_attach_consumer_reinserts_its_line_and_reannounces() {
        let signals = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = signals.clone();
        let router = Arc::new(StdoutRouter::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        line_tx.send(BG_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 1).await;

        // The attach consumer dies before draining anything.
        let (attach_tx, attach_rx) = mpsc::channel::<String>(16);
        drop(attach_rx);
        assert_eq!(
            router.register(attach_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        // The failed drain reinserts the line and re-announces the turn.
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 2).await;
        let (retry_tx, mut retry_rx) = mpsc::channel(16);
        let (registration, _retry_guard) = router.register_owned(retry_tx, true).unwrap();
        assert_eq!(registration, RouterRegistration::Streaming);
        assert_eq!(recv_line(&mut retry_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut retry_rx).await.as_deref(), Some(BG_RESULT));
        assert!(recv_line(&mut retry_rx).await.is_none());
    }

    #[tokio::test]
    async fn owned_attach_registration_clears_when_dropped_before_polling() {
        let signals = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = signals.clone();
        let router = Arc::new(StdoutRouter::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        line_tx.send(BG_LINE.to_string()).await.unwrap();
        line_tx.send(BG_RESULT.to_string()).await.unwrap();
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 1).await;

        let (attach_tx, _attach_rx) = mpsc::channel::<String>(16);
        let (registration, guard) = router.register_owned(attach_tx, true).unwrap();
        assert_eq!(registration, RouterRegistration::Streaming);
        assert!(router.is_busy(), "the eager registration owns the router");
        drop(guard);
        assert!(
            !router.is_busy(),
            "dropping an unpolled registration must release the router"
        );
        assert!(router.has_pending_background());
        assert_eq!(signals.load(std::sync::atomic::Ordering::SeqCst), 2);

        let (retry_tx, mut retry_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(retry_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        assert_eq!(recv_line(&mut retry_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut retry_rx).await.as_deref(), Some(BG_RESULT));
    }

    #[tokio::test]
    async fn buffered_backlog_reannounces_after_each_attach_turn() {
        let signals = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = signals.clone();
        let router = Arc::new(StdoutRouter::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let (line_tx, line_rx) = mpsc::channel(16);
        let _task = tokio::spawn(Arc::clone(&router).run(line_rx));

        // Two complete autonomous turns buffer while no listener attaches;
        // even if one of their signals had been lost, draining the first
        // turn must re-announce the second.
        for _ in 0..2 {
            line_tx.send(BG_LINE.to_string()).await.unwrap();
            line_tx.send(BG_RESULT.to_string()).await.unwrap();
        }
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 2).await;

        let (attach_tx, mut attach_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(attach_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut attach_rx).await.as_deref(), Some(BG_RESULT));
        assert!(recv_line(&mut attach_rx).await.is_none());
        // Draining turn one re-announced turn two.
        wait_for(|| signals.load(std::sync::atomic::Ordering::SeqCst) == 3).await;
        let (second_tx, mut second_rx) = mpsc::channel(16);
        assert_eq!(
            router.register(second_tx, true).unwrap(),
            RouterRegistration::Streaming
        );
        assert_eq!(recv_line(&mut second_rx).await.as_deref(), Some(BG_LINE));
        assert_eq!(recv_line(&mut second_rx).await.as_deref(), Some(BG_RESULT));
    }

    #[test]
    fn parses_authoritative_claude_auth_status() {
        assert!(auth_status_is_logged_in(br#"{"loggedIn":true}"#));
        assert!(!auth_status_is_logged_in(br#"{"loggedIn":false}"#));
        assert!(!auth_status_is_logged_in(br#"{"authMethod":"none"}"#));
        assert!(!auth_status_is_logged_in(b"not json"));
    }

    fn turn(model: &str, key: &str, value: &str) -> BackendTurn {
        BackendTurn {
            cancel: Default::default(),
            thread_id: "thread".into(),
            worktree: std::env::temp_dir(),
            session: None,
            model: model.into(),
            model_options: serde_json::Map::from_iter([(key.into(), Value::String(value.into()))]),
            prompt: String::new(),
            attachments: Vec::new(),
            instructions: None,
            permission: BackendPermission::ReadOnly,
            tool_free: false,
            attach_background: false,
            mcp_bridge: None,
            mcp_servers: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    fn write_claude_stub(
        directory: &std::path::Path,
        name: &str,
        body: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = directory.join(name);
        std::fs::write(&path, body).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(target_os = "linux")]
    fn marker_path(stub: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("{}.{suffix}", stub.display()))
    }

    #[cfg(target_os = "linux")]
    fn lifecycle_turn(
        worktree: &std::path::Path,
        thread_id: &str,
        model: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> BackendTurn {
        BackendTurn {
            cancel,
            thread_id: thread_id.into(),
            worktree: worktree.to_path_buf(),
            session: Some("session-1".into()),
            model: model.into(),
            model_options: serde_json::Map::new(),
            prompt: "test prompt".into(),
            attachments: Vec::new(),
            instructions: None,
            permission: BackendPermission::ReadOnly,
            tool_free: true,
            attach_background: false,
            mcp_bridge: None,
            mcp_servers: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_pids(path: &std::path::Path, count: usize) -> Vec<u32> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(contents) = std::fs::read_to_string(path) {
                    let pids: Vec<u32> = contents
                        .lines()
                        .filter_map(|line| line.trim().parse().ok())
                        .collect();
                    if pids.len() >= count {
                        break pids;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {} PID records", path.display()))
    }

    #[cfg(target_os = "linux")]
    async fn assert_pids_gone(pids: &[u32]) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if pids
                    .iter()
                    .all(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Claude process tree still present: {pids:?}"));
    }

    #[cfg(target_os = "linux")]
    const HANGING_CLAUDE_STUB: &str = r#"#!/bin/sh
printf '%s\n' "$$" >> "$0.leaders"
sleep 60 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!" >> "$0.descendants"
cat >/dev/null
"#;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancellation_acknowledges_complete_process_tree_before_stream_closes() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(directory.path(), "claude-cancel", HANGING_CLAUDE_STUB);
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));
        let cancel = tokio_util::sync::CancellationToken::new();
        let turn = lifecycle_turn(directory.path(), "cancel-thread", "model-a", cancel.clone());
        let mut stream = backend.run_turn(turn).await.unwrap();
        let leaders = wait_for_pids(&marker_path(&stub, "leaders"), 1).await;
        let descendants = wait_for_pids(&marker_path(&stub, "descendants"), 1).await;

        cancel.cancel();
        while tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("cancelled Claude stream did not finish after cleanup")
            .is_some()
        {}

        assert_pids_gone(&leaders).await;
        assert_pids_gone(&descendants).await;
        assert!(backend.pool.procs.lock().await.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_stream_acknowledges_complete_process_tree() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(directory.path(), "claude-drop", HANGING_CLAUDE_STUB);
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));
        let turn = lifecycle_turn(
            directory.path(),
            "drop-thread",
            "model-a",
            tokio_util::sync::CancellationToken::new(),
        );
        let stream = backend.run_turn(turn).await.unwrap();
        let leaders = wait_for_pids(&marker_path(&stub, "leaders"), 1).await;
        let descendants = wait_for_pids(&marker_path(&stub, "descendants"), 1).await;

        drop(stream);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if backend.pool.procs.lock().await.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropping the Claude stream did not acknowledge cleanup");
        assert_pids_gone(&leaders).await;
        assert_pids_gone(&descendants).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancellation_before_run_turn_returns_does_not_spawn() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(directory.path(), "claude-pre-cancel", HANGING_CLAUDE_STUB);
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let turn = lifecycle_turn(directory.path(), "cancelled", "model-a", cancel);

        assert!(matches!(
            backend.run_turn(turn).await,
            Err(BackendError::Cancelled)
        ));
        assert!(!marker_path(&stub, "leaders").exists());
        assert!(backend.pool.procs.lock().await.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cleanup_failure_denies_replacement_until_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(directory.path(), "claude-quarantine", HANGING_CLAUDE_STUB);
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));
        let cancel = tokio_util::sync::CancellationToken::new();
        let original_turn = lifecycle_turn(
            directory.path(),
            "quarantine-thread",
            "model-a",
            cancel.clone(),
        );
        let original = backend.proc_for(&original_turn, &cancel).await.unwrap();
        let original_leaders = wait_for_pids(&marker_path(&stub, "leaders"), 1).await;
        let original_descendants = wait_for_pids(&marker_path(&stub, "descendants"), 1).await;
        original
            .injected_terminate_failure
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let changed_turn = lifecycle_turn(
            directory.path(),
            "quarantine-thread",
            "model-b",
            cancel.clone(),
        );

        for _ in 0..2 {
            let error = match backend.proc_for(&changed_turn, &cancel).await {
                Ok(_) => panic!("replacement started over an unclean Claude tree"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("injected Claude"));
        }
        assert_eq!(
            std::fs::read_to_string(marker_path(&stub, "leaders"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let retained = backend
            .pool
            .procs
            .lock()
            .await
            .get("quarantine-thread")
            .cloned()
            .unwrap();
        assert!(Arc::ptr_eq(&retained, &original));
        assert!(!retained.is_reusable());

        original
            .injected_terminate_failure
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let replacement = backend.proc_for(&changed_turn, &cancel).await.unwrap();
        let all_leaders = wait_for_pids(&marker_path(&stub, "leaders"), 2).await;
        let all_descendants = wait_for_pids(&marker_path(&stub, "descendants"), 2).await;
        assert!(!Arc::ptr_eq(&replacement, &original));
        assert_pids_gone(&original_leaders).await;
        assert_pids_gone(&original_descendants).await;

        backend
            .pool
            .terminate_and_remove("quarantine-thread", &replacement)
            .await
            .unwrap();
        assert_pids_gone(&all_leaders).await;
        assert_pids_gone(&all_descendants).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn eof_cleanup_reaps_surviving_descendant_before_reporting() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(
            directory.path(),
            "claude-eof",
            r#"#!/bin/sh
printf '%s\n' "$$" >> "$0.leaders"
sleep 60 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!" >> "$0.descendants"
exit 7
"#,
        );
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));
        let turn = lifecycle_turn(
            directory.path(),
            "eof-thread",
            "model-a",
            tokio_util::sync::CancellationToken::new(),
        );
        let mut stream = backend.run_turn(turn).await.unwrap();
        let leaders = wait_for_pids(&marker_path(&stub, "leaders"), 1).await;
        let descendants = wait_for_pids(&marker_path(&stub, "descendants"), 1).await;
        let mut saw_exit = false;
        while let Some(event) = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("Claude EOF cleanup did not finish")
        {
            if event.is_err() {
                saw_exit = true;
            }
        }
        assert!(saw_exit);
        assert_pids_gone(&leaders).await;
        assert_pids_gone(&descendants).await;
        assert!(backend.pool.procs.lock().await.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn usage_query_reaps_its_complete_process_tree() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(
            directory.path(),
            "claude-usage",
            r#"#!/bin/sh
printf '%s\n' "$$" >> "$0.leaders"
sleep 60 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!" >> "$0.descendants"
IFS= read -r request
printf '%s\n' '{"type":"control_response","response":{"request_id":"trouve-usage","subtype":"success","response":{"subscription_type":"pro"}}}'
cat >/dev/null
"#,
        );
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));

        let payload = backend.query_usage().await.unwrap();
        let leaders = wait_for_pids(&marker_path(&stub, "leaders"), 1).await;
        let descendants = wait_for_pids(&marker_path(&stub, "descendants"), 1).await;
        assert_eq!(payload["subscription_type"], "pro");
        assert_pids_gone(&leaders).await;
        assert_pids_gone(&descendants).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn usage_cleanup_failure_denies_next_probe_until_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let stub = write_claude_stub(
            directory.path(),
            "claude-usage-quarantine",
            r#"#!/bin/sh
printf '%s\n' "$$" >> "$0.leaders"
sleep 60 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!" >> "$0.descendants"
IFS= read -r request
printf '%s\n' '{"type":"control_response","response":{"request_id":"trouve-usage","subtype":"success","response":{"subscription_type":"max"}}}'
cat >/dev/null
"#,
        );
        let backend = ClaudeBackend::new("claude-code", Some(stub.to_string_lossy().into_owned()));
        backend
            .injected_usage_cleanup_failure
            .store(true, std::sync::atomic::Ordering::Relaxed);

        for _ in 0..2 {
            let error = backend.query_usage().await.unwrap_err();
            assert!(error.to_string().contains("injected Claude usage"));
        }
        let first_leaders = wait_for_pids(&marker_path(&stub, "leaders"), 1).await;
        let first_descendants = wait_for_pids(&marker_path(&stub, "descendants"), 1).await;
        assert_eq!(
            std::fs::read_to_string(marker_path(&stub, "leaders"))
                .unwrap()
                .lines()
                .count(),
            1,
            "a second usage process started over an unclean first tree"
        );
        assert!(backend.usage_process.lock().await.is_some());

        backend
            .injected_usage_cleanup_failure
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let payload = backend.query_usage().await.unwrap();
        let all_leaders = wait_for_pids(&marker_path(&stub, "leaders"), 2).await;
        let all_descendants = wait_for_pids(&marker_path(&stub, "descendants"), 2).await;
        assert_eq!(payload["subscription_type"], "max");
        assert_pids_gone(&first_leaders).await;
        assert_pids_gone(&first_descendants).await;
        assert_pids_gone(&all_leaders).await;
        assert_pids_gone(&all_descendants).await;
        assert!(backend.usage_process.lock().await.is_none());
    }

    #[test]
    fn adaptive_models_use_cli_effort_flag() {
        let mut cmd = tokio::process::Command::new("claude");
        let catalog = trouve_providers::models_dev::ModelsDevCatalog::embedded();
        configure_thinking(
            &mut cmd,
            &turn("claude-fable-5", "thinking_level", "xhigh"),
            &catalog,
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["--effort", "xhigh"]);
    }

    #[test]
    fn models_dev_owns_claude_code_display_metadata() {
        let backend = ClaudeBackend::new("claude-code", None);
        let model = backend
            .models()
            .into_iter()
            .find(|model| model.id == "claude-code/claude-fable-5")
            .unwrap();
        assert_eq!(model.display_name, "Claude Fable 5");
        assert_eq!(model.context_window, 1_000_000);
        assert_eq!(model.input_price_per_mtok, None);
        assert_eq!(model.output_price_per_mtok, None);
    }

    #[test]
    fn legacy_off_explicitly_disables_thinking() {
        let mut cmd = tokio::process::Command::new("claude");
        let catalog = trouve_providers::models_dev::ModelsDevCatalog::embedded();
        configure_thinking(
            &mut cmd,
            &turn("claude-haiku-4-5", "thinking_level", "off"),
            &catalog,
        );
        let disabled = cmd
            .as_std()
            .get_envs()
            .find(|(key, _)| key.to_string_lossy() == "CLAUDE_CODE_DISABLE_THINKING")
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(disabled.as_deref(), Some("1"));
    }

    fn rfc3339_in(secs: i64) -> String {
        chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + secs, 0)
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn parses_flat_usage_buckets() {
        let payload = json!({
            "subscription_type": "max",
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": 42.4, "resets_at": rfc3339_in(2 * 3600 + 600) },
                "seven_day": { "utilization": 13.0, "resets_at": rfc3339_in(3 * 86_400 + 600) },
                "seven_day_sonnet": { "utilization": 7.6, "resets_at": rfc3339_in(86_400) },
                "seven_day_opus": null,
                "extra_usage": { "is_enabled": false },
            },
        });
        let health = parse_usage_health("claude-code", &payload);
        assert_eq!(health.status, "ok");
        assert_eq!(health.plan, "max");
        assert_eq!(health.credits, "");
        let labels: Vec<&str> = health.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["5h window", "Weekly (all models)", "Weekly (Sonnet)"]
        );
        assert_eq!(health.windows[0].used_percent, 42);
        assert!(health.windows[0].resets.starts_with("resets in 2h"));
        assert_eq!(health.windows[2].used_percent, 8, "rounded");
        assert!(health.windows[1].resets.starts_with("resets in 3d"));
    }

    #[test]
    fn parses_limits_array_and_dedupes_flat_buckets() {
        // Transitional payloads can carry both shapes for the same bucket;
        // the scoped Opus week exists only in the array.
        let soon = chrono::Utc::now().timestamp() + 3600;
        let payload = json!({
            "subscription_type": "pro",
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": 30.0, "resets_at": rfc3339_in(3600) },
                "seven_day": null,
                "limits": [
                    { "kind": "session", "percent": 30.0, "resets_at": soon },
                    { "kind": "weekly_all", "percent": 55.0, "resets_at": soon + 86_400 },
                    {
                        "kind": "weekly_scoped",
                        "percent": 61.0,
                        "resets_at": soon,
                        "scope": { "model": { "display_name": "Opus" } },
                    },
                    { "kind": "mystery", "percent": 1.0 },
                ],
            },
        });
        let health = parse_usage_health("claude-code", &payload);
        assert_eq!(health.status, "ok");
        let labels: Vec<&str> = health.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["5h window", "Weekly (all models)", "Weekly (Opus)"],
            "session came from the flat bucket; the array filled the rest"
        );
        assert_eq!(health.windows[1].used_percent, 55);
        assert!(health.windows[2].resets.starts_with("resets in "));
    }

    #[test]
    fn formats_extra_usage_credits() {
        let payload = json!({
            "subscription_type": "max",
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": 10.0 },
                "extra_usage": {
                    "is_enabled": true,
                    "monthly_limit": 5000,
                    "used_credits": 42.0,
                },
            },
        });
        let health = parse_usage_health("claude-code", &payload);
        assert_eq!(health.credits, "extra usage: $0.42 of $50.00");
        assert_eq!(health.windows[0].resets, "", "no reset info is fine");
    }

    #[test]
    fn no_rate_limits_means_unavailable() {
        // Not logged in (or API-key auth): the CLI answers the control
        // request but has no subscription data.
        let payload = json!({
            "subscription_type": null,
            "rate_limits_available": false,
            "rate_limits": null,
        });
        let health = parse_usage_health("claude-code", &payload);
        assert_eq!(health.status, "unavailable");
        assert!(health.note.contains("claude.ai login"));
        assert!(health.windows.is_empty());
    }

    #[test]
    fn result_error_detects_error_results() {
        // Subscription limit error with is_error flag
        let ev = json!({
            "type": "result",
            "is_error": true,
            "session_id": "session-123",
            "result": "You've reached your usage limit",
        });
        assert!(result_error(&ev).is_some());
        assert!(result_error(&ev).unwrap().contains("usage limit"));

        // Error subtype
        let ev = json!({
            "type": "result",
            "subtype": "error_subscription_limit",
            "session_id": "session-456",
            "error": "Limit exceeded",
        });
        assert!(result_error(&ev).is_some());

        // Successful result should not be an error
        let ev = json!({
            "type": "result",
            "is_error": false,
            "session_id": "session-789",
            "usage": { "input_tokens": 100 },
        });
        assert!(result_error(&ev).is_none());
    }

    #[test]
    fn error_results_preserve_session_id() {
        // Error results should carry session_id just like successful ones.
        // This test verifies the structure used by the event loop fix.
        let error_result = json!({
            "type": "result",
            "is_error": true,
            "session_id": "session-after-error-abc123",
            "result": "Subscription limit exceeded",
            "usage": {
                "input_tokens": 50,
                "output_tokens": 0,
            },
        });

        // Verify it's detected as an error
        assert!(result_error(&error_result).is_some());

        // Verify session_id is accessible (as the event loop fix relies on)
        assert_eq!(
            error_result["session_id"].as_str(),
            Some("session-after-error-abc123")
        );

        // Successful results also have session_id via map_event
        let success_result = json!({
            "type": "result",
            "session_id": "session-success-xyz789",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
            },
        });

        let events = map_event(&success_result);
        // Should produce SessionStarted + Completed events
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], BackendEvent::SessionStarted { .. }));
        if let BackendEvent::SessionStarted { session_id } = &events[0] {
            assert_eq!(session_id, "session-success-xyz789");
        }
    }
}
