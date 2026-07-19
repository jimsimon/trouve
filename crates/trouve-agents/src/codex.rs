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
//!   answered with `{ decision: "accept" | "decline" }`

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc, oneshot};
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventStream, BackendLogin, BackendPermission,
    BackendStatus, BackendTurn, async_stream, binary_on_path, format_reset, model, spawn_login,
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
        if let Some(s) = guard.as_ref()
            && !s.is_closed()
        {
            return Ok(s.clone());
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
            if let Some((at, models)) = cache.as_ref()
                && at.elapsed() < MODELS_TTL
            {
                let mut models = models.clone();
                self.apply_observed_windows(&mut models);
                return models;
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

    async fn subscription_health(&self) -> Option<trouve_protocol::SubscriptionHealth> {
        let result = async {
            let server = self.server().await?;
            server.request("account/rateLimits/read", Value::Null).await
        }
        .await;
        Some(match result {
            Ok(value) => parse_rate_limits(&self.id, &value),
            Err(e) => trouve_protocol::SubscriptionHealth {
                provider_id: self.id.clone(),
                status: "unavailable".into(),
                plan: String::new(),
                windows: Vec::new(),
                credits: String::new(),
                note: format!("could not read usage from the Codex app-server: {e}"),
            },
        })
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

        // Per-thread config overrides: the trouve MCP bridge rides along so
        // codex gets trouve's semantic search / question tools, plus any
        // user-configured MCP servers (both thread/start and thread/resume
        // accept `config`, and resumed threads re-spawn their MCP servers
        // from it).
        let config_override = mcp_config_override(&turn);
        let with_config = |mut params: Value| {
            if let Some(config) = &config_override {
                params["config"] = config.clone();
            }
            params
        };

        // Start or resume the vendor-side thread.
        let start_params = with_config(json!({
            "model": model_or_default(model_name),
            "cwd": turn.worktree,
            "approvalPolicy": approval_policy,
            "sandbox": sandbox,
            "serviceName": "trouve",
        }));
        let mut fresh_session = false;
        let codex_thread_id = match &turn.session {
            Some(sid) => {
                let resumed = server
                    .request("thread/resume", with_config(json!({ "threadId": sid })))
                    .await;
                match resumed {
                    Ok(v) => thread_id_of(&v)?,
                    Err(e) => {
                        tracing::warn!("codex thread/resume failed ({e}); starting fresh");
                        fresh_session = true;
                        let v = server.request("thread/start", start_params.clone()).await?;
                        thread_id_of(&v)?
                    }
                }
            }
            None => {
                fresh_session = true;
                let v = server.request("thread/start", start_params.clone()).await?;
                thread_id_of(&v)?
            }
        };

        let route = server.subscribe(&codex_thread_id).await;

        // Mode instructions (which include the search-tool guidance when
        // the bridge is mounted) ride along in the first user message of a
        // fresh vendor session (app-server owns the system prompt).
        let text = match (&turn.instructions, fresh_session) {
            (Some(instr), true) => format!(
                "<mode-instructions>\n{instr}\n</mode-instructions>\n\n{}",
                turn.prompt
            ),
            _ => turn.prompt.clone(),
        };

        // Images ride as localImage items (app-server reads the file
        // itself); the engine already turned non-image uploads into path
        // references inside the prompt text.
        let mut input = vec![json!({ "type": "text", "text": text })];
        for att in &turn.attachments {
            input.push(json!({ "type": "localImage", "path": att.path }));
        }
        let mut turn_params = json!({
            "threadId": codex_thread_id,
            "model": model_or_default(model_name),
            "approvalPolicy": approval_policy,
            // Codex defaults networkAccess to false for both read-only and
            // workspace-write sandboxes. Turns need outbound access for
            // fetches, package managers, remote git operations, and MCP
            // servers; filesystem mutation remains governed independently
            // by the sandbox type and approval policy above.
            "sandboxPolicy": {
                "type": sandbox_policy_type,
                "networkAccess": true,
            },
            "input": input,
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

/// Codex config overrides mounting the trouve MCP bridge and the user's
/// configured MCP servers as per-thread MCP servers (same shape as
/// `mcp_servers` in codex's config.toml). `None` when there is nothing to
/// mount.
fn mcp_config_override(turn: &crate::BackendTurn) -> Option<Value> {
    let env_map = |env: &[(String, String)]| -> serde_json::Map<String, Value> {
        env.iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect()
    };
    let mut servers = serde_json::Map::new();
    for server in &turn.mcp_servers {
        servers.insert(
            server.name.clone(),
            json!({
                "command": server.command,
                "args": server.args,
                "env": env_map(&server.env),
            }),
        );
    }
    if let Some(bridge) = &turn.mcp_bridge {
        // Streamable-HTTP server (`url` instead of `command` selects the
        // transport in codex's mcp_servers config shape).
        servers.insert("trouve".into(), json!({ "url": bridge.url }));
    }
    if servers.is_empty() {
        return None;
    }
    Some(json!({ "mcp_servers": servers }))
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
        // Some Codex/app-server combinations only populate the completed
        // reasoning item instead of emitting its optional delta
        // notifications. Remember which items did stream so `item/completed`
        // can be a lossless fallback without displaying the same thought
        // twice.
        let mut streamed_reasoning = HashSet::new();
        let mut turn_finished = false;
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
                            if let Some(id) = params["itemId"].as_str() {
                                streamed_reasoning.insert(id.to_string());
                            }
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
                        if ty == "reasoning"
                            && item["id"]
                                .as_str()
                                .is_none_or(|id| !streamed_reasoning.contains(id))
                            && let Some(text) = completed_reasoning_text(item)
                        {
                            let _ = tx.send(Ok(BackendEvent::ThinkingDelta(text))).await;
                        }
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
                            observed_windows
                                .lock()
                                .unwrap()
                                .insert(model_name.clone(), n);
                        }
                    }
                    "turn/completed" => {
                        turn_finished = true;
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
                    // MCP tool-call permission elicitation (codex's rmcp
                    // client asks before every MCP tool call). The trouve
                    // bridge's tools are gated inside trouve's own
                    // permission layer, so auto-accept those; other MCP
                    // servers go through the normal approval flow.
                    if method == "mcpServer/elicitation/request" {
                        if params["serverName"] == "trouve" {
                            server
                                .respond(id, json!({ "action": "accept", "content": {} }))
                                .await;
                            continue;
                        }
                        let (ok_tx, ok_rx) = oneshot::channel();
                        // JSON-RPC request ids are unique for this app-server
                        // process. Preserve that identity in trouve so
                        // concurrent MCP approvals cannot overwrite the same
                        // empty ApprovalHub key.
                        let call_id = format!("codex-mcp-{}", json_rpc_id(&id));
                        let _ = tx
                            .send(Ok(BackendEvent::ApprovalNeeded {
                                call_id,
                                tool: "mcpToolCall".into(),
                                args: params.clone(),
                                responder: ok_tx,
                            }))
                            .await;
                        let action = if ok_rx.await.unwrap_or(false) {
                            "accept"
                        } else {
                            "decline"
                        };
                        server
                            .respond(id, json!({ "action": action, "content": {} }))
                            .await;
                        continue;
                    }
                    let tool = match method.as_str() {
                        "item/commandExecution/requestApproval" => "commandExecution",
                        "item/fileChange/requestApproval" => "fileChange",
                        _ => {
                            // Unknown server request: deny rather than hang.
                            tracing::warn!(
                                "codex: denying unknown server request {method}: {}",
                                serde_json::to_string(&params).unwrap_or_default()
                            );
                            server.respond(id, json!({ "decision": "decline" })).await;
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
                    // ReviewDecision: "decline" (vs "abort") lets the agent
                    // continue and explain instead of killing the turn.
                    let decision = if approved { "accept" } else { "decline" };
                    server.respond(id, json!({ "decision": decision })).await;
                }
            }
        }
        if !turn_finished {
            let reason = if server.is_closed() {
                "app-server closed before turn completed"
            } else {
                "app-server event route closed before turn completed"
            };
            let _ = tx.send(Err(BackendError::Protocol(reason.into()))).await;
        }
        server.unsubscribe(&codex_thread_id).await;
    })
}

/// Extract the displayable text from a completed Codex reasoning item.
/// Summary text is preferred; raw content is used by open-source models.
fn completed_reasoning_text(item: &Value) -> Option<String> {
    for field in ["summary", "content"] {
        let parts = item[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return Some(parts.join("\n\n"));
        }
    }
    None
}

fn json_rpc_id(id: &Value) -> String {
    match id {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

/// Turn an `account/rateLimits/read` response into subscription health.
fn parse_rate_limits(provider_id: &str, value: &Value) -> trouve_protocol::SubscriptionHealth {
    let snapshot = value.get("rateLimits").unwrap_or(&Value::Null);
    let plan = snapshot
        .get("planType")
        .and_then(|p| p.as_str())
        .filter(|p| *p != "unknown")
        .unwrap_or("")
        .to_string();

    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(window) = snapshot.get(key).filter(|w| !w.is_null()) else {
            continue;
        };
        let Some(used) = window.get("usedPercent").and_then(|u| u.as_i64()) else {
            continue;
        };
        windows.push(trouve_protocol::SubscriptionWindow {
            label: window_label(window.get("windowDurationMins").and_then(|m| m.as_i64())),
            used_percent: used.clamp(0, 100),
            resets: window
                .get("resetsAt")
                .and_then(|r| r.as_i64())
                .map(format_reset)
                .unwrap_or_default(),
        });
    }

    let credits = snapshot
        .get("credits")
        .filter(|c| !c.is_null())
        .map(|c| {
            if c.get("unlimited")
                .and_then(|u| u.as_bool())
                .unwrap_or(false)
            {
                "unlimited credits".to_string()
            } else if c
                .get("hasCredits")
                .and_then(|h| h.as_bool())
                .unwrap_or(false)
            {
                match c.get("balance").and_then(|b| b.as_str()) {
                    Some(balance) => format!("credits: {balance}"),
                    None => String::new(),
                }
            } else {
                String::new()
            }
        })
        .unwrap_or_default();

    if windows.is_empty() && plan.is_empty() {
        return trouve_protocol::SubscriptionHealth {
            provider_id: provider_id.to_string(),
            status: "unavailable".into(),
            plan,
            windows,
            credits,
            note: "the app-server reported no usage data — is codex logged in?".into(),
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

/// "5h window" / "Weekly" / "3d window" from a window duration.
fn window_label(mins: Option<i64>) -> String {
    match mins {
        Some(10080) => "Weekly".to_string(),
        Some(m) if m > 0 && m % 1440 == 0 => format!("{}d window", m / 1440),
        Some(m) if m > 0 && m % 60 == 0 => format!("{}h window", m / 60),
        Some(m) if m > 0 => format!("{m}m window"),
        _ => "Usage window".to_string(),
    }
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
type Routes = Arc<Mutex<HashMap<String, mpsc::Sender<ServerMsg>>>>;
type Buffered = Arc<Mutex<HashMap<String, Vec<ServerMsg>>>>;
const ROUTE_CAPACITY: usize = 256;

async fn close_transport(
    pending: &Pending,
    routes: &Routes,
    buffered: &Buffered,
    closed: &std::sync::atomic::AtomicBool,
) {
    // Publish closure before taking async locks so no caller can reuse this
    // transport while its abandoned waiters are being drained.
    closed.store(true, Ordering::Relaxed);
    pending.lock().await.clear();
    routes.lock().await.clear();
    buffered.lock().await.clear();
}

async fn read_stdout<R: AsyncRead + Unpin>(
    stdout: R,
    pending: Pending,
    routes: Routes,
    buffered: Buffered,
    closed: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut failed_routes = HashSet::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let has_id = !msg["id"].is_null();
        let has_method = msg["method"].is_string();
        if has_id && !has_method {
            // Response to one of our requests.
            if let Some(id) = msg["id"].as_i64()
                && let Some(tx) = pending.lock().await.remove(&id)
            {
                let result = if msg.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                    Err(msg["error"]["message"]
                        .as_str()
                        .unwrap_or("unknown error")
                        .to_string())
                } else {
                    Ok(msg["result"].clone())
                };
                let _ = tx.send(result);
            }
        } else if has_method {
            let method = msg["method"].as_str().unwrap_or("").to_string();
            let params = msg["params"].clone();
            let thread_id = params["threadId"]
                .as_str()
                .or_else(|| params["thread"]["id"].as_str())
                .unwrap_or("")
                .to_string();
            let m = if has_id {
                ServerMsg::Request {
                    id: msg["id"].clone(),
                    method,
                    params,
                }
            } else {
                ServerMsg::Notification { method, params }
            };
            let routed = {
                let routes = routes.lock().await;
                routes.get(&thread_id).cloned()
            };
            match routed {
                Some(tx) => match tx.try_send(m) {
                    Ok(()) => {
                        failed_routes.remove(&thread_id);
                    }
                    Err(error) => {
                        // The stdout reader is shared by every Codex turn. A
                        // stalled route must fail independently rather than
                        // applying backpressure that wedges all turns and
                        // prevents this task from ever observing EOF.
                        tracing::warn!(
                            "codex: dropping {thread_id} event route: {}",
                            match error {
                                mpsc::error::TrySendError::Full(_) => "route buffer is full",
                                mpsc::error::TrySendError::Closed(_) => "route receiver is closed",
                            }
                        );
                        let mut routes = routes.lock().await;
                        if routes
                            .get(&thread_id)
                            .is_some_and(|active| active.same_channel(&tx))
                        {
                            routes.remove(&thread_id);
                        }
                        // Do not reinterpret later events from this failed
                        // turn as pre-subscription events. A future route for
                        // the same thread clears this marker on delivery.
                        failed_routes.insert(thread_id);
                    }
                },
                // No subscriber yet: buffer for a thread id we've seen named
                // (skip the empty catch-all) so nothing emitted between
                // thread/start and subscribe is lost.
                None if !thread_id.is_empty() && !failed_routes.contains(&thread_id) => {
                    buffered.lock().await.entry(thread_id).or_default().push(m);
                }
                None => {}
            }
        }
    }
    // Dropping stdout means the app-server can never complete any
    // outstanding request or turn. Drop every sender it left behind so
    // request waiters and routed turn streams wake immediately instead of
    // remaining active forever.
    close_transport(&pending, &routes, &buffered, &closed).await;
}

struct AppServer {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Pending,
    routes: Routes,
    /// Thread-scoped messages that arrived before anyone subscribed to that
    /// thread — the id is only known after thread/start returns, so the
    /// app-server can emit notifications in the gap before `subscribe`.
    /// Delivered when the route is registered instead of being dropped.
    buffered: Buffered,
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
            buffered: Arc::new(Mutex::new(HashMap::new())),
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
        let buffered = self.buffered.clone();
        tokio::spawn(read_stdout(stdout, pending, routes, buffered, closed));
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
        let (tx, rx) = mpsc::channel(ROUTE_CAPACITY);
        self.routes
            .lock()
            .await
            .insert(thread_id.to_string(), tx.clone());
        // Flush anything the reader buffered for this thread before we
        // subscribed (notifications emitted right after thread/start).
        if let Some(msgs) = self.buffered.lock().await.remove(thread_id) {
            for m in msgs {
                let _ = tx.send(m).await;
            }
        }
        rx
    }

    async fn unsubscribe(&self, thread_id: &str) {
        self.routes.lock().await.remove(thread_id);
        self.buffered.lock().await.remove(thread_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_turn() -> crate::BackendTurn {
        crate::BackendTurn {
            thread_id: "th_1".into(),
            worktree: "/tmp".into(),
            session: None,
            model: "gpt-5.6".into(),
            model_options: serde_json::Map::new(),
            prompt: "hi".into(),
            attachments: vec![],
            instructions: None,
            permission: crate::BackendPermission::Ask,
            mcp_bridge: None,
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn json_rpc_ids_make_stable_approval_ids() {
        assert_eq!(json_rpc_id(&json!(42)), "42");
        assert_eq!(json_rpc_id(&json!("request-7")), "request-7");
    }

    #[test]
    fn extracts_completed_codex_reasoning_as_a_stream_fallback() {
        let summarized = json!({
            "id": "reason-1",
            "type": "reasoning",
            "summary": ["Checking the adapter", "Found the missing fallback"],
            "content": ["raw text is secondary"],
        });
        assert_eq!(
            completed_reasoning_text(&summarized).as_deref(),
            Some("Checking the adapter\n\nFound the missing fallback")
        );

        let raw = json!({ "type": "reasoning", "summary": [], "content": ["thinking"] });
        assert_eq!(completed_reasoning_text(&raw).as_deref(), Some("thinking"));
        assert_eq!(
            completed_reasoning_text(&json!({ "type": "reasoning" })),
            None
        );
    }

    #[tokio::test]
    async fn reader_eof_releases_pending_requests_and_turn_routes() {
        let deadline = std::time::Duration::from_secs(1);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let buffered: Buffered = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (request_tx, request_rx) = oneshot::channel();
        pending.lock().await.insert(1, request_tx);
        let (route_tx, mut route_rx) = mpsc::channel(1);
        routes.lock().await.insert("thread-1".into(), route_tx);

        let (mut writer, reader) = tokio::io::duplex(16);
        let task = tokio::spawn(read_stdout(
            reader,
            pending.clone(),
            routes.clone(),
            buffered.clone(),
            closed.clone(),
        ));
        writer.shutdown().await.unwrap();
        tokio::time::timeout(deadline, task)
            .await
            .expect("stdout reader should stop at EOF")
            .unwrap();

        assert!(closed.load(Ordering::Relaxed));
        assert!(
            tokio::time::timeout(deadline, request_rx)
                .await
                .expect("pending request should be released")
                .is_err()
        );
        assert!(
            tokio::time::timeout(deadline, route_rx.recv())
                .await
                .expect("turn route should be released")
                .is_none()
        );
    }

    #[tokio::test]
    async fn full_turn_route_does_not_block_stdout_eof_cleanup() {
        let deadline = std::time::Duration::from_secs(1);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let buffered: Buffered = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (route_tx, mut route_rx) = mpsc::channel(1);
        route_tx
            .try_send(ServerMsg::Notification {
                method: "already/queued".into(),
                params: json!({}),
            })
            .unwrap();
        routes.lock().await.insert("thread-1".into(), route_tx);

        let (mut writer, reader) = tokio::io::duplex(256);
        let task = tokio::spawn(read_stdout(
            reader,
            pending.clone(),
            routes.clone(),
            buffered.clone(),
            closed.clone(),
        ));
        writer
            .write_all(
                br#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thread-1","delta":"blocked"}}
"#,
            )
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        tokio::time::timeout(deadline, task)
            .await
            .expect("route backpressure must not block EOF cleanup")
            .unwrap();

        assert!(closed.load(Ordering::Relaxed));
        assert!(routes.lock().await.is_empty());
        assert!(buffered.lock().await.is_empty());
        assert!(
            tokio::time::timeout(deadline, route_rx.recv())
                .await
                .expect("pre-existing route event should remain available")
                .is_some()
        );
        assert!(
            tokio::time::timeout(deadline, route_rx.recv())
                .await
                .expect("saturated route should close")
                .is_none()
        );
    }

    #[test]
    fn mcp_config_override_merges_bridge_and_user_servers() {
        let mut turn = bare_turn();
        assert!(mcp_config_override(&turn).is_none());

        turn.mcp_servers.push(crate::McpServerLaunch {
            name: "jira".into(),
            command: "jira-mcp".into(),
            args: vec!["--stdio".into()],
            env: vec![("TOKEN".into(), "sekrit".into())],
        });
        turn.mcp_bridge = Some(crate::McpBridgeConfig {
            url: "http://127.0.0.1:1/internal/threads/th_1/mcp?tools=0&approval=0".into(),
            bridge_tools: false,
            disallowed_tools: Vec::new(),
        });
        let config = mcp_config_override(&turn).unwrap();
        let servers = &config["mcp_servers"];
        assert_eq!(servers["jira"]["command"], "jira-mcp");
        assert_eq!(servers["jira"]["env"]["TOKEN"], "sekrit");
        assert_eq!(
            servers["trouve"]["url"],
            "http://127.0.0.1:1/internal/threads/th_1/mcp?tools=0&approval=0"
        );
        assert!(servers["trouve"]["command"].is_null());

        // User servers alone (no bridge) still produce an override.
        turn.mcp_bridge = None;
        let config = mcp_config_override(&turn).unwrap();
        assert!(config["mcp_servers"]["jira"].is_object());
        assert!(config["mcp_servers"]["trouve"].is_null());
    }

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
    fn parses_rate_limit_snapshots() {
        let soon = chrono::Utc::now().timestamp() + 2 * 3600 + 600;
        let value = json!({
            "rateLimits": {
                "planType": "plus",
                "primary": { "usedPercent": 62, "resetsAt": soon, "windowDurationMins": 300 },
                "secondary": { "usedPercent": 31, "resetsAt": soon + 86400, "windowDurationMins": 10080 },
                "credits": { "hasCredits": true, "unlimited": false, "balance": "12.50" },
            },
        });
        let health = parse_rate_limits("codex", &value);
        assert_eq!(health.status, "ok");
        assert_eq!(health.plan, "plus");
        assert_eq!(health.credits, "credits: 12.50");
        assert_eq!(health.windows.len(), 2);
        assert_eq!(health.windows[0].label, "5h window");
        assert_eq!(health.windows[0].used_percent, 62);
        assert!(health.windows[0].resets.starts_with("resets in 2h"));
        assert_eq!(health.windows[1].label, "Weekly");
        assert_eq!(health.windows[1].used_percent, 31);
        assert!(health.windows[1].resets.starts_with("resets in 1d"));

        // Empty payload → unavailable (typically not logged in).
        let health = parse_rate_limits("codex", &json!({ "rateLimits": {} }));
        assert_eq!(health.status, "unavailable");
        assert!(health.note.contains("logged in"));
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
