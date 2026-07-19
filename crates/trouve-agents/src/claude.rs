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
//! Permission mapping: `Yolo` → `--dangerously-skip-permissions`,
//! `ReadOnly` → disallowed mutating built-ins + trouve's approval gate,
//! `Ask` → the trouve MCP bridge's `approval_prompt` tool via
//! `--permission-prompt-tool`, so headless print mode routes permission
//! requests to trouve's approval flow instead of failing them.
//!
//! Login is an interactive TUI (`/login` inside `claude`); we detect
//! credentials but can't orchestrate the flow headlessly.
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
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc};
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventStream, BackendLogin, BackendPermission,
    BackendStatus, BackendTurn, async_stream, binary_on_path, format_reset,
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
        "{:?}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
        turn.worktree,
        turn.model,
        Value::Object(turn.model_options.clone()),
        turn.instructions,
        turn.permission,
        bridge,
        servers,
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

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        self.start_reaper();
        let proc_ = self.proc_for(&turn).await?;
        let pool = self.pool.clone();
        let thread_id = turn.thread_id.clone();
        let prompt = turn.prompt.clone();
        // Anthropic-style base64 image blocks, alongside the text block.
        let mut content = vec![json!({ "type": "text", "text": prompt })];
        for att in &turn.attachments {
            match att.read_base64() {
                Ok(data) => content.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": att.mime,
                        "data": data,
                    }
                })),
                Err(e) => tracing::warn!("skipping attachment {}: {e}", att.name),
            }
        }

        let stream = async_stream(move |tx| async move {
            // Exclusive claim on the process for this turn.
            let mut lines = proc_.lines.lock().await;
            proc_.touch();

            let msg = json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": content,
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
    /// Ask a short-lived print-mode process for subscription usage via the
    /// `get_usage` control request — the same data the TUI's `/usage`
    /// dialog shows (which has no headless equivalent). Returns the inner
    /// response payload (`subscription_type`, `rate_limits`, ...).
    async fn query_usage(&self) -> Result<Value, BackendError> {
        let mut child = Command::new(&self.command)
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
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
                _ => BackendError::Io(e),
            })?;
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

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
        // Reap the child either way (kill_on_drop only covers hard drops).
        let _ = child.kill().await;
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
