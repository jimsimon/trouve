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
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, OnceCell, oneshot};
use trouve_protocol::{ModelInfo, Usage};
use trouve_providers::codex::completed_raw_reasoning_text;

use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventStream, BackendLogin, BackendPermission,
    BackendStatus, BackendTurn, async_stream, binary_on_path, format_reset, model,
    route::{ROUTE_EVENT_BUDGET, RouteReceiver, RouteSendError, RouteSender, route_channel},
    spawn_codex_login,
};

pub struct CodexBackend {
    id: String,
    command: String,
    server: Arc<Mutex<Option<Arc<AppServer>>>>,
    /// `model/list` result, cached for [`MODELS_TTL`].
    models_cache: Mutex<Option<(std::time::Instant, Vec<ModelInfo>)>>,
    /// Real context windows by model name, learned from
    /// `thread/tokenUsage/updated` (`model/list` doesn't report them).
    observed_windows: Arc<std::sync::Mutex<HashMap<String, u64>>>,
}

/// How long a fetched vendor model list stays fresh.
const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Product-level capabilities Trouve suppresses when the running Codex
/// app-server advertises them. The dynamic feature catalog is the schema
/// authority: older CLIs never receive config keys they do not understand.
const PRODUCT_SURFACE_FEATURES: &[&str] = &[
    "apps",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "current_time_reminder",
    "goals",
    "hooks",
    "image_generation",
    "memories",
    "multi_agent",
    "plugins",
    "remote_plugin",
    "skill_mcp_dependency_install",
    "tool_suggest",
    "workspace_dependencies",
];

/// Codex's two sandbox spellings: `thread/start` uses the kebab-case mode,
/// while `turn/start` uses the camel-case policy discriminator.
///
/// Mutable local turns deliberately rely on trouve's permission posture
/// instead of Codex's OS sandbox (ADR 0004): Ask keeps command approvals and
/// Yolo is explicitly unrestricted. This is also required for linked
/// worktrees because Codex's workspace-write mode protects both `.git` and
/// the resolved external gitdir, so Git cannot even create its per-worktree
/// `index.lock`. Read-only modes retain that sandbox.
fn permission_settings(
    permission: BackendPermission,
) -> (&'static str, &'static str, &'static str) {
    match permission {
        BackendPermission::ReadOnly => ("never", "read-only", "readOnly"),
        BackendPermission::Ask => ("untrusted", "danger-full-access", "dangerFullAccess"),
        BackendPermission::Yolo => ("never", "danger-full-access", "dangerFullAccess"),
    }
}

impl CodexBackend {
    pub fn new(id: impl Into<String>, command: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "codex".into()),
            server: Arc::new(Mutex::new(None)),
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
        // The app-server is the catalog authority. In particular it does
        // not expose context windows in `model/list`, so there is no honest
        // offline model record to synthesize here.
        Vec::new()
    }

    async fn list_models(&self) -> Vec<ModelInfo> {
        let stale = {
            let cache = self.models_cache.lock().await;
            if let Some((at, models)) = cache.as_ref()
                && at.elapsed() < MODELS_TTL
            {
                let mut models = models.clone();
                self.apply_observed_windows(&mut models);
                return models;
            }
            cache.as_ref().map(|(_, models)| models.clone())
        };
        let fetched = async {
            let server = self.server().await?;
            server.request("model/list", json!({})).await
        }
        .await;
        match fetched {
            Ok(result) => {
                let mut models = parse_model_list(&self.id, &result);
                if models.is_empty() {
                    let mut models = stale.unwrap_or_default();
                    self.apply_observed_windows(&mut models);
                    return models;
                }
                *self.models_cache.lock().await = Some((std::time::Instant::now(), models.clone()));
                self.apply_observed_windows(&mut models);
                models
            }
            Err(e) => {
                tracing::debug!("codex model/list failed: {e}; using stale live list");
                let mut models = stale.unwrap_or_default();
                self.apply_observed_windows(&mut models);
                models
            }
        }
    }

    fn status(&self) -> BackendStatus {
        let auth = codex_auth_path().is_some_and(|path| path.exists());
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
        let BackendLogin {
            verification_url,
            user_code,
            callback_sender,
            done,
        } = spawn_codex_login(&self.command).await?;
        let server = self.server.clone();
        Ok(BackendLogin {
            verification_url,
            user_code,
            callback_sender,
            done: Box::pin(async move {
                let result = done.await;
                if result.is_ok() {
                    // AppServer snapshots auth into an isolated CODEX_HOME.
                    // Force the next request to create a fresh snapshot after
                    // login rather than retaining a pre-login process.
                    *server.lock().await = None;
                }
                result
            }),
        })
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        if turn.mcp_bridge.is_some() && !turn.mcp_servers.is_empty() {
            return Err(BackendError::Protocol(
                "optimized Codex turns mount user MCP only through Trouve's capability bridge"
                    .into(),
            ));
        }
        let server = self.server().await?;

        // Effort comes from the thread's model options; `@effort` model ids
        // from before the options split still resolve.
        let (model_name, id_effort) = split_effort(&turn.model);
        let effort = turn
            .model_options
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or(id_effort);
        let (approval_policy, sandbox, sandbox_policy_type) = permission_settings(turn.permission);
        let sandbox_policy = if matches!(turn.permission, BackendPermission::ReadOnly) {
            // Sandboxed read-only turns still need outbound access for
            // fetches, remote inspection, and MCP servers.
            json!({ "type": sandbox_policy_type, "networkAccess": true })
        } else {
            // dangerFullAccess has no networkAccess field: with no OS
            // sandbox, network access is already unrestricted.
            json!({ "type": sandbox_policy_type })
        };

        // Per-thread config overrides: request raw reasoning from models that
        // expose it and mount trouve/user MCP servers. Both thread/start and
        // thread/resume accept `config`, and resumed threads re-spawn their
        // MCP servers from it.
        let config_override = codex_config_override(&turn);
        let with_config = |mut params: Value| {
            params["config"] = config_override.clone();
            params
        };

        // Start or resume the vendor-side thread.
        let mut start_params = with_config(json!({
            "cwd": turn.worktree,
            "approvalPolicy": approval_policy,
            "sandbox": sandbox,
            "serviceName": "trouve",
            "developerInstructions": turn.instructions,
        }));
        if !model_name.is_empty() {
            start_params["model"] = json!(model_name);
        }
        let mut fresh_session = false;
        let codex_thread_id = match &turn.session {
            Some(sid) => {
                let resumed = server
                    .request(
                        "thread/resume",
                        with_config(json!({
                            "threadId": sid,
                            "developerInstructions": turn.instructions,
                        })),
                    )
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

        // A cancelled trouve stream may still have a live vendor turn if the
        // app-server was blocked in a model or tool request when its consumer
        // disappeared. Await its interruption before starting a replacement;
        // otherwise Codex folds the new prompt into the old turn and its late
        // completion is misattributed to the replacement.
        let _lifecycle = server.lock_turn_lifecycle(&codex_thread_id).await;
        server.interrupt_active_turn(&codex_thread_id).await?;
        let route = server.subscribe(&codex_thread_id).await;

        let text = turn.prompt.clone();

        // Images ride as localImage items (app-server reads the file
        // itself); the engine already turned non-image uploads into path
        // references inside the prompt text.
        let mut input = vec![json!({ "type": "text", "text": text })];
        for att in &turn.attachments {
            input.push(json!({ "type": "localImage", "path": att.path }));
        }
        let mut turn_params = json!({
            "threadId": codex_thread_id,
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
            "input": input,
        });
        if !model_name.is_empty() {
            turn_params["model"] = json!(model_name);
        }
        apply_reasoning_options(&mut turn_params, effort);
        let started_turn = match server.request("turn/start", turn_params).await {
            Ok(started) => started,
            Err(error) => {
                server.unsubscribe(&codex_thread_id, &route.tx).await;
                return Err(error);
            }
        };
        let codex_turn_id = match turn_id_of(&started_turn) {
            Ok(turn_id) => turn_id,
            Err(error) => {
                server.unsubscribe(&codex_thread_id, &route.tx).await;
                return Err(error);
            }
        };
        server
            .register_active_turn(&codex_thread_id, &codex_turn_id)
            .await;

        let stream = turn_stream(
            server.clone(),
            codex_thread_id.clone(),
            codex_turn_id,
            route,
            fresh_session,
            model_name.to_string(),
            self.observed_windows.clone(),
        );
        Ok(stream.boxed())
    }
}

/// Codex config overrides enabling raw reasoning when available and mounting
/// the trouve MCP bridge plus the user's configured MCP servers as per-thread
/// MCP servers (same shape as `mcp_servers` in codex's config.toml).
fn codex_config_override(turn: &crate::BackendTurn) -> Value {
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
        servers.insert(
            "trouve".into(),
            json!({
                "url": bridge.url,
                // ToolExecutor already classifies and gates every call. Do
                // not wrap it in Codex's separate MCP approval heuristic.
                "default_tools_approval_mode": "approve",
            }),
        );
    }
    let mut config = json!({ "show_raw_agent_reasoning": true });
    if !servers.is_empty() {
        config["mcp_servers"] = Value::Object(servers);
    }
    config
}

fn parse_supported_features(result: &Value) -> HashSet<String> {
    result
        .get("data")
        .or_else(|| result.get("features"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|feature| {
            feature
                .get("name")
                .or_else(|| feature.get("key"))
                .or_else(|| feature.get("feature"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
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

/// Commentary messages, rather than reasoning summaries, drive trouve's
/// thinking blocks. Disable summaries so their heading-like text is not
/// generated alongside the richer commentary stream.
fn apply_reasoning_options(params: &mut Value, effort: Option<&str>) {
    params["summary"] = json!("none");
    if let Some(effort) = effort {
        params["effort"] = json!(effort);
    }
}

fn agent_message_delta(
    params: &Value,
    commentary_messages: &HashSet<String>,
) -> Option<BackendEvent> {
    let delta = params["delta"].as_str()?;
    if params["itemId"]
        .as_str()
        .is_some_and(|id| commentary_messages.contains(id))
    {
        Some(BackendEvent::ThinkingDelta(delta.into()))
    } else {
        Some(BackendEvent::TextDelta(delta.into()))
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
        // `model/list` deliberately has no context-window field. A live
        // `thread/tokenUsage/updated` notification fills this in after the
        // first turn; zero means unknown until then.
        let mut info = model(backend_id, id, display, 0);
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

fn turn_id_of(result: &Value) -> Result<String, BackendError> {
    result["turn"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| BackendError::Protocol("turn/start result missing turn.id".into()))
}

/// Translate routed app-server messages into `BackendEvent`s until the turn
/// completes.
fn turn_stream(
    server: Arc<AppServer>,
    codex_thread_id: String,
    codex_turn_id: String,
    route: RouteSubscription,
    fresh_session: bool,
    model_name: String,
    observed_windows: Arc<std::sync::Mutex<HashMap<String, u64>>>,
) -> impl futures::Stream<Item = Result<BackendEvent, BackendError>> {
    async_stream(move |tx| async move {
        let RouteSubscription {
            tx: route_tx,
            mut rx,
        } = route;
        if fresh_session {
            let _ = tx
                .send(Ok(BackendEvent::SessionStarted {
                    session_id: codex_thread_id.clone(),
                }))
                .await;
        }
        let mut usage = Usage::default();
        // Message phase is carried by item/started, not by the corresponding
        // delta. Remember commentary item ids so interim model-authored
        // progress is displayed as thinking rather than appended to the final
        // answer. Missing phases retain the legacy final-answer behavior.
        let mut commentary_messages = HashSet::new();
        // Some providers only populate raw reasoning on the completed item.
        // Track streamed raw items so the completion fallback does not repeat
        // content already shown.
        let mut streamed_raw_reasoning = HashSet::new();
        let mut turn_finished = false;
        let mut client_gone = false;
        let mut route_overloaded = false;
        let mut overload_signal = rx.overload_signal();
        let process_route = async {
            while let Some(msg) = rx.recv().await {
                if message_turn_id(&msg).is_some_and(|turn_id| turn_id != codex_turn_id) {
                    tracing::warn!("codex: ignoring event for stale turn on {codex_thread_id}");
                    continue;
                }
                match msg {
                    ServerMsg::Notification { method, params } => match method.as_str() {
                        "item/agentMessage/delta" => {
                            if let Some(event) = agent_message_delta(&params, &commentary_messages)
                            {
                                let _ = tx.send(Ok(event)).await;
                            }
                        }
                        // Raw reasoning is only exposed by some models (notably
                        // open-source models). Summary deltas are deliberately not
                        // used as thinking; they are section headings, while
                        // agent-message commentary is the readable progress stream.
                        "item/reasoning/textDelta" => {
                            if let Some(d) = params["delta"].as_str() {
                                if let Some(id) = params["itemId"].as_str() {
                                    streamed_raw_reasoning.insert(id.to_string());
                                }
                                let _ = tx.send(Ok(BackendEvent::ThinkingDelta(d.into()))).await;
                            }
                        }
                        "item/started" => {
                            let item = &params["item"];
                            let ty = item["type"].as_str().unwrap_or("");
                            if ty == "agentMessage"
                                && item["phase"].as_str() == Some("commentary")
                                && let Some(id) = item["id"].as_str()
                            {
                                commentary_messages.insert(id.to_string());
                            }
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
                                    .is_none_or(|id| !streamed_raw_reasoning.contains(id))
                                && let Some(text) = completed_raw_reasoning_text(item)
                            {
                                let _ = tx.send(Ok(BackendEvent::ThinkingDelta(text))).await;
                            }
                            if ty == "agentMessage"
                                && let Some(id) = item["id"].as_str()
                            {
                                commentary_messages.remove(id);
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
                            // Publish completion only after active-turn cleanup
                            // is serialized with any replacement startup.
                            let _lifecycle = server.lock_turn_lifecycle(&codex_thread_id).await;
                            server
                                .clear_active_turn(&codex_thread_id, &codex_turn_id)
                                .await;
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
        };
        tokio::select! {
            biased;
            _ = tx.closed() => {
                client_gone = true;
            }
            _ = overload_signal.wait() => {
                route_overloaded = true;
            }
            _ = process_route => {}
        }
        let _cleanup_lifecycle = if turn_finished {
            None
        } else {
            Some(server.lock_turn_lifecycle(&codex_thread_id).await)
        };
        if client_gone {
            server
                .cleanup_active_turn_best_effort(&codex_thread_id, &codex_turn_id, "cancelled")
                .await;
        } else if route_overloaded {
            let _ = tx
                .send(Err(BackendError::Protocol(format!(
                    "app-server event backlog exceeded the per-turn limit of \
                     {ROUTE_EVENT_BUDGET} messages"
                ))))
                .await;
            // Interrupting the turn clears any server-initiated approval
            // request that `process_route` may have been awaiting when the
            // overload signal won the select. Report the overload first so
            // an unresponsive app-server cannot suppress the stream error.
            server
                .cleanup_active_turn_best_effort(&codex_thread_id, &codex_turn_id, "overloaded")
                .await;
        } else if !turn_finished {
            let reason = if server.is_closed() {
                "app-server closed before turn completed"
            } else {
                "app-server event route closed before turn completed"
            };
            let _ = tx.send(Err(BackendError::Protocol(reason.into()))).await;
        }
        server.unsubscribe(&codex_thread_id, &route_tx).await;
    })
}

/// Extract the vendor turn identity from every documented event shape.
fn message_turn_id(message: &ServerMsg) -> Option<&str> {
    let params = match message {
        ServerMsg::Notification { params, .. } | ServerMsg::Request { params, .. } => params,
    };
    params["turnId"]
        .as_str()
        .or_else(|| params["turn"]["id"].as_str())
        .or_else(|| params["item"]["turnId"].as_str())
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
type Routes = Arc<Mutex<HashMap<String, RouteSender<ServerMsg>>>>;
type ActiveTurns = Arc<Mutex<HashMap<String, String>>>;
type TurnLifecycles = Arc<std::sync::Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>;

struct TurnLifecycleGuard {
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    thread_id: String,
    lifecycle: Arc<Mutex<()>>,
    registry: TurnLifecycles,
}

impl Drop for TurnLifecycleGuard {
    fn drop(&mut self) {
        // Release ownership before checking whether any waiter or holder still
        // retains this lifecycle. Acquirers upgrade the weak entry while
        // holding the same registry lock, so pruning cannot race a reuse.
        self.guard.take();
        let mut registry = self.registry.lock().unwrap();
        let same_entry = registry
            .get(&self.thread_id)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.lifecycle)));
        if same_entry && Arc::strong_count(&self.lifecycle) == 1 {
            registry.remove(&self.thread_id);
        }
    }
}

async fn acquire_turn_lifecycle(registry: &TurnLifecycles, thread_id: &str) -> TurnLifecycleGuard {
    let lifecycle = {
        let mut registry = registry.lock().unwrap();
        match registry.get(thread_id).and_then(std::sync::Weak::upgrade) {
            Some(lifecycle) => lifecycle,
            None => {
                let lifecycle = Arc::new(Mutex::new(()));
                registry.insert(thread_id.to_string(), Arc::downgrade(&lifecycle));
                lifecycle
            }
        }
    };
    let guard = Arc::clone(&lifecycle).lock_owned().await;
    TurnLifecycleGuard {
        guard: Some(guard),
        thread_id: thread_id.to_string(),
        lifecycle,
        registry: Arc::clone(registry),
    }
}

#[derive(Default)]
struct BufferedRoute {
    messages: Vec<ServerMsg>,
    overloaded: bool,
}

type Buffered = Arc<Mutex<HashMap<String, BufferedRoute>>>;

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
            let routes_guard = routes.lock().await;
            match routes_guard.get(&thread_id).cloned() {
                Some(tx) => {
                    drop(routes_guard);
                    match tx.try_send(m) {
                        Ok(()) => {
                            failed_routes.remove(&thread_id);
                        }
                        Err(error) => {
                            // The stdout reader is shared by every Codex turn. A
                            // closed or overloaded route must fail independently
                            // rather than blocking every concurrent turn and
                            // JSON-RPC response on this transport.
                            tracing::warn!(
                                "codex: dropping {thread_id} event route: {}",
                                match error {
                                    RouteSendError::Closed => "receiver is closed",
                                    RouteSendError::Overloaded => "event backlog limit exceeded",
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
                    }
                }
                // No subscriber yet: buffer for a thread id we've seen named
                // (skip the empty catch-all) so nothing emitted between
                // thread/start and subscribe is lost. Keep the routes lock
                // until the buffer write completes so subscribe cannot miss a
                // reader that already observed the route as absent.
                None if !thread_id.is_empty() && !failed_routes.contains(&thread_id) => {
                    let mut buffered = buffered.lock().await;
                    let route = buffered.entry(thread_id.clone()).or_default();
                    if route.messages.len() < ROUTE_EVENT_BUDGET {
                        route.messages.push(m);
                    } else {
                        route.overloaded = true;
                        failed_routes.insert(thread_id);
                    }
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

/// Keeps Codex's refreshed subscription credentials while the rest of its
/// home remains isolated. A baseline comparison prevents an old app-server
/// from overwriting a newer login performed by another process.
struct AuthSync {
    source: PathBuf,
    isolated: PathBuf,
    baseline: std::sync::Mutex<Option<Vec<u8>>>,
}

impl AuthSync {
    fn new(source: PathBuf, isolated: PathBuf, baseline: Vec<u8>) -> Self {
        Self {
            source,
            isolated,
            baseline: std::sync::Mutex::new(Some(baseline)),
        }
    }

    fn sync(&self) {
        let mut baseline = self.baseline.lock().unwrap();
        let Some(previous) = baseline.clone() else {
            return;
        };
        let Ok(isolated) = std::fs::read(&self.isolated) else {
            return;
        };
        if isolated == previous {
            return;
        }
        let source = std::fs::read(&self.source).unwrap_or_default();
        if source != previous && source != isolated {
            tracing::warn!(
                "Codex auth changed outside the isolated app-server; preserving the newer source"
            );
            *baseline = None;
            return;
        }
        if source != isolated
            && let Err(error) = std::fs::copy(&self.isolated, &self.source)
        {
            tracing::warn!("failed to preserve refreshed Codex credentials: {error}");
            return;
        }
        *baseline = Some(isolated);
    }
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
    /// Vendor turn currently running for each Codex thread. A replacement
    /// turn interrupts this first so Codex cannot merge prompts across trouve
    /// turn boundaries after cancellation.
    active_turns: ActiveTurns,
    /// Per-thread guards serializing interruption through replacement
    /// registration.
    turn_lifecycles: TurnLifecycles,
    /// Held so the child (kill_on_drop) lives as long as the server handle.
    _child: Child,
    /// A clean Codex home containing credentials only. Keeping the TempDir
    /// alive prevents ambient config, skills, plugins, hooks, and MCP servers
    /// from becoming a second capability source.
    _isolated_home: tempfile::TempDir,
    /// The app-server schema, discovered through its version-specific
    /// experimental feature catalog on the first optimized turn.
    supported_features: OnceCell<HashSet<String>>,
    /// Syncs token rotations from the isolated home back to Codex's real
    /// credential file without exposing any other ambient configuration.
    auth_sync: Option<Arc<AuthSync>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl AppServer {
    async fn spawn(command: &str) -> Result<Self, BackendError> {
        let isolated_home = tempfile::Builder::new()
            .prefix("trouve-codex-home-")
            .tempdir()
            .map_err(BackendError::Io)?;
        let auth_sync = if let Some(source) = codex_auth_path()
            && source.is_file()
        {
            let isolated = isolated_home.path().join("auth.json");
            std::fs::copy(&source, &isolated).map_err(BackendError::Io)?;
            let baseline = std::fs::read(&isolated).map_err(BackendError::Io)?;
            Some(Arc::new(AuthSync::new(source, isolated, baseline)))
        } else {
            None
        };
        let mut child = tokio::process::Command::new(command)
            .arg("app-server")
            .arg("--strict-config")
            .env("CODEX_HOME", isolated_home.path())
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
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            turn_lifecycles: Arc::new(std::sync::Mutex::new(HashMap::new())),
            _child: child,
            _isolated_home: isolated_home,
            supported_features: OnceCell::new(),
            auth_sync,
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

    async fn supported_features(&self) -> HashSet<String> {
        match self
            .supported_features
            .get_or_try_init(|| async {
                self.request("experimentalFeature/list", json!({ "limit": 100 }))
                    .await
                    .map(|result| parse_supported_features(&result))
            })
            .await
        {
            Ok(features) => features.clone(),
            Err(error) => {
                // Old app-server schemas may not expose this method. In that
                // case omit all speculative feature overrides; the isolated
                // CODEX_HOME still excludes ambient skills/plugins/config.
                tracing::debug!(
                    "Codex feature catalog unavailable; omitting feature overrides: {error}"
                );
                HashSet::new()
            }
        }
    }

    async fn handshake(&self) -> Result<(), BackendError> {
        self.request(
            "initialize",
            json!({
                "clientInfo": { "name": "trouve", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true },
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

    async fn sync_auth(&self) {
        if let Some(auth_sync) = self.auth_sync.clone()
            && let Err(error) = tokio::task::spawn_blocking(move || auth_sync.sync()).await
        {
            tracing::warn!("Codex credential sync task failed: {error}");
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

    /// Lock the complete replacement lifecycle for one Codex thread.
    async fn lock_turn_lifecycle(&self, thread_id: &str) -> TurnLifecycleGuard {
        acquire_turn_lifecycle(&self.turn_lifecycles, thread_id).await
    }

    /// Record the vendor turn currently running on a Codex thread.
    async fn register_active_turn(&self, thread_id: &str, turn_id: &str) {
        self.active_turns
            .lock()
            .await
            .insert(thread_id.to_string(), turn_id.to_string());
    }

    /// Clear an active turn only when the caller still owns that turn id.
    async fn clear_active_turn(&self, thread_id: &str, expected_turn_id: &str) {
        let mut active = self.active_turns.lock().await;
        if active
            .get(thread_id)
            .is_some_and(|turn_id| turn_id == expected_turn_id)
        {
            active.remove(thread_id);
        }
    }

    /// Interrupt a predecessor before replacement startup, propagating failure.
    async fn interrupt_active_turn(&self, thread_id: &str) -> Result<(), BackendError> {
        let turn_id = self.active_turns.lock().await.get(thread_id).cloned();
        if let Some(turn_id) = turn_id {
            self.interrupt_turn(thread_id, &turn_id).await?;
            self.clear_active_turn(thread_id, &turn_id).await;
        }
        Ok(())
    }

    /// Ask app-server to stop one exact vendor turn.
    async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), BackendError> {
        self.request(
            "turn/interrupt",
            json!({
                "threadId": thread_id,
                "turnId": turn_id,
            }),
        )
        .await
        .map(|_| ())
    }

    /// Best-effort cleanup when this turn still owns the active marker.
    async fn cleanup_active_turn_best_effort(&self, thread_id: &str, turn_id: &str, reason: &str) {
        if !self
            .active_turns
            .lock()
            .await
            .get(thread_id)
            .is_some_and(|active| active == turn_id)
        {
            return;
        }
        match self.interrupt_turn(thread_id, turn_id).await {
            Ok(()) => self.clear_active_turn(thread_id, turn_id).await,
            Err(error) => {
                tracing::warn!("codex: failed to interrupt {reason} turn {turn_id}: {error}");
            }
        }
    }

    async fn write(&self, msg: Value) -> Result<(), BackendError> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_vec(&msg).expect("serializable");
        line.push(b'\n');
        stdin.write_all(&line).await.map_err(BackendError::Io)?;
        stdin.flush().await.map_err(BackendError::Io)
    }

    async fn subscribe(&self, thread_id: &str) -> RouteSubscription {
        let (tx, rx) = route_channel();
        {
            // The reader takes these locks in the same order when it has no
            // active route. Holding both through registration and draining
            // keeps older buffered notifications ahead of newly routed ones.
            let mut routes = self.routes.lock().await;
            let mut buffered_routes = self.buffered.lock().await;
            let buffered = buffered_routes.remove(thread_id);
            routes.insert(thread_id.to_string(), tx.clone());
            if let Some(buffered) = buffered {
                for message in buffered.messages {
                    if tx.try_send(message).is_err() {
                        break;
                    }
                }
                if buffered.overloaded {
                    tx.mark_overloaded();
                }
            }
        }
        RouteSubscription { tx, rx }
    }

    async fn unsubscribe(&self, thread_id: &str, expected: &RouteSender<ServerMsg>) {
        remove_route(&self.routes, &self.buffered, thread_id, expected).await;
    }
}

/// One routed turn stream paired with the sender identity that owns it.
struct RouteSubscription {
    tx: RouteSender<ServerMsg>,
    rx: RouteReceiver<ServerMsg>,
}

/// Remove a route only when cleanup still owns the active subscription.
async fn remove_route(
    routes: &Routes,
    buffered: &Buffered,
    thread_id: &str,
    expected: &RouteSender<ServerMsg>,
) {
    let mut routes = routes.lock().await;
    if !routes
        .get(thread_id)
        .is_some_and(|active| active.same_channel(expected))
    {
        return;
    }
    routes.remove(thread_id);
    // Keep the lock order aligned with subscribe and the stdout reader.
    // Buffered events belong to the route being removed only when it is
    // still the active route; stale turn cleanup must not erase events for
    // a replacement subscription.
    buffered.lock().await.remove(thread_id);
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
            tool_free: false,
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
    fn extracts_turn_identity_from_codex_event_shapes() {
        let direct = ServerMsg::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({ "threadId": "thread-1", "turnId": "turn-1" }),
        };
        assert_eq!(message_turn_id(&direct), Some("turn-1"));

        let completed = ServerMsg::Notification {
            method: "turn/completed".into(),
            params: json!({ "threadId": "thread-1", "turn": { "id": "turn-2" } }),
        };
        assert_eq!(message_turn_id(&completed), Some("turn-2"));

        let thread_scoped = ServerMsg::Notification {
            method: "thread/tokenUsage/updated".into(),
            params: json!({ "threadId": "thread-1" }),
        };
        assert_eq!(message_turn_id(&thread_scoped), None);
    }

    #[test]
    fn extracts_completed_raw_codex_reasoning_as_a_stream_fallback() {
        let summarized = json!({
            "id": "reason-1",
            "type": "reasoning",
            "summary": ["Checking the adapter", "Found the missing fallback"],
            "content": ["raw text is secondary"],
        });
        assert_eq!(
            completed_raw_reasoning_text(&summarized).as_deref(),
            Some("raw text is secondary")
        );

        let raw = json!({ "type": "reasoning", "summary": [], "content": ["thinking"] });
        assert_eq!(
            completed_raw_reasoning_text(&raw).as_deref(),
            Some("thinking")
        );
        let response_item = json!({
            "type": "reasoning",
            "summary": [
                { "type": "summary_text", "text": "Checking the adapter" },
                { "type": "summary_text", "text": "Found the rich item shape" },
            ],
            "content": [{ "type": "reasoning_text", "text": "raw thought" }],
        });
        assert_eq!(
            completed_raw_reasoning_text(&response_item).as_deref(),
            Some("raw thought")
        );
        assert_eq!(
            completed_raw_reasoning_text(&json!({ "type": "reasoning" })),
            None
        );
    }

    #[test]
    fn turn_disables_reasoning_summaries() {
        let mut params = json!({ "threadId": "thread-1", "input": [] });
        apply_reasoning_options(&mut params, Some("high"));
        assert_eq!(params["summary"], "none");
        assert_eq!(params["effort"], "high");

        let mut without_effort = json!({});
        apply_reasoning_options(&mut without_effort, None);
        assert_eq!(without_effort["summary"], "none");
        assert!(without_effort["effort"].is_null());
    }

    #[test]
    fn routes_codex_commentary_as_thinking() {
        let commentary = HashSet::from(["commentary-1".to_string()]);
        let params = json!({ "itemId": "commentary-1", "delta": "Checking the parser." });
        assert!(matches!(
            agent_message_delta(&params, &commentary),
            Some(BackendEvent::ThinkingDelta(text)) if text == "Checking the parser."
        ));

        let final_params = json!({ "itemId": "final-1", "delta": "Done." });
        assert!(matches!(
            agent_message_delta(&final_params, &commentary),
            Some(BackendEvent::TextDelta(text)) if text == "Done."
        ));

        // Older Codex versions omit itemId/phase; preserve their historical
        // final-answer routing rather than guessing.
        let legacy = json!({ "delta": "Legacy response." });
        assert!(matches!(
            agent_message_delta(&legacy, &commentary),
            Some(BackendEvent::TextDelta(text)) if text == "Legacy response."
        ));
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
        let (route_tx, mut route_rx) = route_channel();
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
    async fn stale_turn_cleanup_preserves_replacement_route() {
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let buffered: Buffered = Arc::new(Mutex::new(HashMap::new()));
        let (stale_tx, _stale_rx) = route_channel();
        let (replacement_tx, mut replacement_rx) = route_channel();
        routes
            .lock()
            .await
            .insert("thread-1".into(), replacement_tx.clone());

        remove_route(&routes, &buffered, "thread-1", &stale_tx).await;

        let active = routes
            .lock()
            .await
            .get("thread-1")
            .cloned()
            .expect("stale cleanup must preserve the replacement route");
        active
            .try_send(ServerMsg::Notification {
                method: "turn/started".into(),
                params: json!({ "threadId": "thread-1" }),
            })
            .unwrap();
        assert!(replacement_rx.recv().await.is_some());

        remove_route(&routes, &buffered, "thread-1", &replacement_tx).await;
        assert!(routes.lock().await.is_empty());
    }

    #[tokio::test]
    async fn lifecycle_entry_survives_waiters_and_prunes_after_final_guard() {
        let registry: TurnLifecycles = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let first = acquire_turn_lifecycle(&registry, "thread-1").await;
        let waiter_registry = Arc::clone(&registry);
        let waiter =
            tokio::spawn(async move { acquire_turn_lifecycle(&waiter_registry, "thread-1").await });

        loop {
            let strong = registry
                .lock()
                .unwrap()
                .get("thread-1")
                .map_or(0, std::sync::Weak::strong_count);
            if strong >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }

        drop(first);
        let second = waiter.await.unwrap();
        assert_eq!(registry.lock().unwrap().len(), 1);
        drop(second);
        assert!(registry.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn turn_route_burst_is_lossless_and_does_not_block_stdout_eof_cleanup() {
        let deadline = std::time::Duration::from_secs(1);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let buffered: Buffered = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (route_tx, mut route_rx) = route_channel();
        routes.lock().await.insert("thread-1".into(), route_tx);

        let (mut writer, reader) = tokio::io::duplex(256);
        let task = tokio::spawn(read_stdout(
            reader,
            pending.clone(),
            routes.clone(),
            buffered.clone(),
            closed.clone(),
        ));
        let event_count = ROUTE_EVENT_BUDGET;
        for sequence in 0..event_count {
            writer
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"thread-1\",\"delta\":\"{sequence}\"}}}}\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        writer.shutdown().await.unwrap();
        tokio::time::timeout(deadline, task)
            .await
            .expect("route backpressure must not block EOF cleanup")
            .unwrap();

        assert!(closed.load(Ordering::Relaxed));
        assert!(routes.lock().await.is_empty());
        assert!(buffered.lock().await.is_empty());
        for sequence in 0..event_count {
            let msg = tokio::time::timeout(deadline, route_rx.recv())
                .await
                .expect("routed event should be available")
                .expect("route should retain every event in the burst");
            let ServerMsg::Notification { method, params } = msg else {
                panic!("expected notification");
            };
            assert_eq!(method, "item/agentMessage/delta");
            assert_eq!(params["delta"], sequence.to_string());
        }
        assert!(
            tokio::time::timeout(deadline, route_rx.recv())
                .await
                .expect("route should close after stdout EOF")
                .is_none()
        );
    }

    #[test]
    fn mutable_permissions_leave_git_metadata_writable() {
        assert_eq!(
            permission_settings(crate::BackendPermission::ReadOnly),
            ("never", "read-only", "readOnly")
        );
        assert_eq!(
            permission_settings(crate::BackendPermission::Ask),
            ("untrusted", "danger-full-access", "dangerFullAccess")
        );
        assert_eq!(
            permission_settings(crate::BackendPermission::Yolo),
            ("never", "danger-full-access", "dangerFullAccess")
        );
    }

    #[test]
    fn codex_config_override_requests_raw_reasoning_and_merges_mcp_servers() {
        let mut turn = bare_turn();
        let config = codex_config_override(&turn);
        assert_eq!(config["show_raw_agent_reasoning"], true);
        assert!(config["mcp_servers"].is_null());

        turn.mcp_servers.push(crate::McpServerLaunch {
            name: "jira".into(),
            command: "jira-mcp".into(),
            args: vec!["--stdio".into()],
            env: vec![("TOKEN".into(), "sekrit".into())],
        });
        turn.mcp_bridge = Some(crate::McpBridgeConfig {
            url: "http://127.0.0.1:1/internal/threads/th_1/mcp?approval=0".into(),
        });
        let config = codex_config_override(&turn);
        let servers = &config["mcp_servers"];
        assert_eq!(servers["jira"]["command"], "jira-mcp");
        assert_eq!(servers["jira"]["env"]["TOKEN"], "sekrit");
        assert_eq!(
            servers["trouve"]["url"],
            "http://127.0.0.1:1/internal/threads/th_1/mcp?approval=0"
        );
        assert_eq!(servers["trouve"]["default_tools_approval_mode"], "approve");
        assert!(servers["trouve"]["command"].is_null());

        // User servers alone (no bridge) still produce an override.
        turn.mcp_bridge = None;
        let config = codex_config_override(&turn);
        assert!(config["mcp_servers"]["jira"].is_object());
        assert!(config["mcp_servers"]["trouve"].is_null());
    }

    #[test]
    fn optimized_config_disables_product_surface_but_keeps_native_tools() {
        let mut turn = bare_turn();
        turn.mcp_bridge = Some(crate::McpBridgeConfig {
            url: "http://127.0.0.1:1/internal/threads/th_1/mcp?approval=0".into(),
        });
        let supported_features = [
            "apps",
            "hooks",
            "multi_agent",
            "plugins",
            "shell_tool",
            "unknown_future_feature",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();

        let config = mcp_config_override(&turn, &supported_features).unwrap();
        assert!(config["web_search"].is_null());
        assert_eq!(
            config["mcp_servers"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["trouve"]
        );
        for feature in ["apps", "hooks", "multi_agent", "plugins"] {
            assert_eq!(
                config["features"][feature], false,
                "feature {feature} escaped product-surface isolation"
            );
        }
        // Unsupported schema keys are omitted under --strict-config, while
        // model-optimized native tools remain enabled by omission.
        assert!(config["features"]["browser_use"].is_null());
        assert!(config["features"]["shell_tool"].is_null());
        assert!(config["features"]["unknown_future_feature"].is_null());
        assert!(config["agents"].is_null());
        assert!(config["experimental_request_user_input_enabled"].is_null());
        assert_eq!(
            config["mcp_servers"]["trouve"]["default_tools_approval_mode"],
            "approve"
        );
    }

    #[test]
    fn parses_version_specific_feature_catalog_shapes() {
        assert_eq!(
            parse_supported_features(&json!({
                "data": [{"name": "plugins"}, {"key": "multi_agent"}]
            })),
            ["plugins", "multi_agent"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
        assert_eq!(
            parse_supported_features(&json!({
                "features": [{"feature": "apps"}, {"name": 42}, "bad"]
            })),
            ["apps"].into_iter().map(str::to_string).collect()
        );
    }

    #[test]
    fn auth_sync_preserves_refreshes_and_never_overwrites_newer_login() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source-auth.json");
        let isolated = temp.path().join("isolated-auth.json");
        std::fs::write(&source, b"old").unwrap();
        std::fs::write(&isolated, b"old").unwrap();
        let sync = AuthSync::new(source.clone(), isolated.clone(), b"old".to_vec());

        std::fs::write(&isolated, b"refreshed").unwrap();
        sync.sync();
        assert_eq!(std::fs::read(&source).unwrap(), b"refreshed");

        std::fs::write(&source, b"new-login").unwrap();
        std::fs::write(&isolated, b"stale-refresh").unwrap();
        sync.sync();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");

        // Once an external login wins, later isolated changes remain unable
        // to overwrite it.
        std::fs::write(&isolated, b"later-stale-refresh").unwrap();
        sync.sync();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");
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
    fn observed_windows_fill_unknown_live_sizes() {
        let backend = CodexBackend::new("codex", None);
        let result = json!({ "data": [{
            "id": "gpt-5.6",
            "displayName": "GPT-5.6",
            "hidden": false,
        }]});
        let mut models = parse_model_list("codex", &result);
        assert_eq!(models[0].context_window, 0);

        backend
            .observed_windows
            .lock()
            .unwrap()
            .insert("gpt-5.6".into(), 1_050_000);
        backend.apply_observed_windows(&mut models);
        assert_eq!(models[0].context_window, 1_050_000);
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
        assert_eq!(models[0].context_window, 0);
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
