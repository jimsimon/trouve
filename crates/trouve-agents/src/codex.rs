//! Codex app-server backend.
//!
//! Speaks the sanctioned `codex app-server` JSON-RPC-over-stdio interface
//! (the same one the Codex IDE extension uses). One child process is spawned
//! lazily and shared across threads; trouve threads map 1:1 to app-server
//! threads via the persisted backend session id.
//!
//! Wire shape (from the official app-server docs):
//! - handshake: `initialize` request then `initialized` notification
//! - `thread/start` / `thread/resume` → `{ result: { thread: { id } } }`
//! - `turn/start { threadId, input: [{type:"text",text}] }` then notifications:
//!   `item/agentMessage/delta`, `item/started`, `item/completed`,
//!   `item/commandExecution/outputDelta`, `thread/tokenUsage/updated`,
//!   `turn/completed`
//! - server-initiated approval requests:
//!   `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`
//!   answered with `{ decision: "approved" | "denied" }`

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    async_stream, binary_on_path, model, spawn_login, AgentBackend, BackendError, BackendEvent,
    BackendEventStream, BackendLogin, BackendPermission, BackendStatus, BackendTurn,
};

pub struct CodexBackend {
    id: String,
    command: String,
    server: Mutex<Option<Arc<AppServer>>>,
    /// `model/list` result, cached for [`MODELS_TTL`].
    models_cache: Mutex<Option<(std::time::Instant, Vec<ModelInfo>)>>,
    /// Real context windows by model name, learned from
    /// `thread/tokenUsage/updated` (`model/list` doesn't report them).
    observed_windows: Arc<std::sync::Mutex<HashMap<String, u64>>>,
}

/// How long a fetched vendor model list stays fresh.
const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

impl CodexBackend {
    pub fn new(id: impl Into<String>, command: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "codex".into()),
            server: Mutex::new(None),
            models_cache: Mutex::new(None),
            observed_windows: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Replace catalog context windows with real values observed from live
    /// turns, matched on the model name after the backend prefix.
    fn apply_observed_windows(&self, models: &mut [ModelInfo]) {
        let observed = self.observed_windows.lock().unwrap();
        if observed.is_empty() {
            return;
        }
        for m in models {
            let name = m.id.rsplit_once('/').map_or(m.id.as_str(), |(_, n)| n);
            if let Some(n) = observed.get(name) {
                m.context_window = *n;
            }
        }
    }

    async fn server(&self) -> Result<Arc<AppServer>, BackendError> {
        let mut guard = self.server.lock().await;
        if let Some(s) = guard.as_ref() {
            if !s.is_closed() {
                return Ok(s.clone());
            }
        }
        let s = Arc::new(AppServer::spawn(&self.command).await?);
        s.handshake().await?;
        *guard = Some(s.clone());
        Ok(s)
    }
}

#[async_trait::async_trait]
impl AgentBackend for CodexBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Minimal offline fallback; `list_models` asks the app-server for
        // the real catalog (with per-model reasoning-effort variants).
        let mut models = vec![
            model(&self.id, "gpt-5.4-codex", "GPT-5.4 Codex", 272_000),
            model(&self.id, "gpt-5.4", "GPT-5.4", 272_000),
        ];
        self.apply_observed_windows(&mut models);
        models
    }

    async fn list_models(&self) -> Vec<ModelInfo> {
        {
            let cache = self.models_cache.lock().await;
            if let Some((at, models)) = cache.as_ref() {
                if at.elapsed() < MODELS_TTL {
                    let mut models = models.clone();
                    self.apply_observed_windows(&mut models);
                    return models;
                }
            }
        }
        let fetched = async {
            let server = self.server().await?;
            server.request("model/list", json!({})).await
        }
        .await;
        match fetched {
            Ok(result) => {
                let mut models = parse_model_list(&self.id, &result);
                if models.is_empty() {
                    return self.models();
                }
                *self.models_cache.lock().await = Some((std::time::Instant::now(), models.clone()));
                self.apply_observed_windows(&mut models);
                models
            }
            Err(e) => {
                tracing::debug!("codex model/list failed: {e}; using static list");
                self.models()
            }
        }
    }

    fn status(&self) -> BackendStatus {
        let auth = dirs::home_dir()
            .map(|h| h.join(".codex").join("auth.json").exists())
            .unwrap_or(false);
        BackendStatus {
            installed: binary_on_path(&self.command),
            has_credentials: auth,
        }
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        spawn_login(&self.command, &["login"]).await
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        let server = self.server().await?;

        // Effort comes from the thread's model options; `@effort` model ids
        // from before the options split still resolve.
        let (model_name, id_effort) = split_effort(&turn.model);
        let effort = turn
            .model_options
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or(id_effort);
        let (approval_policy, sandbox, sandbox_policy_type) = match turn.permission {
            // Approval policy: untrusted | on-request | granular | never;
            // "untrusted" = ask before anything not on the trusted list.
            // The two sandbox strings are the same mode in the protocol's
            // two casings: thread/start's `sandbox` enum is kebab-case
            // while turn/start's `sandboxPolicy` type tag is camelCase.
            BackendPermission::ReadOnly => ("never", "read-only", "readOnly"),
            BackendPermission::Ask => ("untrusted", "workspace-write", "workspaceWrite"),
            BackendPermission::Yolo => ("never", "workspace-write", "workspaceWrite"),
        };

        // Start or resume the vendor-side thread.
        let mut fresh_session = false;
        let codex_thread_id = match &turn.session {
            Some(sid) => {
                let resumed = server
                    .request("thread/resume", json!({ "threadId": sid }))
                    .await;
                match resumed {
                    Ok(v) => thread_id_of(&v)?,
                    Err(e) => {
                        tracing::warn!("codex thread/resume failed ({e}); starting fresh");
                        fresh_session = true;
                        let v = server
                            .request(
                                "thread/start",
                                json!({
                                    "model": model_or_default(model_name),
                                    "cwd": turn.worktree,
                                    "approvalPolicy": approval_policy,
                                    "sandbox": sandbox,
                                    "serviceName": "trouve",
                                }),
                            )
                            .await?;
                        thread_id_of(&v)?
                    }
                }
            }
            None => {
                fresh_session = true;
                let v = server
                    .request(
                        "thread/start",
                        json!({
                            "model": model_or_default(model_name),
                            "cwd": turn.worktree,
                            "approvalPolicy": approval_policy,
                            "sandbox": sandbox,
                            "serviceName": "trouve",
                        }),
                    )
                    .await?;
                thread_id_of(&v)?
            }
        };

        let route = server.subscribe(&codex_thread_id).await;

        // Mode instructions ride along in the first user message of a fresh
        // vendor session (app-server owns the system prompt).
        let text = match (&turn.instructions, fresh_session) {
            (Some(instr), true) => format!(
                "<mode-instructions>\n{instr}\n</mode-instructions>\n\n{}",
                turn.prompt
            ),
            _ => turn.prompt.clone(),
        };

        let mut turn_params = json!({
            "threadId": codex_thread_id,
            "model": model_or_default(model_name),
            "approvalPolicy": approval_policy,
            "sandboxPolicy": { "type": sandbox_policy_type },
            "input": [ { "type": "text", "text": text } ],
        });
        if let Some(effort) = effort {
            turn_params["effort"] = json!(effort);
        }
        server.request("turn/start", turn_params).await?;

        let stream = turn_stream(
            server.clone(),
            codex_thread_id.clone(),
            route,
            fresh_session,
            model_or_default(model_name).to_string(),
            self.observed_windows.clone(),
        );
        Ok(stream.boxed())
    }
}

fn model_or_default(model: &str) -> &str {
    if model.is_empty() {
        "gpt-5.4-codex"
    } else {
        model
    }
}

/// Split a `<model>@<effort>` id into its parts. Threads created before the
/// options split stored the chosen effort as an `@` suffix; the effort now
/// travels in the thread's model options instead.
fn split_effort(model: &str) -> (&str, Option<&str>) {
    match model.rsplit_once('@') {
        Some((m, e)) if !m.is_empty() && !e.is_empty() => (m, Some(e)),
        _ => (model, None),
    }
}

/// Map a `model/list` result to ModelInfos: one entry per model, with the
/// supported reasoning efforts as a `reasoning_effort` options schema
/// (rendered as the client's thinking dropdown).
fn parse_model_list(backend_id: &str, result: &Value) -> Vec<ModelInfo> {
    let Some(data) = result["data"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in data {
        if entry["hidden"].as_bool() == Some(true) {
            continue;
        }
        let Some(id) = entry["id"].as_str() else {
            continue;
        };
        let display = entry["displayName"].as_str().unwrap_or(id);
        let default_effort = entry["defaultReasoningEffort"].as_str().unwrap_or("");
        let efforts: Vec<&str> = entry["supportedReasoningEfforts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|e| e["reasoningEffort"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let mut info = model(backend_id, id, display, 272_000);
        if efforts.len() > 1 {
            info.options_schema = json!({
                "type": "object",
                "properties": {
                    "reasoning_effort": {
                        "type": "string",
                        "enum": efforts,
                        "default": default_effort,
                        "description": "How much thinking the model does before answering"
                    }
                }
            });
        }
        out.push(info);
    }
    out
}

fn thread_id_of(result: &Value) -> Result<String, BackendError> {
    result["thread"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| BackendError::Protocol("thread/start result missing thread.id".into()))
}

/// Translate routed app-server messages into `BackendEvent`s until the turn
/// completes.
fn turn_stream(
    server: Arc<AppServer>,
    codex_thread_id: String,
    mut route: mpsc::Receiver<ServerMsg>,
    fresh_session: bool,
    model_name: String,
    observed_windows: Arc<std::sync::Mutex<HashMap<String, u64>>>,
) -> impl futures::Stream<Item = Result<BackendEvent, BackendError>> {
    async_stream(move |tx| async move {
        if fresh_session {
            let _ = tx
                .send(Ok(BackendEvent::SessionStarted {
                    session_id: codex_thread_id.clone(),
                }))
                .await;
        }
        let mut usage = Usage::default();
        while let Some(msg) = route.recv().await {
            match msg {
                ServerMsg::Notification { method, params } => match method.as_str() {
                    "item/agentMessage/delta" => {
                        if let Some(d) = params["delta"].as_str() {
                            let _ = tx.send(Ok(BackendEvent::TextDelta(d.into()))).await;
                        }
                    }
                    // Reasoning summaries (all OpenAI models) and raw
                    // reasoning (open-source models).
                    "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                        if let Some(d) = params["delta"].as_str() {
                            let _ = tx.send(Ok(BackendEvent::ThinkingDelta(d.into()))).await;
                        }
                    }
                    // Boundary between summary sections.
                    "item/reasoning/summaryPartAdded" => {
                        let _ = tx
                            .send(Ok(BackendEvent::ThinkingDelta("\n\n".into())))
                            .await;
                    }
                    "item/started" => {
                        let item = &params["item"];
                        let ty = item["type"].as_str().unwrap_or("");
                        if !matches!(
                            ty,
                            "" | "agentMessage" | "userMessage" | "plan" | "reasoning"
                        ) {
                            let _ = tx
                                .send(Ok(BackendEvent::ToolStarted {
                                    call_id: item["id"].as_str().unwrap_or("").into(),
                                    tool: ty.into(),
                                    args: item.clone(),
                                }))
                                .await;
                        }
                    }
                    "item/commandExecution/outputDelta" => {
                        if let (Some(id), Some(d)) =
                            (params["itemId"].as_str(), params["delta"].as_str())
                        {
                            let _ = tx
                                .send(Ok(BackendEvent::ToolOutput {
                                    call_id: id.into(),
                                    chunk: d.into(),
                                }))
                                .await;
                        }
                    }
                    "item/completed" => {
                        let item = &params["item"];
                        let ty = item["type"].as_str().unwrap_or("");
                        if !matches!(
                            ty,
                            "" | "agentMessage" | "userMessage" | "plan" | "reasoning"
                        ) {
                            let failed = item["status"].as_str() == Some("failed");
                            let _ = tx
                                .send(Ok(BackendEvent::ToolCompleted {
                                    call_id: item["id"].as_str().unwrap_or("").into(),
                                    ok: !failed,
                                    result: item.clone(),
                                }))
                                .await;
                        }
                    }
                    "thread/tokenUsage/updated" => {
                        // One update per model call. The input span of the
                        // newest call is the whole conversation context, so
                        // it replaces; output is per-call, so it accumulates
                        // across the calls of a multi-step turn.
                        let u = parse_usage(&params);
                        usage.input_tokens = u.input_tokens;
                        usage.cached_input_tokens = u.cached_input_tokens;
                        usage.output_tokens += u.output_tokens;
                        if let Some(n) = u.context_window {
                            usage.context_window = Some(n);
                            observed_windows.lock().unwrap().insert(model_name.clone(), n);
                        }
                    }
                    "turn/completed" => {
                        let status = params["turn"]["status"].as_str().unwrap_or("completed");
                        if status == "failed" {
                            let msg = params["turn"]["error"]["message"]
                                .as_str()
                                .unwrap_or("turn failed")
                                .to_string();
                            let _ = tx.send(Err(BackendError::Protocol(msg))).await;
                        } else {
                            let _ = tx
                                .send(Ok(BackendEvent::Completed {
                                    usage: usage.clone(),
                                }))
                                .await;
                        }
                        break;
                    }
                    _ => {}
                },
                ServerMsg::Request { id, method, params } => {
                    let tool = match method.as_str() {
                        "item/commandExecution/requestApproval" => "commandExecution",
                        "item/fileChange/requestApproval" => "fileChange",
                        _ => {
                            // Unknown server request: deny rather than hang.
                            server.respond(id, json!({ "decision": "denied" })).await;
                            continue;
                        }
                    };
                    let (ok_tx, ok_rx) = oneshot::channel();
                    let call_id = params["itemId"].as_str().unwrap_or("").to_string();
                    let _ = tx
                        .send(Ok(BackendEvent::ApprovalNeeded {
                            call_id,
                            tool: tool.into(),
                            args: params.clone(),
                            responder: ok_tx,
                        }))
                        .await;
                    let approved = ok_rx.await.unwrap_or(false);
                    // ReviewDecision: "denied" (vs "abort") lets the agent
                    // continue and explain instead of killing the turn.
                    let decision = if approved { "approved" } else { "denied" };
                    server.respond(id, json!({ "decision": decision })).await;
                }
            }
        }
        server.unsubscribe(&codex_thread_id).await;
    })
}

/// Best-effort parse of `thread/tokenUsage/updated` payloads (field naming
/// has shifted across app-server versions).
fn parse_usage(params: &Value) -> Usage {
    let u = params
        .get("tokenUsage")
        .or_else(|| params.get("usage"))
        .unwrap_or(params);
    // The model's real context window rides along at the tokenUsage level;
    // `model/list` never reports it, so this is the only source of truth.
    let context_window = u
        .get("modelContextWindow")
        .or_else(|| u.get("model_context_window"))
        .and_then(Value::as_u64)
        .filter(|n| *n > 0);
    // Current app-servers nest per-call usage under "last" (a thread-wide
    // "total" sits alongside); older builds put the fields at the top level.
    let u = u.get("last").unwrap_or(u);
    let get = |keys: &[&str]| -> u64 {
        for k in keys {
            if let Some(n) = u.get(*k).and_then(Value::as_u64) {
                return n;
            }
        }
        0
    };
    Usage {
        input_tokens: get(&["inputTokens", "input_tokens", "promptTokens"]),
        output_tokens: get(&["outputTokens", "output_tokens", "completionTokens"]),
        cached_input_tokens: get(&[
            "cachedInputTokens",
            "cached_input_tokens",
            "cacheReadTokens",
        ]),
        cost_usd: None,
        context_window,
    }
}

// --- JSON-RPC plumbing -----------------------------------------------------

enum ServerMsg {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

struct AppServer {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Pending,
    routes: Arc<Mutex<HashMap<String, mpsc::Sender<ServerMsg>>>>,
    /// Held so the child (kill_on_drop) lives as long as the server handle.
    _child: Child,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl AppServer {
    async fn spawn(command: &str) -> Result<Self, BackendError> {
        let mut child = tokio::process::Command::new(command)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BackendError::NotInstalled(command.to_string()),
                _ => BackendError::Io(e),
            })?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let server = Self {
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            routes: Arc::new(Mutex::new(HashMap::new())),
            _child: child,
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        server.start_reader(stdout);
        Ok(server)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    fn start_reader(&self, stdout: tokio::process::ChildStdout) {
        let routes = self.routes.clone();
        let closed = self.closed.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let has_id = !msg["id"].is_null();
                let has_method = msg["method"].is_string();
                if has_id && !has_method {
                    // Response to one of our requests.
                    if let Some(id) = msg["id"].as_i64() {
                        if let Some(tx) = pending.lock().await.remove(&id) {
                            let result = if msg.get("error").map(|e| !e.is_null()).unwrap_or(false)
                            {
                                Err(msg["error"]["message"]
                                    .as_str()
                                    .unwrap_or("unknown error")
                                    .to_string())
                            } else {
                                Ok(msg["result"].clone())
                            };
                            let _ = tx.send(result);
                        }
                    }
                } else if has_method {
                    let method = msg["method"].as_str().unwrap_or("").to_string();
                    let params = msg["params"].clone();
                    let thread_id = params["threadId"]
                        .as_str()
                        .or_else(|| params["thread"]["id"].as_str())
                        .unwrap_or("")
                        .to_string();
                    let routed = {
                        let routes = routes.lock().await;
                        routes.get(&thread_id).cloned()
                    };
                    if let Some(tx) = routed {
                        let m = if has_id {
                            ServerMsg::Request {
                                id: msg["id"].clone(),
                                method,
                                params,
                            }
                        } else {
                            ServerMsg::Notification { method, params }
                        };
                        let _ = tx.send(m).await;
                    }
                }
            }
            closed.store(true, Ordering::Relaxed);
        });
    }

    async fn handshake(&self) -> Result<(), BackendError> {
        self.request(
            "initialize",
            json!({
                "clientInfo": { "name": "trouve", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
        self.notify("initialized", json!({})).await;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, BackendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(BackendError::Protocol(format!("{method}: {e}"))),
            Err(_) => Err(BackendError::Protocol(format!(
                "{method}: app-server closed before responding"
            ))),
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .write(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await;
    }

    async fn respond(&self, id: Value, result: Value) {
        let _ = self
            .write(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await;
    }

    async fn write(&self, msg: Value) -> Result<(), BackendError> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_vec(&msg).expect("serializable");
        line.push(b'\n');
        stdin.write_all(&line).await.map_err(BackendError::Io)?;
        stdin.flush().await.map_err(BackendError::Io)
    }

    async fn subscribe(&self, thread_id: &str) -> mpsc::Receiver<ServerMsg> {
        let (tx, rx) = mpsc::channel(256);
        self.routes.lock().await.insert(thread_id.to_string(), tx);
        rx
    }

    async fn unsubscribe(&self, thread_id: &str) {
        self.routes.lock().await.remove(thread_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_token_usage() {
        // Current app-server shape: per-call usage under tokenUsage.last.
        let params = json!({
            "threadId": "t1",
            "turnId": "u1",
            "tokenUsage": {
                "last": {
                    "inputTokens": 1200,
                    "cachedInputTokens": 1000,
                    "outputTokens": 50,
                    "reasoningOutputTokens": 10,
                    "totalTokens": 1250,
                },
                "total": {
                    "inputTokens": 9999,
                    "cachedInputTokens": 9000,
                    "outputTokens": 500,
                    "reasoningOutputTokens": 100,
                    "totalTokens": 10499,
                },
                "modelContextWindow": 272000,
            },
        });
        let u = parse_usage(&params);
        assert_eq!(u.input_tokens, 1200);
        assert_eq!(u.cached_input_tokens, 1000);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.context_window, Some(272000));

        // Older flat shape still parses; no window reported means None.
        let flat = json!({ "usage": { "inputTokens": 7, "outputTokens": 3 } });
        let u = parse_usage(&flat);
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.output_tokens, 3);
        assert_eq!(u.context_window, None);
    }

    #[test]
    fn observed_windows_override_catalog_sizes() {
        let backend = CodexBackend::new("codex", None);
        let before = backend.models();
        assert!(before.iter().all(|m| m.context_window == 272_000));

        backend
            .observed_windows
            .lock()
            .unwrap()
            .insert("gpt-5.4-codex".into(), 400_000);
        let after = backend.models();
        let by_id = |id: &str| {
            after
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.context_window)
                .unwrap()
        };
        assert_eq!(by_id("codex/gpt-5.4-codex"), 400_000);
        assert_eq!(by_id("codex/gpt-5.4"), 272_000);
    }

    #[test]
    fn splits_effort_suffix() {
        assert_eq!(split_effort("gpt-5.5@high"), ("gpt-5.5", Some("high")));
        assert_eq!(split_effort("gpt-5.5"), ("gpt-5.5", None));
        assert_eq!(split_effort(""), ("", None));
        assert_eq!(split_effort("gpt@"), ("gpt@", None));
    }

    #[test]
    fn maps_model_list_efforts_into_options_schema() {
        let result = json!({ "data": [
            {
                "id": "gpt-5.5",
                "displayName": "GPT-5.5",
                "hidden": false,
                "defaultReasoningEffort": "medium",
                "supportedReasoningEfforts": [
                    { "reasoningEffort": "low" },
                    { "reasoningEffort": "medium" },
                    { "reasoningEffort": "high" },
                ],
            },
            { "id": "secret", "displayName": "Hidden", "hidden": true },
        ]});
        let models = parse_model_list("codex", &result);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["codex/gpt-5.5"]);
        assert_eq!(
            models[0]
                .options_schema
                .pointer("/properties/reasoning_effort/enum")
                .unwrap(),
            &json!(["low", "medium", "high"])
        );
        assert_eq!(
            models[0]
                .options_schema
                .pointer("/properties/reasoning_effort/default")
                .and_then(Value::as_str),
            Some("medium")
        );
    }
}
