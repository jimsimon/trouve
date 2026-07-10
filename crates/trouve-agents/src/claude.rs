//! Claude Code backend, driving the `claude` CLI in print mode.
//!
//! One persistent `claude -p --input-format stream-json` process is kept per
//! trouve thread (see [`Pool`]): turns after the first skip the CLI's cold
//! start, the transcript re-read, and the MCP bridge re-handshake. The pool
//! is bounded (LRU cap + idle reaping); killing a process loses nothing
//! because Claude Code persists the transcript and `--resume` restores it.
//! Claude Code rotates its session id on every resume, so we re-persist the
//! id from each turn's `system/init` / `result` events.
//!
//! Permission mapping: `Yolo` → `--dangerously-skip-permissions`,
//! `ReadOnly` → disallowed mutating built-ins + trouve's approval gate,
//! `Ask` → the trouve MCP bridge's `approval_prompt` tool via
//! `--permission-prompt-tool`, so headless print mode routes permission
//! requests to trouve's approval flow instead of failing them.
//!
//! Login is an interactive TUI (`/login` inside `claude`); we detect
//! credentials but can't orchestrate the flow headlessly.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    async_stream, binary_on_path, AgentBackend, BackendError, BackendEvent, BackendEventStream,
    BackendLogin, BackendPermission, BackendStatus, BackendTurn,
};

/// Most live processes kept at once; the least recently used is evicted.
const POOL_CAP: usize = 3;
/// Idle time after which a pooled process is reaped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How often the reaper scans the pool.
const REAP_INTERVAL: Duration = Duration::from_secs(60);

pub struct ClaudeBackend {
    id: String,
    command: String,
    pool: Arc<Pool>,
}

impl ClaudeBackend {
    pub fn new(id: impl Into<String>, command: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "claude".into()),
            pool: Arc::new(Pool::default()),
        }
    }
}

/// Live `claude` processes keyed by trouve thread id.
#[derive(Default)]
struct Pool {
    procs: Mutex<HashMap<String, Arc<ClaudeProc>>>,
    reaper_started: std::sync::atomic::AtomicBool,
}

impl Pool {
    async fn remove(&self, thread_id: &str, proc_: &Arc<ClaudeProc>) {
        let mut procs = self.procs.lock().await;
        // Only remove the entry if it is still this process (a respawn may
        // have replaced it already).
        if procs.get(thread_id).is_some_and(|p| Arc::ptr_eq(p, proc_)) {
            procs.remove(thread_id);
        }
    }

    /// Kill processes idle past the timeout, skipping any with a turn in
    /// flight (their line receiver is locked).
    async fn reap_idle(&self) {
        let mut procs = self.procs.lock().await;
        let mut dead = Vec::new();
        for (id, p) in procs.iter() {
            if p.lines.try_lock().is_err() {
                continue; // turn in flight
            }
            if p.last_used.lock().unwrap().elapsed() > IDLE_TIMEOUT {
                dead.push(id.clone());
            }
        }
        for id in dead {
            if let Some(p) = procs.remove(&id) {
                p.kill().await;
            }
        }
    }

    /// Evict the least recently used idle process while over capacity.
    async fn enforce_cap(procs: &mut HashMap<String, Arc<ClaudeProc>>) {
        while procs.len() >= POOL_CAP {
            let lru = procs
                .iter()
                .filter(|(_, p)| p.lines.try_lock().is_ok())
                .min_by_key(|(_, p)| *p.last_used.lock().unwrap())
                .map(|(id, _)| id.clone());
            let Some(id) = lru else { break }; // all busy: allow overflow
            if let Some(p) = procs.remove(&id) {
                p.kill().await;
            }
        }
    }
}

/// One persistent `claude` process serving one trouve thread.
struct ClaudeProc {
    stdin: Mutex<ChildStdin>,
    /// Stdout lines; locked by the active turn for its whole duration.
    lines: Mutex<mpsc::Receiver<String>>,
    child: Mutex<Child>,
    /// Spawn-time configuration; a differing turn forces a respawn.
    config_fp: String,
    /// Vendor session id the process is holding, updated from its events.
    /// A turn arriving with a different id (e.g. after undo) respawns.
    session: std::sync::Mutex<Option<String>>,
    last_used: std::sync::Mutex<Instant>,
    /// Rolling stderr tail for error reporting.
    stderr_tail: Arc<std::sync::Mutex<String>>,
}

impl ClaudeProc {
    async fn kill(&self) {
        let _ = self.child.lock().await.kill().await;
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }
}

/// Spawn-time configuration that must match for a process to be reused.
fn config_fingerprint(turn: &BackendTurn) -> String {
    let bridge = turn.mcp_bridge.as_ref().map(|b| {
        format!(
            "{}|{:?}|{:?}|{}|{:?}",
            b.command, b.args, b.env, b.bridge_tools, b.disallowed_tools
        )
    });
    format!(
        "{:?}|{}|{:?}|{:?}|{:?}|{:?}",
        turn.worktree,
        turn.model,
        Value::Object(turn.model_options.clone()),
        turn.instructions,
        turn.permission,
        bridge,
    )
}

#[async_trait::async_trait]
impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // The same catalog as the per-use Anthropic API provider, so both
        // surface the same list. Claude Code accepts full model ids; the
        // subscription bills nothing per token, so pricing is dropped.
        trouve_providers::catalog::anthropic_models(&self.id)
            .into_iter()
            .map(|mut m| {
                m.display_name = format!("{} (Claude Code)", m.display_name);
                m.input_price_per_mtok = None;
                m.output_price_per_mtok = None;
                // Temperature isn't controllable through the CLI.
                m.options_schema = serde_json::json!({
                    "type": "object",
                    "properties": {
                        "thinking_level": m.options_schema
                            .pointer("/properties/thinking_level")
                            .cloned()
                            .unwrap_or_default(),
                    }
                });
                m
            })
            .collect()
    }

    fn status(&self) -> BackendStatus {
        let home = dirs::home_dir();
        let has_credentials = home
            .map(|h| {
                h.join(".claude").join(".credentials.json").exists()
                    || h.join(".claude.json").exists()
            })
            .unwrap_or(false);
        BackendStatus {
            installed: binary_on_path(&self.command),
            has_credentials,
        }
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        Err(BackendError::Auth(
            "Claude Code login is interactive: run `claude` in a terminal and use /login".into(),
        ))
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        self.start_reaper();
        let proc_ = self.proc_for(&turn).await?;
        let pool = self.pool.clone();
        let thread_id = turn.thread_id.clone();
        let prompt = turn.prompt.clone();

        let stream = async_stream(move |tx| async move {
            // Exclusive claim on the process for this turn.
            let mut lines = proc_.lines.lock().await;
            proc_.touch();

            let msg = json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [ { "type": "text", "text": prompt } ],
                }
            });
            let sent = {
                let mut stdin = proc_.stdin.lock().await;
                async {
                    stdin.write_all(msg.to_string().as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                }
                .await
            };
            if let Err(e) = sent {
                // Likely the process died between turns; keep reading — the
                // no-result exit path below reports it (with stderr) and
                // drops it from the pool so the next turn respawns.
                tracing::debug!("claude stdin write failed: {e}");
            }

            let mut completed = false;
            while let Some(line) = lines.recv().await {
                let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
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
                    if tx.send(Ok(out)).await.is_err() {
                        // Consumer dropped mid-turn (cancel): the CLI has no
                        // per-turn abort in this mode, so kill the process.
                        // The transcript is on disk; next turn resumes it.
                        pool.remove(&thread_id, &proc_).await;
                        proc_.kill().await;
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
                // Stdout closed without a result: the process died.
                pool.remove(&thread_id, &proc_).await;
                let status = proc_.child.lock().await.wait().await;
                let _ = tx
                    .send(Err(BackendError::Protocol(format!(
                        "claude exited with {:?}: {}",
                        status.ok(),
                        proc_.stderr_tail.lock().unwrap().trim()
                    ))))
                    .await;
            }
        });
        Ok(stream.boxed())
    }
}

impl ClaudeBackend {
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
    async fn proc_for(&self, turn: &BackendTurn) -> Result<Arc<ClaudeProc>, BackendError> {
        let fp = config_fingerprint(turn);
        let mut procs = self.pool.procs.lock().await;
        if let Some(p) = procs.get(&turn.thread_id) {
            let alive = p.child.lock().await.try_wait().ok().flatten().is_none();
            let session_matches = match (&turn.session, p.session.lock().unwrap().as_ref()) {
                (Some(want), Some(have)) => want == have,
                (None, _) => false, // explicit fresh session: start over
                (Some(_), None) => false,
            };
            if alive && p.config_fp == fp && session_matches {
                return Ok(p.clone());
            }
            let p = procs.remove(&turn.thread_id).expect("checked above");
            p.kill().await;
        }

        Pool::enforce_cap(&mut procs).await;
        let proc_ = Arc::new(self.spawn(turn, fp)?);
        procs.insert(turn.thread_id.clone(), proc_.clone());
        Ok(proc_)
    }

    /// Spawn a persistent `claude` process configured for this turn's
    /// thread. The prompt is NOT passed here; turns arrive over stdin.
    fn spawn(&self, turn: &BackendTurn, config_fp: String) -> Result<ClaudeProc, BackendError> {
        let mut cmd = Command::new(&self.command);
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
        // Extended thinking rides on Claude Code's budget env var.
        if let Some(budget) = turn
            .model_options
            .get("thinking_level")
            .and_then(Value::as_str)
            .and_then(trouve_providers::catalog::thinking_budget_tokens)
        {
            cmd.env("MAX_THINKING_TOKENS", budget.to_string());
        }
        if let Some(instr) = &turn.instructions {
            cmd.args(["--append-system-prompt", instr]);
        }
        // MCP bridge back into trouve. Two roles, both optional:
        //  - approval gate: in Ask mode, Claude's permission requests go to
        //    the bridge's approval_prompt tool (trouve's approval flow)
        //    instead of failing in headless print mode;
        //  - tool bridge: Claude's built-ins stand down and trouve's
        //    ToolExecutor serves tools (approvals then gate inside trouve,
        //    so the bridged server is pre-allowed).
        if let Some(bridge) = &turn.mcp_bridge {
            let env: serde_json::Map<String, serde_json::Value> = bridge
                .env
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            let mcp_config = serde_json::json!({
                "mcpServers": {
                    "trouve": {
                        "command": bridge.command,
                        "args": bridge.args,
                        "env": env,
                    }
                }
            });
            let path = std::env::temp_dir().join(format!("trouve-mcp-{}.json", turn.thread_id));
            std::fs::write(&path, mcp_config.to_string())?;
            cmd.arg("--mcp-config").arg(&path);
            cmd.arg("--strict-mcp-config");
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
            if matches!(
                turn.permission,
                BackendPermission::Ask | BackendPermission::ReadOnly
            ) {
                cmd.args(["--permission-prompt-tool", "mcp__trouve__approval_prompt"]);
            }
        }
        match turn.permission {
            BackendPermission::Yolo => {
                cmd.arg("--dangerously-skip-permissions");
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

        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
            _ => BackendError::Io(e),
        })?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // Stdout pump: lines flow into the channel the active turn drains.
        let (line_tx, line_rx) = mpsc::channel::<String>(256);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line_tx.send(line).await.is_err() {
                    break;
                }
            }
        });

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
            stdin: Mutex::new(stdin),
            lines: Mutex::new(line_rx),
            child: Mutex::new(child),
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
                    // The CLI reports an estimate even on subscription
                    // plans, where nothing is billed per turn; suppress it
                    // like the other subscription backends.
                    cost_usd: None,
                },
            });
            events
        }
        _ => vec![],
    }
}
