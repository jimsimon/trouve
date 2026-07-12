//! Cursor backend, driving `cursor-agent acp` (Agent Client Protocol).
//!
//! One `cursor-agent acp` child is spawned lazily and shared across threads
//! (JSON-RPC over stdio, like Codex's app-server). Each trouve thread maps
//! to an ACP session; turns run `session/prompt` and stream
//! `session/update` notifications.
//!
//! ACP fixes the two long-standing gaps of the old `-p --output-format
//! stream-json` integration:
//! - structured model metadata (`cursor/list_available_models` exposes
//!   thinking/context/effort/fast knobs per model, including the 300k/1M
//!   context choice), applied per session via `session/set_config_option`;
//! - an interactive approval bridge (`session/request_permission`), mapped
//!   onto [`BackendEvent::ApprovalNeeded`] so trouve's permission layer
//!   decides instead of cursor's own allowlist prompts dying headless.
//!
//! Model selection needs cursor-agent 2026.07 or newer: older builds accept
//! `session/set_config_option` but silently keep the previous model. The
//! adapter detects that from the response snapshot and fails the turn with
//! a pointer at the managed CLI installer.
//!
//! Auth: `cursor-agent login` (subscription) or the `CURSOR_API_KEY` env
//! var / configured API key — both handled by the CLI.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc, oneshot};
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventStream, BackendLogin, BackendPermission,
    BackendStatus, BackendTurn, async_stream, binary_on_path, model, spawn_login,
};

pub struct CursorBackend {
    id: String,
    command: String,
    api_key: Option<String>,
    server: Mutex<Option<Arc<AcpServer>>>,
    /// `cursor/list_available_models` result, cached for [`MODELS_TTL`].
    models_cache: Mutex<Option<(std::time::Instant, Vec<ModelInfo>)>>,
}

/// How long a fetched vendor model list stays fresh.
const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

impl CursorBackend {
    pub fn new(id: impl Into<String>, command: Option<String>, api_key: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "cursor-agent".into()),
            api_key,
            server: Mutex::new(None),
            models_cache: Mutex::new(None),
        }
    }

    async fn server(&self) -> Result<Arc<AcpServer>, BackendError> {
        let mut guard = self.server.lock().await;
        if let Some(s) = guard.as_ref()
            && !s.is_closed()
        {
            return Ok(s.clone());
        }
        let s = Arc::new(AcpServer::spawn(&self.command, self.api_key.as_deref()).await?);
        s.handshake().await?;
        *guard = Some(s.clone());
        Ok(s)
    }
}

#[async_trait::async_trait]
impl AgentBackend for CursorBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Minimal offline fallback; `list_models` asks the vendor for the
        // real catalog (per-account, with per-model config options).
        vec![model(&self.id, "default", "Cursor Auto", 200_000)]
    }

    async fn list_models(&self) -> Vec<ModelInfo> {
        {
            let cache = self.models_cache.lock().await;
            if let Some((at, models)) = cache.as_ref()
                && at.elapsed() < MODELS_TTL
            {
                return models.clone();
            }
        }
        let fetched = async {
            let server = self.server().await?;
            server
                .request("cursor/list_available_models", json!({}))
                .await
        }
        .await;
        match fetched {
            Ok(result) => {
                let models = parse_acp_models(&self.id, &result);
                if models.is_empty() {
                    return self.models();
                }
                *self.models_cache.lock().await = Some((std::time::Instant::now(), models.clone()));
                models
            }
            Err(e) => {
                tracing::debug!("cursor/list_available_models failed: {e}; using static list");
                self.models()
            }
        }
    }

    fn status(&self) -> BackendStatus {
        let installed = binary_on_path(&self.command);
        let has_credentials = self.api_key.is_some()
            || std::env::var("CURSOR_API_KEY").is_ok()
            || dirs::config_dir()
                .map(|d| d.join("cursor-agent").exists())
                .unwrap_or(false)
            || dirs::home_dir()
                .map(|h| h.join(".cursor").join("cli-config.json").exists())
                .unwrap_or(false);
        BackendStatus {
            installed,
            has_credentials,
        }
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        spawn_login(&self.command, &["login"]).await
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        let server = self.server().await?;

        // Resume the ACP session for this thread, or start a fresh one. A
        // failed load (e.g. server restarted and lost it) degrades to fresh.
        let mut fresh_session = false;
        let session_id = match &turn.session {
            Some(sid) if server.knows_session(sid).await => sid.clone(),
            Some(sid) => match server
                .load_session(sid, &turn.worktree, &turn.mcp_servers)
                .await
            {
                Ok(()) => sid.clone(),
                Err(e) => {
                    tracing::warn!("cursor session/load failed ({e}); starting fresh");
                    fresh_session = true;
                    server
                        .new_session(&turn.worktree, &turn.mcp_servers)
                        .await?
                }
            },
            None => {
                fresh_session = true;
                server
                    .new_session(&turn.worktree, &turn.mcp_servers)
                    .await?
            }
        };

        let text = match (&turn.instructions, fresh_session) {
            (Some(instr), true) => format!(
                "<mode-instructions>\n{instr}\n</mode-instructions>\n\n{}",
                turn.prompt
            ),
            _ => turn.prompt.clone(),
        };

        // Mode + model config, then the prompt, under the config lock:
        // cursor applies model selection process-wide (all sessions sync to
        // the current model), so racing turns must not interleave their
        // set-model and prompt-start.
        let (route, prompt_rx) = {
            let _config = server.config_lock.lock().await;

            let mode = match turn.permission {
                // Cursor's plan mode is its read-only posture.
                BackendPermission::ReadOnly => "plan",
                BackendPermission::Ask | BackendPermission::Yolo => "agent",
            };
            if let Err(e) = server.set_config_option(&session_id, "mode", mode).await {
                tracing::warn!("cursor set mode {mode} failed: {e}");
            }

            if !turn.model.is_empty() && !matches!(turn.model.as_str(), "auto" | "default") {
                apply_model_config(&server, &session_id, &turn).await?;
            }

            // ACP image content blocks carry base64 data inline.
            let mut prompt_blocks = vec![json!({ "type": "text", "text": text })];
            for att in &turn.attachments {
                match att.read_base64() {
                    Ok(data) => prompt_blocks.push(json!({
                        "type": "image",
                        "mimeType": att.mime,
                        "data": data,
                    })),
                    Err(e) => tracing::warn!("skipping attachment {}: {e}", att.name),
                }
            }

            // Subscribe after session setup so a session/load's history
            // replay doesn't re-emit old text into the thread.
            let route = server.subscribe(&session_id).await;
            let prompt_rx = server
                .request_deferred(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": prompt_blocks,
                    }),
                )
                .await?;
            (route, prompt_rx)
        };

        let stream = turn_stream(
            server.clone(),
            session_id.clone(),
            route,
            prompt_rx,
            fresh_session,
            turn.permission,
        );
        Ok(stream.boxed())
    }
}

/// Set the session's model and its config options (thinking/context/effort/
/// fast), translating trouve's stored model + options into ACP config calls.
async fn apply_model_config(
    server: &AcpServer,
    session_id: &str,
    turn: &BackendTurn,
) -> Result<(), BackendError> {
    // Threads from before the ACP migration may still store a variant id
    // like "claude-opus-4-8-high"; peel the level back off.
    let (base, legacy_level, legacy_fast) = split_variant(&turn.model);

    let result = server
        .set_config_option(session_id, "model", base)
        .await
        .map_err(|e| {
            BackendError::Protocol(format!(
                "cursor-agent rejected model {base}: {e} \
                 (if this persists, update the CLI in Settings → Vendor CLIs)"
            ))
        })?;
    // Old cursor-agent builds (< 2026.07) accept the call but silently keep
    // the previous model; the response snapshot betrays them.
    if let Some(current) = config_snapshot_value(&result, "model")
        && current != base
    {
        return Err(BackendError::Protocol(format!(
            "cursor-agent ignored the model change to {base} (still {current}); \
                 this build is too old for ACP model selection — update the CLI in \
                 Settings → Vendor CLIs"
        )));
    }

    // Options: schema-keyed values from the thread, plus legacy fallbacks.
    let mut options: Vec<(String, String)> = Vec::new();
    for (key, value) in &turn.model_options {
        let value = match value {
            Value::Bool(b) => b.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        match key.as_str() {
            // Pre-ACP threads stored the thinking dropdown under
            // thinking_level (cursor) or reasoning_effort (codex-style).
            "thinking_level" | "reasoning_effort" => {
                options.push(("effort".into(), value.clone()));
                options.push(("reasoning".into(), value));
            }
            _ => options.push((key.clone(), value)),
        }
    }
    if let Some(level) = legacy_level {
        options.push(("effort".into(), level.to_string()));
        options.push(("reasoning".into(), level.to_string()));
    }
    if legacy_fast {
        options.push(("fast".into(), "true".into()));
    }

    // Unknown options are expected (effort vs reasoning depends on the
    // model); failures are logged, not fatal.
    for (key, value) in options {
        if let Err(e) = server.set_config_option(session_id, &key, &value).await {
            tracing::debug!("cursor set_config_option {key}={value}: {e}");
        }
    }
    Ok(())
}

/// Pull one option's currentValue out of a `set_config_option` response
/// (`{ configOptions: [ { id, currentValue, ... } ] }`).
fn config_snapshot_value(result: &Value, id: &str) -> Option<String> {
    result["configOptions"].as_array()?.iter().find_map(|o| {
        (o["id"].as_str() == Some(id))
            .then(|| o["currentValue"].as_str().map(str::to_string))
            .flatten()
    })
}

/// Translate routed ACP messages into `BackendEvent`s until the prompt
/// request resolves (end of turn).
fn turn_stream(
    server: Arc<AcpServer>,
    session_id: String,
    mut route: mpsc::Receiver<ServerMsg>,
    mut prompt_rx: oneshot::Receiver<Result<Value, String>>,
    fresh_session: bool,
    permission: BackendPermission,
) -> impl futures::Stream<Item = Result<BackendEvent, BackendError>> {
    async_stream(move |tx| async move {
        if fresh_session {
            let _ = tx
                .send(Ok(BackendEvent::SessionStarted {
                    session_id: session_id.clone(),
                }))
                .await;
        }
        let mut client_gone = false;
        loop {
            tokio::select! {
                msg = route.recv() => {
                    let Some(msg) = msg else { break };
                    if handle_msg(&server, msg, &tx, permission).await.is_err() {
                        // Receiver dropped (turn cancelled): stop cursor's
                        // generation instead of letting it run headless.
                        client_gone = true;
                        server.notify("session/cancel", json!({ "sessionId": session_id })).await;
                        break;
                    }
                }
                result = &mut prompt_rx => {
                    // Reader delivers in wire order, so any updates sent
                    // before the response are already queued; drain them.
                    while let Ok(msg) = route.try_recv() {
                        if handle_msg(&server, msg, &tx, permission).await.is_err() {
                            client_gone = true;
                            break;
                        }
                    }
                    match result {
                        Ok(Ok(value)) => {
                            let _ = tx.send(Ok(BackendEvent::Completed {
                                usage: parse_usage(&value["usage"]),
                            })).await;
                        }
                        Ok(Err(e)) => {
                            let _ = tx.send(Err(BackendError::Protocol(
                                format!("session/prompt: {e}")))).await;
                        }
                        Err(_) => {
                            let _ = tx.send(Err(BackendError::Protocol(
                                "cursor-agent closed before the turn completed".into()))).await;
                        }
                    }
                    break;
                }
            }
        }
        if client_gone {
            // Best effort; the vendor process keeps running for other threads.
            tracing::debug!("cursor turn for {session_id} cancelled by client");
        }
        server.unsubscribe(&session_id).await;
    })
}

/// Map one routed ACP message to backend events. `Err(())` means the
/// receiving stream is gone.
async fn handle_msg(
    server: &AcpServer,
    msg: ServerMsg,
    tx: &mpsc::Sender<Result<BackendEvent, BackendError>>,
    permission: BackendPermission,
) -> Result<(), ()> {
    match msg {
        ServerMsg::Notification { method, params } => {
            if method != "session/update" {
                return Ok(());
            }
            for mut ev in map_update(&params["update"]) {
                // Plan tool calls complete without a rawOutput; the plan
                // itself arrived via cursor/create_plan and was stashed by
                // the reader — attach it as the tool's result.
                if let BackendEvent::ToolCompleted {
                    call_id, result, ..
                } = &mut ev
                    && result.is_null()
                    && let Some(plan) = server.plans.lock().await.remove(call_id)
                {
                    *result = plan;
                }
                tx.send(Ok(ev)).await.map_err(|_| ())?;
            }
            Ok(())
        }
        ServerMsg::Request { id, method, params } => {
            if method == "cursor/ask_question" {
                return handle_ask_question(server, id, &params, tx).await;
            }
            if method != "session/request_permission" {
                // Unknown server request: refuse rather than hang.
                server
                    .respond_err(id, -32601, &format!("unsupported method {method}"))
                    .await;
                return Ok(());
            }
            let allow_option = permission_option(&params, true);
            let reject_option = permission_option(&params, false);
            if matches!(permission, BackendPermission::Yolo) {
                server.respond(id, permission_outcome(allow_option)).await;
                return Ok(());
            }
            let tool_call = &params["toolCall"];
            let (ok_tx, ok_rx) = oneshot::channel();
            let call_id = tool_call["toolCallId"].as_str().unwrap_or("").to_string();
            tx.send(Ok(BackendEvent::ApprovalNeeded {
                call_id,
                tool: tool_call["kind"]
                    .as_str()
                    .or_else(|| tool_call["title"].as_str())
                    .unwrap_or("tool")
                    .to_string(),
                args: tool_call.clone(),
                responder: ok_tx,
            }))
            .await
            .map_err(|_| ())?;
            let approved = ok_rx.await.unwrap_or(false);
            let option = if approved {
                allow_option
            } else {
                reject_option
            };
            server.respond(id, permission_outcome(option)).await;
            Ok(())
        }
    }
}

/// Pick the offered option id for allowing (once, never "always" — trouve's
/// permission layer owns persistence) or rejecting.
fn permission_option(params: &Value, allow: bool) -> String {
    let want = if allow { "allow_once" } else { "reject_once" };
    params["options"]
        .as_array()
        .and_then(|opts| {
            opts.iter()
                .find(|o| o["kind"].as_str() == Some(want))
                .and_then(|o| o["optionId"].as_str())
        })
        .unwrap_or(if allow { "allow-once" } else { "reject-once" })
        .to_string()
}

fn permission_outcome(option_id: String) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

/// Bridge a `cursor/ask_question` extension request into
/// [`BackendEvent::QuestionsNeeded`] and answer with cursor's outcome shape.
/// The agent blocks its turn on this response.
///
/// As of cursor-agent 2026.07.01, Cursor's backend does not include the
/// AskQuestion tool in the model's toolset on the ACP surface (any mode, any
/// model — probed empirically; there is no client-side capability to request
/// it, and the `ask_question_all_modes` flag is server-assigned). This
/// handler is ready for when Cursor enables it; until then cursor models
/// ask questions as plain text.
async fn handle_ask_question(
    server: &AcpServer,
    id: Value,
    params: &Value,
    tx: &mpsc::Sender<Result<BackendEvent, BackendError>>,
) -> Result<(), ()> {
    let questions: Vec<trouve_protocol::Question> = params["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(qi, q)| {
            let prompt = q["prompt"].as_str()?.to_string();
            let options: Vec<trouve_protocol::QuestionOption> = q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    Some(trouve_protocol::QuestionOption {
                        id: o["id"].as_str()?.to_string(),
                        label: o["label"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect();
            Some(trouve_protocol::Question {
                id: q["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("q{}", qi + 1)),
                prompt,
                options,
                allow_multiple: q["allowMultiple"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    if questions.is_empty() {
        server
            .respond(
                id,
                json!({ "outcome": { "outcome": "skipped", "reason": "no questions" } }),
            )
            .await;
        return Ok(());
    }
    let title = params["title"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string);
    let request_id = params["toolCallId"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("q_{}", std::process::id()));
    let (ans_tx, ans_rx) = oneshot::channel();
    tx.send(Ok(BackendEvent::QuestionsNeeded {
        request_id,
        title,
        questions,
        responder: ans_tx,
    }))
    .await
    .map_err(|_| ())?;
    let outcome = match ans_rx.await.unwrap_or(None) {
        Some(answers) => {
            let answers: Vec<Value> = answers
                .into_iter()
                .map(|a| {
                    json!({
                        "questionId": a.question_id,
                        "selectedOptionIds": a.selected_option_ids,
                        // Older cursor-agent builds drop this; harmless.
                        "freeformText": a.other_text,
                    })
                })
                .collect();
            json!({ "outcome": { "outcome": "answered", "answers": answers } })
        }
        None => json!({ "outcome": { "outcome": "skipped", "reason": "User skipped questions" } }),
    };
    server.respond(id, outcome).await;
    Ok(())
}

/// Map one `session/update` payload to zero or more backend events.
fn map_update(update: &Value) -> Vec<BackendEvent> {
    match update["sessionUpdate"].as_str() {
        Some("agent_message_chunk") => update["content"]["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|t| vec![BackendEvent::TextDelta(t.to_string())])
            .unwrap_or_default(),
        Some("agent_thought_chunk") => update["content"]["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|t| vec![BackendEvent::ThinkingDelta(t.to_string())])
            .unwrap_or_default(),
        Some("tool_call") => {
            let call_id = update["toolCallId"].as_str().unwrap_or("").to_string();
            // "kind" is the tool family (read/execute/edit/…); the human
            // title (e.g. "`git status`") rides along in the args. Catch-all
            // "other" calls carry their real name in rawInput._toolName
            // (e.g. createPlan).
            let kind = update["kind"].as_str().unwrap_or("tool");
            let tool = match kind {
                "other" => update["rawInput"]["_toolName"].as_str().unwrap_or(kind),
                k => k,
            }
            .to_string();
            let mut args = update["rawInput"].clone();
            if !args.is_object() {
                args = json!({});
            }
            if let Some(title) = update["title"].as_str() {
                args["title"] = json!(title);
            }
            vec![BackendEvent::ToolStarted {
                call_id,
                tool,
                args,
            }]
        }
        Some("tool_call_update") => {
            let call_id = update["toolCallId"].as_str().unwrap_or("").to_string();
            match update["status"].as_str() {
                Some("completed") => vec![BackendEvent::ToolCompleted {
                    call_id,
                    ok: true,
                    result: update["rawOutput"].clone(),
                }],
                Some("failed") => vec![BackendEvent::ToolCompleted {
                    call_id,
                    ok: false,
                    result: update["rawOutput"].clone(),
                }],
                _ => vec![], // pending / in_progress
            }
        }
        // The slash commands / skills this session accepts in prompts,
        // surfaced as prompt-box completions.
        Some("available_commands_update") => {
            let commands = update["availableCommands"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|c| {
                            let name = c["name"].as_str()?.to_string();
                            let description =
                                c["description"].as_str().unwrap_or_default().to_string();
                            Some(trouve_protocol::CommandInfo { name, description })
                        })
                        .collect()
                })
                .unwrap_or_default();
            vec![BackendEvent::CommandsUpdated { commands }]
        }
        // Plans, title updates, mode echoes: nothing trouve renders from
        // these yet.
        _ => vec![],
    }
}

/// Parse the optional `usage` object of a `session/prompt` response.
/// Current cursor-agent builds omit it; the default keeps the turn valid.
fn parse_usage(usage: &Value) -> Usage {
    Usage {
        input_tokens: usage["inputTokens"].as_u64().unwrap_or(0),
        output_tokens: usage["outputTokens"].as_u64().unwrap_or(0),
        cached_input_tokens: usage["cachedReadTokens"].as_u64().unwrap_or(0),
        cost_usd: None,
        context_window: None,
    }
}

// --- model catalog -----------------------------------------------------------

/// Map a `cursor/list_available_models` result to ModelInfos: one entry per
/// model with its config options (thinking/context/effort/reasoning/fast)
/// as an options schema.
fn parse_acp_models(backend_id: &str, result: &Value) -> Vec<ModelInfo> {
    let Some(models) = result["models"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in models {
        let Some(id) = entry["value"].as_str() else {
            continue;
        };
        let display = entry["name"].as_str().unwrap_or(id);
        let options = entry["configOptions"].as_array();

        let mut properties = serde_json::Map::new();
        let mut context_window = None;
        for opt in options.into_iter().flatten() {
            let Some(opt_id) = opt["id"].as_str() else {
                continue;
            };
            let values: Vec<&str> = opt["options"]
                .as_array()
                .map(|list| list.iter().filter_map(|o| o["value"].as_str()).collect())
                .unwrap_or_default();
            let default = opt["currentValue"].as_str().unwrap_or("");
            let description = opt["description"].as_str().unwrap_or("");

            if opt_id == "context" {
                // The default context choice is the advertised window; the
                // schema lets clients pick larger (1M) or smaller.
                context_window = parse_context_size(default);
            }
            // Binary on/off options render as toggles.
            let is_bool =
                values.len() == 2 && values.contains(&"true") && values.contains(&"false");
            let prop = if is_bool {
                json!({
                    "type": "boolean",
                    "default": default == "true",
                    "description": description,
                })
            } else {
                json!({
                    "type": "string",
                    "enum": values,
                    "default": default,
                    "description": description,
                })
            };
            properties.insert(opt_id.to_string(), prop);
        }

        let mut info = model(backend_id, id, display, context_window.unwrap_or(200_000));
        info.options_schema = json!({
            "type": "object",
            "properties": properties,
        });
        out.push(info);
    }
    out
}

/// Parse cursor's context-size tokens ("300k", "1m", "272k") into a window.
fn parse_context_size(token: &str) -> Option<u64> {
    let token = token.trim().to_lowercase();
    let (digits, mult) = if let Some(d) = token.strip_suffix('m') {
        (d, 1_000_000)
    } else if let Some(d) = token.strip_suffix('k') {
        (d, 1_000)
    } else {
        (token.as_str(), 1)
    };
    digits.parse::<u64>().ok().map(|n| n * mult)
}

/// Thinking/effort level tokens the pre-ACP catalog used as id suffixes.
const LEVELS: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

/// Split a pre-ACP variant id into `(base, level, fast)`; threads created
/// before the migration may still store "claude-opus-4-8-high-fast".
fn split_variant(id: &str) -> (&str, Option<&str>, bool) {
    let (rest, fast) = match id.strip_suffix("-fast") {
        Some(rest) => (rest, true),
        None => (id, false),
    };
    if let Some((head, tail)) = rest.rsplit_once('-')
        && LEVELS.contains(&tail)
    {
        return (head, Some(tail), fast);
    }
    (rest, None, fast)
}

// --- JSON-RPC plumbing (ACP over stdio) ---------------------------------------

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

struct AcpServer {
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: Pending,
    routes: Arc<Mutex<HashMap<String, mpsc::Sender<ServerMsg>>>>,
    /// Sessions this process has created or loaded (session/prompt on an
    /// unknown session fails, so resumes go through session/load first).
    sessions: Mutex<HashSet<String>>,
    /// Serializes model/mode config + prompt start: cursor applies model
    /// selection process-wide, so concurrent turns must not interleave.
    config_lock: Mutex<()>,
    /// Plan-mode plans by tool call id: `cursor/create_plan` arrives as a
    /// session-less request (answered by the reader); the stashed content
    /// becomes the plan tool's result when its completion update lands.
    plans: Arc<Mutex<HashMap<String, Value>>>,
    /// Tool call id → session id, recorded from `session/update`
    /// notifications: session-less requests like `cursor/ask_question` only
    /// carry a toolCallId, so this is how they find their session's route.
    calls: Arc<Mutex<HashMap<String, String>>>,
    /// Held so the child (kill_on_drop) lives as long as the server handle.
    _child: Child,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl AcpServer {
    async fn spawn(command: &str, api_key: Option<&str>) -> Result<Self, BackendError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.arg("acp");
        if let Some(key) = api_key {
            cmd.env("CURSOR_API_KEY", key);
        }
        let mut child = cmd
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
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            routes: Arc::new(Mutex::new(HashMap::new())),
            sessions: Mutex::new(HashSet::new()),
            config_lock: Mutex::new(()),
            plans: Arc::new(Mutex::new(HashMap::new())),
            calls: Arc::new(Mutex::new(HashMap::new())),
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
        let plans = self.plans.clone();
        let calls = self.calls.clone();
        let stdin = self.stdin.clone();
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
                    if let Some(id) = msg["id"].as_i64()
                        && let Some(tx) = pending.lock().await.remove(&id)
                    {
                        let result = if msg.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                            let e = &msg["error"];
                            let detail = e["data"]["message"]
                                .as_str()
                                .or_else(|| e["message"].as_str())
                                .unwrap_or("unknown error");
                            Err(detail.to_string())
                        } else {
                            Ok(msg["result"].clone())
                        };
                        let _ = tx.send(result);
                    }
                } else if has_method {
                    let method = msg["method"].as_str().unwrap_or("").to_string();
                    let params = msg["params"].clone();
                    // Plan mode: the agent submits the finished plan as a
                    // session-less request and blocks the turn on the
                    // response. Ack it here and stash the content — it
                    // becomes the plan tool call's result when that call's
                    // completion update arrives.
                    if method == "cursor/create_plan" && has_id {
                        if let Some(call_id) = params["toolCallId"].as_str() {
                            plans
                                .lock()
                                .await
                                .insert(call_id.to_string(), params.clone());
                        }
                        let reply = json!({ "jsonrpc": "2.0", "id": msg["id"], "result": {} });
                        let mut line = serde_json::to_vec(&reply).expect("serializable");
                        line.push(b'\n');
                        let mut stdin = stdin.lock().await;
                        let _ = stdin.write_all(&line).await;
                        let _ = stdin.flush().await;
                        continue;
                    }
                    let mut session_id = params["sessionId"].as_str().unwrap_or("").to_string();
                    // Remember which session owns each tool call: extension
                    // requests like cursor/ask_question are session-less and
                    // find their route through the toolCallId.
                    if method == "session/update"
                        && !session_id.is_empty()
                        && let Some(call_id) = params["update"]["toolCallId"].as_str()
                    {
                        let mut calls = calls.lock().await;
                        calls.insert(call_id.to_string(), session_id.clone());
                        if calls.len() > 4096 {
                            calls.clear(); // crude bound; live calls re-register
                        }
                    }
                    if session_id.is_empty()
                        && let Some(call_id) = params["toolCallId"].as_str()
                        && let Some(owner) = calls.lock().await.get(call_id)
                    {
                        session_id = owner.clone();
                    }
                    let routed = {
                        let routes = routes.lock().await;
                        routes.get(&session_id).cloned()
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
                    } else if has_id {
                        // A request nobody can answer must still get a
                        // response — the agent blocks its turn on it.
                        tracing::warn!("cursor acp: refusing unroutable request {method}");
                        let reply = json!({
                            "jsonrpc": "2.0", "id": msg["id"],
                            "error": { "code": -32601,
                                       "message": format!("unsupported method {method}") },
                        });
                        let mut line = serde_json::to_vec(&reply).expect("serializable");
                        line.push(b'\n');
                        let mut stdin = stdin.lock().await;
                        let _ = stdin.write_all(&line).await;
                        let _ = stdin.flush().await;
                    }
                }
            }
            closed.store(true, Ordering::Relaxed);
        });
    }

    async fn handshake(&self) -> Result<(), BackendError> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        // Clean model ids + per-parameter config options
                        // instead of one exploded variant list.
                        "_meta": { "parameterizedModelPicker": true },
                    },
                }),
            )
            .await?;
        let _ = result;
        Ok(())
    }

    async fn new_session(
        &self,
        worktree: &std::path::Path,
        mcp_servers: &[crate::McpServerLaunch],
    ) -> Result<String, BackendError> {
        let result = self
            .request(
                "session/new",
                json!({ "cwd": worktree, "mcpServers": acp_mcp_servers(mcp_servers) }),
            )
            .await
            .map_err(auth_hint)?;
        let id = result["sessionId"]
            .as_str()
            .ok_or_else(|| BackendError::Protocol("session/new result missing sessionId".into()))?
            .to_string();
        self.sessions.lock().await.insert(id.clone());
        Ok(id)
    }

    async fn load_session(
        &self,
        session_id: &str,
        worktree: &std::path::Path,
        mcp_servers: &[crate::McpServerLaunch],
    ) -> Result<(), BackendError> {
        self.request(
            "session/load",
            json!({
                "sessionId": session_id,
                "cwd": worktree,
                "mcpServers": acp_mcp_servers(mcp_servers),
            }),
        )
        .await
        .map_err(auth_hint)?;
        self.sessions.lock().await.insert(session_id.to_string());
        Ok(())
    }

    async fn knows_session(&self, session_id: &str) -> bool {
        self.sessions.lock().await.contains(session_id)
    }

    async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<Value, BackendError> {
        self.request(
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": config_id, "value": value }),
        )
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, BackendError> {
        let rx = self.request_deferred(method, params).await?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(BackendError::Protocol(format!("{method}: {e}"))),
            Err(_) => Err(BackendError::Protocol(format!(
                "{method}: cursor-agent closed before responding"
            ))),
        }
    }

    /// Send a request and return the response channel without awaiting it
    /// (session/prompt resolves only at end of turn).
    async fn request_deferred(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, BackendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        Ok(rx)
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

    async fn respond_err(&self, id: Value, code: i64, message: &str) {
        let _ = self
            .write(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": code, "message": message },
            }))
            .await;
    }

    async fn write(&self, msg: Value) -> Result<(), BackendError> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_vec(&msg).expect("serializable");
        line.push(b'\n');
        stdin.write_all(&line).await.map_err(BackendError::Io)?;
        stdin.flush().await.map_err(BackendError::Io)
    }

    async fn subscribe(&self, session_id: &str) -> mpsc::Receiver<ServerMsg> {
        let (tx, rx) = mpsc::channel(256);
        self.routes.lock().await.insert(session_id.to_string(), tx);
        rx
    }

    async fn unsubscribe(&self, session_id: &str) {
        self.routes.lock().await.remove(session_id);
    }
}

/// Surface auth failures as such (the UI offers the login flow for them).
fn auth_hint(e: BackendError) -> BackendError {
    match e {
        BackendError::Protocol(msg)
            if msg.to_lowercase().contains("auth") || msg.contains("login") =>
        {
            BackendError::Auth(msg)
        }
        other => other,
    }
}

/// User MCP servers in ACP `mcpServers` shape: stdio transport with env as
/// an array of name/value pairs.
fn acp_mcp_servers(servers: &[crate::McpServerLaunch]) -> Value {
    Value::Array(
        servers
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "command": s.command,
                    "args": s.args,
                    "env": s.env
                        .iter()
                        .map(|(name, value)| json!({ "name": name, "value": value }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_mcp_servers_shape() {
        let servers = vec![crate::McpServerLaunch {
            name: "jira".into(),
            command: "jira-mcp".into(),
            args: vec!["--stdio".into()],
            env: vec![("TOKEN".into(), "sekrit".into())],
        }];
        let value = acp_mcp_servers(&servers);
        assert_eq!(
            value,
            json!([{
                "name": "jira",
                "command": "jira-mcp",
                "args": ["--stdio"],
                "env": [{ "name": "TOKEN", "value": "sekrit" }],
            }])
        );
        assert_eq!(acp_mcp_servers(&[]), json!([]));
    }

    #[test]
    fn parses_acp_model_catalog() {
        let result = json!({ "models": [
            { "value": "default", "name": "Auto", "configOptions": [] },
            { "value": "claude-fable-5", "name": "Fable 5", "configOptions": [
                { "id": "thinking", "name": "Thinking", "description": "Thinking on/off",
                  "type": "select", "currentValue": "true",
                  "options": [ { "value": "false", "name": "Off" },
                               { "value": "true", "name": "On" } ] },
                { "id": "context", "name": "Context", "description": "Context size",
                  "type": "select", "currentValue": "300k",
                  "options": [ { "value": "300k", "name": "300K" },
                               { "value": "1m", "name": "1M" } ] },
                { "id": "effort", "name": "Effort", "description": "Effort level",
                  "type": "select", "currentValue": "high",
                  "options": [ { "value": "low", "name": "Low" },
                               { "value": "high", "name": "High" },
                               { "value": "max", "name": "Max" } ] },
            ]},
            { "value": "composer-2.5", "name": "Composer 2.5", "configOptions": [
                { "id": "fast", "name": "Fast", "description": "Faster",
                  "type": "select", "currentValue": "true",
                  "options": [ { "value": "false", "name": "Off" },
                               { "value": "true", "name": "Fast" } ] },
            ]},
        ]});
        let models = parse_acp_models("cursor", &result);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "cursor/default",
                "cursor/claude-fable-5",
                "cursor/composer-2.5"
            ]
        );

        let fable = &models[1];
        assert_eq!(fable.display_name, "Fable 5");
        // The default context choice (300k) is the advertised window.
        assert_eq!(fable.context_window, 300_000);
        assert_eq!(
            fable
                .options_schema
                .pointer("/properties/context/enum")
                .unwrap(),
            &json!(["300k", "1m"])
        );
        assert_eq!(
            fable
                .options_schema
                .pointer("/properties/effort/default")
                .and_then(Value::as_str),
            Some("high")
        );
        // Binary options become booleans (rendered as toggles).
        assert_eq!(
            fable
                .options_schema
                .pointer("/properties/thinking/type")
                .and_then(Value::as_str),
            Some("boolean")
        );
        assert_eq!(
            fable.options_schema.pointer("/properties/thinking/default"),
            Some(&json!(true))
        );

        let composer = &models[2];
        assert_eq!(composer.context_window, 200_000); // no context option
        assert_eq!(
            composer.options_schema.pointer("/properties/fast/default"),
            Some(&json!(true))
        );
    }

    #[test]
    fn parses_context_sizes() {
        assert_eq!(parse_context_size("300k"), Some(300_000));
        assert_eq!(parse_context_size("1m"), Some(1_000_000));
        assert_eq!(parse_context_size("272K"), Some(272_000));
        assert_eq!(parse_context_size("full"), None);
    }

    #[test]
    fn splits_legacy_variant_ids() {
        assert_eq!(
            split_variant("claude-opus-4-8-high-fast"),
            ("claude-opus-4-8", Some("high"), true)
        );
        assert_eq!(
            split_variant("claude-fable-5"),
            ("claude-fable-5", None, false)
        );
        assert_eq!(
            split_variant("gpt-5.3-codex"),
            ("gpt-5.3-codex", None, false)
        );
    }

    #[test]
    fn maps_updates_to_events() {
        let text = json!({ "sessionUpdate": "agent_message_chunk",
                           "content": { "type": "text", "text": "hi" } });
        assert!(matches!(
            map_update(&text).as_slice(),
            [BackendEvent::TextDelta(t)] if t == "hi"
        ));

        let thought = json!({ "sessionUpdate": "agent_thought_chunk",
                              "content": { "type": "text", "text": "hmm" } });
        assert!(matches!(
            map_update(&thought).as_slice(),
            [BackendEvent::ThinkingDelta(t)] if t == "hmm"
        ));

        let call = json!({ "sessionUpdate": "tool_call", "toolCallId": "t1",
                           "title": "`ls`", "kind": "execute", "status": "pending",
                           "rawInput": { "command": "ls" } });
        match map_update(&call).as_slice() {
            [
                BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    args,
                },
            ] => {
                assert_eq!(call_id, "t1");
                assert_eq!(tool, "execute");
                assert_eq!(args["command"], "ls");
                assert_eq!(args["title"], "`ls`");
            }
            other => panic!("unexpected: {other:?}"),
        }

        let done = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                           "status": "completed",
                           "rawOutput": { "exitCode": 0, "stdout": "a\n" } });
        assert!(matches!(
            map_update(&done).as_slice(),
            [BackendEvent::ToolCompleted { call_id, ok: true, .. }] if call_id == "t1"
        ));

        let progress = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
                               "status": "in_progress" });
        assert!(map_update(&progress).is_empty());

        let title = json!({ "sessionUpdate": "session_info_update", "title": "T" });
        assert!(map_update(&title).is_empty());
    }

    #[test]
    fn reads_config_snapshot_values() {
        let result = json!({ "configOptions": [
            { "id": "mode", "currentValue": "agent" },
            { "id": "model", "currentValue": "composer-2.5" },
        ]});
        assert_eq!(
            config_snapshot_value(&result, "model").as_deref(),
            Some("composer-2.5")
        );
        assert_eq!(config_snapshot_value(&result, "context"), None);
        assert_eq!(config_snapshot_value(&json!({}), "model"), None);
    }
}
