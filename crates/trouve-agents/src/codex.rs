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
//!   `item/commandExecution/outputDelta`, `turn/plan/updated`,
//!   `thread/tokenUsage/updated`, `turn/completed`
//! - server-initiated approval requests:
//!   `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`
//!   answered with `{ decision: "accept" | "decline" }`

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, mpsc, oneshot};
use trouve_protocol::{ModelInfo, TodoItem, TodoStatus, Usage};
use trouve_providers::codex::completed_raw_reasoning_text;
use trouve_providers::models_dev::{ModelsDevCatalog, OptionsDialect};

use crate::process_env::{ProcessTreeChild, spawn_process_tree};
use crate::{
    AgentBackend, BackendCollaboratorAccess, BackendCollaboratorEvent, BackendError, BackendEvent,
    BackendEventStream, BackendLogin, BackendPermission, BackendStartupActivity, BackendStatus,
    BackendSteer, BackendTurn, async_stream, binary_on_path, format_reset,
    route::{ROUTE_EVENT_BUDGET, RouteReceiver, RouteSendError, RouteSender, route_channel},
    spawn_codex_login,
};

#[cfg(not(test))]
const COLLABORATOR_START_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(test)]
const COLLABORATOR_START_GRACE: std::time::Duration = std::time::Duration::from_millis(25);

/// A shared credential operation is normally only a read or atomic rename.
/// If an interactive login owns the lock, callers yield instead of occupying
/// Tokio's blocking pool until the user finishes the browser flow.
const AUTH_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

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

pub struct CodexBackend {
    id: String,
    command: String,
    server: Arc<Mutex<Option<Arc<AppServer>>>>,
    catalog: Arc<ModelsDevCatalog>,
}

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
        // Even Yolo asks Codex to emit native approval callbacks. Trouve's
        // permission gate auto-approves them, but still validates the target
        // and acquires the session mutation lane before replying. `never`
        // would let vendor-native writes bypass both boundaries.
        BackendPermission::Yolo => ("untrusted", "danger-full-access", "dangerFullAccess"),
    }
}

fn sandbox_policy(permission: BackendPermission, sandbox_policy_type: &str) -> Value {
    if matches!(permission, BackendPermission::ReadOnly) {
        // A read-only native shell must not exfiltrate repository data or
        // mutate remote services outside Trouve's approval/audit gate. The
        // app-server MCP transport is outside this command sandbox.
        json!({ "type": sandbox_policy_type, "networkAccess": false })
    } else {
        json!({ "type": sandbox_policy_type })
    }
}

impl CodexBackend {
    pub fn new(id: impl Into<String>, command: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "codex".into()),
            server: Arc::new(Mutex::new(None)),
            catalog: Arc::new(ModelsDevCatalog::embedded()),
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelsDevCatalog>) -> Self {
        self.catalog = catalog;
        self
    }

    fn without_usage_pricing(mut models: Vec<ModelInfo>) -> Vec<ModelInfo> {
        for model in &mut models {
            // Codex runs against the user's ChatGPT subscription rather than
            // OpenAI's per-token API billing.
            model.input_price_per_mtok = None;
            model.output_price_per_mtok = None;
        }
        models
    }

    async fn server(&self) -> Result<Arc<AppServer>, BackendError> {
        self.server_with_cancel(None).await
    }

    async fn server_cancellable(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Arc<AppServer>, BackendError> {
        self.server_with_cancel(Some(cancel)).await
    }

    async fn server_with_cancel(
        &self,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<Arc<AppServer>, BackendError> {
        let mut guard = match cancel {
            Some(cancel) => tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(BackendError::Cancelled),
                guard = self.server.lock() => guard,
            },
            None => self.server.lock().await,
        };
        if let Some(cached) = guard.as_ref() {
            if !cached.is_closed() {
                if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                    return Err(BackendError::Cancelled);
                }
                return Ok(cached.clone());
            }

            // EOF wakes routed streams before the reader's blocking process
            // reap necessarily completes. Keep the closed entry installed
            // while awaiting its idempotent cleanup: concurrent callers stay
            // behind this spawn lock, and aborting this waiter leaves the
            // stale entry for the next caller to finish instead of exposing an
            // empty cache that could spawn an overlapping replacement.
            cached.terminate_transport().await?;
            guard.take();
        }
        if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Err(BackendError::Cancelled);
        }
        let spawn_started = std::time::Instant::now();
        let s = Arc::new(AppServer::spawn(&self.command).await?);
        tracing::info!(
            elapsed_ms = spawn_started.elapsed().as_millis(),
            "codex startup timing: app-server process spawned"
        );
        let handshake_started = std::time::Instant::now();
        let handshake = match cancel {
            Some(cancel) => tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(BackendError::Cancelled),
                result = s.handshake() => result,
            },
            None => s.handshake().await,
        };
        if let Err(error) = handshake {
            // A well-formed initialize error leaves the process and both
            // transport tasks alive. Cancellation can also drop the handshake
            // future after initialize was flushed. Reap the complete tree
            // before releasing the spawn lock so a retry cannot overlap it.
            if let Err(cleanup_error) = s.terminate_transport().await {
                let message =
                    format!("{error}; app-server cleanup was not acknowledged: {cleanup_error}");
                *guard = Some(s);
                return Err(BackendError::Protocol(message));
            }
            return Err(error);
        }
        tracing::info!(
            elapsed_ms = handshake_started.elapsed().as_millis(),
            "codex startup timing: app-server handshake completed"
        );
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
        // Codex is a distinct serving surface: its static trouve-owned
        // provider inherits shared metadata from models.dev and owns the
        // OAuth roster, context limits, defaults, and reasoning levels.
        Self::without_usage_pricing(self.catalog.provider_models(
            "openai-codex",
            &self.id,
            OptionsDialect::CodexCli,
        ))
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

    fn supports_steering(&self) -> bool {
        true
    }

    async fn startup_activity(&self, turn: &BackendTurn) -> Option<BackendStartupActivity> {
        let mcp_config = thread_mcp_config(&codex_config_override(turn));
        if mcp_config.is_null() {
            return None;
        }
        let cached = self.server.lock().await.clone();
        let needs_load = match (cached.as_ref(), turn.session.as_deref()) {
            (Some(server), Some(thread_id)) if !server.is_closed() => {
                !server
                    .thread_settings_match(thread_id, &mcp_config, turn.instructions.as_deref())
                    .await
            }
            _ => true,
        };
        (needs_load && !mcp_config.is_null()).then_some(BackendStartupActivity::ConnectingTools)
    }

    async fn steer_turn(&self, steer: BackendSteer) -> Result<(), BackendError> {
        let cancel = steer.cancel.clone();
        let server = self.server_cancellable(&cancel).await?;
        let mut input = Vec::with_capacity(1 + steer.attachments.len());
        if !steer.prompt.is_empty() {
            input.push(json!({ "type": "text", "text": steer.prompt }));
        }
        for attachment in steer.attachments {
            let path = attachment.local_path.ok_or_else(|| {
                BackendError::Protocol(format!(
                    "attachment {} has no verified worktree-local image path",
                    attachment.name
                ))
            })?;
            input.push(json!({ "type": "localImage", "path": path }));
        }
        server.steer_turn(&steer.session, input, &cancel).await
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        // The isolated app-server and Trouve's own login flow both publish to
        // the user's shared auth.json. Serialize the whole login so an
        // app-server refresh cannot race the vendor CLI's final write.
        let auth_lock = match codex_auth_path() {
            Some(source) => Some(acquire_auth_lock(source).await?),
            None => None,
        };
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
                // Release before evicting the server: AppServer::drop syncs
                // refreshed credentials through this same lock.
                drop(auth_lock);
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
        let trouve_thread_id = turn.thread_id.clone();
        let cancel = turn.cancel.clone();
        let mut server = self.server_cancellable(&cancel).await?;

        // Effort comes from the thread's model options; `@effort` model ids
        // from before the options split still resolve.
        let (model_name, id_effort) = split_effort(&turn.model);
        let effort = turn
            .model_options
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or(id_effort);
        let (approval_policy, sandbox, sandbox_policy_type) = permission_settings(turn.permission);
        let sandbox_policy = sandbox_policy(turn.permission, sandbox_policy_type);

        // Per-thread config overrides: request raw reasoning from models that
        // expose it and mount trouve/user MCP servers. Both thread/start and
        // thread/resume accept `config`, and resumed threads re-spawn their
        // MCP servers from it. Developer instructions also travel on both
        // requests so a resumed thread retains its current mode and, most
        // importantly, the ToolExecutor bridge guidance after an app restart
        // or vendor-side compaction.
        let supported_features = if turn.mcp_bridge.is_some() {
            server.supported_features().await
        } else {
            HashSet::new()
        };
        let config_override = codex_config_override_with_features(&turn, &supported_features);
        let mcp_config = thread_mcp_config(&config_override);
        let with_thread_settings = |mut params: Value| {
            params["config"] = config_override.clone();
            if let Some(instructions) = &turn.instructions {
                params["developerInstructions"] = json!(instructions);
            }
            params
        };

        // Serialize the entire persisted-thread replacement boundary. Cleanup
        // may have published the previous terminal event while its vendor
        // unsubscribe is still in flight; do not inspect cached load state or
        // resume that thread until cleanup has made the outcome unambiguous.
        let persisted_lifecycle_server = Arc::clone(&server);
        let persisted_lifecycle = match turn.session.as_deref() {
            Some(thread_id) => {
                let lifecycle = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(BackendError::Cancelled),
                    lifecycle = server.lock_turn_lifecycle(thread_id) => Ok(lifecycle),
                }?;
                Some(lifecycle)
            }
            None => None,
        };

        // Start or resume the vendor-side thread.
        let mut start_params = with_thread_settings(json!({
            "cwd": turn.worktree,
            "approvalPolicy": approval_policy,
            "sandbox": sandbox,
            "serviceName": "trouve",
            "developerInstructions": turn.instructions,
        }));
        if !model_name.is_empty() {
            start_params["model"] = json!(model_name);
        }
        let needs_load = match turn.session.as_deref() {
            Some(thread_id) => {
                !server
                    .thread_settings_match(thread_id, &mcp_config, turn.instructions.as_deref())
                    .await
            }
            None => true,
        };
        let mut fresh_session = false;
        let thread_request_started = std::time::Instant::now();
        let codex_thread_id = match (&turn.session, needs_load) {
            (Some(sid), false) => sid.clone(),
            (Some(sid), true) => {
                let resumed = server
                    .request_effect_cancellable(
                        "thread/resume",
                        with_thread_settings(json!({ "threadId": sid })),
                        &cancel,
                    )
                    .await;
                match resumed {
                    Ok(v) => {
                        server
                            .validated_thread_id("thread/resume", &v, Some(sid))
                            .await?
                    }
                    Err(BackendError::Cancelled) => return Err(BackendError::Cancelled),
                    Err(e) => {
                        tracing::warn!("codex thread/resume failed ({e}); starting fresh");
                        if server.is_closed() {
                            server = self.server_cancellable(&cancel).await?;
                        }
                        fresh_session = true;
                        let v = server
                            .request_effect_cancellable(
                                "thread/start",
                                start_params.clone(),
                                &cancel,
                            )
                            .await?;
                        server.validated_thread_id("thread/start", &v, None).await?
                    }
                }
            }
            (None, _) => {
                fresh_session = true;
                let v = server
                    .request_effect_cancellable("thread/start", start_params.clone(), &cancel)
                    .await?;
                server.validated_thread_id("thread/start", &v, None).await?
            }
        };
        if needs_load {
            server
                .mark_thread_loaded(&codex_thread_id, mcp_config, turn.instructions.clone())
                .await;
        }
        tracing::info!(
            thread_id = %trouve_thread_id,
            fresh_session,
            loaded_thread = needs_load,
            elapsed_ms = thread_request_started.elapsed().as_millis(),
            "codex startup timing: thread ready"
        );

        // A failed resume may replace the app-server or return a fresh vendor
        // thread. Transfer the setup boundary to that thread without trying
        // to reacquire the same non-reentrant lifecycle lock.
        let lifecycle = if persisted_lifecycle.as_ref().is_some_and(|lifecycle| {
            lifecycle.thread_id == codex_thread_id
                && Arc::ptr_eq(&server, &persisted_lifecycle_server)
        }) {
            persisted_lifecycle.expect("matching persisted lifecycle is present")
        } else {
            let replacement_lifecycle = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(BackendError::Cancelled),
                lifecycle = server.lock_turn_lifecycle(&codex_thread_id) => Ok(lifecycle),
            };
            let replacement_lifecycle = match replacement_lifecycle {
                Ok(lifecycle) => lifecycle,
                Err(error) => {
                    return session_setup_failure(fresh_session, &codex_thread_id, error);
                }
            };
            drop(persisted_lifecycle);
            replacement_lifecycle
        };
        // A cancelled trouve stream may still have a live vendor turn if the
        // app-server was blocked in a model or tool request when its consumer
        // disappeared. Await its interruption before starting a replacement;
        // otherwise Codex folds the new prompt into the old turn and its late
        // completion is misattributed to the replacement.
        if let Err(error) = server.interrupt_active_turn(&codex_thread_id).await {
            return session_setup_failure(fresh_session, &codex_thread_id, error);
        }
        if cancel.is_cancelled() {
            return session_setup_failure(fresh_session, &codex_thread_id, BackendError::Cancelled);
        }
        let route = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BackendError::Cancelled),
            route = server.subscribe(&codex_thread_id) => route,
        };
        let route = match route {
            Ok(route) => route,
            Err(error) => {
                return session_setup_failure(fresh_session, &codex_thread_id, error);
            }
        };

        // A cold `thread/resume` accepts developerInstructions but some Codex
        // versions do not expose a changed value to the first resumed model
        // request. Preserve the clean developer-instruction path normally,
        // while carrying one user-input fallback after each app-server
        // restart (or instruction change). Once the server has successfully
        // started a turn with this instruction set, subsequent prompts stay
        // untouched.
        let instructions_need_fallback = !fresh_session
            && turn.instructions.as_ref().is_some_and(|instructions| {
                server.instructions_need_prompt_fallback(&codex_thread_id, instructions)
            });
        let text = match (&turn.instructions, instructions_need_fallback) {
            (Some(instructions), true) => format!(
                "<mode-instructions>\n{instructions}\n</mode-instructions>\n\n{}",
                turn.prompt
            ),
            _ => turn.prompt.clone(),
        };

        // Images ride as localImage items (app-server reads the file
        // itself); the engine already turned non-image uploads into path
        // references inside the prompt text.
        let mut input = vec![json!({ "type": "text", "text": text })];
        for att in &turn.attachments {
            let path = att.local_path.as_ref().ok_or_else(|| {
                BackendError::Protocol(format!(
                    "attachment {} has no verified worktree-local image path",
                    att.name
                ))
            })?;
            input.push(json!({ "type": "localImage", "path": path }));
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
        let turn_request_started = std::time::Instant::now();
        let (codex_turn_id, cleanup) = match server
            .start_turn(&codex_thread_id, &route.tx, turn_params, lifecycle, &cancel)
            .await
        {
            Ok(started) => started,
            Err(error) => {
                return session_setup_failure(fresh_session, &codex_thread_id, error);
            }
        };
        if let Some(instructions) = &turn.instructions {
            server.remember_thread_instructions(&codex_thread_id, instructions);
        }
        tracing::info!(
            thread_id = %trouve_thread_id,
            codex_thread_id = %codex_thread_id,
            elapsed_ms = turn_request_started.elapsed().as_millis(),
            "codex startup timing: turn/start accepted"
        );

        let stream = turn_stream(
            server.clone(),
            codex_thread_id.clone(),
            codex_turn_id,
            route,
            fresh_session,
            cleanup,
            cancel,
        );
        Ok(stream.boxed())
    }
}

/// Codex config overrides enabling raw reasoning when available.
fn codex_config_override(turn: &crate::BackendTurn) -> Value {
    codex_config_override_with_features(turn, &HashSet::new())
}

fn codex_config_override_with_features(
    turn: &crate::BackendTurn,
    supported_features: &HashSet<String>,
) -> Value {
    let mut config = json!({ "show_raw_agent_reasoning": true });
    if let Some(mcp_config) = mcp_config_override(turn, supported_features)
        && let (Some(config), Some(mcp)) = (config.as_object_mut(), mcp_config.as_object())
    {
        config.extend(mcp.clone());
    }
    config
}

/// Per-thread MCP mounts in Codex's config.toml shape. When the Trouve bridge
/// is present, overlapping ambient product capabilities stand down while
/// Codex's optimized shell, patch, image, and web tools remain enabled.
fn mcp_config_override(
    turn: &crate::BackendTurn,
    supported_features: &HashSet<String>,
) -> Option<Value> {
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
        let http_headers: serde_json::Map<String, Value> = bridge
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect();
        // Streamable-HTTP server (`url` instead of `command` selects the
        // transport in codex's mcp_servers config shape).
        servers.insert(
            "trouve".into(),
            json!({
                "url": bridge.url,
                "http_headers": http_headers,
                // ToolExecutor already classifies and gates every call. Do
                // not wrap it in Codex's separate MCP approval heuristic.
                "default_tools_approval_mode": "approve",
            }),
        );
    }
    if servers.is_empty() {
        return None;
    }
    let mut config = json!({ "mcp_servers": servers });
    if turn.mcp_bridge.is_some() {
        let features: serde_json::Map<String, Value> = PRODUCT_SURFACE_FEATURES
            .iter()
            .filter(|name| supported_features.contains(**name))
            .map(|name| ((*name).to_string(), Value::Bool(false)))
            .collect();
        if !features.is_empty() {
            config["features"] = Value::Object(features);
        }
    }
    Some(config)
}

fn thread_mcp_config(config: &Value) -> Value {
    config.get("mcp_servers").cloned().unwrap_or(Value::Null)
}

fn loaded_thread_settings_match(
    loaded_threads: &HashMap<String, LoadedThreadSettings>,
    thread_id: &str,
    mcp_config: &Value,
    developer_instructions: Option<&str>,
) -> bool {
    loaded_threads.get(thread_id).is_some_and(|loaded| {
        loaded.mcp_config == *mcp_config
            && loaded.developer_instructions.as_deref() == developer_instructions
    })
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

/// Commentary messages drive trouve's progress blocks. Disable reasoning
/// summaries so their heading-like text is not generated alongside the
/// richer commentary stream.
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
        Some(BackendEvent::ProgressDelta(delta.into()))
    } else {
        Some(BackendEvent::TextDelta(delta.into()))
    }
}

#[derive(Default)]
struct CollaboratorStreamState {
    usage: Usage,
    user_messages: HashSet<String>,
    commentary_messages: HashSet<String>,
    streamed_raw_reasoning: HashSet<String>,
}

/// Tracks every provider-native collaborator announced anywhere below the
/// root turn. Codex can announce grandchildren on the root event route; the
/// root completion is therefore only a terminal *candidate* until every
/// announced descendant has emitted its own terminal turn event.
#[derive(Default)]
struct CollaboratorLifecycle {
    active: HashSet<String>,
    terminal: HashSet<String>,
    /// Announcements precede `turn/started`, but a provider can also announce
    /// a child that never starts. Keep the root route alive for a bounded
    /// handoff window instead of either closing immediately or hanging
    /// forever.
    pending_start: HashMap<String, tokio::time::Instant>,
}

impl CollaboratorLifecycle {
    fn announce(&mut self, session_id: &str) {
        self.terminal.remove(session_id);
        self.pending_start.insert(
            session_id.to_string(),
            tokio::time::Instant::now() + COLLABORATOR_START_GRACE,
        );
    }

    fn observe(&mut self, session_id: &str, event: &BackendCollaboratorEvent) {
        match event {
            BackendCollaboratorEvent::TurnStarted => {
                self.pending_start.remove(session_id);
                self.terminal.remove(session_id);
                self.active.insert(session_id.to_string());
            }
            BackendCollaboratorEvent::Completed { .. }
            | BackendCollaboratorEvent::Failed { .. } => {
                self.pending_start.remove(session_id);
                self.active.remove(session_id);
                self.terminal.insert(session_id.to_string());
            }
            _ => {
                if !self.terminal.contains(session_id) {
                    self.pending_start.remove(session_id);
                    self.active.insert(session_id.to_string());
                }
            }
        }
    }

    fn root_can_finish(&mut self) -> bool {
        let now = tokio::time::Instant::now();
        self.pending_start.retain(|_, deadline| *deadline > now);
        self.active.is_empty() && self.pending_start.is_empty()
    }

    fn next_start_deadline(&self) -> Option<tokio::time::Instant> {
        self.pending_start.values().copied().min()
    }
}

enum CodexRouteInput {
    Message(Option<ServerMsg>),
    PromptLookup(Option<CollaboratorPromptLookup>),
    StartGraceElapsed,
}

struct CollaboratorPromptLookup {
    session_id: String,
    generation: u64,
    prompt: Option<String>,
}

fn root_route_can_finish(
    lifecycle: &mut CollaboratorLifecycle,
    prompt_lookups: &HashMap<String, u64>,
) -> bool {
    prompt_lookups.is_empty() && lifecycle.root_can_finish()
}

fn add_usage(total: &mut Usage, current: &Usage) {
    total.input_tokens += current.input_tokens;
    total.cached_input_tokens += current.cached_input_tokens;
    total.output_tokens += current.output_tokens;
    total.context_input_tokens = current.context_input_tokens;
    if let Some(cost) = current.cost_usd {
        total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + cost);
    }
    if let Some(window) = current.context_window {
        total.context_window = Some(window);
    }
}

fn collaborator_user_message(item: &Value) -> Option<String> {
    let text = item["content"]
        .as_array()?
        .iter()
        // `thread/turns/list` currently returns the initial prompt with the
        // Responses-style `input_text` discriminator, while live app-server
        // notifications and older releases have used `text` or `inputText`.
        // Treat all three as equivalent textual user content so a spawned
        // collaborator always receives a durable Prompt node in its child
        // thread projection.
        .filter(|content| {
            matches!(
                content["type"].as_str(),
                Some("text") | Some("input_text") | Some("inputText")
            )
        })
        .filter_map(|content| content["text"].as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// Recover the prompt that opened a collaborator's latest turn. Codex's
/// `subAgentActivity` announcement identifies the child thread but currently
/// omits the spawn prompt, and the child route does not replay its initial
/// `userMessage` item. A one-turn full listing is therefore the authoritative
/// fallback without loading the collaborator's complete transcript.
fn collaborator_prompt_from_turn_page(response: &Value) -> Option<String> {
    response["data"].as_array()?.iter().find_map(|turn| {
        turn["items"].as_array()?.iter().find_map(|item| {
            (item["type"].as_str() == Some("userMessage"))
                .then(|| collaborator_user_message(item))
                .flatten()
        })
    })
}

/// Translate one child-thread notification without allowing it to mutate the
/// root turn's parser state or terminal lifecycle.
fn collaborator_notification(
    method: &str,
    params: &Value,
    state: &mut CollaboratorStreamState,
) -> Vec<BackendCollaboratorEvent> {
    let mut events = Vec::new();
    match method {
        "turn/started" => {
            *state = CollaboratorStreamState::default();
            events.push(BackendCollaboratorEvent::TurnStarted);
        }
        "item/agentMessage/delta" => {
            let Some(delta) = params["delta"].as_str() else {
                return events;
            };
            if params["itemId"]
                .as_str()
                .is_some_and(|id| state.commentary_messages.contains(id))
            {
                events.push(BackendCollaboratorEvent::ProgressDelta(delta.into()));
            } else {
                events.push(BackendCollaboratorEvent::TextDelta(delta.into()));
            }
        }
        "turn/plan/updated" => {
            if let Some(todos) = codex_plan_todos(params) {
                events.push(BackendCollaboratorEvent::TodosUpdated { todos });
            }
        }
        "item/reasoning/textDelta" => {
            if let Some(delta) = params["delta"].as_str() {
                if let Some(id) = params["itemId"].as_str() {
                    state.streamed_raw_reasoning.insert(id.to_string());
                }
                events.push(BackendCollaboratorEvent::ThinkingDelta(delta.into()));
            }
        }
        "item/started" => {
            let item = &params["item"];
            let kind = item["type"].as_str().unwrap_or("");
            if kind == "agentMessage"
                && item["phase"].as_str() == Some("commentary")
                && let Some(id) = item["id"].as_str()
            {
                state.commentary_messages.insert(id.to_string());
            }
            if kind == "userMessage"
                && let Some(id) = item["id"].as_str()
                && state.user_messages.insert(id.to_string())
                && let Some(content) = collaborator_user_message(item)
            {
                events.push(BackendCollaboratorEvent::UserMessage(content));
            }
            if kind == "contextCompaction" {
                events.push(BackendCollaboratorEvent::CompactionStarted);
            } else if !matches!(
                kind,
                "" | "agentMessage" | "userMessage" | "plan" | "reasoning"
            ) {
                events.push(BackendCollaboratorEvent::ToolStarted {
                    call_id: item["id"].as_str().unwrap_or("").into(),
                    tool: kind.into(),
                    args: item.clone(),
                });
            }
        }
        "item/commandExecution/outputDelta" => {
            if let (Some(call_id), Some(chunk)) =
                (params["itemId"].as_str(), params["delta"].as_str())
            {
                events.push(BackendCollaboratorEvent::ToolOutput {
                    call_id: call_id.into(),
                    chunk: chunk.into(),
                });
            }
        }
        "item/completed" => {
            let item = &params["item"];
            let kind = item["type"].as_str().unwrap_or("");
            if kind == "userMessage"
                && let Some(id) = item["id"].as_str()
                && state.user_messages.insert(id.to_string())
                && let Some(content) = collaborator_user_message(item)
            {
                events.push(BackendCollaboratorEvent::UserMessage(content));
            }
            let raw_reasoning_streamed = kind == "reasoning"
                && item["id"]
                    .as_str()
                    .is_some_and(|id| state.streamed_raw_reasoning.remove(id));
            let mut thinking_emitted = raw_reasoning_streamed;
            if kind == "reasoning"
                && !raw_reasoning_streamed
                && let Some(text) = completed_raw_reasoning_text(item)
            {
                thinking_emitted = true;
                events.push(BackendCollaboratorEvent::ThinkingDelta(text));
            }
            let commentary_completed = kind == "agentMessage"
                && item["id"]
                    .as_str()
                    .is_some_and(|id| state.commentary_messages.remove(id));
            if thinking_emitted {
                events.push(BackendCollaboratorEvent::ThinkingCompleted);
            }
            if commentary_completed {
                events.push(BackendCollaboratorEvent::ProgressCompleted);
            }
            if kind == "contextCompaction" {
                events.push(if item["status"].as_str() == Some("failed") {
                    BackendCollaboratorEvent::CompactionFailed
                } else {
                    BackendCollaboratorEvent::CompactionCompleted
                });
            } else if !matches!(
                kind,
                "" | "agentMessage" | "userMessage" | "plan" | "reasoning"
            ) {
                events.push(BackendCollaboratorEvent::ToolCompleted {
                    call_id: item["id"].as_str().unwrap_or("").into(),
                    ok: item["status"].as_str() != Some("failed"),
                    result: item.clone(),
                });
            }
        }
        "thread/tokenUsage/updated" => {
            let usage = parse_usage(params);
            add_usage(&mut state.usage, &usage);
            events.push(BackendCollaboratorEvent::UsageUpdated { usage });
        }
        "turn/completed" => {
            match params["turn"]["status"].as_str() {
                Some("completed") => events.push(BackendCollaboratorEvent::Completed {
                    usage: state.usage.clone(),
                }),
                Some("failed") => events.push(BackendCollaboratorEvent::Failed {
                    error: params["turn"]["error"]["message"]
                        .as_str()
                        .unwrap_or("collaborator turn failed")
                        .to_string(),
                }),
                Some("interrupted") => events.push(BackendCollaboratorEvent::Failed {
                    error: "turn cancelled".into(),
                }),
                Some(status) => events.push(BackendCollaboratorEvent::Failed {
                    error: format!("turn completed with unknown status '{status}'"),
                }),
                None => events.push(BackendCollaboratorEvent::Failed {
                    error: "turn/completed omitted its terminal status".into(),
                }),
            }
            *state = CollaboratorStreamState::default();
        }
        _ => {}
    }
    events
}

/// Translate Codex's authoritative plan replacement into trouve's canonical
/// todo snapshot. App-server plan steps do not carry ids, so use their content
/// plus duplicate occurrence as a deterministic identity that survives plan
/// reordering and normal status-only updates.
fn codex_plan_todos(params: &Value) -> Option<Vec<TodoItem>> {
    let mut occurrences = HashMap::<String, usize>::new();
    params
        .get("plan")?
        .as_array()?
        .iter()
        .map(|step| {
            let content = step.get("step")?.as_str()?.to_string();
            let occurrence = occurrences.entry(content.clone()).or_default();
            *occurrence += 1;
            let status = match step.get("status")?.as_str()? {
                "pending" => TodoStatus::Pending,
                "inProgress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => return None,
            };
            Some(TodoItem {
                id: format!("codex-plan:{}:{content}:{occurrence}", content.len()),
                content,
                status,
            })
        })
        .collect()
}

fn thread_id_of(result: &Value, method: &str) -> Result<String, BackendError> {
    result["thread"]["id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| BackendError::Protocol(format!("{method} result missing thread.id")))
}

fn turn_id_of(result: &Value) -> Result<String, BackendError> {
    result["turn"]["id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| BackendError::Protocol("turn/start result missing turn.id".into()))
}

/// Parse the acknowledgement returned by `turn/steer`.
///
/// Current Codex app-server releases return a flat `turnId`, unlike the
/// nested `turn.id` returned by `turn/start`. Retain the nested fallback for
/// older app-server builds so updating trouve does not force a coordinated
/// vendor CLI upgrade.
fn steered_turn_id_of(result: &Value) -> Result<String, BackendError> {
    result["turnId"]
        .as_str()
        .or_else(|| result["turn"]["id"].as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| BackendError::Protocol("turn/steer result missing turnId".into()))
}

fn session_setup_failure(
    fresh_session: bool,
    session_id: &str,
    error: BackendError,
) -> Result<BackendEventStream, BackendError> {
    if !fresh_session {
        return Err(error);
    }
    Ok(futures::stream::iter(vec![
        Ok(BackendEvent::SessionStarted {
            session_id: session_id.to_string(),
        }),
        Err(error),
    ])
    .boxed())
}

/// Translate routed app-server messages into `BackendEvent`s until the turn
/// completes.
fn turn_stream(
    server: Arc<AppServer>,
    codex_thread_id: String,
    codex_turn_id: String,
    route: RouteSubscription,
    fresh_session: bool,
    cleanup: StartedTurnGuard,
    cancel: tokio_util::sync::CancellationToken,
) -> impl futures::Stream<Item = Result<BackendEvent, BackendError>> {
    async_stream(move |tx| async move {
        let mut cleanup = cleanup;
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
        // commentary is emitted through the progress stream rather than
        // displayed as thinking or appended to the final answer. Missing
        // phases retain the legacy final-answer behavior.
        let mut commentary_messages = HashSet::new();
        // Some providers only populate raw reasoning on the completed item.
        // Track streamed raw items so the completion fallback does not repeat
        // content already shown.
        let mut streamed_raw_reasoning = HashSet::new();
        let mut client_gone = false;
        let mut cancelled = false;
        let mut route_overloaded = false;
        let mut route_closed = false;
        let mut terminal_params = None;
        let mut announced_collaborators = HashSet::<(String, String)>::new();
        let mut collaborator_prompt_lookups = HashMap::<String, u64>::new();
        let mut next_prompt_lookup_generation = 0_u64;
        let mut collaborator_states = HashMap::<String, CollaboratorStreamState>::new();
        let mut collaborator_lifecycle = CollaboratorLifecycle::default();
        let mut collaborator_topology = CollaboratorTopology::default();
        let (prompt_lookup_tx, mut prompt_lookup_rx) =
            mpsc::unbounded_channel::<CollaboratorPromptLookup>();
        let mut overload_signal = rx.overload_signal();
        let mut close_signal = rx.close_signal();
        let process_route = async {
            // Give a queued terminal event one chance to win a simultaneous
            // consumer drop, then observe closure between every routed input.
            // Without this checkpoint an always-ready route can monopolize
            // the outer biased select and keep an abandoned turn alive.
            let mut processed_route_input = false;
            loop {
                if processed_route_input && tx.is_closed() {
                    client_gone = true;
                    break;
                }
                let input = if terminal_params.is_some() {
                    if let Some(deadline) = collaborator_lifecycle.next_start_deadline() {
                        tokio::select! {
                            biased;
                            prompt = prompt_lookup_rx.recv(), if !collaborator_prompt_lookups.is_empty() => {
                                CodexRouteInput::PromptLookup(prompt)
                            }
                            message = rx.recv() => CodexRouteInput::Message(message),
                            _ = tokio::time::sleep_until(deadline) => CodexRouteInput::StartGraceElapsed,
                        }
                    } else {
                        tokio::select! {
                            biased;
                            prompt = prompt_lookup_rx.recv(), if !collaborator_prompt_lookups.is_empty() => {
                                CodexRouteInput::PromptLookup(prompt)
                            }
                            message = rx.recv() => CodexRouteInput::Message(message),
                        }
                    }
                } else {
                    tokio::select! {
                        biased;
                        prompt = prompt_lookup_rx.recv(), if !collaborator_prompt_lookups.is_empty() => {
                            CodexRouteInput::PromptLookup(prompt)
                        }
                        message = rx.recv() => CodexRouteInput::Message(message),
                    }
                };
                processed_route_input = true;
                let msg = match input {
                    CodexRouteInput::Message(Some(message)) => message,
                    CodexRouteInput::Message(None) => break,
                    CodexRouteInput::PromptLookup(Some(lookup)) => {
                        if collaborator_prompt_lookups.get(&lookup.session_id)
                            != Some(&lookup.generation)
                        {
                            tracing::debug!(
                                session_id = %lookup.session_id,
                                generation = lookup.generation,
                                "codex: ignoring superseded collaborator prompt lookup"
                            );
                            continue;
                        }
                        collaborator_prompt_lookups.remove(&lookup.session_id);
                        if let Some(prompt) = lookup.prompt {
                            let _ = tx
                                .send(Ok(BackendEvent::CollaboratorEvent {
                                    session_id: lookup.session_id,
                                    turn_id: None,
                                    event: BackendCollaboratorEvent::UserMessage(prompt),
                                }))
                                .await;
                        } else {
                            tracing::debug!(
                                session_id = %lookup.session_id,
                                generation = lookup.generation,
                                "codex: collaborator prompt lookup exhausted"
                            );
                        }
                        if terminal_params.is_some()
                            && root_route_can_finish(
                                &mut collaborator_lifecycle,
                                &collaborator_prompt_lookups,
                            )
                        {
                            break;
                        }
                        continue;
                    }
                    CodexRouteInput::PromptLookup(None) => {
                        for (session_id, generation) in collaborator_prompt_lookups.drain() {
                            tracing::debug!(
                                %session_id,
                                generation,
                                "codex: collaborator prompt lookup channel closed before resolution"
                            );
                        }
                        if terminal_params.is_some()
                            && root_route_can_finish(
                                &mut collaborator_lifecycle,
                                &collaborator_prompt_lookups,
                            )
                        {
                            break;
                        }
                        continue;
                    }
                    CodexRouteInput::StartGraceElapsed => {
                        if root_route_can_finish(
                            &mut collaborator_lifecycle,
                            &collaborator_prompt_lookups,
                        ) {
                            break;
                        }
                        continue;
                    }
                };
                let root_message = message_belongs_to_thread(&msg, &codex_thread_id);
                if root_message
                    && message_turn_id(&msg).is_some_and(|turn_id| turn_id != codex_turn_id)
                {
                    tracing::warn!("codex: ignoring event for stale turn on {codex_thread_id}");
                    continue;
                }
                match msg {
                    ServerMsg::Notification { method, params } => {
                        let announcement_id =
                            params["item"]["id"].as_str().unwrap_or("").to_string();
                        for collaborator in collaborator_announcements(&params) {
                            if !collaborator_topology.admit(
                                &codex_thread_id,
                                &collaborator.parent_session_id,
                                &collaborator.session_id,
                            ) {
                                tracing::debug!(
                                    root_session_id = %codex_thread_id,
                                    parent_session_id = %collaborator.parent_session_id,
                                    session_id = %collaborator.session_id,
                                    "codex: ignoring cyclic or conflicting collaborator announcement"
                                );
                                continue;
                            }
                            if announced_collaborators
                                .insert((collaborator.session_id.clone(), announcement_id.clone()))
                            {
                                collaborator_lifecycle.announce(&collaborator.session_id);
                                let recover_prompt = collaborator.prompt.is_none()
                                    && !collaborator_prompt_lookups
                                        .contains_key(&collaborator.session_id);
                                let lookup_session_id = collaborator.session_id.clone();
                                let _ = tx
                                    .send(Ok(BackendEvent::CollaboratorStarted {
                                        session_id: collaborator.session_id,
                                        parent_session_id: collaborator.parent_session_id,
                                        name: collaborator.name,
                                        prompt: collaborator.prompt,
                                        model: collaborator.model,
                                        thinking_level: collaborator.thinking_level,
                                        access: collaborator.access,
                                    }))
                                    .await;
                                if recover_prompt {
                                    next_prompt_lookup_generation =
                                        next_prompt_lookup_generation.wrapping_add(1);
                                    let lookup_generation = next_prompt_lookup_generation;
                                    collaborator_prompt_lookups
                                        .insert(lookup_session_id.clone(), lookup_generation);
                                    let lookup_server = Arc::clone(&server);
                                    let lookup_cancel = cancel.clone();
                                    let lookup_tx = prompt_lookup_tx.clone();
                                    tokio::spawn(async move {
                                        let prompt = lookup_server
                                            .collaborator_prompt(&lookup_session_id, &lookup_cancel)
                                            .await;
                                        let _ = lookup_tx.send(CollaboratorPromptLookup {
                                            session_id: lookup_session_id,
                                            generation: lookup_generation,
                                            prompt,
                                        });
                                    });
                                }
                            }
                        }
                        if !root_message {
                            let Some(session_id) = params["threadId"].as_str() else {
                                continue;
                            };
                            let state = collaborator_states
                                .entry(session_id.to_string())
                                .or_default();
                            let turn_id = params_turn_id(&params).map(str::to_string);
                            for event in collaborator_notification(&method, &params, state) {
                                collaborator_lifecycle.observe(session_id, &event);
                                let _ = tx
                                    .send(Ok(BackendEvent::CollaboratorEvent {
                                        session_id: session_id.to_string(),
                                        turn_id: turn_id.clone(),
                                        event,
                                    }))
                                    .await;
                            }
                            if terminal_params.is_some()
                                && root_route_can_finish(
                                    &mut collaborator_lifecycle,
                                    &collaborator_prompt_lookups,
                                )
                            {
                                break;
                            }
                            continue;
                        }
                        match method.as_str() {
                            "item/agentMessage/delta" => {
                                if let Some(event) =
                                    agent_message_delta(&params, &commentary_messages)
                                {
                                    let _ = tx.send(Ok(event)).await;
                                }
                            }
                            "turn/plan/updated" => {
                                if let Some(todos) = codex_plan_todos(&params) {
                                    let _ = tx.send(Ok(BackendEvent::TodosUpdated { todos })).await;
                                } else {
                                    tracing::warn!(
                                        "codex: ignoring malformed turn/plan/updated notification"
                                    );
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
                                    let _ =
                                        tx.send(Ok(BackendEvent::ThinkingDelta(d.into()))).await;
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
                                if ty == "contextCompaction" {
                                    let _ = tx.send(Ok(BackendEvent::CompactionStarted)).await;
                                } else if !matches!(
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
                                let raw_reasoning_streamed = ty == "reasoning"
                                    && item["id"]
                                        .as_str()
                                        .is_some_and(|id| streamed_raw_reasoning.remove(id));
                                let mut thinking_emitted = raw_reasoning_streamed;
                                if ty == "reasoning"
                                    && !raw_reasoning_streamed
                                    && let Some(text) = completed_raw_reasoning_text(item)
                                {
                                    thinking_emitted = true;
                                    let _ = tx.send(Ok(BackendEvent::ThinkingDelta(text))).await;
                                }
                                let commentary_completed = ty == "agentMessage"
                                    && item["id"]
                                        .as_str()
                                        .is_some_and(|id| commentary_messages.remove(id));
                                if thinking_emitted {
                                    let _ = tx.send(Ok(BackendEvent::ThinkingCompleted)).await;
                                }
                                if commentary_completed {
                                    let _ = tx.send(Ok(BackendEvent::ProgressCompleted)).await;
                                }
                                if ty == "contextCompaction" {
                                    let event = if item["status"].as_str() == Some("failed") {
                                        BackendEvent::CompactionFailed
                                    } else {
                                        BackendEvent::CompactionCompleted
                                    };
                                    let _ = tx.send(Ok(event)).await;
                                } else if !matches!(
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
                                // One update per model call. Aggregate its billing
                                // counters for the final turn usage, but retain the
                                // newest call's authoritative context measurement.
                                // Publish the per-call value immediately so clients
                                // follow a vendor-owned compaction without waiting
                                // for the turn to end.
                                let u = parse_usage(&params);
                                usage.input_tokens += u.input_tokens;
                                usage.cached_input_tokens += u.cached_input_tokens;
                                usage.output_tokens += u.output_tokens;
                                usage.context_input_tokens = u.context_input_tokens;
                                if let Some(cost) = u.cost_usd {
                                    usage.cost_usd = Some(usage.cost_usd.unwrap_or(0.0) + cost);
                                }
                                if let Some(n) = u.context_window {
                                    usage.context_window = Some(n);
                                }
                                let _ = tx.send(Ok(BackendEvent::UsageUpdated { usage: u })).await;
                            }
                            "turn/completed" => {
                                // Root completion does not imply descendant
                                // completion. Keep this route subscribed while
                                // any direct or nested collaborator is active so
                                // capacity-delayed grandchildren can finish and
                                // publish their terminal transcript instead of
                                // being failed during core cleanup.
                                terminal_params = Some(params);
                                if root_route_can_finish(
                                    &mut collaborator_lifecycle,
                                    &collaborator_prompt_lookups,
                                ) {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
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
                            let approval = BackendCollaboratorEvent::ApprovalNeeded {
                                call_id: call_id.clone(),
                                tool: "mcpToolCall".into(),
                                args: params.clone(),
                                responder: ok_tx,
                            };
                            let event = if !root_message {
                                params["threadId"].as_str().map(|session_id| {
                                    BackendEvent::CollaboratorEvent {
                                        session_id: session_id.to_string(),
                                        turn_id: params_turn_id(&params).map(str::to_string),
                                        event: approval,
                                    }
                                })
                            } else {
                                let BackendCollaboratorEvent::ApprovalNeeded {
                                    call_id,
                                    tool,
                                    args,
                                    responder,
                                } = approval
                                else {
                                    unreachable!()
                                };
                                Some(BackendEvent::ApprovalNeeded {
                                    call_id,
                                    tool,
                                    args,
                                    responder,
                                })
                            };
                            if let Some(event) = event {
                                let _ = tx.send(Ok(event)).await;
                            }
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
                        if !root_message {
                            if let Some(session_id) = params["threadId"].as_str() {
                                let _ = tx
                                    .send(Ok(BackendEvent::CollaboratorEvent {
                                        session_id: session_id.to_string(),
                                        turn_id: params_turn_id(&params).map(str::to_string),
                                        event: BackendCollaboratorEvent::ApprovalNeeded {
                                            call_id,
                                            tool: tool.into(),
                                            args: params.clone(),
                                            responder: ok_tx,
                                        },
                                    }))
                                    .await;
                            }
                        } else {
                            let _ = tx
                                .send(Ok(BackendEvent::ApprovalNeeded {
                                    call_id,
                                    tool: tool.into(),
                                    args: params.clone(),
                                    responder: ok_tx,
                                }))
                                .await;
                        }
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
            _ = cancel.cancelled() => {
                cancelled = true;
            }
            _ = overload_signal.wait() => {
                route_overloaded = true;
            }
            _ = process_route => {}
            _ = tx.closed() => {
                client_gone = true;
            }
            _ = close_signal.wait() => {
                route_closed = true;
            }
        }
        if !cancelled
            && !client_gone
            && !route_overloaded
            && !route_closed
            && let Some(params) = terminal_params
        {
            // Publish completion only after active-turn cleanup is serialized
            // with any replacement startup.
            let _lifecycle = server.lock_turn_lifecycle(&codex_thread_id).await;
            server
                .clear_active_turn(&codex_thread_id, &codex_turn_id)
                .await;
            // Remove the route before yielding completion. A consumer is
            // allowed to drop immediately after receiving the terminal event.
            server.unsubscribe(&codex_thread_id, &route_tx).await;
            cleanup.disarm();
            // Publish the terminal result before best-effort vendor cleanup.
            // The lifecycle guard still prevents this thread from resuming
            // until an older unsubscribe can no longer race the replacement.
            match params["turn"]["status"].as_str() {
                Some("completed") => {
                    let _ = tx
                        .send(Ok(BackendEvent::Completed {
                            usage: usage.clone(),
                        }))
                        .await;
                }
                Some("failed") => {
                    let message = params["turn"]["error"]["message"]
                        .as_str()
                        .unwrap_or("turn failed")
                        .to_string();
                    let _ = tx.send(Err(BackendError::Protocol(message))).await;
                }
                Some("interrupted") => {
                    let _ = tx.send(Err(BackendError::Cancelled)).await;
                }
                Some(status) => {
                    let _ = tx
                        .send(Err(BackendError::Protocol(format!(
                            "turn completed with unknown status '{status}'"
                        ))))
                        .await;
                }
                None => {
                    let _ = tx
                        .send(Err(BackendError::Protocol(
                            "turn/completed omitted its terminal status".into(),
                        )))
                        .await;
                }
            }
            if let Err(error) = server.release_thread(&codex_thread_id).await {
                tracing::warn!(
                    thread_id = %codex_thread_id,
                    "codex: failed to unsubscribe completed app-server thread: {error}"
                );
            }
            return;
        }
        let _cleanup_lifecycle = server.lock_turn_lifecycle(&codex_thread_id).await;
        if cancelled || client_gone {
            server
                .cleanup_active_turn_best_effort(
                    &codex_thread_id,
                    &codex_turn_id,
                    if cancelled { "cancelled" } else { "abandoned" },
                )
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
        } else {
            let reason = if route_closed || server.is_closed() {
                "app-server closed before turn completed"
            } else {
                "app-server event route closed before turn completed"
            };
            let _ = tx.send(Err(BackendError::Protocol(reason.into()))).await;
            server
                .cleanup_active_turn_best_effort(&codex_thread_id, &codex_turn_id, "unroutable")
                .await;
        }
        // OAuth refreshes are rare; preserve any rotated credentials once per
        // turn instead of reading both auth files after every JSON-RPC reply.
        server.sync_auth().await;
        server.unsubscribe(&codex_thread_id, &route_tx).await;
        if let Err(error) = server.release_thread(&codex_thread_id).await {
            tracing::warn!(
                thread_id = %codex_thread_id,
                "codex: failed to unsubscribe cleaned-up app-server thread: {error}"
            );
        }
        cleanup.disarm();
    })
}

/// Extract the vendor turn identity from every documented event shape.
fn message_turn_id(message: &ServerMsg) -> Option<&str> {
    params_turn_id(message_params(message))
}

fn params_turn_id(params: &Value) -> Option<&str> {
    params["turnId"]
        .as_str()
        .or_else(|| params["turn"]["id"].as_str())
        .or_else(|| params["item"]["turnId"].as_str())
}

fn message_thread_id(message: &ServerMsg) -> Option<&str> {
    let params = message_params(message);
    params["threadId"]
        .as_str()
        .or_else(|| params["thread"]["id"].as_str())
}

fn message_belongs_to_thread(message: &ServerMsg, thread_id: &str) -> bool {
    message_thread_id(message).is_none_or(|message_thread_id| message_thread_id == thread_id)
}

fn message_params(message: &ServerMsg) -> &Value {
    match message {
        ServerMsg::Notification { params, .. } | ServerMsg::Request { params, .. } => params,
    }
}

/// Child Codex threads announced by collaboration items on a parent thread.
fn announced_child_threads(message: &ServerMsg) -> Vec<&str> {
    let item = &message_params(message)["item"];
    match item["type"].as_str() {
        Some("collabAgentToolCall") => {
            let mut children: Vec<&str> = item["receiverThreadIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if let Some(states) = item["agentsStates"].as_object() {
                children.extend(states.keys().map(String::as_str));
            }
            children.sort_unstable();
            children.dedup();
            children
        }
        Some("collabToolCall") => ["newThreadId", "receiverThreadId"]
            .into_iter()
            .filter_map(|key| item[key].as_str())
            .collect(),
        Some("subAgentActivity") => item["agentThreadId"].as_str().into_iter().collect(),
        _ => Vec::new(),
    }
}

struct CollaboratorAnnouncement {
    session_id: String,
    parent_session_id: String,
    name: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    thinking_level: Option<String>,
    access: BackendCollaboratorAccess,
}

/// The provider's collaboration stream contains both ownership announcements
/// and ordinary inter-agent activity. Activity sent from a child back to one
/// of its ancestors can name that ancestor in `agentThreadId`; treating it as
/// a fresh child creates a cycle that can keep the root turn alive forever.
/// Retain the first authenticated parent for each collaborator and reject any
/// later edge that points at the root, reparents a collaborator, or closes a
/// cycle in the known descendant graph.
#[derive(Default)]
struct CollaboratorTopology {
    parents: HashMap<String, String>,
}

impl CollaboratorTopology {
    fn admit(&mut self, root_session_id: &str, parent_session_id: &str, session_id: &str) -> bool {
        if session_id.is_empty()
            || parent_session_id.is_empty()
            || session_id == root_session_id
            || session_id == parent_session_id
        {
            return false;
        }
        if let Some(existing_parent) = self.parents.get(session_id) {
            return existing_parent == parent_session_id;
        }

        let mut ancestor = parent_session_id;
        let mut visited = HashSet::new();
        loop {
            if ancestor == session_id {
                return false;
            }
            if ancestor == root_session_id {
                break;
            }
            if !visited.insert(ancestor.to_string()) {
                return false;
            }
            let Some(parent) = self.parents.get(ancestor) else {
                break;
            };
            ancestor = parent;
        }

        self.parents
            .insert(session_id.to_string(), parent_session_id.to_string());
        true
    }
}

fn collaborator_access_label(value: &str) -> BackendCollaboratorAccess {
    let tokens = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let read_only_phrase = tokens.windows(2).any(|pair| {
        matches!(pair, [first, second] if
            (first == "read" || first == "transcript") && second == "only")
    });
    if read_only_phrase
        || tokens.iter().any(|token| {
            matches!(
                token.as_str(),
                "audit"
                    | "auditor"
                    | "auditing"
                    | "explore"
                    | "explorer"
                    | "exploration"
                    | "inspect"
                    | "inspector"
                    | "inspection"
                    | "research"
                    | "researcher"
                    | "researching"
                    | "review"
                    | "reviewer"
                    | "reviewing"
                    | "readonly"
            )
        })
    {
        BackendCollaboratorAccess::ReadOnly
    } else if tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "build"
                | "builder"
                | "code"
                | "coder"
                | "general"
                | "implement"
                | "implementer"
                | "implementation"
                | "interactive"
                | "worker"
                | "write"
                | "writer"
        )
    }) {
        BackendCollaboratorAccess::Interactive
    } else {
        BackendCollaboratorAccess::Inherit
    }
}

fn collaborator_access(item: &Value, session_id: &str) -> BackendCollaboratorAccess {
    let state = item
        .get("agentsStates")
        .and_then(Value::as_object)
        .and_then(|states| states.get(session_id));
    for source in state.into_iter().chain(std::iter::once(item)) {
        for key in ["readOnly", "read_only"] {
            if let Some(read_only) = source.get(key).and_then(Value::as_bool) {
                return if read_only {
                    BackendCollaboratorAccess::ReadOnly
                } else {
                    BackendCollaboratorAccess::Interactive
                };
            }
        }
        for key in ["access", "mode", "agentType", "agent_type", "role"] {
            let Some(label) = source.get(key).and_then(Value::as_str) else {
                continue;
            };
            let access = collaborator_access_label(label);
            if access != BackendCollaboratorAccess::Inherit {
                return access;
            }
        }
    }
    BackendCollaboratorAccess::Inherit
}

fn collaborator_name(item: &Value, session_id: &str) -> Option<String> {
    let state = item
        .get("agentsStates")
        .and_then(Value::as_object)
        .and_then(|states| states.get(session_id));
    for source in state.into_iter().chain(std::iter::once(item)) {
        for key in ["name", "agentName", "taskName", "nickname", "role"] {
            if let Some(name) = source
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                return Some(name.to_string());
            }
        }
    }
    state
        .into_iter()
        .chain(std::iter::once(item))
        .filter_map(|source| source.get("agentPath").and_then(Value::as_str))
        .filter_map(|path| path.rsplit('/').find(|segment| !segment.is_empty()))
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_string)
}

/// Rich counterpart to `announced_child_threads`, used by the backend event
/// bridge after routing has authenticated the parent/child relationship.
fn collaborator_announcements(params: &Value) -> Vec<CollaboratorAnnouncement> {
    let item = &params["item"];
    let kind = item["type"].as_str().unwrap_or("");
    if !matches!(
        kind,
        "collabAgentToolCall" | "collabToolCall" | "subAgentActivity"
    ) {
        return Vec::new();
    }
    let parent_session_id = item["senderThreadId"]
        .as_str()
        .or_else(|| params["threadId"].as_str())
        .unwrap_or("")
        .to_string();
    if parent_session_id.is_empty() {
        return Vec::new();
    }
    let mut session_ids = match kind {
        "collabAgentToolCall" => {
            let mut ids: Vec<String> = item["receiverThreadIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            if let Some(states) = item["agentsStates"].as_object() {
                ids.extend(states.keys().cloned());
            }
            ids
        }
        "collabToolCall" => ["newThreadId", "receiverThreadId"]
            .into_iter()
            .filter_map(|key| item[key].as_str().map(str::to_string))
            .collect(),
        "subAgentActivity" => item["agentThreadId"]
            .as_str()
            .map(str::to_string)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    session_ids.sort_unstable();
    session_ids.dedup();
    session_ids
        .into_iter()
        .filter(|session_id| session_id != &parent_session_id)
        .map(|session_id| {
            let access = collaborator_access(item, &session_id);
            CollaboratorAnnouncement {
                name: collaborator_name(item, &session_id),
                session_id,
                parent_session_id: parent_session_id.clone(),
                prompt: item["prompt"].as_str().map(str::to_string),
                model: item["model"].as_str().map(str::to_string),
                thinking_level: item["reasoningEffort"].as_str().map(str::to_string),
                access,
            }
        })
        .collect()
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
    // The turn's effective runtime context window rides along at the
    // tokenUsage level. Preserve it in usage even though the model catalog
    // separately advertises the serving surface's maximum.
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
    let provider_input_tokens = get(&["inputTokens", "input_tokens", "promptTokens"]);
    // Codex's own compaction trigger and clients use `last.totalTokens`, not
    // `last.inputTokens`, as the amount occupying the effective context
    // window. Older app-server builds omitted it, so retain the input-total
    // fallback for compatibility.
    let provider_context_tokens = get(&["totalTokens", "total_tokens"]);
    let cached_input_tokens = get(&[
        "cachedInputTokens",
        "cached_input_tokens",
        "cacheReadTokens",
    ]);
    Usage {
        // Codex follows the OpenAI Responses shape: inputTokens includes its
        // cached subset. Normalize to trouve's mutually exclusive billing
        // counters, while preserving Codex's total-token measurement for the
        // current context.
        input_tokens: provider_input_tokens.saturating_sub(cached_input_tokens),
        output_tokens: get(&["outputTokens", "output_tokens", "completionTokens"]),
        cached_input_tokens,
        context_input_tokens: Some(if provider_context_tokens > 0 {
            provider_context_tokens
        } else {
            provider_input_tokens
        }),
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
type SharedStdin = Arc<Mutex<ChildStdin>>;
type Routing = Arc<Mutex<RoutingState>>;
type ActiveTurns = Arc<Mutex<HashMap<String, String>>>;
type CompletedTurns = Arc<Mutex<CompletedTurnState>>;
type TurnLifecycles = Arc<std::sync::Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>;
const ROUTE_TOMBSTONE_BUDGET: usize = ROUTE_EVENT_BUDGET * 4;
const UNKNOWN_BUFFER_BUDGET: usize = 64;
const CANCELLED_START_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const INTERRUPT_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const REQUEST_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const THREAD_UNSUBSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const TRANSPORT_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const CHILD_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Default)]
struct CompletedTurnState {
    turns: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
}

impl CompletedTurnState {
    fn record(&mut self, thread_id: &str, turn_id: &str) {
        let key = (thread_id.to_string(), turn_id.to_string());
        if self.turns.insert(key.clone()) {
            self.order.push_back(key);
        }
        while self.order.len() > ROUTE_TOMBSTONE_BUDGET {
            if let Some(expired) = self.order.pop_front() {
                self.turns.remove(&expired);
            }
        }
    }

    fn contains(&self, thread_id: &str, turn_id: &str) -> bool {
        self.turns
            .contains(&(thread_id.to_string(), turn_id.to_string()))
    }
}

async fn record_completed_turn(
    completed_turns: &CompletedTurns,
    active_turns: &ActiveTurns,
    thread_id: &str,
    turn_id: &str,
) {
    // Registration takes these locks in the same order. Recording the
    // completion and clearing its exact marker is therefore atomic with a
    // late `turn/start` response trying to publish that marker.
    let mut completed = completed_turns.lock().await;
    completed.record(thread_id, turn_id);
    let mut active = active_turns.lock().await;
    if active
        .get(thread_id)
        .is_some_and(|active| active == turn_id)
    {
        active.remove(thread_id);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveTurnRegistration {
    Registered,
    Completed,
    OwnedByReplacement,
}

async fn register_active_turn_state(
    completed_turns: &CompletedTurns,
    active_turns: &ActiveTurns,
    thread_id: &str,
    turn_id: &str,
    only_if_vacant: bool,
) -> ActiveTurnRegistration {
    let completed = completed_turns.lock().await;
    if completed.contains(thread_id, turn_id) {
        return ActiveTurnRegistration::Completed;
    }
    let mut active = active_turns.lock().await;
    if only_if_vacant
        && active
            .get(thread_id)
            .is_some_and(|active| active != turn_id)
    {
        return ActiveTurnRegistration::OwnedByReplacement;
    }
    active.insert(thread_id.to_string(), turn_id.to_string());
    ActiveTurnRegistration::Registered
}

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

/// Cancellation-safe ownership of a vendor turn between `turn/start` and the
/// end of its trouve stream. Async construction can be dropped at any await;
/// the guard then serializes exact-turn interruption and route cleanup.
struct StartedTurnGuard {
    server: Arc<AppServer>,
    thread_id: String,
    turn_id: Option<String>,
    route_tx: RouteSender<ServerMsg>,
    response: Option<oneshot::Receiver<Result<Value, String>>>,
    request_id: i64,
    write_started: Arc<std::sync::atomic::AtomicBool>,
    lifecycle: Option<TurnLifecycleGuard>,
    startup_recovery: bool,
    armed: bool,
}

impl StartedTurnGuard {
    fn new(
        server: Arc<AppServer>,
        thread_id: String,
        route_tx: RouteSender<ServerMsg>,
        response: oneshot::Receiver<Result<Value, String>>,
        request_id: i64,
        write_started: Arc<std::sync::atomic::AtomicBool>,
        lifecycle: TurnLifecycleGuard,
    ) -> Self {
        Self {
            server,
            thread_id,
            turn_id: None,
            route_tx,
            response: Some(response),
            request_id,
            write_started,
            lifecycle: Some(lifecycle),
            startup_recovery: true,
            armed: true,
        }
    }

    async fn wait_for_response(&mut self) -> Result<Value, BackendError> {
        let result = self
            .response
            .as_mut()
            .expect("turn/start response installed")
            .await;
        self.response = None;
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(BackendError::Protocol(format!("turn/start: {error}"))),
            Err(_) => Err(BackendError::Protocol(
                "turn/start: app-server closed before responding".into(),
            )),
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn finish_startup(&mut self) {
        self.startup_recovery = false;
        self.lifecycle.take();
    }
}

impl Drop for StartedTurnGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let server = self.server.clone();
        let thread_id = self.thread_id.clone();
        let known_turn_id = self.turn_id.clone();
        let route_tx = self.route_tx.clone();
        let response = self.response.take();
        let request_id = self.request_id;
        let write_started = self.write_started.clone();
        let startup_recovery = self.startup_recovery;
        // Transfer startup serialization into cancellation cleanup. A
        // replacement cannot register before this late response is either
        // interrupted or the transport is definitively terminated.
        let lifecycle = self.lifecycle.take();
        let fallback_server = server.clone();
        let cleanup = async move {
            let _lifecycle = match lifecycle {
                Some(lifecycle) => lifecycle,
                None => server.lock_turn_lifecycle(&thread_id).await,
            };
            if response.is_some() && !write_started.load(Ordering::Relaxed) {
                // Cancellation while waiting for the shared stdin lock sent
                // no bytes, so there is no vendor turn to recover.
                server.pending.lock().await.remove(&request_id);
                server.unsubscribe(&thread_id, &route_tx).await;
                return;
            }
            let recovered_turn_id = match (known_turn_id, response) {
                (Some(turn_id), _) => Some(turn_id),
                (None, Some(response)) => {
                    match tokio::time::timeout(CANCELLED_START_RESPONSE_TIMEOUT, response).await {
                        Ok(Ok(Ok(started))) => match turn_id_of(&started) {
                            Ok(turn_id) => Some(turn_id),
                            Err(error) => {
                                tracing::warn!(
                                    "codex: cancelled turn/start response invalid: {error}"
                                );
                                if let Err(cleanup_error) = server.terminate_transport().await {
                                    tracing::warn!(
                                        "codex: failed to acknowledge cancelled-start cleanup: {cleanup_error}"
                                    );
                                }
                                None
                            }
                        },
                        Ok(Ok(Err(error))) => {
                            tracing::warn!("codex: cancelled turn/start failed: {error}");
                            None
                        }
                        Ok(Err(_)) => None,
                        Err(_) => {
                            tracing::warn!(
                                "codex: cancelled turn/start did not respond within {}s; closing \
                             its app-server transport",
                                CANCELLED_START_RESPONSE_TIMEOUT.as_secs()
                            );
                            if let Err(cleanup_error) = server.terminate_transport().await {
                                tracing::warn!(
                                    "codex: failed to acknowledge cancelled-start cleanup: {cleanup_error}"
                                );
                            }
                            None
                        }
                    }
                }
                (None, None) => None,
            };
            if let Some(turn_id) = recovered_turn_id {
                if startup_recovery {
                    // Only startup recovery may install a marker that is not
                    // already present. Stream cleanup must never resurrect a
                    // completed or already-interrupted turn.
                    match server
                        .register_active_turn_if_vacant(&thread_id, &turn_id)
                        .await
                    {
                        ActiveTurnRegistration::Registered => {}
                        ActiveTurnRegistration::Completed => {
                            tracing::debug!(
                                "codex: cancelled startup turn {turn_id} had already completed"
                            );
                            server.unsubscribe(&thread_id, &route_tx).await;
                            return;
                        }
                        ActiveTurnRegistration::OwnedByReplacement => {
                            tracing::warn!(
                                "codex: cancelled startup turn {turn_id} lost ownership to a replacement"
                            );
                            server.unsubscribe(&thread_id, &route_tx).await;
                            return;
                        }
                    }
                } else if !server.active_turn_is(&thread_id, &turn_id).await {
                    server.unsubscribe(&thread_id, &route_tx).await;
                    return;
                }
                match server.interrupt_turn(&thread_id, &turn_id).await {
                    Ok(()) => server.clear_active_turn(&thread_id, &turn_id).await,
                    Err(error) => {
                        tracing::warn!(
                            "codex: failed to interrupt cancelled startup turn {turn_id}: {error}"
                        );
                    }
                }
            }
            server.unsubscribe(&thread_id, &route_tx).await;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(cleanup);
            }
            Err(error) => {
                tracing::error!(
                    "codex: cannot clean up cancelled turn/start without a Tokio runtime: {error}"
                );
                fallback_server.invalidate_transport_now();
            }
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

struct ActiveRoute {
    tx: RouteSender<ServerMsg>,
    /// Subscription generation created only after the predecessor's
    /// interruption has completed under the per-thread lifecycle lock.
    generation: u64,
    /// None until turn/start returns. Messages stay buffered until the route
    /// is bound to the exact turn that is allowed to announce descendants.
    turn_id: Option<String>,
}

#[derive(Clone)]
struct RouteOwner {
    root_thread_id: String,
    root_turn_id: String,
    root_generation: u64,
    /// The exact collaborator turn claimed by this announcement. Codex can
    /// reuse a collaborator thread id, so the thread id alone is not an
    /// ownership boundary.
    child_turn_id: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ChildTurnKey {
    thread_id: String,
    turn_id: String,
}

struct BufferedMessage {
    message: ServerMsg,
    /// Root subscription generation at receipt. Unknown/pre-subscription
    /// traffic has no authenticated generation.
    root_generation: Option<u64>,
}

#[derive(Default)]
struct BufferedRoute {
    messages: Vec<BufferedMessage>,
    request_overloaded: bool,
    announcement_overloaded: bool,
    /// Child transcript arrived faster than ownership could be established.
    /// Once the parent adopts it, fail closed rather than publishing a
    /// durable collaborator thread with missing output.
    notification_overloaded: bool,
    /// Turn identities for messages lost from an inactive root route. `None`
    /// means the loss was thread-scoped and therefore applies to whichever
    /// turn activates the route.
    root_overflowed_turns: HashSet<Option<String>>,
}

#[derive(Default)]
struct RoutingState {
    next_generation: u64,
    routes: HashMap<String, ActiveRoute>,
    owners: HashMap<String, RouteOwner>,
    buffered: HashMap<String, BufferedRoute>,
    /// Unowned thread ids are bounded separately from their per-thread event
    /// queues so late traffic cannot create an unbounded number of buffers.
    unknown_buffered: HashSet<String>,
    unknown_buffer_order: VecDeque<String>,
    /// Recently retired roots. Child retirement is turn-scoped below because
    /// Codex may reuse a collaborator thread id for a later turn.
    failed: HashSet<String>,
    failed_order: VecDeque<String>,
    retired_children: HashSet<ChildTurnKey>,
    retired_child_order: VecDeque<ChildTurnKey>,
    /// A child announced but never observed with a turn id before its root
    /// retired. The next announcement must discard pre-announcement traffic
    /// before allowing a fresh child turn to bind.
    retired_unbound_children: HashSet<String>,
    retired_unbound_child_order: VecDeque<String>,
    retired_responses: Vec<Value>,
    retired_response_overloaded: bool,
}

impl RoutingState {
    fn reject_retired_request(&mut self, message: &ServerMsg) {
        if let Some(response) = Self::decline_response(message) {
            self.push_retired_response(response);
        }
    }

    fn decline_response(message: &ServerMsg) -> Option<Value> {
        let ServerMsg::Request { id, method, .. } = message else {
            return None;
        };
        let result = if method == "mcpServer/elicitation/request" {
            json!({ "action": "decline", "content": {} })
        } else {
            json!({ "decision": "decline" })
        };
        Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
    }

    fn push_retired_response(&mut self, response: Value) {
        if self.retired_responses.len() < ROUTE_EVENT_BUDGET {
            self.retired_responses.push(response);
        } else {
            self.retired_response_overloaded = true;
        }
    }

    fn take_retired_responses(&mut self) -> (Vec<Value>, bool) {
        (
            std::mem::take(&mut self.retired_responses),
            std::mem::take(&mut self.retired_response_overloaded),
        )
    }

    fn track_unknown_buffer(&mut self, thread_id: &str) {
        if !self.unknown_buffered.insert(thread_id.to_string()) {
            return;
        }
        self.unknown_buffer_order.push_back(thread_id.to_string());
        while self.unknown_buffered.len() > UNKNOWN_BUFFER_BUDGET {
            let Some(expired) = self.unknown_buffer_order.pop_front() else {
                break;
            };
            if self.unknown_buffered.remove(&expired)
                && let Some(buffered) = self.buffered.remove(&expired)
            {
                self.reject_buffered_route(buffered);
            }
        }
    }

    fn reject_buffered_route(&mut self, buffered: BufferedRoute) {
        for message in buffered.messages {
            self.reject_retired_request(&message.message);
        }
    }

    fn adopt_unknown_buffer(&mut self, thread_id: &str) {
        if self.unknown_buffered.remove(thread_id) {
            self.unknown_buffer_order
                .retain(|buffered| buffered != thread_id);
        }
    }

    fn take_buffered(&mut self, thread_id: &str) -> Option<BufferedRoute> {
        self.adopt_unknown_buffer(thread_id);
        self.buffered.remove(thread_id)
    }

    fn is_failed(&self, thread_id: &str) -> bool {
        self.failed.contains(thread_id)
    }

    fn mark_failed(&mut self, thread_id: String) {
        if !self.failed.insert(thread_id.clone()) {
            return;
        }
        // `clear_failed` deliberately leaves stale FIFO entries behind so its
        // hot-path membership removal stays O(1). Purge a reused id here,
        // where route failure/teardown frequency is low.
        self.failed_order.retain(|failed| failed != &thread_id);
        self.failed_order.push_back(thread_id);
        while self.failed_order.len() > ROUTE_TOMBSTONE_BUDGET {
            let Some(expired) = self.failed_order.pop_front() else {
                break;
            };
            self.failed.remove(&expired);
        }
    }

    fn clear_failed(&mut self, thread_id: &str) {
        self.failed.remove(thread_id);
    }

    fn child_key(thread_id: &str, message: &ServerMsg) -> Option<ChildTurnKey> {
        Some(ChildTurnKey {
            thread_id: thread_id.to_string(),
            turn_id: message_turn_id(message)?.to_string(),
        })
    }

    fn retire_child(&mut self, thread_id: String, turn_id: String) {
        let key = ChildTurnKey { thread_id, turn_id };
        if !self.retired_children.insert(key.clone()) {
            return;
        }
        self.retired_child_order.retain(|retired| retired != &key);
        self.retired_child_order.push_back(key);
        while self.retired_child_order.len() > ROUTE_TOMBSTONE_BUDGET {
            let Some(expired) = self.retired_child_order.pop_front() else {
                break;
            };
            self.retired_children.remove(&expired);
        }
    }

    #[cfg(test)]
    fn unretire_child(&mut self, thread_id: &str, turn_id: &str) {
        self.retired_children.remove(&ChildTurnKey {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
        });
    }

    fn retire_owner(&mut self, thread_id: String, owner: RouteOwner) {
        if let Some(turn_id) = owner.child_turn_id {
            self.retire_child(thread_id, turn_id);
        } else {
            self.retire_unbound_child(thread_id);
        }
    }

    fn retire_unbound_child(&mut self, thread_id: String) {
        if !self.retired_unbound_children.insert(thread_id.clone()) {
            return;
        }
        self.retired_unbound_child_order
            .retain(|retired| retired != &thread_id);
        self.retired_unbound_child_order.push_back(thread_id);
        while self.retired_unbound_child_order.len() > ROUTE_TOMBSTONE_BUDGET {
            let Some(expired) = self.retired_unbound_child_order.pop_front() else {
                break;
            };
            self.retired_unbound_children.remove(&expired);
        }
    }

    fn buffer_message(
        &mut self,
        thread_id: String,
        message: ServerMsg,
        root_generation: Option<u64>,
    ) {
        if thread_id.is_empty() || self.is_failed(&thread_id) {
            self.reject_retired_request(&message);
            return;
        }
        let root_buffer = self.routes.contains_key(&thread_id);
        if !root_buffer && !self.buffered.contains_key(&thread_id) {
            self.track_unknown_buffer(&thread_id);
        }
        let route = self.buffered.entry(thread_id).or_default();
        if route.messages.len() < ROUTE_EVENT_BUDGET {
            route.messages.push(BufferedMessage {
                message,
                root_generation,
            });
            return;
        }
        if matches!(&message, ServerMsg::Request { .. }) {
            // Requests and nested ownership announcements must still win a
            // full pre-ownership buffer. Remember any evicted transcript so
            // adoption fails closed instead of projecting partial history.
            if let Some(index) = route.messages.iter().position(|buffered| {
                matches!(buffered.message, ServerMsg::Notification { .. })
                    && (root_buffer || announced_child_threads(&buffered.message).is_empty())
            }) {
                let removed = route.messages.remove(index);
                if root_buffer {
                    route
                        .root_overflowed_turns
                        .insert(message_turn_id(&removed.message).map(str::to_string));
                } else {
                    route.notification_overloaded = true;
                }
                route.messages.push(BufferedMessage {
                    message,
                    root_generation,
                });
            } else if root_buffer {
                route
                    .root_overflowed_turns
                    .insert(message_turn_id(&message).map(str::to_string));
                self.reject_retired_request(&message);
            } else {
                route.request_overloaded = true;
                self.reject_retired_request(&message);
            }
        } else if root_buffer {
            route
                .root_overflowed_turns
                .insert(message_turn_id(&message).map(str::to_string));
        } else if !announced_child_threads(&message).is_empty() {
            if let Some(index) = route.messages.iter().position(|buffered| {
                matches!(buffered.message, ServerMsg::Notification { .. })
                    && announced_child_threads(&buffered.message).is_empty()
            }) {
                route.messages.remove(index);
                route.notification_overloaded = true;
                route.messages.push(BufferedMessage {
                    message,
                    root_generation,
                });
            } else {
                route.announcement_overloaded = true;
            }
        } else {
            route.notification_overloaded = true;
        }
    }

    fn descendant_ids(&self, root_thread_id: &str) -> Vec<String> {
        self.owners
            .iter()
            .filter(|(_, owner)| owner.root_thread_id == root_thread_id)
            .map(|(child, _)| child.clone())
            .collect()
    }

    fn clear_descendants(&mut self, root_thread_id: &str) {
        let descendants = self.descendant_ids(root_thread_id);
        for descendant in descendants {
            if let Some(owner) = self.owners.remove(&descendant) {
                self.retire_owner(descendant.clone(), owner);
            }
            if let Some(buffered) = self.take_buffered(&descendant) {
                self.reject_buffered_route(buffered);
            }
        }
    }

    fn remove_route_if_same(
        &mut self,
        root_thread_id: &str,
        expected: &RouteSender<ServerMsg>,
        _route_failed: bool,
    ) -> bool {
        if !self
            .routes
            .get(root_thread_id)
            .is_some_and(|route| route.tx.same_channel(expected))
        {
            return false;
        }
        self.routes.remove(root_thread_id);
        self.clear_descendants(root_thread_id);
        if let Some(owner) = self.owners.remove(root_thread_id) {
            self.retire_owner(root_thread_id.to_string(), owner);
        }
        if let Some(buffered) = self.take_buffered(root_thread_id) {
            self.reject_buffered_route(buffered);
        }
        // Always retire the root so its late events cannot become a future
        // pre-subscription buffer. Descendants retain only their bounded exact
        // child-turn identities, so a reused thread id remains eligible.
        self.mark_failed(root_thread_id.to_string());
        true
    }

    /// Route one message and learn descendant ownership only from the exact
    /// active root turn. Parent and authenticated descendant events share one
    /// bounded channel so their transport order is preserved end to end.
    fn route_message(&mut self, message: ServerMsg) {
        let thread_id = message_thread_id(&message).unwrap_or("");
        if Self::child_key(thread_id, &message)
            .is_some_and(|key| self.retired_children.contains(&key))
            && !self.routes.contains_key(thread_id)
        {
            self.reject_retired_request(&message);
            return;
        }
        let root_generation = self
            .routes
            .get(thread_id)
            .map(|route| route.generation)
            .or_else(|| {
                self.owners
                    .get(thread_id)
                    .map(|owner| owner.root_generation)
            });
        self.route_buffered_message(BufferedMessage {
            message,
            root_generation,
        });
    }

    fn route_buffered_message(&mut self, first: BufferedMessage) {
        let mut queue = VecDeque::from([first]);
        let mut overload_after_drain = Vec::new();

        while let Some(buffered_message) = queue.pop_front() {
            let BufferedMessage {
                message,
                root_generation: message_generation,
            } = buffered_message;
            let thread_id = message_thread_id(&message).unwrap_or("").to_string();
            if thread_id.is_empty() {
                continue;
            }

            let (root_thread_id, root_turn_id, root_generation, tx, child_message) =
                if let Some(route) = self.routes.get(&thread_id) {
                    let Some(turn_id) = route.turn_id.clone() else {
                        self.buffer_message(thread_id, message, message_generation);
                        continue;
                    };
                    (
                        thread_id.clone(),
                        turn_id,
                        route.generation,
                        route.tx.clone(),
                        false,
                    )
                } else if let Some(mut owner) = self.owners.get(&thread_id).cloned() {
                    let message_turn_id = message_turn_id(&message).map(str::to_string);
                    match (&owner.child_turn_id, &message_turn_id) {
                        (Some(owner_turn_id), Some(message_turn_id))
                            if owner_turn_id != message_turn_id =>
                        {
                            if self.retired_children.contains(&ChildTurnKey {
                                thread_id: thread_id.clone(),
                                turn_id: message_turn_id.clone(),
                            }) {
                                self.reject_retired_request(&message);
                            } else {
                                // A different child turn may precede the
                                // replacement parent's announcement. Keep it
                                // separate until that announcement claims it.
                                self.buffer_message(thread_id, message, message_generation);
                            }
                            continue;
                        }
                        (Some(_), None) => {
                            self.reject_retired_request(&message);
                            continue;
                        }
                        (None, Some(message_turn_id)) => {
                            if self.retired_children.contains(&ChildTurnKey {
                                thread_id: thread_id.clone(),
                                turn_id: message_turn_id.clone(),
                            }) {
                                self.reject_retired_request(&message);
                                continue;
                            }
                            owner.child_turn_id = Some(message_turn_id.clone());
                            self.owners.insert(thread_id.clone(), owner.clone());
                        }
                        (None, None) => {
                            self.reject_retired_request(&message);
                            continue;
                        }
                        _ => {}
                    }
                    let Some(route) = self.routes.get(&owner.root_thread_id) else {
                        self.reject_retired_request(&message);
                        continue;
                    };
                    if route.turn_id.as_deref() != Some(&owner.root_turn_id)
                        || route.generation != owner.root_generation
                    {
                        self.reject_retired_request(&message);
                        continue;
                    }
                    (
                        owner.root_thread_id,
                        owner.root_turn_id,
                        owner.root_generation,
                        route.tx.clone(),
                        true,
                    )
                } else {
                    self.buffer_message(thread_id, message, message_generation);
                    continue;
                };

            let child_threads: Vec<String> = announced_child_threads(&message)
                .into_iter()
                .map(str::to_string)
                .collect();
            if !child_message
                && message_turn_id(&message).is_none()
                && message_generation != Some(root_generation)
            {
                // A turn-less root event is trusted only when it arrived under
                // this subscription generation. Pre-subscription traffic has
                // no authenticated association with the active turn.
                self.reject_retired_request(&message);
                continue;
            }
            if !child_message
                && message_turn_id(&message)
                    .is_some_and(|message_turn_id| message_turn_id != root_turn_id)
            {
                // Reject stale root announcements before they can claim child
                // requests for a replacement turn. Do not mutate child state by
                // id alone: Codex may already have reused that id for the
                // replacement generation. Unknown buffers are bounded globally.
                self.reject_retired_request(&message);
                continue;
            }

            let can_announce_children = if child_message {
                true
            } else {
                match message_turn_id(&message) {
                    Some(turn_id) => turn_id == root_turn_id,
                    None => message_generation == Some(root_generation),
                }
            };
            let child_terminal = child_message
                && matches!(
                    &message,
                    ServerMsg::Notification { method, .. } if method == "turn/completed"
                );
            {
                let rejection = Self::decline_response(&message);
                match tx.try_send(message) {
                    Ok(()) => {
                        self.clear_failed(&thread_id);
                        self.clear_failed(&root_thread_id);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "codex: dropping {root_thread_id} event route while routing \
                             {thread_id}: {}",
                            match error {
                                RouteSendError::Closed => "receiver is closed",
                                RouteSendError::Overloaded => "event backlog limit exceeded",
                            }
                        );
                        if let Some(response) = rejection {
                            self.push_retired_response(response);
                        }
                        self.remove_route_if_same(&root_thread_id, &tx, true);
                        continue;
                    }
                }
            }
            if child_terminal && let Some(owner) = self.owners.remove(&thread_id) {
                self.retire_owner(thread_id.clone(), owner);
            }

            if can_announce_children {
                for child_thread_id in child_threads {
                    if child_thread_id == root_thread_id {
                        continue;
                    }
                    let unresolved_retirement =
                        self.retired_unbound_children.remove(&child_thread_id);
                    let buffered_route =
                        self.take_buffered(&child_thread_id).map(|mut buffered| {
                            let mut eligible = Vec::with_capacity(buffered.messages.len());
                            for message in std::mem::take(&mut buffered.messages) {
                                let retired_turn =
                                    Self::child_key(&child_thread_id, &message.message)
                                        .is_some_and(|key| self.retired_children.contains(&key));
                                if unresolved_retirement
                                    || retired_turn
                                    || message
                                        .root_generation
                                        .is_some_and(|generation| generation != root_generation)
                                {
                                    self.reject_retired_request(&message.message);
                                } else {
                                    eligible.push(message);
                                }
                            }
                            buffered.messages = eligible;
                            buffered
                        });
                    let mut candidate_turns = buffered_route
                        .iter()
                        .flat_map(|buffered| buffered.messages.iter())
                        .filter_map(|buffered| message_turn_id(&buffered.message))
                        .filter(|turn_id| {
                            !self.retired_children.contains(&ChildTurnKey {
                                thread_id: child_thread_id.clone(),
                                turn_id: (*turn_id).to_string(),
                            })
                        })
                        .map(str::to_string)
                        .collect::<HashSet<_>>();
                    let ambiguous_child_turn = candidate_turns.len() > 1;
                    let child_turn_id = if candidate_turns.len() == 1 {
                        candidate_turns.drain().next()
                    } else {
                        None
                    };
                    let previous = self.owners.get(&child_thread_id).cloned();
                    let preserve_previous = previous.as_ref().is_some_and(|previous| {
                        previous.root_thread_id == root_thread_id
                            && previous.root_turn_id == root_turn_id
                            && previous.root_generation == root_generation
                            && (child_turn_id.is_none() || previous.child_turn_id == child_turn_id)
                    });
                    if !preserve_previous {
                        if let Some(previous) = self.owners.remove(&child_thread_id) {
                            self.retire_owner(child_thread_id.clone(), previous);
                        }
                        self.owners.insert(
                            child_thread_id.clone(),
                            RouteOwner {
                                root_thread_id: root_thread_id.clone(),
                                root_turn_id: root_turn_id.clone(),
                                root_generation,
                                child_turn_id,
                            },
                        );
                    }
                    if let Some(buffered_route) = buffered_route {
                        if ambiguous_child_turn {
                            self.reject_buffered_route(buffered_route);
                            overload_after_drain.push(tx.clone());
                            continue;
                        }
                        queue.extend(buffered_route.messages);
                        // Any child overflow makes its durable projection
                        // incomplete, so fail the owning stream closed after
                        // draining the retained prefix.
                        if buffered_route.request_overloaded
                            || buffered_route.announcement_overloaded
                            || buffered_route.notification_overloaded
                        {
                            overload_after_drain.push(tx.clone());
                        }
                    }
                }
            }
        }

        for tx in overload_after_drain {
            tx.mark_overloaded();
        }
    }

    fn subscribe(&mut self, thread_id: &str, tx: RouteSender<ServerMsg>) {
        // A collaborator id may be reused by the replacement turn. Remove old
        // ownership and buffered traffic without tombstoning descendants so a
        // request that precedes the new announcement can still be retained.
        self.clear_descendants(thread_id);
        if let Some(owner) = self.owners.remove(thread_id) {
            self.retire_owner(thread_id.to_string(), owner);
        }
        self.clear_failed(thread_id);
        self.adopt_unknown_buffer(thread_id);
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.routes.insert(
            thread_id.to_string(),
            ActiveRoute {
                tx,
                generation,
                turn_id: None,
            },
        );
    }

    fn activate_route(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        expected: &RouteSender<ServerMsg>,
    ) -> bool {
        let Some(route) = self.routes.get_mut(thread_id) else {
            return false;
        };
        if !route.tx.same_channel(expected) {
            return false;
        }
        route.turn_id = Some(turn_id.to_string());
        let tx = route.tx.clone();
        let buffered = self.take_buffered(thread_id);
        if let Some(buffered) = buffered {
            let relevant_overflow = buffered.request_overloaded
                || buffered.announcement_overloaded
                || buffered.root_overflowed_turns.contains(&None)
                || buffered
                    .root_overflowed_turns
                    .contains(&Some(turn_id.to_string()));
            for message in buffered.messages {
                self.route_buffered_message(message);
            }
            if relevant_overflow {
                tx.mark_overloaded();
            }
        }
        true
    }
}

async fn close_transport(
    pending: &Pending,
    routing: &Routing,
    active_turns: Option<&ActiveTurns>,
    closed: &std::sync::atomic::AtomicBool,
) {
    // Publish closure before taking async locks so no caller can reuse this
    // transport while its abandoned waiters are being drained.
    closed.store(true, Ordering::Relaxed);
    pending.lock().await.clear();
    let mut routing = routing.lock().await;
    for route in routing.routes.values() {
        route.tx.mark_closed();
    }
    *routing = RoutingState::default();
    drop(routing);
    if let Some(active_turns) = active_turns {
        active_turns.lock().await.clear();
    }
}

fn close_transport_blocking(
    pending: &Pending,
    routing: &Routing,
    active_turns: &ActiveTurns,
    closed: &std::sync::atomic::AtomicBool,
) {
    closed.store(true, Ordering::Relaxed);
    pending.blocking_lock().clear();
    let mut routing = routing.blocking_lock();
    for route in routing.routes.values() {
        route.tx.mark_closed();
    }
    *routing = RoutingState::default();
    drop(routing);
    active_turns.blocking_lock().clear();
}

fn close_transport_available(
    pending: &Pending,
    routing: &Routing,
    active_turns: &ActiveTurns,
    closed: &std::sync::atomic::AtomicBool,
) {
    closed.store(true, Ordering::Relaxed);
    if let Ok(mut pending) = pending.try_lock() {
        pending.clear();
    }
    if let Ok(mut routing) = routing.try_lock() {
        for route in routing.routes.values() {
            route.tx.mark_closed();
        }
        *routing = RoutingState::default();
    }
    if let Ok(mut active_turns) = active_turns.try_lock() {
        active_turns.clear();
    }
}

async fn terminate_transport_parts(
    pending: Pending,
    routing: Routing,
    active_turns: Option<ActiveTurns>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    child: Option<Arc<std::sync::Mutex<ProcessTreeChild>>>,
) -> Result<(), BackendError> {
    // The detached task owns the complete invalidation sequence, so dropping
    // a caller cannot strand waiters after `closed` becomes visible.
    closed.store(true, Ordering::Relaxed);
    let cleanup = tokio::spawn(async move {
        close_transport(&pending, &routing, active_turns.as_ref(), &closed).await;
        if let Some(child) = child {
            tokio::task::spawn_blocking(move || kill_and_reap_child(child))
                .await
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "app-server cleanup task failed before acknowledgement: {error}"
                    ))
                })??;
        }
        Ok::<(), std::io::Error>(())
    });
    cleanup
        .await
        .map_err(|error| {
            BackendError::Protocol(format!(
                "app-server cleanup coordinator failed before acknowledgement: {error}"
            ))
        })?
        .map_err(BackendError::Io)
}

fn kill_and_reap_child(child: Arc<std::sync::Mutex<ProcessTreeChild>>) -> std::io::Result<()> {
    let Ok(mut child) = child.lock() else {
        tracing::warn!("codex: app-server process lock is poisoned");
        return Err(std::io::Error::other("app-server process lock is poisoned"));
    };
    let mut terminate_error = child.terminate_now().err();
    if let Some(error) = terminate_error.as_ref() {
        tracing::warn!("codex: failed to terminate unusable app-server tree: {error}");
    }
    let deadline = std::time::Instant::now() + CHILD_REAP_TIMEOUT;
    let mut retried_after_leader_exit = false;
    loop {
        match child.try_wait_tree() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {
                if !retried_after_leader_exit {
                    match child.retry_termination_after_leader_exit() {
                        Ok(retried) => retried_after_leader_exit = retried,
                        Err(error) => {
                            retried_after_leader_exit = true;
                            tracing::warn!(
                                "codex: failed to terminate late app-server descendants: {error}"
                            );
                            terminate_error.get_or_insert(error);
                        }
                    }
                }
                if std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                tracing::warn!(
                    "codex: app-server did not exit within {}s",
                    CHILD_REAP_TIMEOUT.as_secs()
                );
                return Err(terminate_error.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "app-server process tree did not exit within {}s",
                            CHILD_REAP_TIMEOUT.as_secs()
                        ),
                    )
                }));
            }
            Err(error) => {
                tracing::warn!("codex: failed to reap app-server: {error}");
                return Err(error);
            }
        }
    }
}

fn start_kill_child_now(child: &std::sync::Mutex<ProcessTreeChild>) {
    match child.try_lock() {
        Ok(mut child) => {
            if let Err(error) = child.terminate_now() {
                tracing::warn!("codex: failed to terminate unusable app-server tree: {error}");
            }
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            // Another termination path owns the process lock and signals the
            // child before waiting for it, so no duplicate action is needed.
        }
        Err(std::sync::TryLockError::Poisoned(_)) => {
            tracing::warn!("codex: app-server process lock is poisoned");
        }
    }
}

struct ReaderTurnState {
    active_turns: Option<ActiveTurns>,
    completed_turns: Option<CompletedTurns>,
    turn_lifecycles: Option<TurnLifecycles>,
}

async fn read_stdout<R: AsyncRead + Unpin>(
    stdout: R,
    pending: Pending,
    routing: Routing,
    turn_state: ReaderTurnState,
    retired_response_tx: mpsc::Sender<Value>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    child: Option<std::sync::Weak<std::sync::Mutex<ProcessTreeChild>>>,
) {
    let ReaderTurnState {
        active_turns,
        completed_turns,
        turn_lifecycles,
    } = turn_state;
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
            let completed_turn = (method == "turn/completed")
                .then(|| {
                    Some((
                        params["threadId"].as_str()?.to_string(),
                        params["turn"]["id"].as_str()?.to_string(),
                    ))
                })
                .flatten();
            let message = if has_id {
                ServerMsg::Request {
                    id: msg["id"].clone(),
                    method,
                    params,
                }
            } else {
                ServerMsg::Notification { method, params }
            };
            let (responses, response_overloaded) = {
                let mut routing = routing.lock().await;
                routing.route_message(message);
                routing.take_retired_responses()
            };
            if let Some((thread_id, turn_id)) = completed_turn
                && let Some(active_turns) = active_turns.clone()
            {
                if let Some(completed_turns) = completed_turns.clone() {
                    record_completed_turn(&completed_turns, &active_turns, &thread_id, &turn_id)
                        .await;
                } else {
                    // Unit plumbing without shared completion state still
                    // preserves exact-marker matching.
                    let mut active = active_turns.lock().await;
                    if active.get(&thread_id) == Some(&turn_id) {
                        active.remove(&thread_id);
                    }
                }
                if let Some(turn_lifecycles) = turn_lifecycles.clone() {
                    // Do not block the multiplexed stdout reader on a thread
                    // lifecycle: an interrupt response for this or another
                    // turn may be the next line. The detached cleanup shares
                    // startup serialization and catches legacy/direct marker
                    // publication, but owns no process handle.
                    tokio::spawn(async move {
                        let _lifecycle = acquire_turn_lifecycle(&turn_lifecycles, &thread_id).await;
                        let mut active = active_turns.lock().await;
                        if active.get(&thread_id) == Some(&turn_id) {
                            active.remove(&thread_id);
                        }
                    });
                }
            }
            if response_overloaded {
                tracing::error!(
                    "codex: terminating app-server after retired-response buffer overflow"
                );
                if let Err(error) = terminate_transport_parts(
                    pending.clone(),
                    routing.clone(),
                    active_turns.clone(),
                    closed.clone(),
                    child.as_ref().and_then(std::sync::Weak::upgrade),
                )
                .await
                {
                    tracing::warn!("codex: failed to acknowledge overflow cleanup: {error}");
                }
                return;
            }
            for response in responses {
                if retired_response_tx.try_send(response).is_err() {
                    tracing::error!(
                        "codex: terminating app-server after retired-response queue overflow"
                    );
                    if let Err(error) = terminate_transport_parts(
                        pending.clone(),
                        routing.clone(),
                        active_turns.clone(),
                        closed.clone(),
                        child.as_ref().and_then(std::sync::Weak::upgrade),
                    )
                    .await
                    {
                        tracing::warn!("codex: failed to acknowledge overflow cleanup: {error}");
                    }
                    return;
                }
            }
        }
    }
    // Dropping stdout means the app-server can never complete any
    // outstanding request or turn. Drop every sender it left behind so
    // request waiters and routed turn streams wake immediately instead of
    // remaining active forever.
    close_transport(&pending, &routing, active_turns.as_ref(), &closed).await;
    if let Some(child) = child.and_then(|child| child.upgrade()) {
        match tokio::task::spawn_blocking(move || kill_and_reap_child(child)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!("codex: stdout EOF cleanup was not acknowledged: {error}");
            }
            Err(error) => {
                tracing::warn!("codex: stdout EOF cleanup task failed: {error}");
            }
        }
    }
}

const THREAD_CACHE_CAP: usize = 256;

#[derive(Debug, PartialEq)]
struct LoadedThreadSettings {
    mcp_config: Value,
    developer_instructions: Option<String>,
}

#[derive(Default)]
struct LoadedThreadCache {
    settings: HashMap<String, LoadedThreadSettings>,
    order: VecDeque<String>,
}

impl LoadedThreadCache {
    fn remember(
        &mut self,
        thread_id: &str,
        mcp_config: Value,
        developer_instructions: Option<String>,
    ) {
        if !self.settings.contains_key(thread_id) {
            if self.settings.len() >= THREAD_CACHE_CAP
                && let Some(evicted) = self.order.pop_front()
            {
                self.settings.remove(&evicted);
            }
            self.order.push_back(thread_id.to_string());
        }
        self.settings.insert(
            thread_id.to_string(),
            LoadedThreadSettings {
                mcp_config,
                developer_instructions,
            },
        );
    }

    fn forget(&mut self, thread_id: &str) {
        if self.settings.remove(thread_id).is_some() {
            self.order.retain(|loaded| loaded != thread_id);
        }
    }
}

/// Keeps Codex's refreshed subscription credentials while the rest of its
/// home remains isolated. A baseline comparison prevents an old app-server
/// from overwriting a newer login performed by another process.
///
/// The adjacent lock file is stable across atomic auth.json replacements, so
/// every Trouve app-server and Trouve-initiated login shares one writer lock.
/// A direct vendor CLI does not participate in that lock; the second source
/// read immediately before publication detects and preserves such a write
/// whenever it overlaps staging.
struct AuthSync {
    source: PathBuf,
    isolated: PathBuf,
    baseline: std::sync::Mutex<Option<Vec<u8>>>,
}

struct AuthFileLock {
    _file: std::fs::File,
}

impl AuthFileLock {
    fn open(source: &Path) -> std::io::Result<std::fs::File> {
        let parent = source.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Codex auth path has no parent directory",
            )
        })?;
        std::fs::create_dir_all(parent)?;
        let mut name = source
            .file_name()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Codex auth path has no file name",
                )
            })?
            .to_os_string();
        name.push(".trouve.lock");
        let path = source.with_file_name(name);
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path)
    }

    #[cfg(test)]
    fn acquire(source: &Path) -> std::io::Result<Self> {
        let file = Self::open(source)?;
        file.lock()?;
        Ok(Self { _file: file })
    }

    fn try_acquire_for(source: &Path, wait: std::time::Duration) -> std::io::Result<Option<Self>> {
        let file = Self::open(source)?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

async fn acquire_auth_lock(source: PathBuf) -> Result<AuthFileLock, BackendError> {
    tokio::task::spawn_blocking(move || AuthFileLock::try_acquire_for(&source, AUTH_LOCK_WAIT))
        .await
        .map_err(|error| BackendError::Io(std::io::Error::other(error.to_string())))?
        .map_err(BackendError::Io)?
        .ok_or_else(|| {
            BackendError::Protocol(
                "Codex credentials are being updated; retry login shortly".into(),
            )
        })
}

fn read_auth_or_empty(path: &Path) -> std::io::Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn auth_publication_backup(source: &Path) -> std::io::Result<PathBuf> {
    let mut name = source
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Codex auth path has no file name",
            )
        })?
        .to_os_string();
    name.push(".trouve.previous");
    Ok(source.with_file_name(name))
}

/// Restore a claimed credential file without replacing a path that a direct
/// vendor login created concurrently. A hard link is an atomic no-clobber
/// publication on every platform we support and both names share a directory.
fn restore_claimed_auth(source: &Path, backup: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(backup, source) {
        Ok(()) => std::fs::remove_file(backup),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(backup)
        }
        Err(error) => Err(error),
    }
}

/// Recover an interrupted claim before starting a new publication. The live
/// source always wins when both names exist.
fn recover_auth_publication(source: &Path, backup: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(backup) {
        Ok(_) => restore_claimed_auth(source, backup),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn stage_auth_snapshot(isolated_home: &Path) -> std::io::Result<Option<AuthSync>> {
    let Some(source) = codex_auth_path() else {
        return Ok(None);
    };
    stage_auth_snapshot_from(source, isolated_home)
}

fn stage_auth_snapshot_from(
    source: PathBuf,
    isolated_home: &Path,
) -> std::io::Result<Option<AuthSync>> {
    let Some(_lock) = AuthFileLock::try_acquire_for(&source, AUTH_LOCK_WAIT)? else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "Codex credentials are being updated (usually by an interactive login); retry shortly",
        ));
    };
    let baseline = match std::fs::read(&source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let isolated = isolated_home.join("auth.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut file = options.open(&isolated)?;
        file.write_all(&baseline)?;
        file.sync_all()?;
    }
    Ok(Some(AuthSync::new(source, isolated, baseline)))
}

impl AuthSync {
    fn new(source: PathBuf, isolated: PathBuf, baseline: Vec<u8>) -> Self {
        Self {
            source,
            isolated,
            baseline: std::sync::Mutex::new(Some(baseline)),
        }
    }

    fn sync(&self) -> std::io::Result<()> {
        self.sync_with_publish_hook(|| {})
    }

    fn sync_with_publish_hook(&self, before_claim: impl FnOnce()) -> std::io::Result<()> {
        self.sync_with_publish_hooks(before_claim, || {})
    }

    fn sync_with_publish_hooks(
        &self,
        before_claim: impl FnOnce(),
        after_claim: impl FnOnce(),
    ) -> std::io::Result<()> {
        let mut baseline = self.baseline.lock().unwrap();
        let Some(previous) = baseline.clone() else {
            return Ok(());
        };
        let isolated = std::fs::read(&self.isolated)?;
        if isolated == previous {
            return Ok(());
        }
        let Some(_lock) = AuthFileLock::try_acquire_for(&self.source, AUTH_LOCK_WAIT)? else {
            tracing::debug!(
                "Codex auth is busy; deferring this isolated credential refresh until a later sync"
            );
            return Ok(());
        };
        let backup = auth_publication_backup(&self.source)?;
        recover_auth_publication(&self.source, &backup)?;
        let source = read_auth_or_empty(&self.source)?;
        if source != previous && source != isolated {
            tracing::warn!(
                "Codex auth changed outside the isolated app-server; preserving the newer source"
            );
            *baseline = None;
            return Ok(());
        }
        if source != isolated {
            let parent = self.source.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Codex auth path has no parent directory",
                )
            })?;
            let mut staged = tempfile::Builder::new()
                .prefix(".trouve-auth-")
                .tempfile_in(parent)?;
            {
                use std::io::Write as _;
                staged.write_all(&isolated)?;
                staged.as_file().sync_all()?;
            }

            before_claim();
            let current = read_auth_or_empty(&self.source)?;
            if current != source {
                if current != isolated {
                    tracing::warn!(
                        "Codex auth changed while a refresh was staged; preserving the newer source"
                    );
                    *baseline = None;
                    return Ok(());
                }
                *baseline = Some(isolated);
                return Ok(());
            }

            // Claim the exact path we inspected. A direct CLI replacement
            // before the rename is captured and detected below; one after
            // the rename makes persist_noclobber fail, so it also wins.
            std::fs::rename(&self.source, &backup)?;
            let claimed = std::fs::read(&backup)?;
            if claimed != source {
                restore_claimed_auth(&self.source, &backup)?;
                if claimed != isolated {
                    tracing::warn!(
                        "Codex auth changed while its refresh was claimed; preserving the newer source"
                    );
                    *baseline = None;
                }
                return Ok(());
            }

            after_claim();
            match staged.persist_noclobber(&self.source) {
                Ok(_) => std::fs::remove_file(&backup)?,
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let winner = read_auth_or_empty(&self.source)?;
                    restore_claimed_auth(&self.source, &backup)?;
                    if winner != isolated {
                        tracing::warn!(
                            "Codex login completed during isolated credential publication; preserving it"
                        );
                        *baseline = None;
                        return Ok(());
                    }
                }
                Err(error) => {
                    let publish_error = error.error;
                    restore_claimed_auth(&self.source, &backup)?;
                    return Err(publish_error);
                }
            }
        }
        // Publication (or another writer's identical publication) succeeded.
        // Keep the old baseline on every error so the next sync retries.
        *baseline = Some(isolated);
        Ok(())
    }
}

struct AppServer {
    stdin: SharedStdin,
    next_id: AtomicI64,
    pending: Pending,
    /// Active roots, turn-bound child ownership, and pre-subscription events
    /// share one lock so route replacement cannot observe partial cleanup.
    routing: Routing,
    /// Vendor turn currently running for each Codex thread. A replacement
    /// turn interrupts this first so Codex cannot merge prompts across trouve
    /// turn boundaries after cancellation.
    active_turns: ActiveTurns,
    completed_turns: CompletedTurns,
    /// Per-thread guards serializing interruption through replacement
    /// registration.
    turn_lifecycles: TurnLifecycles,
    /// Thread-level settings attached to each thread loaded in this app-server
    /// process. A newly spawned process starts empty.
    loaded_threads: Mutex<LoadedThreadCache>,
    /// Instruction set known to have reached at least one turn in this
    /// app-server process. This is intentionally process-local: an empty map
    /// after restart is what activates the cold-resume prompt fallback.
    thread_instructions: std::sync::Mutex<HashMap<String, String>>,
    /// A clean Codex home containing credentials only. Keeping the TempDir
    /// alive prevents ambient config, skills, plugins, hooks, and MCP servers
    /// from becoming a second capability source.
    _isolated_home: tempfile::TempDir,
    /// The app-server schema, discovered through its version-specific
    /// experimental feature catalog on the first optimized turn.
    supported_features: tokio::sync::OnceCell<HashSet<String>>,
    /// Syncs token rotations from the isolated home back to Codex's real
    /// credential file without exposing any other ambient configuration.
    auth_sync: Option<Arc<AuthSync>>,
    /// Held so the complete owned process tree lives as long as the server
    /// handle and can be synchronously signalled during Drop-time cleanup.
    child: Arc<std::sync::Mutex<ProcessTreeChild>>,
    retired_response_tx: mpsc::Sender<Value>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    transport_cleanup_started: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for AppServer {
    fn drop(&mut self) {
        if let Some(auth_sync) = &self.auth_sync
            && let Err(error) = auth_sync.sync()
        {
            tracing::warn!("Codex credential sync failed during shutdown: {error}");
        }
        // Startup may be cancelled while the handshake future owns the last
        // server handle. Keep process-tree cleanup independent of that
        // future's completion and of the Tokio runtime's lifetime.
        self.invalidate_transport_now();
    }
}

struct TransportWriteGuard<'a> {
    server: &'a AppServer,
    armed: bool,
}

impl Drop for TransportWriteGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            // `write_all` is not cancellation-safe. Once it starts, any early
            // exit leaves framing uncertain and the transport must not be
            // reused, even if the caller itself was cancelled.
            self.server.invalidate_transport_now();
        }
    }
}

impl AppServer {
    async fn spawn(command: &str) -> Result<Self, BackendError> {
        let isolated_home = tempfile::Builder::new()
            .prefix("trouve-codex-home-")
            .tempdir()
            .map_err(BackendError::Io)?;
        let isolated_path = isolated_home.path().to_path_buf();
        let auth_sync = tokio::task::spawn_blocking(move || stage_auth_snapshot(&isolated_path))
            .await
            .map_err(|error| BackendError::Io(std::io::Error::other(error.to_string())))?
            .map_err(BackendError::Io)?
            .map(Arc::new);
        let mut command_process = crate::process_env::tokio_command(command);
        command_process
            .arg("app-server")
            .arg("--strict-config")
            .env("CODEX_HOME", isolated_home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command_process).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(command.to_string()),
            _ => BackendError::Io(e),
        })?;
        let stdin = Arc::new(Mutex::new(child.take_stdin().expect("stdin piped")));
        let stdout = child.take_stdout().expect("stdout piped");
        let (retired_response_tx, retired_response_rx) = mpsc::channel(ROUTE_EVENT_BUDGET);

        let server = Self {
            stdin,
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            routing: Arc::new(Mutex::new(RoutingState::default())),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            completed_turns: Arc::new(Mutex::new(CompletedTurnState::default())),
            turn_lifecycles: Arc::new(std::sync::Mutex::new(HashMap::new())),
            loaded_threads: Mutex::new(LoadedThreadCache::default()),
            thread_instructions: std::sync::Mutex::new(HashMap::new()),
            _isolated_home: isolated_home,
            supported_features: tokio::sync::OnceCell::new(),
            auth_sync,
            child: Arc::new(std::sync::Mutex::new(child)),
            retired_response_tx,
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            transport_cleanup_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        server.start_response_writer(retired_response_rx);
        server.start_reader(stdout);
        Ok(server)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    async fn thread_settings_match(
        &self,
        thread_id: &str,
        mcp_config: &Value,
        developer_instructions: Option<&str>,
    ) -> bool {
        let loaded_threads = self.loaded_threads.lock().await;
        loaded_thread_settings_match(
            &loaded_threads.settings,
            thread_id,
            mcp_config,
            developer_instructions,
        )
    }

    async fn mark_thread_loaded(
        &self,
        thread_id: &str,
        mcp_config: Value,
        developer_instructions: Option<String>,
    ) {
        self.loaded_threads
            .lock()
            .await
            .remember(thread_id, mcp_config, developer_instructions);
    }

    /// Release app-server's subscription after a terminal turn. A later
    /// trouve turn resumes the persisted vendor thread and subscribes again.
    async fn release_thread(&self, thread_id: &str) -> Result<(), BackendError> {
        // Forget before the RPC. Callers hold the per-thread lifecycle guard,
        // so replacements cannot inspect this state until the unsubscribe has
        // either completed or its shared transport has been retired.
        self.loaded_threads.lock().await.forget(thread_id);
        self.request_with_cancel_timeout(
            "thread/unsubscribe",
            json!({ "threadId": thread_id }),
            None,
            true,
            THREAD_UNSUBSCRIBE_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    fn instructions_need_prompt_fallback(&self, thread_id: &str, instructions: &str) -> bool {
        self.thread_instructions
            .lock()
            .unwrap()
            .get(thread_id)
            .is_none_or(|known| known != instructions)
    }

    fn remember_thread_instructions(&self, thread_id: &str, instructions: &str) {
        let mut known = self.thread_instructions.lock().unwrap();
        if known.len() >= THREAD_CACHE_CAP
            && !known.contains_key(thread_id)
            && let Some(evicted) = known.keys().next().cloned()
        {
            known.remove(&evicted);
        }
        known.insert(thread_id.to_string(), instructions.to_string());
    }

    fn invalidate_transport_now(&self) {
        self.invalidate_transport_now_with(|cleanup| {
            std::thread::Builder::new()
                .name("codex-transport-cleanup".into())
                .spawn(cleanup)
                .map(|_| ())
        });
    }

    fn invalidate_transport_now_with<F>(&self, spawn_cleanup: F)
    where
        F: FnOnce(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>,
    {
        self.closed.store(true, Ordering::Relaxed);
        // `TransportWriteGuard::drop` may run during runtime shutdown. Send
        // the kill signal synchronously, then move waiter release and reaping
        // to an OS thread whose lifetime is independent of Tokio.
        start_kill_child_now(&self.child);
        if self.transport_cleanup_started.swap(true, Ordering::AcqRel) {
            return;
        }
        // Release every uncontended waiter synchronously. This remains useful
        // even if the host cannot allocate the fallback cleanup thread.
        close_transport_available(
            &self.pending,
            &self.routing,
            &self.active_turns,
            &self.closed,
        );
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let pending = self.pending.clone();
            let routing = self.routing.clone();
            let active_turns = self.active_turns.clone();
            let closed = self.closed.clone();
            let child = self.child.clone();
            if let Err(error) = spawn_cleanup(Box::new(move || {
                close_transport_blocking(&pending, &routing, &active_turns, &closed);
                let _ = kill_and_reap_child(child);
            })) {
                tracing::error!("codex: failed to start transport cleanup thread: {error}");
                let pending = self.pending.clone();
                let routing = self.routing.clone();
                let active_turns = self.active_turns.clone();
                let closed = self.closed.clone();
                let child = self.child.clone();
                runtime.spawn_blocking(move || {
                    close_transport_blocking(&pending, &routing, &active_turns, &closed);
                    let _ = kill_and_reap_child(child);
                });
            }
            return;
        }

        // Outside a runtime there is nowhere to schedule async cleanup. Block
        // until every waiter is released and the subprocess is reaped.
        close_transport_blocking(
            &self.pending,
            &self.routing,
            &self.active_turns,
            &self.closed,
        );
        let _ = kill_and_reap_child(self.child.clone());
    }

    fn start_reader(&self, stdout: tokio::process::ChildStdout) {
        let closed = self.closed.clone();
        let pending = self.pending.clone();
        let routing = self.routing.clone();
        let active_turns = self.active_turns.clone();
        let child = Arc::downgrade(&self.child);
        tokio::spawn(read_stdout(
            stdout,
            pending,
            routing,
            ReaderTurnState {
                active_turns: Some(active_turns),
                completed_turns: Some(self.completed_turns.clone()),
                turn_lifecycles: Some(self.turn_lifecycles.clone()),
            },
            self.retired_response_tx.clone(),
            closed,
            Some(child),
        ));
    }

    fn start_response_writer(&self, mut responses: mpsc::Receiver<Value>) {
        let stdin = self.stdin.clone();
        let pending = self.pending.clone();
        let routing = self.routing.clone();
        let active_turns = self.active_turns.clone();
        let closed = self.closed.clone();
        let child = Arc::downgrade(&self.child);
        tokio::spawn(async move {
            while let Some(message) = responses.recv().await {
                let mut line = serde_json::to_vec(&message).expect("serializable");
                line.push(b'\n');
                let write = async {
                    let mut stdin = stdin.lock().await;
                    stdin.write_all(&line).await?;
                    stdin.flush().await
                };
                if !matches!(
                    tokio::time::timeout(TRANSPORT_WRITE_TIMEOUT, write).await,
                    Ok(Ok(()))
                ) {
                    tracing::error!(
                        "codex: terminating app-server after retired-response write failure"
                    );
                    if let Err(error) = terminate_transport_parts(
                        pending.clone(),
                        routing.clone(),
                        Some(active_turns.clone()),
                        closed.clone(),
                        child.upgrade(),
                    )
                    .await
                    {
                        tracing::warn!(
                            "codex: retired-response writer cleanup was not acknowledged: {error}"
                        );
                    }
                    break;
                }
            }
        });
    }

    async fn send_retired_responses(&self, responses: Vec<Value>) -> Result<(), BackendError> {
        for response in responses {
            if self.retired_response_tx.try_send(response).is_err() {
                tracing::error!(
                    "codex: terminating app-server after retired-response queue overflow"
                );
                self.terminate_transport().await?;
                return Err(BackendError::Protocol(
                    "app-server decline-response queue exceeded its limit".into(),
                ));
            }
        }
        Ok(())
    }

    async fn sync_auth(&self) {
        if let Some(auth_sync) = self.auth_sync.clone() {
            match tokio::task::spawn_blocking(move || auth_sync.sync()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!("Codex credential sync failed: {error}"),
                Err(error) => tracing::warn!("Codex credential sync task failed: {error}"),
            }
        }
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
        self.request_with_cancel(method, params, None, false).await
    }

    async fn request_cancellable(
        &self,
        method: &str,
        params: Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value, BackendError> {
        self.request_with_cancel(method, params, Some(cancel), false)
            .await
    }

    /// Cancellable before transmission, then fenced to the exact response.
    /// Use only for requests whose vendor-side effect would otherwise outlive
    /// the result reported to core.
    async fn request_effect_cancellable(
        &self,
        method: &str,
        params: Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value, BackendError> {
        self.request_with_cancel_timeout(
            method,
            params,
            Some(cancel),
            true,
            REQUEST_RESPONSE_TIMEOUT,
        )
        .await
    }

    async fn validate_effect_id(
        &self,
        method: &str,
        parsed: Result<String, BackendError>,
        expected: Option<&str>,
    ) -> Result<String, BackendError> {
        let id = match parsed {
            Ok(id) => id,
            Err(error) => {
                // The provider reported success for an effect whose owner is
                // unknowable. It may still be live, so this transport cannot
                // safely serve another request.
                self.terminate_transport().await?;
                return Err(error);
            }
        };
        if let Some(expected) = expected
            && id != expected
        {
            self.terminate_transport().await?;
            return Err(BackendError::Protocol(format!(
                "{method} returned id {id}, expected {expected}"
            )));
        }
        Ok(id)
    }

    async fn validated_thread_id(
        &self,
        method: &str,
        result: &Value,
        expected: Option<&str>,
    ) -> Result<String, BackendError> {
        self.validate_effect_id(method, thread_id_of(result, method), expected)
            .await
    }

    async fn collaborator_prompt(
        &self,
        thread_id: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Option<String> {
        const MAX_ATTEMPTS: usize = 3;
        for attempt in 0..MAX_ATTEMPTS {
            match self
                .request_cancellable(
                    "thread/turns/list",
                    json!({
                        "threadId": thread_id,
                        "limit": 1,
                        "sortDirection": "desc",
                        "itemsView": "full",
                    }),
                    cancel,
                )
                .await
            {
                Ok(response) => {
                    if let Some(prompt) = collaborator_prompt_from_turn_page(&response) {
                        return Some(prompt);
                    }
                }
                Err(BackendError::Cancelled) => return None,
                Err(error) => {
                    tracing::warn!(
                        "codex: unable to load initial collaborator prompt for {thread_id}: {error}"
                    );
                    return None;
                }
            }

            if attempt + 1 < MAX_ATTEMPTS {
                let delay =
                    tokio::time::sleep(std::time::Duration::from_millis(25 * (attempt as u64 + 1)));
                tokio::pin!(delay);
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return None,
                    () = &mut delay => {}
                }
            }
        }
        tracing::debug!(
            "codex: initial collaborator prompt for {thread_id} was not visible after {MAX_ATTEMPTS} attempts"
        );
        None
    }

    async fn request_with_cancel(
        &self,
        method: &str,
        params: Value,
        cancel: Option<&tokio_util::sync::CancellationToken>,
        fence_after_write: bool,
    ) -> Result<Value, BackendError> {
        self.request_with_cancel_timeout(
            method,
            params,
            cancel,
            fence_after_write,
            REQUEST_RESPONSE_TIMEOUT,
        )
        .await
    }

    async fn request_with_cancel_timeout(
        &self,
        method: &str,
        params: Value,
        cancel: Option<&tokio_util::sync::CancellationToken>,
        fence_after_write: bool,
        response_timeout: std::time::Duration,
    ) -> Result<Value, BackendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let write =
            self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let written = match cancel {
            Some(cancel) => tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(BackendError::Cancelled),
                result = write => result,
            },
            None => write.await,
        };
        if let Err(error) = written {
            self.pending.lock().await.remove(&id);
            if self.is_closed() {
                // Cancellation after write_all began invalidates framing via
                // TransportWriteGuard. A detached Drop cleanup is not enough:
                // acknowledge process-tree reaping before the caller can
                // publish cancellation or spawn a replacement server.
                self.terminate_transport().await?;
            }
            return Err(error);
        }
        let response = async {
            match tokio::time::timeout(response_timeout, rx).await {
                Ok(response) => response.map_err(|_| {
                    BackendError::Protocol(format!("{method}: app-server closed before responding"))
                }),
                Err(_) => Err(BackendError::Protocol(format!(
                    "{method}: no response within {}s",
                    response_timeout.as_secs_f64()
                ))),
            }
        };
        let response = if fence_after_write {
            // Once the complete request has been flushed, its vendor-side
            // effect may already be committed. Fence the exact response
            // instead of reporting that a transmitted effect never happened.
            response.await
        } else {
            match cancel {
                Some(cancel) => tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(BackendError::Cancelled),
                    response = response => response,
                },
                None => response.await,
            }
        };
        if response.is_err() && fence_after_write {
            // A transmitted effect with no exact response has an ambiguous
            // vendor-side outcome. Invalidate and reap the shared app-server
            // before returning so a late start/resume/steer cannot surface on
            // a replacement turn or share its transport with later requests.
            self.terminate_transport().await?;
        } else if response.is_err() {
            self.pending.lock().await.remove(&id);
        }
        match response? {
            Ok(v) => Ok(v),
            Err(e) => Err(BackendError::Protocol(format!("{method}: {e}"))),
        }
    }

    /// Append input to the exact turn currently active on a Codex thread.
    /// `expectedTurnId` makes a completion/replacement race fail closed
    /// instead of steering whichever turn happens to run next.
    async fn steer_turn(
        &self,
        thread_id: &str,
        input: Vec<Value>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), BackendError> {
        let expected_turn_id = self
            .active_turns
            .lock()
            .await
            .get(thread_id)
            .cloned()
            .ok_or_else(|| {
                BackendError::Protocol(format!(
                    "turn/steer: no active turn on Codex thread {thread_id}"
                ))
            })?;
        let response = self
            .request_effect_cancellable(
                "turn/steer",
                json!({
                    "threadId": thread_id,
                    "input": input,
                    "expectedTurnId": expected_turn_id,
                }),
                cancel,
            )
            .await?;
        self.validate_effect_id(
            "turn/steer",
            steered_turn_id_of(&response),
            Some(&expected_turn_id),
        )
        .await?;
        Ok(())
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

    /// Start and bind a turn under cancellation-safe ownership. The response
    /// receiver is installed in the guard before the request is written, so a
    /// dropped caller can recover a late turn id and interrupt it exactly.
    async fn start_turn(
        self: &Arc<Self>,
        thread_id: &str,
        route_tx: &RouteSender<ServerMsg>,
        params: Value,
        lifecycle: TurnLifecycleGuard,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(String, StartedTurnGuard), BackendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let write_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut cleanup = StartedTurnGuard::new(
            self.clone(),
            thread_id.to_string(),
            route_tx.clone(),
            rx,
            id,
            write_started.clone(),
            lifecycle,
        );
        self.pending.lock().await.insert(id, tx);
        let write = self.write_tracking(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "turn/start",
                "params": params,
            }),
            Some(&write_started),
        );
        let written = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BackendError::Cancelled),
            result = write => result,
        };
        if let Err(error) = written {
            self.pending.lock().await.remove(&id);
            drop(cleanup);
            // The guard's Drop owns exact-turn recovery. Reacquiring the
            // lifecycle is its acknowledgement that recovery/interrupt and
            // route cleanup have finished.
            drop(self.lock_turn_lifecycle(thread_id).await);
            return Err(error);
        }
        let started = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                drop(cleanup);
                drop(self.lock_turn_lifecycle(thread_id).await);
                return Err(BackendError::Cancelled);
            }
            response = cleanup.wait_for_response() => response,
        };
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                drop(cleanup);
                drop(self.lock_turn_lifecycle(thread_id).await);
                return Err(error);
            }
        };
        let turn_id = self
            .validate_effect_id("turn/start", turn_id_of(&started), None)
            .await?;
        cleanup.turn_id = Some(turn_id.clone());
        self.register_active_turn(thread_id, &turn_id).await;
        let activated = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BackendError::Cancelled),
            result = self.activate_route(thread_id, &turn_id, route_tx) => result,
        };
        if let Err(error) = activated {
            drop(cleanup);
            drop(self.lock_turn_lifecycle(thread_id).await);
            return Err(error);
        }
        // No await may follow this release: the active marker and route are
        // now atomically visible to the next lifecycle owner.
        cleanup.finish_startup();
        Ok((turn_id, cleanup))
    }

    /// Record the vendor turn currently running on a Codex thread.
    async fn register_active_turn(&self, thread_id: &str, turn_id: &str) {
        let _ = register_active_turn_state(
            &self.completed_turns,
            &self.active_turns,
            thread_id,
            turn_id,
            false,
        )
        .await;
    }

    async fn register_active_turn_if_vacant(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> ActiveTurnRegistration {
        register_active_turn_state(
            &self.completed_turns,
            &self.active_turns,
            thread_id,
            turn_id,
            true,
        )
        .await
    }

    async fn active_turn_is(&self, thread_id: &str, expected_turn_id: &str) -> bool {
        self.active_turns
            .lock()
            .await
            .get(thread_id)
            .is_some_and(|turn_id| turn_id == expected_turn_id)
    }

    async fn activate_route(
        &self,
        thread_id: &str,
        turn_id: &str,
        expected: &RouteSender<ServerMsg>,
    ) -> Result<(), BackendError> {
        let (activated, responses, response_overloaded) = {
            let mut routing = self.routing.lock().await;
            let activated = routing.activate_route(thread_id, turn_id, expected);
            let (responses, response_overloaded) = routing.take_retired_responses();
            (activated, responses, response_overloaded)
        };
        if response_overloaded {
            tracing::error!(
                "codex: terminating app-server after activation response buffer overflow"
            );
            self.terminate_transport().await?;
            return Err(BackendError::Protocol(
                "app-server activation generated too many decline responses".into(),
            ));
        }
        self.send_retired_responses(responses).await?;
        if !activated {
            // Keep the exact active marker installed. The still-armed startup
            // guard will interrupt it even though its route disappeared.
            return Err(BackendError::Protocol(
                "turn/start route disappeared before activation".into(),
            ));
        }
        Ok(())
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if let Err(error) = self
            .write(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "turn/interrupt",
                "params": {
                    "threadId": thread_id,
                    "turnId": turn_id,
                },
            }))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(INTERRUPT_RESPONSE_TIMEOUT, rx).await {
            Ok(Ok(Ok(_))) => Ok(()),
            // A well-formed application rejection leaves framing and every
            // unrelated turn intact. Keep this turn's active marker so a
            // replacement must retry interruption instead of merging prompts.
            Ok(Ok(Err(error))) => Err(BackendError::Protocol(format!("turn/interrupt: {error}"))),
            Ok(Err(_)) => Err(BackendError::Protocol(
                "turn/interrupt: app-server closed before responding".into(),
            )),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                // The interrupt may have been applied even though its response
                // was lost. The transport is now ambiguous and cannot safely
                // accept a replacement prompt.
                self.terminate_transport().await?;
                Err(BackendError::Protocol(format!(
                    "turn/interrupt: no response within {}s",
                    INTERRUPT_RESPONSE_TIMEOUT.as_secs()
                )))
            }
        }
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
        self.write_tracking(msg, None).await
    }

    async fn write_tracking(
        &self,
        msg: Value,
        write_started: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), BackendError> {
        if self.is_closed() {
            return Err(BackendError::Protocol(
                "app-server transport is closed".into(),
            ));
        }
        let mut stdin = self.stdin.lock().await;
        if let Some(write_started) = write_started {
            // No cancellation point separates acquiring exclusive stdin from
            // publishing that the request may now be partially written.
            write_started.store(true, Ordering::Relaxed);
        }
        let mut line = serde_json::to_vec(&msg).expect("serializable");
        line.push(b'\n');
        let mut write_guard = TransportWriteGuard {
            server: self,
            armed: true,
        };
        let result = tokio::time::timeout(TRANSPORT_WRITE_TIMEOUT, async {
            stdin.write_all(&line).await?;
            stdin.flush().await
        })
        .await;
        drop(stdin);
        match result {
            Ok(Ok(())) => {
                write_guard.armed = false;
                Ok(())
            }
            Ok(Err(error)) => {
                self.terminate_transport().await?;
                Err(BackendError::Io(error))
            }
            Err(_) => {
                self.terminate_transport().await?;
                Err(BackendError::Protocol(format!(
                    "app-server stdin blocked for {}s",
                    TRANSPORT_WRITE_TIMEOUT.as_secs()
                )))
            }
        }
    }

    async fn terminate_transport(&self) -> Result<(), BackendError> {
        terminate_transport_parts(
            self.pending.clone(),
            self.routing.clone(),
            Some(self.active_turns.clone()),
            self.closed.clone(),
            Some(self.child.clone()),
        )
        .await
    }

    async fn subscribe(&self, thread_id: &str) -> Result<RouteSubscription, BackendError> {
        let (tx, rx) = route_channel();
        let (responses, response_overloaded) = {
            let mut routing = self.routing.lock().await;
            routing.subscribe(thread_id, tx.clone());
            routing.take_retired_responses()
        };
        if response_overloaded {
            self.terminate_transport().await?;
            return Err(BackendError::Protocol(
                "app-server subscription generated too many decline responses".into(),
            ));
        }
        self.send_retired_responses(responses).await?;
        Ok(RouteSubscription { tx, rx })
    }

    async fn unsubscribe(&self, thread_id: &str, expected: &RouteSender<ServerMsg>) {
        let (responses, response_overloaded) = {
            let mut routing = self.routing.lock().await;
            routing.remove_route_if_same(thread_id, expected, false);
            routing.take_retired_responses()
        };
        if response_overloaded {
            if let Err(error) = self.terminate_transport().await {
                tracing::warn!("codex: unsubscribe cleanup was not acknowledged: {error}");
            }
        } else if let Err(error) = self.send_retired_responses(responses).await {
            tracing::warn!("codex: failed to flush route-cleanup declines: {error}");
        }
    }
}

/// One routed turn stream paired with the sender identity that owns it.
struct RouteSubscription {
    tx: RouteSender<ServerMsg>,
    rx: RouteReceiver<ServerMsg>,
}

/// Remove a route only when cleanup still owns the active subscription.
#[cfg(test)]
async fn remove_route(routing: &Routing, thread_id: &str, expected: &RouteSender<ServerMsg>) {
    // Buffered events belong to the route being removed only when it is
    // still the active route; stale turn cleanup must not erase events for
    // a replacement subscription.
    routing
        .lock()
        .await
        .remove_route_if_same(thread_id, expected, false);
}

fn codex_auth_path() -> Option<std::path::PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .map(|home| home.join("auth.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_turn() -> crate::BackendTurn {
        crate::BackendTurn {
            cancel: Default::default(),
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

        let child_completed = ServerMsg::Notification {
            method: "turn/completed".into(),
            params: json!({ "threadId": "child", "turn": { "id": "child-turn" } }),
        };
        assert!(!message_belongs_to_thread(&child_completed, "thread-1"));
    }

    #[test]
    fn extracts_spawned_threads_from_codex_collaboration_items() {
        let spawn = ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "root",
                "item": {
                    "type": "collabAgentToolCall",
                    "senderThreadId": "root",
                    "receiverThreadIds": ["child-1", "child-2"]
                }
            }),
        };
        assert_eq!(announced_child_threads(&spawn), vec!["child-1", "child-2"]);

        let activity = ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "root",
                "item": { "type": "subAgentActivity", "agentThreadId": "child-3" }
            }),
        };
        assert_eq!(announced_child_threads(&activity), vec!["child-3"]);
    }

    #[test]
    fn collaborator_topology_rejects_activity_that_points_back_to_an_ancestor() {
        let mut topology = CollaboratorTopology::default();
        assert!(topology.admit("root", "root", "child"));
        assert!(topology.admit("root", "child", "grandchild"));
        assert!(topology.admit("root", "root", "child"));

        let child_to_root = json!({
            "threadId": "child",
            "item": {
                "type": "subAgentActivity",
                "agentThreadId": "root",
                "agentPath": "/root",
                "kind": "interacted"
            }
        });
        let announcements = collaborator_announcements(&child_to_root);
        assert_eq!(announcements.len(), 1);
        assert_eq!(announcements[0].parent_session_id, "child");
        assert_eq!(announcements[0].session_id, "root");
        assert!(!topology.admit(
            "root",
            &announcements[0].parent_session_id,
            &announcements[0].session_id,
        ));

        assert!(!topology.admit("root", "grandchild", "child"));
        assert!(!topology.admit("root", "child", "child"));
        assert!(!topology.admit("root", "unrelated", "child"));
    }

    #[tokio::test]
    async fn child_requests_follow_the_root_route_even_when_they_arrive_first() {
        let mut routing = RoutingState::default();
        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);

        routing.route_message(ServerMsg::Request {
            id: json!(7),
            method: "mcpServer/elicitation/request".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn" }),
        });
        routing.route_message(ServerMsg::Request {
            id: json!(6),
            method: "item/fileChange/requestApproval".into(),
            params: json!({ "threadId": "root", "turnId": "stale-turn" }),
        });
        assert_eq!(routing.retired_responses.len(), 1);
        assert_eq!(routing.retired_responses[0]["id"], 6);
        assert_eq!(
            routing.retired_responses[0]["result"]["decision"],
            "decline"
        );
        assert!(root_rx.try_recv().is_err());
        assert!(routing.buffered.contains_key("child"));

        routing.route_message(ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "root",
                "turnId": "root-turn",
                "item": {
                    "type": "collabAgentToolCall",
                    "receiverThreadIds": ["child"]
                }
            }),
        });

        let ServerMsg::Notification { params, .. } = root_rx.recv().await.unwrap() else {
            panic!("parent announcement should be delivered first");
        };
        assert_eq!(params["threadId"], "root");
        let ServerMsg::Request { id, params, .. } = root_rx.recv().await.unwrap() else {
            panic!("buffered child request should follow its parent announcement");
        };
        assert_eq!(id, 7);
        assert_eq!(params["threadId"], "child");
        let owner = routing.owners.get("child").unwrap();
        assert_eq!(owner.root_thread_id, "root");
        assert_eq!(owner.root_turn_id, "root-turn");
        assert!(!routing.buffered.contains_key("child"));

        assert!(routing.remove_route_if_same("root", &root_tx, true));
        assert!(routing.owners.is_empty());
    }

    #[test]
    fn stale_parent_announcement_cannot_claim_or_delete_reused_child_requests() {
        let mut routing = RoutingState::default();
        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "replacement-turn", &root_tx);
        routing.route_message(ServerMsg::Request {
            id: json!(7),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({ "threadId": "stale-child", "turnId": "child-turn" }),
        });

        routing.route_message(ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "root",
                "turnId": "stale-turn",
                "item": {
                    "type": "collabAgentToolCall",
                    "receiverThreadIds": ["stale-child"]
                }
            }),
        });

        assert!(!routing.owners.contains_key("stale-child"));
        assert!(routing.buffered.contains_key("stale-child"));
        assert!(!routing.is_failed("stale-child"));
        assert!(root_rx.try_recv().is_err());

        routing.route_message(spawn_notification(
            "root",
            "replacement-turn",
            "stale-child",
        ));
        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Notification { .. })
        ));
        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Request { id, .. }) if id == 7
        ));
    }

    #[tokio::test]
    async fn root_announcement_without_turn_id_adopts_buffered_child_request() {
        let mut routing = RoutingState::default();
        routing.route_message(ServerMsg::Request {
            id: json!(21),
            method: "item/fileChange/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn", "itemId": "edit" }),
        });
        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);
        routing.route_message(ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "root",
                "item": {
                    "type": "collabAgentToolCall",
                    "receiverThreadIds": ["child"]
                }
            }),
        });

        assert!(matches!(
            root_rx.recv().await,
            Some(ServerMsg::Notification { .. })
        ));
        assert!(matches!(
            root_rx.recv().await,
            Some(ServerMsg::Request { id, .. }) if id == 21
        ));
        assert_eq!(routing.owners["child"].root_turn_id, "root-turn");
    }

    #[test]
    fn pre_subscription_turnless_announcement_cannot_claim_children() {
        let mut routing = RoutingState::default();
        routing.route_message(ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "root",
                "item": {
                    "type": "collabAgentToolCall",
                    "receiverThreadIds": ["stale-child"]
                }
            }),
        });
        assert!(routing.unknown_buffered.contains("root"));
        assert!(
            routing
                .unknown_buffer_order
                .iter()
                .any(|thread_id| thread_id == "root")
        );
        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        assert!(!routing.unknown_buffered.contains("root"));
        assert!(
            !routing
                .unknown_buffer_order
                .iter()
                .any(|thread_id| thread_id == "root")
        );
        routing.activate_route("root", "root-turn", &root_tx);

        assert!(!routing.owners.contains_key("stale-child"));
        assert!(root_rx.try_recv().is_err());
    }

    #[test]
    fn child_notifications_preserve_parent_transport_order() {
        let mut routing = RoutingState::default();
        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);
        routing.route_message(spawn_notification("root", "root-turn", "child"));

        for sequence in 0..3 {
            routing.route_message(ServerMsg::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({
                    "threadId": "child",
                    "turnId": "child-turn",
                    "delta": sequence.to_string()
                }),
            });
        }
        routing.route_message(ServerMsg::Notification {
            method: "thread/status/changed".into(),
            params: json!({ "threadId": "root", "turnId": "root-turn" }),
        });

        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Notification { method, .. }) if method == "item/started"
        ));
        for sequence in 0..3 {
            let ServerMsg::Notification { params, .. } = root_rx.try_recv().unwrap() else {
                panic!("collaborator notification should use the ordered turn route");
            };
            assert_eq!(params["delta"], sequence.to_string());
        }
        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Notification { method, .. }) if method == "thread/status/changed"
        ));
    }

    #[tokio::test]
    async fn notification_overflow_before_announcement_fails_projection_closed() {
        let mut routing = RoutingState::default();
        for sequence in 0..=ROUTE_EVENT_BUDGET {
            routing.route_message(ServerMsg::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({ "threadId": "child", "delta": sequence.to_string() }),
            });
        }
        routing.route_message(ServerMsg::Request {
            id: json!(9),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn", "itemId": "command" }),
        });

        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);
        routing.route_message(spawn_notification("root", "root-turn", "child"));

        assert!(matches!(
            root_rx.recv().await,
            Some(ServerMsg::Notification { .. })
        ));
        assert!(matches!(
            root_rx.recv().await,
            Some(ServerMsg::Request { id, .. }) if id == 9
        ));
        assert_eq!(
            root_tx.try_send(ServerMsg::Notification {
                method: "thread/status/changed".into(),
                params: json!({ "threadId": "root" }),
            }),
            Err(RouteSendError::Overloaded)
        );
    }

    #[test]
    fn unknown_child_buffer_preserves_nested_ownership_announcements() {
        let mut routing = RoutingState::default();
        for sequence in 0..ROUTE_EVENT_BUDGET {
            routing.route_message(ServerMsg::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({ "threadId": "child", "delta": sequence.to_string() }),
            });
        }
        routing.route_message(ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": "child",
                "turnId": "child-turn",
                "item": {
                    "type": "subAgentActivity",
                    "agentThreadId": "grandchild"
                }
            }),
        });

        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);
        routing.route_message(spawn_notification("root", "root-turn", "child"));

        assert!(root_rx.try_recv().is_ok());
        assert!(routing.owners.contains_key("grandchild"));
        assert_eq!(
            root_tx.try_send(ServerMsg::Notification {
                method: "thread/status/changed".into(),
                params: json!({ "threadId": "root" }),
            }),
            Err(RouteSendError::Overloaded)
        );
    }

    #[tokio::test]
    async fn root_notification_eviction_marks_the_active_turn_overloaded() {
        let mut routing = RoutingState::default();
        let (root_tx, root_rx) = route_channel();
        let mut overloaded = root_rx.overload_signal();
        routing.subscribe("root", root_tx.clone());
        for sequence in 0..ROUTE_EVENT_BUDGET {
            routing.route_message(ServerMsg::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({
                    "threadId": "root",
                    "turnId": "root-turn",
                    "delta": sequence.to_string()
                }),
            });
        }
        routing.route_message(ServerMsg::Request {
            id: json!(31),
            method: "item/fileChange/requestApproval".into(),
            params: json!({ "threadId": "root", "turnId": "root-turn" }),
        });

        routing.activate_route("root", "root-turn", &root_tx);
        overloaded.wait().await;
    }

    #[tokio::test]
    async fn stale_root_overflow_does_not_fail_the_replacement_turn() {
        let mut routing = RoutingState::default();
        let (root_tx, root_rx) = route_channel();
        let mut overloaded = root_rx.overload_signal();
        routing.subscribe("root", root_tx.clone());
        for sequence in 0..=ROUTE_EVENT_BUDGET {
            routing.route_message(ServerMsg::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({
                    "threadId": "root",
                    "turnId": "stale-turn",
                    "delta": sequence.to_string()
                }),
            });
        }

        routing.activate_route("root", "replacement-turn", &root_tx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), overloaded.wait())
                .await
                .is_err()
        );
        assert!(
            root_tx
                .try_send(ServerMsg::Notification {
                    method: "thread/status/changed".into(),
                    params: json!({ "threadId": "root" }),
                })
                .is_ok()
        );
    }

    #[tokio::test]
    async fn pre_subscription_request_loss_fails_root_activation() {
        let mut routing = RoutingState::default();
        for id in 0..=ROUTE_EVENT_BUDGET {
            routing.route_message(ServerMsg::Request {
                id: json!(id),
                method: "item/fileChange/requestApproval".into(),
                params: json!({ "threadId": "root", "itemId": id.to_string() }),
            });
        }
        let (root_tx, root_rx) = route_channel();
        let mut overloaded = root_rx.overload_signal();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);

        overloaded.wait().await;
    }

    #[test]
    fn unknown_thread_buffers_are_globally_bounded() {
        let mut routing = RoutingState::default();
        for id in 0..=UNKNOWN_BUFFER_BUDGET {
            routing.route_message(ServerMsg::Request {
                id: json!(id),
                method: "item/commandExecution/requestApproval".into(),
                params: json!({ "threadId": format!("orphan-{id}") }),
            });
        }

        assert_eq!(routing.unknown_buffered.len(), UNKNOWN_BUFFER_BUDGET);
        assert_eq!(routing.unknown_buffer_order.len(), UNKNOWN_BUFFER_BUDGET);
        assert!(!routing.buffered.contains_key("orphan-0"));
        assert!(
            routing
                .buffered
                .contains_key(&format!("orphan-{UNKNOWN_BUFFER_BUDGET}"))
        );
        assert_eq!(routing.retired_responses.len(), 1);
        assert_eq!(routing.retired_responses[0]["id"], 0);
        assert_eq!(
            routing.retired_responses[0]["result"]["decision"],
            "decline"
        );
    }

    #[test]
    fn retired_child_turn_is_declined_while_reused_child_turn_routes() {
        let mut routing = RoutingState::default();
        let (first_tx, mut first_rx) = route_channel();
        routing.subscribe("root", first_tx.clone());
        routing.activate_route("root", "first-root-turn", &first_tx);
        routing.route_message(spawn_notification(
            "root",
            "first-root-turn",
            "reused-child",
        ));
        assert!(first_rx.try_recv().is_ok());
        routing.route_message(ServerMsg::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({
                "threadId": "reused-child",
                "turnId": "old-child-turn",
                "delta": "old"
            }),
        });
        assert!(routing.remove_route_if_same("root", &first_tx, false));

        let (second_tx, mut second_rx) = route_channel();
        routing.subscribe("root", second_tx.clone());
        routing.activate_route("root", "second-root-turn", &second_tx);
        routing.route_message(ServerMsg::Request {
            id: json!(71),
            method: "item/fileChange/requestApproval".into(),
            params: json!({
                "threadId": "reused-child",
                "turnId": "old-child-turn"
            }),
        });
        routing.route_message(ServerMsg::Request {
            id: json!(72),
            method: "item/fileChange/requestApproval".into(),
            params: json!({
                "threadId": "reused-child",
                "turnId": "new-child-turn"
            }),
        });
        routing.route_message(spawn_notification(
            "root",
            "second-root-turn",
            "reused-child",
        ));

        assert!(matches!(
            second_rx.try_recv(),
            Ok(ServerMsg::Notification { .. })
        ));
        assert!(matches!(
            second_rx.try_recv(),
            Ok(ServerMsg::Request { id, .. }) if id == 72
        ));
        assert!(second_rx.try_recv().is_err());
        assert_eq!(routing.retired_responses.len(), 1);
        assert_eq!(routing.retired_responses[0]["id"], 71);
        assert_eq!(
            routing.owners["reused-child"].child_turn_id.as_deref(),
            Some("new-child-turn")
        );
    }

    #[test]
    fn unresolved_retired_child_rejects_preannouncement_traffic_once() {
        let mut routing = RoutingState::default();
        let (first_tx, mut first_rx) = route_channel();
        routing.subscribe("root", first_tx.clone());
        routing.activate_route("root", "first-root-turn", &first_tx);
        routing.route_message(spawn_notification(
            "root",
            "first-root-turn",
            "unresolved-child",
        ));
        assert!(first_rx.try_recv().is_ok());
        assert!(routing.remove_route_if_same("root", &first_tx, false));
        assert!(
            routing
                .retired_unbound_children
                .contains("unresolved-child")
        );

        let (second_tx, mut second_rx) = route_channel();
        routing.subscribe("root", second_tx.clone());
        routing.activate_route("root", "second-root-turn", &second_tx);
        routing.route_message(ServerMsg::Request {
            id: json!(81),
            method: "item/fileChange/requestApproval".into(),
            params: json!({
                "threadId": "unresolved-child",
                "turnId": "possibly-retired-turn"
            }),
        });
        routing.route_message(spawn_notification(
            "root",
            "second-root-turn",
            "unresolved-child",
        ));

        assert!(matches!(
            second_rx.try_recv(),
            Ok(ServerMsg::Notification { .. })
        ));
        assert!(second_rx.try_recv().is_err());
        assert_eq!(routing.retired_responses.len(), 1);
        assert_eq!(routing.retired_responses[0]["id"], 81);
        assert_eq!(routing.owners["unresolved-child"].child_turn_id, None);

        routing.route_message(ServerMsg::Request {
            id: json!(82),
            method: "item/fileChange/requestApproval".into(),
            params: json!({
                "threadId": "unresolved-child",
                "turnId": "fresh-child-turn"
            }),
        });
        assert!(matches!(
            second_rx.try_recv(),
            Ok(ServerMsg::Request { id, .. }) if id == 82
        ));
    }

    #[test]
    fn repeated_live_child_announcement_preserves_bound_turn() {
        let mut routing = RoutingState::default();
        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);
        routing.route_message(spawn_notification("root", "root-turn", "child"));
        assert!(root_rx.try_recv().is_ok());
        routing.route_message(ServerMsg::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn", "delta": "live" }),
        });

        routing.route_message(spawn_notification("root", "root-turn", "child"));
        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Notification { method, params })
                if method == "item/agentMessage/delta" && params["delta"] == "live"
        ));
        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Notification { method, .. }) if method == "item/started"
        ));
        assert_eq!(
            routing.owners["child"].child_turn_id.as_deref(),
            Some("child-turn")
        );
        assert!(!routing.retired_children.contains(&ChildTurnKey {
            thread_id: "child".into(),
            turn_id: "child-turn".into(),
        }));

        routing.route_message(ServerMsg::Request {
            id: json!(83),
            method: "item/fileChange/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn" }),
        });
        assert!(matches!(
            root_rx.try_recv(),
            Ok(ServerMsg::Request { id, .. }) if id == 83
        ));
    }

    #[test]
    fn retired_child_turns_and_decline_responses_are_bounded() {
        let mut routing = RoutingState::default();
        for sequence in 0..=ROUTE_TOMBSTONE_BUDGET {
            routing.retire_child("child".into(), format!("turn-{sequence}"));
        }
        assert_eq!(routing.retired_children.len(), ROUTE_TOMBSTONE_BUDGET);
        assert_eq!(routing.retired_child_order.len(), ROUTE_TOMBSTONE_BUDGET);
        assert!(!routing.retired_children.contains(&ChildTurnKey {
            thread_id: "child".into(),
            turn_id: "turn-0".into(),
        }));

        for id in 0..=ROUTE_EVENT_BUDGET {
            routing.reject_retired_request(&ServerMsg::Request {
                id: json!(id),
                method: "item/fileChange/requestApproval".into(),
                params: json!({}),
            });
        }
        assert_eq!(routing.retired_responses.len(), ROUTE_EVENT_BUDGET);
        assert!(routing.retired_response_overloaded);

        let mut unresolved = RoutingState::default();
        for sequence in 0..=ROUTE_TOMBSTONE_BUDGET {
            unresolved.retire_unbound_child(format!("child-{sequence}"));
        }
        assert_eq!(
            unresolved.retired_unbound_children.len(),
            ROUTE_TOMBSTONE_BUDGET
        );
        assert_eq!(
            unresolved.retired_unbound_child_order.len(),
            ROUTE_TOMBSTONE_BUDGET
        );
        assert!(!unresolved.retired_unbound_children.contains("child-0"));
    }

    #[test]
    fn reused_child_tombstone_keeps_exact_fifo_membership() {
        let mut routing = RoutingState::default();
        routing.retire_child("child".into(), "reused-turn".into());
        routing.unretire_child("child", "reused-turn");
        for sequence in 0..ROUTE_TOMBSTONE_BUDGET - 1 {
            routing.retire_child("child".into(), format!("other-{sequence}"));
        }
        routing.retire_child("child".into(), "reused-turn".into());

        assert_eq!(routing.retired_children.len(), ROUTE_TOMBSTONE_BUDGET);
        assert_eq!(routing.retired_child_order.len(), ROUTE_TOMBSTONE_BUDGET);
        assert!(routing.retired_children.contains(&ChildTurnKey {
            thread_id: "child".into(),
            turn_id: "reused-turn".into(),
        }));
    }

    #[tokio::test]
    async fn activation_learns_buffered_parent_announcements() {
        let mut routing = RoutingState::default();
        routing.route_message(spawn_notification("root", "root-turn", "child"));
        routing.route_message(ServerMsg::Request {
            id: json!(11),
            method: "item/fileChange/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn", "itemId": "edit" }),
        });

        let (root_tx, mut root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        assert!(root_rx.try_recv().is_err());
        routing.activate_route("root", "root-turn", &root_tx);

        assert!(matches!(
            root_rx.recv().await,
            Some(ServerMsg::Notification { .. })
        ));
        assert!(matches!(
            root_rx.recv().await,
            Some(ServerMsg::Request { id, .. }) if id == 11
        ));
        assert!(routing.owners.contains_key("child"));
        assert!(!routing.buffered.contains_key("child"));
    }

    #[test]
    fn activation_rejects_missing_or_replaced_route() {
        let mut routing = RoutingState::default();
        let (stale_tx, _stale_rx) = route_channel();
        assert!(!routing.activate_route("root", "turn", &stale_tx));

        let (replacement_tx, _replacement_rx) = route_channel();
        routing.subscribe("root", replacement_tx);
        assert!(!routing.activate_route("root", "turn", &stale_tx));
    }

    #[test]
    fn route_failure_cleans_descendant_ownership_and_buffers() {
        let mut routing = RoutingState::default();
        let (root_tx, root_rx) = route_channel();
        routing.subscribe("root", root_tx.clone());
        routing.activate_route("root", "root-turn", &root_tx);
        routing.route_message(spawn_notification("root", "root-turn", "child"));
        drop(root_rx);

        routing.route_message(ServerMsg::Request {
            id: json!(12),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "child-turn", "itemId": "command" }),
        });

        assert!(!routing.routes.contains_key("root"));
        assert!(!routing.owners.contains_key("child"));
        assert!(!routing.buffered.contains_key("child"));
        assert!(routing.is_failed("root"));
        assert!(routing.retired_children.contains(&ChildTurnKey {
            thread_id: "child".into(),
            turn_id: "child-turn".into(),
        }));
    }

    #[tokio::test]
    async fn clean_teardown_allows_reused_child_request_before_announcement() {
        let mut routing = RoutingState::default();
        let (first_tx, mut first_rx) = route_channel();
        routing.subscribe("root", first_tx.clone());
        routing.activate_route("root", "first-turn", &first_tx);
        routing.route_message(spawn_notification("root", "first-turn", "child"));
        assert!(first_rx.try_recv().is_ok());
        routing.route_message(ServerMsg::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({ "threadId": "child", "turnId": "old-child-turn", "delta": "old" }),
        });
        assert!(routing.remove_route_if_same("root", &first_tx, false));
        assert!(!routing.is_failed("child"));
        assert!(!routing.owners.contains_key("child"));
        routing.route_message(ServerMsg::Request {
            id: json!(40),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "old-child-turn", "itemId": "retired-command" }),
        });
        assert_eq!(routing.retired_responses.len(), 1);
        assert_eq!(routing.retired_responses[0]["id"], 40);
        assert_eq!(
            routing.retired_responses[0]["result"]["decision"],
            "decline"
        );
        assert!(!routing.buffered.contains_key("child"));

        let (second_tx, mut second_rx) = route_channel();
        routing.subscribe("root", second_tx.clone());
        routing.activate_route("root", "second-turn", &second_tx);
        assert!(!routing.owners.contains_key("child"));
        routing.route_message(ServerMsg::Request {
            id: json!(41),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({ "threadId": "child", "turnId": "new-child-turn", "itemId": "command" }),
        });
        assert!(routing.buffered.contains_key("child"));

        routing.route_message(spawn_notification("root", "second-turn", "child"));
        assert!(matches!(
            second_rx.recv().await,
            Some(ServerMsg::Notification { .. })
        ));
        assert!(matches!(
            second_rx.recv().await,
            Some(ServerMsg::Request { id, .. }) if id == 41
        ));
    }

    #[test]
    fn clean_route_teardown_keeps_tombstones_bounded() {
        let mut routing = RoutingState::default();
        for sequence in 0..=ROUTE_TOMBSTONE_BUDGET {
            let thread_id = format!("root-{sequence}");
            let (tx, _rx) = route_channel();
            routing.subscribe(&thread_id, tx.clone());
            assert!(routing.remove_route_if_same(&thread_id, &tx, true));
        }

        assert_eq!(routing.failed.len(), ROUTE_TOMBSTONE_BUDGET);
        assert_eq!(routing.failed_order.len(), ROUTE_TOMBSTONE_BUDGET);
        assert!(!routing.is_failed("root-0"));
        assert!(routing.is_failed(&format!("root-{ROUTE_TOMBSTONE_BUDGET}")));
    }

    #[test]
    fn clearing_and_reusing_tombstones_keeps_fifo_membership_consistent() {
        let mut routing = RoutingState::default();
        routing.mark_failed("reused".into());
        routing.clear_failed("reused");
        routing.mark_failed("reused".into());
        for sequence in 0..ROUTE_TOMBSTONE_BUDGET {
            routing.mark_failed(format!("other-{sequence}"));
        }

        assert!(!routing.is_failed("reused"));
        assert_eq!(routing.failed.len(), ROUTE_TOMBSTONE_BUDGET);
        assert_eq!(routing.failed_order.len(), ROUTE_TOMBSTONE_BUDGET);
    }

    fn spawn_notification(root: &str, turn: &str, child: &str) -> ServerMsg {
        ServerMsg::Notification {
            method: "item/started".into(),
            params: json!({
                "threadId": root,
                "turnId": turn,
                "item": {
                    "type": "collabAgentToolCall",
                    "receiverThreadIds": [child]
                }
            }),
        }
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
    fn normalizes_codex_plan_replacements_as_todos() {
        let todos = codex_plan_todos(&json!({
            "plan": [
                { "step": "Inspect", "status": "completed" },
                { "step": "Implement", "status": "inProgress" },
                { "step": "Verify", "status": "pending" },
            ]
        }))
        .unwrap();

        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].id, "codex-plan:7:Inspect:1");
        assert_eq!(todos[0].status, TodoStatus::Completed);
        assert_eq!(todos[1].content, "Implement");
        assert_eq!(todos[1].status, TodoStatus::InProgress);
        assert_eq!(todos[2].status, TodoStatus::Pending);
        assert_eq!(codex_plan_todos(&json!({ "plan": [] })), Some(vec![]));
        assert!(
            codex_plan_todos(&json!({
                "plan": [{ "step": "Unknown", "status": "blocked" }]
            }))
            .is_none()
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
    fn routes_codex_commentary_as_progress() {
        let commentary = HashSet::from(["commentary-1".to_string()]);
        let params = json!({ "itemId": "commentary-1", "delta": "Checking the parser." });
        assert!(matches!(
            agent_message_delta(&params, &commentary),
            Some(BackendEvent::ProgressDelta(text)) if text == "Checking the parser."
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

    #[test]
    fn collaborator_announcements_preserve_parent_prompt_and_model_metadata() {
        let params = json!({
            "threadId": "root",
            "item": {
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "senderThreadId": "root",
                "receiverThreadIds": ["child"],
                "prompt": "Inspect the router",
                "model": "gpt-5.6-sol",
                "reasoningEffort": "max",
                "agentsStates": {
                    "child": {
                        "status": "running",
                        "name": "Router reviewer"
                    }
                }
            }
        });

        let announcements = collaborator_announcements(&params);
        assert_eq!(announcements.len(), 1);
        let child = &announcements[0];
        assert_eq!(child.session_id, "child");
        assert_eq!(child.parent_session_id, "root");
        assert_eq!(child.name.as_deref(), Some("Router reviewer"));
        assert_eq!(child.prompt.as_deref(), Some("Inspect the router"));
        assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(child.thinking_level.as_deref(), Some("max"));
        assert_eq!(
            child.access,
            BackendCollaboratorAccess::Inherit,
            "a display name must not be interpreted as an authorization role"
        );

        let current = json!({
            "threadId": "root",
            "item": {
                "type": "collabToolCall",
                "tool": "spawn_agent",
                "senderThreadId": "root",
                "newThreadId": "current-child",
                "prompt": "Inspect the current protocol"
            }
        });
        let announcements = collaborator_announcements(&current);
        assert_eq!(announcements.len(), 1);
        assert_eq!(announcements[0].session_id, "current-child");
        assert_eq!(announcements[0].parent_session_id, "root");
        assert_eq!(
            announcements[0].prompt.as_deref(),
            Some("Inspect the current protocol")
        );
        assert_eq!(
            announced_child_threads(&ServerMsg::Notification {
                method: "item/started".into(),
                params: current,
            }),
            vec!["current-child"]
        );

        let live_activity = json!({
            "threadId": "root",
            "item": {
                "id": "spawn-call",
                "type": "subAgentActivity",
                "kind": "started",
                "agentThreadId": "activity-child",
                "agentPath": "/root/reviewer"
            }
        });
        let announcements = collaborator_announcements(&live_activity);
        assert_eq!(announcements.len(), 1);
        assert_eq!(announcements[0].session_id, "activity-child");
        assert_eq!(announcements[0].parent_session_id, "root");
        assert_eq!(announcements[0].name.as_deref(), Some("reviewer"));
        assert_eq!(announcements[0].prompt, None);
        assert_eq!(
            announcements[0].access,
            BackendCollaboratorAccess::Inherit,
            "agentPath is presentation metadata, not an authorization input"
        );

        let worker = json!({
            "threadId": "root",
            "item": {
                "type": "collabToolCall",
                "tool": "spawn_agent",
                "senderThreadId": "root",
                "newThreadId": "worker-child",
                "agent_type": "worker",
                "prompt": "Implement the focused fix"
            }
        });
        let announcements = collaborator_announcements(&worker);
        assert_eq!(
            announcements[0].access,
            BackendCollaboratorAccess::Interactive
        );
    }

    #[test]
    fn collaborator_access_labels_cover_provider_role_variants() {
        for label in [
            "exploration",
            "auditing",
            "implementation reviewer",
            "read-only",
            "transcript_only",
        ] {
            assert_eq!(
                collaborator_access_label(label),
                BackendCollaboratorAccess::ReadOnly,
                "{label}"
            );
        }
        for label in ["implementation", "interactive worker", "general"] {
            assert_eq!(
                collaborator_access_label(label),
                BackendCollaboratorAccess::Interactive,
                "{label}"
            );
        }
        assert_eq!(
            collaborator_access_label("specialist"),
            BackendCollaboratorAccess::Inherit
        );

        let descriptive = json!({
            "name": "implementation worker",
            "taskName": "Write the patch",
            "agentPath": "/root/code"
        });
        assert_eq!(
            collaborator_access(&descriptive, "child"),
            BackendCollaboratorAccess::Inherit,
            "display names and task paths must not grant authority"
        );
        assert_eq!(
            collaborator_access(&json!({ "readOnly": true }), "child"),
            BackendCollaboratorAccess::ReadOnly
        );
    }

    #[test]
    fn root_completion_waits_for_every_nested_collaborator() {
        let mut lifecycle = CollaboratorLifecycle::default();
        lifecycle.announce("direct-a");
        lifecycle.announce("direct-b");
        lifecycle.announce("nested-b-1");
        assert!(!lifecycle.root_can_finish());

        lifecycle.observe(
            "direct-a",
            &BackendCollaboratorEvent::Completed {
                usage: Usage::default(),
            },
        );
        lifecycle.observe(
            "direct-b",
            &BackendCollaboratorEvent::Completed {
                usage: Usage::default(),
            },
        );
        assert!(
            !lifecycle.root_can_finish(),
            "a terminal direct-child set must not hide an active grandchild"
        );

        lifecycle.observe(
            "nested-b-1",
            &BackendCollaboratorEvent::Completed {
                usage: Usage::default(),
            },
        );
        assert!(lifecycle.root_can_finish());

        // A follow-up announcement reopens a bounded handoff window so the
        // root cannot close before the reused child publishes TurnStarted.
        lifecycle.announce("nested-b-1");
        assert!(!lifecycle.root_can_finish());
        std::thread::sleep(COLLABORATOR_START_GRACE + std::time::Duration::from_millis(5));
        assert!(lifecycle.root_can_finish());

        lifecycle.announce("nested-b-1");
        lifecycle.observe("nested-b-1", &BackendCollaboratorEvent::TurnStarted);
        std::thread::sleep(COLLABORATOR_START_GRACE + std::time::Duration::from_millis(5));
        assert!(!lifecycle.root_can_finish());
        lifecycle.observe(
            "nested-b-1",
            &BackendCollaboratorEvent::Completed {
                usage: Usage::default(),
            },
        );
        assert!(lifecycle.root_can_finish());
    }

    #[test]
    fn collaborator_turn_page_recovers_latest_user_prompt() {
        let response = json!({
            "data": [{
                "id": "turn-2",
                "items": [
                    { "type": "agentMessage", "id": "old-output" },
                    {
                        "type": "userMessage",
                        "id": "spawn-prompt",
                        "content": [
                            { "type": "input_text", "text": "Inspect the router." },
                            { "type": "image", "url": "file:///tmp/reference.png" },
                            { "type": "inputText", "text": "Report only actionable issues." },
                            { "type": "input_text", "text": "" }
                        ]
                    },
                    {
                        "type": "userMessage",
                        "id": "later-steer",
                        "content": [{ "type": "text", "text": "Also inspect the tests." }]
                    },
                    { "type": "reasoning", "id": "thought" }
                ]
            }]
        });

        assert_eq!(
            collaborator_prompt_from_turn_page(&response).as_deref(),
            Some("Inspect the router.\nReport only actionable issues.")
        );
    }

    #[test]
    fn collaborator_turn_page_without_user_message_has_no_prompt() {
        let response = json!({
            "data": [{
                "id": "turn-2",
                "items": [{ "type": "agentMessage", "id": "output" }]
            }]
        });

        assert_eq!(collaborator_prompt_from_turn_page(&response), None);
    }

    #[test]
    fn collaborator_parser_keeps_child_thinking_tools_and_completion_scoped() {
        let mut state = CollaboratorStreamState::default();
        assert!(matches!(
            collaborator_notification(
                "turn/started",
                &json!({ "turn": { "id": "child-turn" } }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::TurnStarted]
        ));
        assert!(matches!(
            collaborator_notification(
                "item/started",
                &json!({
                    "item": {
                        "id": "prompt",
                        "type": "userMessage",
                        "content": [{ "type": "input_text", "text": "Inspect the router" }]
                    }
                }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::UserMessage(text)] if text == "Inspect the router"
        ));
        assert!(
            collaborator_notification(
                "item/completed",
                &json!({
                    "item": {
                        "id": "prompt",
                        "type": "userMessage",
                        "content": [{ "type": "input_text", "text": "Inspect the router" }]
                    }
                }),
                &mut state,
            )
            .is_empty(),
            "the completed item must not repeat its started user message"
        );
        assert!(
            collaborator_notification(
                "item/started",
                &json!({
                    "item": { "id": "thought", "type": "agentMessage", "phase": "commentary" }
                }),
                &mut state,
            )
            .is_empty()
        );
        assert!(matches!(
            collaborator_notification(
                "item/agentMessage/delta",
                &json!({ "itemId": "thought", "delta": "Checking." }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::ProgressDelta(text)] if text == "Checking."
        ));
        assert!(matches!(
            collaborator_notification(
                "item/completed",
                &json!({
                    "item": { "id": "thought", "type": "agentMessage", "phase": "commentary" }
                }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::ProgressCompleted]
        ));
        assert!(matches!(
            collaborator_notification(
                "item/started",
                &json!({ "item": { "id": "command", "type": "commandExecution" } }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::ToolStarted { call_id, tool, .. }]
                if call_id == "command" && tool == "commandExecution"
        ));
        collaborator_notification(
            "thread/tokenUsage/updated",
            &json!({
                "tokenUsage": {
                    "last": {
                        "inputTokens": 12,
                        "cachedInputTokens": 3,
                        "outputTokens": 4
                    }
                }
            }),
            &mut state,
        );
        assert!(matches!(
            collaborator_notification(
                "turn/completed",
                &json!({ "turn": { "status": "completed" } }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::Completed { usage }]
                if usage.input_tokens == 9
                    && usage.cached_input_tokens == 3
                    && usage.output_tokens == 4
        ));
        assert!(matches!(
            collaborator_notification(
                "turn/completed",
                &json!({ "turn": { "status": "interrupted" } }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::Failed { error }] if error == "turn cancelled"
        ));
        assert!(matches!(
            collaborator_notification(
                "turn/completed",
                &json!({ "turn": { "status": "mystery" } }),
                &mut state,
            )
            .as_slice(),
            [BackendCollaboratorEvent::Failed { error }]
                if error.contains("unknown status 'mystery'")
        ));
        assert!(matches!(
            collaborator_notification("turn/completed", &json!({ "turn": {} }), &mut state)
                .as_slice(),
            [BackendCollaboratorEvent::Failed { error }]
                if error.contains("omitted its terminal status")
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn steer_turn_targets_the_exact_active_codex_turn() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-steer");
        let request_marker = std::path::PathBuf::from(format!("{}.request", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r request
printf '%s\n' "$request" > "$0.request"
echo '{"jsonrpc":"2.0","id":1,"result":{"turnId":"turn-1"}}'
cat > /dev/null
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = AppServer::spawn(stub.to_str().unwrap()).await.unwrap();
        assert!(matches!(
            server
                .steer_turn(
                    "thread-1",
                    vec![json!({ "type": "text", "text": "early" })],
                    &Default::default(),
                )
                .await,
            Err(BackendError::Protocol(message)) if message.contains("no active turn")
        ));
        server
            .active_turns
            .lock()
            .await
            .insert("thread-1".into(), "turn-1".into());
        server
            .steer_turn(
                "thread-1",
                vec![
                    json!({ "type": "text", "text": "Focus on the regression." }),
                    json!({ "type": "localImage", "path": "/tmp/screenshot.png" }),
                ],
                &Default::default(),
            )
            .await
            .unwrap();

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(request_marker).unwrap()).unwrap();
        assert_eq!(request["method"], "turn/steer");
        assert_eq!(request["params"]["threadId"], "thread-1");
        assert_eq!(request["params"]["expectedTurnId"], "turn-1");
        assert_eq!(
            request["params"]["input"],
            json!([
                { "type": "text", "text": "Focus on the regression." },
                { "type": "localImage", "path": "/tmp/screenshot.png" },
            ])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transmitted_steer_fences_its_exact_response_after_cancellation() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-steer-cancel");
        let request_marker = std::path::PathBuf::from(format!("{}.request", stub.display()));
        let release_marker = std::path::PathBuf::from(format!("{}.release", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r request
printf '%s\n' "$request" > "$0.request"
while [ ! -f "$0.release" ]; do sleep 0.01; done
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"turnId":"turn-1"}}'
sleep 60
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        server
            .active_turns
            .lock()
            .await
            .insert("thread-1".into(), "turn-1".into());
        let cancel = tokio_util::sync::CancellationToken::new();
        let steering = tokio::spawn({
            let server = server.clone();
            let cancel = cancel.clone();
            async move {
                server
                    .steer_turn(
                        "thread-1",
                        vec![json!({ "type": "text", "text": "new direction" })],
                        &cancel,
                    )
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !request_marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("steering request should reach Codex");

        cancel.cancel();
        tokio::task::yield_now().await;
        assert!(
            !steering.is_finished(),
            "transmitted steer escaped its response fence"
        );
        std::fs::write(release_marker, b"release").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), steering)
            .await
            .expect("steering did not consume its exact response")
            .unwrap()
            .unwrap();
        assert!(server.pending.lock().await.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_collaborator_lookup_does_not_wait_for_response_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-collaborator-lookup-cancel");
        let request_marker = std::path::PathBuf::from(format!("{}.request", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r request
printf '%s\n' "$request" > "$0.request"
sleep 60
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let cancel = tokio_util::sync::CancellationToken::new();
        let lookup = tokio::spawn({
            let server = server.clone();
            let cancel = cancel.clone();
            async move { server.collaborator_prompt("child", &cancel).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !request_marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("collaborator lookup request did not reach app-server");

        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), lookup)
                .await
                .expect("cancelled collaborator lookup stalled stream draining")
                .unwrap(),
            None
        );
        assert!(server.pending.lock().await.is_empty());
        assert!(
            !server.is_closed(),
            "read-only cancellation poisoned app-server"
        );
        server.terminate_transport().await.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn initialize_error_reaps_the_app_server_before_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-initialize-error");
        let starts_marker = std::path::PathBuf::from(format!("{}.starts", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys, time
starts_path = sys.argv[0] + ".starts"
try:
    with open(starts_path) as starts:
        first_process = not starts.read().strip()
except FileNotFoundError:
    first_process = True
with open(starts_path, "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize" and first_process:
        response = {
            "jsonrpc": "2.0",
            "id": mid,
            "error": {"code": -32000, "message": "initialize rejected"},
        }
        sys.stdout.write(json.dumps(response) + "\n")
        sys.stdout.flush()
        time.sleep(60)
        continue
    if method == "initialize":
        result = {}
    elif method == "initialized":
        continue
    elif method == "account/rateLimits/read":
        result = {"replacement": True}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
        let Err(error) = backend.server().await else {
            panic!("first app-server unexpectedly completed its handshake");
        };
        assert!(
            matches!(error, BackendError::Protocol(message) if message.contains("initialize rejected"))
        );
        let first_pid = std::fs::read_to_string(&starts_marker)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            !std::path::Path::new(&format!("/proc/{first_pid}")).exists(),
            "initialize error returned before the rejected app-server was reaped"
        );

        let replacement = backend.server().await.unwrap();
        assert_eq!(
            replacement
                .request("account/rateLimits/read", Value::Null)
                .await
                .unwrap()["replacement"],
            true
        );
        assert_eq!(
            std::fs::read_to_string(&starts_marker)
                .unwrap()
                .lines()
                .count(),
            2,
            "a later request did not spawn a replacement app-server"
        );
        replacement.terminate_transport().await.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Deliberately stalls cleanup while exercising cancellation.
    async fn stdout_eof_reaps_cached_server_before_single_flight_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-eof-replacement");
        let starts_marker = std::path::PathBuf::from(format!("{}.starts", stub.display()));
        let close_marker = std::path::PathBuf::from(format!("{}.close", stub.display()));
        let overlap_marker = std::path::PathBuf::from(format!("{}.overlap", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys, time
starts_path = sys.argv[0] + ".starts"
close_path = sys.argv[0] + ".close"
overlap_path = sys.argv[0] + ".overlap"
try:
    with open(starts_path) as starts:
        prior_pids = [line.strip() for line in starts if line.strip()]
except FileNotFoundError:
    prior_pids = []
first_process = not prior_pids
if prior_pids and os.path.exists("/proc/" + prior_pids[0]):
    with open(overlap_path, "w") as overlap:
        overlap.write(prior_pids[0])
        overlap.flush()
with open(starts_path, "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        result = {}
    elif method == "initialized":
        continue
    elif method == "thread/start":
        result = {"thread": {"id": "thread-1"}}
    elif method == "turn/start":
        result = {"turn": {"id": "turn-1"}}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
    if method == "turn/start" and first_process:
        while not os.path.exists(close_path):
            time.sleep(0.005)
        os.close(1)
        time.sleep(60)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CodexBackend::new(
            "codex",
            Some(stub.to_string_lossy().into_owned()),
        ));
        let first = backend.server().await.unwrap();
        let first_pid = std::fs::read_to_string(&starts_marker)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let child = first.child.lock().unwrap();
        let mut turn = bare_turn();
        turn.worktree = temp.path().to_path_buf();
        let mut stream = backend.run_turn(turn).await.unwrap();
        std::fs::write(&close_marker, b"close stdout").unwrap();

        let stream_error = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match stream.next().await {
                    Some(Err(error)) => break error,
                    Some(Ok(_)) => {}
                    None => panic!("turn stream ended without reporting stdout EOF"),
                }
            }
        })
        .await
        .expect("turn stream did not observe stdout EOF");
        assert!(matches!(
            stream_error,
            BackendError::Protocol(message) if message.contains("app-server closed")
        ));
        assert!(first.is_closed());
        assert!(
            std::path::Path::new(&format!("/proc/{first_pid}")).exists(),
            "test did not hold the stale process alive after stdout EOF"
        );

        let cleanup_cancel = tokio_util::sync::CancellationToken::new();
        let cancelled_acquisition = tokio::spawn({
            let backend = backend.clone();
            let cleanup_cancel = cleanup_cancel.clone();
            async move { backend.server_cancellable(&cleanup_cancel).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend.server.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement acquisition did not claim the spawn lock");
        cleanup_cancel.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                loop {
                    let starts = std::fs::read_to_string(&starts_marker).unwrap();
                    if starts.lines().count() > 1 {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .is_err(),
            "replacement started while the stale process reap was blocked"
        );
        assert!(
            !cancelled_acquisition.is_finished(),
            "cancellation bypassed mandatory stale-process cleanup"
        );

        drop(child);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(3), cancelled_acquisition)
                .await
                .expect("cancelled acquisition waited too long for stale reap")
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        assert_eq!(
            std::fs::read_to_string(&starts_marker)
                .unwrap()
                .lines()
                .count(),
            1,
            "cancellation during stale cleanup spawned a replacement process"
        );
        assert!(
            !std::path::Path::new(&format!("/proc/{first_pid}")).exists(),
            "cancelled replacement acquisition returned before stale reap"
        );
        assert!(
            !overlap_marker.exists(),
            "cancelled acquisition started a process alongside the stale PID"
        );

        let first_acquisition = tokio::spawn({
            let backend = backend.clone();
            async move { backend.server().await }
        });
        let second_acquisition = tokio::spawn({
            let backend = backend.clone();
            async move { backend.server().await }
        });
        let replacement =
            tokio::time::timeout(std::time::Duration::from_secs(3), first_acquisition)
                .await
                .expect("first replacement acquisition waited too long for reap")
                .unwrap()
                .unwrap();
        let shared_replacement =
            tokio::time::timeout(std::time::Duration::from_secs(3), second_acquisition)
                .await
                .expect("second replacement acquisition was not single-flight")
                .unwrap()
                .unwrap();

        assert!(Arc::ptr_eq(&replacement, &shared_replacement));
        assert_eq!(
            std::fs::read_to_string(&starts_marker)
                .unwrap()
                .lines()
                .count(),
            2,
            "concurrent callers spawned more than one replacement"
        );
        assert!(
            !std::path::Path::new(&format!("/proc/{first_pid}")).exists(),
            "replacement acquisition returned before the stale PID was reaped"
        );
        assert!(
            !overlap_marker.exists(),
            "replacement process observed the stale PID still alive"
        );
        replacement.terminate_transport().await.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn transmitted_effect_timeout_reaps_and_replaces_the_app_server() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-effect-timeout");
        let starts_marker = std::path::PathBuf::from(format!("{}.starts", stub.display()));
        let effect_marker = std::path::PathBuf::from(format!("{}.effect", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys, time
starts_path = sys.argv[0] + ".starts"
effect_path = sys.argv[0] + ".effect"
try:
    with open(starts_path) as starts:
        first_process = len(starts.readlines()) == 0
except FileNotFoundError:
    first_process = True
with open(starts_path, "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        result = {}
    elif method == "notifications/initialized":
        continue
    elif method == "thread/start" and first_process:
        with open(effect_path, "w") as effect:
            effect.write("transmitted")
            effect.flush()
        time.sleep(60)
        continue
    elif method == "thread/start":
        result = {"thread": {"id": "replacement-thread"}}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
        let first = backend.server().await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let error = first
            .request_with_cancel_timeout(
                "thread/start",
                json!({}),
                Some(&cancel),
                true,
                std::time::Duration::from_millis(250),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, BackendError::Protocol(message) if message.contains("no response"))
        );
        assert!(
            effect_marker.exists(),
            "effect request did not reach the first app-server"
        );
        assert!(first.is_closed());
        assert!(first.pending.lock().await.is_empty());
        assert!(
            first
                .child
                .lock()
                .unwrap()
                .try_wait_tree()
                .unwrap()
                .is_some(),
            "effect timeout returned before reaping the stale process tree"
        );
        assert!(matches!(
            first.request("account/rateLimits/read", Value::Null).await,
            Err(BackendError::Protocol(message)) if message.contains("transport is closed")
        ));
        assert_eq!(
            std::fs::read_to_string(&starts_marker)
                .unwrap()
                .lines()
                .count(),
            1,
            "the stale handle unexpectedly spawned or accepted another request"
        );

        let replacement = backend.server().await.unwrap();
        assert!(!Arc::ptr_eq(&first, &replacement));
        assert_eq!(
            replacement
                .request_with_cancel_timeout(
                    "thread/start",
                    json!({}),
                    Some(&tokio_util::sync::CancellationToken::new()),
                    true,
                    std::time::Duration::from_secs(1),
                )
                .await
                .unwrap()["thread"]["id"],
            "replacement-thread"
        );
        assert_eq!(
            std::fs::read_to_string(&starts_marker)
                .unwrap()
                .lines()
                .count(),
            2,
            "the closed app-server was reused instead of replaced"
        );
        replacement.terminate_transport().await.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_only_response_timeout_keeps_the_app_server_reusable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-read-timeout");
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, sys, time
request_count = 0
for line in sys.stdin:
    msg = json.loads(line)
    request_count += 1
    if request_count == 1:
        time.sleep(0.1)
    result = {"request": request_count}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg.get("id"), "result": result}) + "\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = AppServer::spawn(stub.to_str().unwrap()).await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        assert!(matches!(
            server
                .request_with_cancel_timeout(
                    "thread/turns/list",
                    Value::Null,
                    Some(&cancel),
                    false,
                    std::time::Duration::from_millis(25),
                )
                .await,
            Err(BackendError::Protocol(message)) if message.contains("no response")
        ));
        assert!(
            !server.is_closed(),
            "read-only response timeout poisoned the shared transport"
        );
        tokio::time::sleep(std::time::Duration::from_millis(125)).await;
        assert_eq!(
            server
                .request_with_cancel_timeout(
                    "account/rateLimits/read",
                    Value::Null,
                    None,
                    false,
                    std::time::Duration::from_secs(1),
                )
                .await
                .unwrap()["request"],
            2
        );
        assert!(!server.is_closed());
        server.terminate_transport().await.unwrap();
    }

    #[tokio::test]
    async fn reader_eof_releases_pending_requests_and_turn_routes() {
        let deadline = std::time::Duration::from_secs(1);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routing: Routing = Arc::new(Mutex::new(RoutingState::default()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (request_tx, request_rx) = oneshot::channel();
        pending.lock().await.insert(1, request_tx);
        let (route_tx, mut route_rx) = route_channel();
        routing.lock().await.subscribe("thread-1", route_tx);
        let (retired_response_tx, _retired_response_rx) = mpsc::channel(ROUTE_EVENT_BUDGET);

        let (mut writer, reader) = tokio::io::duplex(16);
        let task = tokio::spawn(read_stdout(
            reader,
            pending.clone(),
            routing.clone(),
            ReaderTurnState {
                active_turns: None,
                completed_turns: None,
                turn_lifecycles: None,
            },
            retired_response_tx,
            closed.clone(),
            None,
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
    async fn reader_clears_completed_marker_without_a_live_route_consumer() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routing: Routing = Arc::new(Mutex::new(RoutingState::default()));
        let active_turns: ActiveTurns = Arc::new(Mutex::new(HashMap::from([(
            "thread-1".to_string(),
            "turn-1".to_string(),
        )])));
        let completed_turns: CompletedTurns = Arc::new(Mutex::new(CompletedTurnState::default()));
        let turn_lifecycles: TurnLifecycles = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let held_lifecycle = acquire_turn_lifecycle(&turn_lifecycles, "thread-1").await;
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (response_tx, response_rx) = oneshot::channel();
        pending.lock().await.insert(2, response_tx);
        let (retired_response_tx, _retired_response_rx) = mpsc::channel(ROUTE_EVENT_BUDGET);
        let (mut writer, reader) = tokio::io::duplex(512);
        let task = tokio::spawn(read_stdout(
            reader,
            pending,
            routing,
            ReaderTurnState {
                active_turns: Some(active_turns.clone()),
                completed_turns: Some(completed_turns.clone()),
                turn_lifecycles: Some(turn_lifecycles.clone()),
            },
            retired_response_tx,
            closed,
            None,
        ));

        writer
            .write_all(
                br#"{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}}
{"jsonrpc":"2.0","id":2,"result":{"next":"response"}}
"#,
            )
            .await
            .unwrap();
        writer.flush().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let waiting = turn_lifecycles
                    .lock()
                    .unwrap()
                    .get("thread-1")
                    .map_or(0, std::sync::Weak::strong_count)
                    >= 3;
                if waiting {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completion reader must wait for startup lifecycle ownership");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), response_rx)
                .await
                .expect("completion cleanup must not block later transport responses")
                .expect("stdout reader must retain the pending response sender")
                .expect("stub response should be successful"),
            json!({ "next": "response" })
        );
        assert!(
            active_turns.lock().await.get("thread-1").is_none(),
            "completion must synchronously clear the exact active marker"
        );
        assert!(
            register_active_turn_state(
                &completed_turns,
                &active_turns,
                "thread-1",
                "turn-1",
                false,
            )
            .await
                == ActiveTurnRegistration::Completed,
            "a late start response must not republish a completed marker"
        );
        assert!(
            register_active_turn_state(&completed_turns, &active_turns, "thread-1", "turn-1", true,)
                .await == ActiveTurnRegistration::Completed,
            "startup recovery must also reject the completed marker"
        );
        active_turns
            .lock()
            .await
            .insert("thread-1".into(), "turn-1".into());
        drop(held_lifecycle);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while active_turns.lock().await.contains_key("thread-1") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stdout reader must clear a marker registered after completion arrived");

        writer.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn vacant_registration_preserves_replacement_marker() {
        let completed_turns: CompletedTurns = Arc::new(Mutex::new(CompletedTurnState::default()));
        let active_turns: ActiveTurns = Arc::new(Mutex::new(HashMap::from([(
            "thread-1".to_string(),
            "replacement-turn".to_string(),
        )])));

        assert_eq!(
            register_active_turn_state(
                &completed_turns,
                &active_turns,
                "thread-1",
                "stale-turn",
                true,
            )
            .await,
            ActiveTurnRegistration::OwnedByReplacement
        );
        assert_eq!(
            active_turns
                .lock()
                .await
                .get("thread-1")
                .map(String::as_str),
            Some("replacement-turn")
        );
    }

    #[tokio::test]
    async fn reader_preserves_replacement_marker_after_stale_completion() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routing: Routing = Arc::new(Mutex::new(RoutingState::default()));
        let active_turns: ActiveTurns = Arc::new(Mutex::new(HashMap::from([(
            "thread-1".to_string(),
            "replacement-turn".to_string(),
        )])));
        let completed_turns: CompletedTurns = Arc::new(Mutex::new(CompletedTurnState::default()));
        let turn_lifecycles: TurnLifecycles = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (route_tx, mut route_rx) = route_channel();
        {
            let mut routing = routing.lock().await;
            routing.subscribe("thread-1", route_tx.clone());
            routing.activate_route("thread-1", "old-turn", &route_tx);
        }
        let (retired_response_tx, _retired_response_rx) = mpsc::channel(ROUTE_EVENT_BUDGET);
        let (mut writer, reader) = tokio::io::duplex(512);
        let task = tokio::spawn(read_stdout(
            reader,
            pending,
            routing,
            ReaderTurnState {
                active_turns: Some(active_turns.clone()),
                completed_turns: Some(completed_turns),
                turn_lifecycles: Some(turn_lifecycles),
            },
            retired_response_tx,
            closed,
            None,
        ));

        writer
            .write_all(
                br#"{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"old-turn","status":"completed"}}}
"#,
            )
            .await
            .unwrap();
        writer.flush().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), route_rx.recv())
            .await
            .expect("stale completion should be processed")
            .expect("stale completion should reach its original route");
        assert_eq!(
            active_turns
                .lock()
                .await
                .get("thread-1")
                .map(String::as_str),
            Some("replacement-turn")
        );

        writer.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stale_turn_cleanup_preserves_replacement_route() {
        let routing: Routing = Arc::new(Mutex::new(RoutingState::default()));
        let (stale_tx, _stale_rx) = route_channel();
        let (replacement_tx, mut replacement_rx) = route_channel();
        routing
            .lock()
            .await
            .subscribe("thread-1", replacement_tx.clone());

        remove_route(&routing, "thread-1", &stale_tx).await;

        let active = routing
            .lock()
            .await
            .routes
            .get("thread-1")
            .expect("stale cleanup must preserve the replacement route")
            .tx
            .clone();
        active
            .try_send(ServerMsg::Notification {
                method: "turn/started".into(),
                params: json!({ "threadId": "thread-1" }),
            })
            .unwrap();
        assert!(replacement_rx.recv().await.is_some());

        remove_route(&routing, "thread-1", &replacement_tx).await;
        assert!(routing.lock().await.routes.is_empty());
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

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_turn_start_recovers_late_id_and_interrupts_vendor_turn() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-late-start");
        let started_marker = std::path::PathBuf::from(format!("{}.started", stub.display()));
        let interrupt_marker = std::path::PathBuf::from(format!("{}.interrupt", stub.display()));
        let release_marker = std::path::PathBuf::from(format!("{}.release", stub.display()));
        let attempted_marker = std::path::PathBuf::from(format!("{}.attempted", stub.display()));
        let acquired_marker = std::path::PathBuf::from(format!("{}.acquired", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r line
: > "$0.started"
sleep 0.05
echo '{"jsonrpc":"2.0","id":1,"result":{"turn":{"id":"late-turn"}}}'
IFS= read -r interrupt
printf '%s\n' "$interrupt" > "$0.interrupt"
while [ ! -f "$0.release" ]; do sleep 0.01; done
echo '{"jsonrpc":"2.0","id":2,"result":{}}'
cat > /dev/null
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let spawn_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let server = loop {
            match AppServer::spawn(stub.to_str().unwrap()).await {
                Ok(server) => break Arc::new(server),
                Err(BackendError::Io(error))
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && std::time::Instant::now() < spawn_deadline =>
                {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("failed to spawn late-start stub: {error}"),
            }
        };
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let starting = tokio::spawn({
            let server = server.clone();
            let route_tx = route.tx.clone();
            let cancel = cancel.clone();
            async move {
                server
                    .start_turn(
                        "root",
                        &route_tx,
                        json!({ "threadId": "root" }),
                        lifecycle,
                        &cancel,
                    )
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !started_marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stub should receive turn/start");
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !interrupt_marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup should send turn/interrupt");
        let waiter_server = server.clone();
        let waiter_attempted = attempted_marker.clone();
        let waiter_acquired = acquired_marker.clone();
        let waiter = tokio::spawn(async move {
            std::fs::write(waiter_attempted, b"").unwrap();
            let lifecycle = waiter_server.lock_turn_lifecycle("root").await;
            std::fs::write(waiter_acquired, b"").unwrap();
            lifecycle
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !attempted_marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement should attempt lifecycle acquisition");
        assert!(
            !acquired_marker.exists(),
            "replacement must not acquire lifecycle before interrupt completes"
        );
        assert!(
            !starting.is_finished(),
            "cancelled turn/start returned before interrupt acknowledgement"
        );
        std::fs::write(release_marker, b"").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let cleaned = interrupt_marker.exists()
                    && server.active_turns.lock().await.get("root").is_none()
                    && !server.routing.lock().await.routes.contains_key("root");
                if cleaned {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), starting)
                .await
                .expect("cancelled startup should return after cleanup")
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        drop(
            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .expect("replacement should acquire lifecycle after cleanup")
                .unwrap(),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_turn_start_response_terminates_transport() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-invalid-start");
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r line
echo '{"jsonrpc":"2.0","id":1,"result":{"turn":{}}}'
while IFS= read -r _; do :; done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = server
            .start_turn(
                "root",
                &route.tx,
                json!({ "threadId": "root" }),
                lifecycle,
                &cancel,
            )
            .await;

        assert!(matches!(result, Err(BackendError::Protocol(_))));
        assert!(server.is_closed());
        assert!(server.routing.lock().await.routes.is_empty());
        assert!(server.active_turns.lock().await.is_empty());
        assert!(server.child.lock().unwrap().try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_invalid_turn_start_response_terminates_transport() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-cancelled-invalid-start");
        let started_marker = std::path::PathBuf::from(format!("{}.started", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r line
: > "$0.started"
sleep 0.05
echo '{"jsonrpc":"2.0","id":1,"result":{"turn":{}}}'
while IFS= read -r _; do :; done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let start = server.start_turn(
                "root",
                &route.tx,
                json!({ "threadId": "root" }),
                lifecycle,
                &cancel,
            );
            tokio::pin!(start);
            tokio::select! {
                _ = &mut start => panic!("turn/start unexpectedly completed"),
                _ = async {
                    tokio::time::timeout(std::time::Duration::from_secs(2), async {
                        while !started_marker.exists() {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("stub should receive turn/start");
                } => {}
            }
        }

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let reaped = server
                    .child
                    .try_lock()
                    .ok()
                    .and_then(|mut child| child.try_wait().ok().flatten())
                    .is_some();
                if server.is_closed() && server.routing.lock().await.routes.is_empty() && reaped {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("invalid recovered start should terminate transport");
        assert!(server.child.lock().unwrap().try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_start_before_stdin_write_preserves_transport() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-start-before-write");
        std::fs::write(&stub, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        let stdin = server.stdin.lock().await;
        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let start = server.start_turn(
                "root",
                &route.tx,
                json!({ "threadId": "root" }),
                lifecycle,
                &cancel,
            );
            tokio::pin!(start);
            tokio::select! {
                biased;
                _ = &mut start => panic!("turn/start unexpectedly completed"),
                _ = tokio::task::yield_now() => {}
            }
        }

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let cleaned = server.pending.lock().await.is_empty()
                    && !server.routing.lock().await.routes.contains_key("root");
                if cleaned {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pre-write cancellation should clean up promptly");
        assert!(!server.is_closed());
        drop(stdin);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completion_removes_route_before_terminal_event_is_yielded() {
        use futures::StreamExt;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-completion-route-cleanup");
        std::fs::write(&stub, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        server
            .routing
            .lock()
            .await
            .activate_route("root", "root-turn", &route.tx);
        server.register_active_turn("root", "root-turn").await;
        let route_tx = route.tx.clone();
        let (_response_tx, response_rx) = oneshot::channel();
        let mut cleanup = StartedTurnGuard::new(
            server.clone(),
            "root".into(),
            route.tx.clone(),
            response_rx,
            99,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            lifecycle,
        );
        cleanup.response = None;
        cleanup.turn_id = Some("root-turn".into());
        cleanup.finish_startup();
        let held_lifecycle = server.lock_turn_lifecycle("root").await;
        let stream = turn_stream(
            server.clone(),
            "root".into(),
            "root-turn".into(),
            route,
            false,
            cleanup,
            Default::default(),
        );
        futures::pin_mut!(stream);
        route_tx
            .try_send(ServerMsg::Notification {
                method: "item/started".into(),
                params: json!({
                    "threadId": "root",
                    "turnId": "root-turn",
                    "item": { "id": "compact-1", "type": "contextCompaction" }
                }),
            })
            .unwrap();
        route_tx
            .try_send(ServerMsg::Notification {
                method: "item/completed".into(),
                params: json!({
                    "threadId": "root",
                    "turnId": "root-turn",
                    "item": {
                        "id": "compact-1",
                        "type": "contextCompaction",
                        "status": "completed"
                    }
                }),
            })
            .unwrap();
        route_tx
            .try_send(ServerMsg::Notification {
                method: "thread/tokenUsage/updated".into(),
                params: json!({
                    "threadId": "root",
                    "tokenUsage": {
                        "last": {
                            "inputTokens": 1200,
                            "cachedInputTokens": 1000,
                            "outputTokens": 50,
                            "totalTokens": 1250
                        },
                        "modelContextWindow": 272000
                    }
                }),
            })
            .unwrap();
        assert!(matches!(
            stream.next().await,
            Some(Ok(BackendEvent::CompactionStarted))
        ));
        assert!(matches!(
            stream.next().await,
            Some(Ok(BackendEvent::CompactionCompleted))
        ));
        assert!(matches!(
            stream.next().await,
            Some(Ok(BackendEvent::UsageUpdated { usage }))
                if usage.input_tokens == 200
                    && usage.cached_input_tokens == 1000
                    && usage.context_input_tokens == Some(1250)
                    && usage.context_window == Some(272000)
        ));
        route_tx
            .try_send(ServerMsg::Notification {
                method: "item/started".into(),
                params: json!({
                    "threadId": "root",
                    "turnId": "root-turn",
                    "item": { "id": "compact-2", "type": "contextCompaction" }
                }),
            })
            .unwrap();
        route_tx
            .try_send(ServerMsg::Notification {
                method: "item/completed".into(),
                params: json!({
                    "threadId": "root",
                    "turnId": "root-turn",
                    "item": {
                        "id": "compact-2",
                        "type": "contextCompaction",
                        "status": "failed"
                    }
                }),
            })
            .unwrap();
        assert!(matches!(
            stream.next().await,
            Some(Ok(BackendEvent::CompactionStarted))
        ));
        assert!(matches!(
            stream.next().await,
            Some(Ok(BackendEvent::CompactionFailed))
        ));
        route_tx
            .try_send(ServerMsg::Notification {
                method: "turn/completed".into(),
                params: json!({
                    "threadId": "root",
                    "turn": { "id": "root-turn", "status": "completed" }
                }),
            })
            .unwrap();
        // EOF may be observed immediately after the terminal line. A queued
        // completion must win over the transport-close signal.
        route_tx.mark_closed();

        let next = stream.next();
        futures::pin_mut!(next);
        tokio::select! {
            biased;
            result = &mut next => panic!("terminal handling was preempted: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        drop(held_lifecycle);
        assert!(matches!(
            next.await,
            Some(Ok(BackendEvent::Completed { .. }))
        ));
        assert!(stream.next().await.is_none());
        assert!(!server.routing.lock().await.routes.contains_key("root"));
        assert!(server.active_turns.lock().await.get("root").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_guard_interrupts_exact_turn_after_route_loss() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-route-loss-cleanup");
        let interrupt_marker = std::path::PathBuf::from(format!("{}.interrupt", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/bin/sh
IFS= read -r interrupt
printf '%s\n' "$interrupt" > "$0.interrupt"
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
cat > /dev/null
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        server.register_active_turn("root", "orphan-turn").await;
        let (_response_tx, response_rx) = oneshot::channel();
        let mut cleanup = StartedTurnGuard::new(
            server.clone(),
            "root".into(),
            route.tx.clone(),
            response_rx,
            99,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            lifecycle,
        );
        cleanup.response = None;
        cleanup.turn_id = Some("orphan-turn".into());
        server
            .routing
            .lock()
            .await
            .remove_route_if_same("root", &route.tx, false);
        drop(cleanup);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !interrupt_marker.exists()
                || server.active_turns.lock().await.contains_key("root")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("route loss must still interrupt the started vendor turn");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_start_guard_does_not_resurrect_cleared_marker() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-post-start-cleanup");
        let write_marker = std::path::PathBuf::from(format!("{}.write", stub.display()));
        std::fs::write(
            &stub,
            "#!/bin/sh\nIFS= read -r line\nprintf '%s\\n' \"$line\" > \"$0.write\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let lifecycle = server.lock_turn_lifecycle("root").await;
        let route = server.subscribe("root").await.unwrap();
        let (_response_tx, response_rx) = oneshot::channel();
        let mut cleanup = StartedTurnGuard::new(
            server.clone(),
            "root".into(),
            route.tx.clone(),
            response_rx,
            99,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            lifecycle,
        );
        cleanup.response = None;
        cleanup.turn_id = Some("completed-turn".into());
        cleanup.finish_startup();
        drop(cleanup);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while server.routing.lock().await.routes.contains_key("root") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("post-start cleanup should remove its route");
        assert!(!write_marker.exists());
        assert!(!server.active_turns.lock().await.contains_key("root"));
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_start_drop_without_runtime_invalidates_transport() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-drop-no-runtime");
        std::fs::write(&stub, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (cleanup, server, close_signal) = runtime.block_on(async {
            let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
            let lifecycle = server.lock_turn_lifecycle("root").await;
            let route = server.subscribe("root").await.unwrap();
            let close_signal = route.rx.close_signal();
            let (_response_tx, response_rx) = oneshot::channel();
            let cleanup = StartedTurnGuard::new(
                server.clone(),
                "root".into(),
                route.tx,
                response_rx,
                1,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                lifecycle,
            );
            (cleanup, server, close_signal)
        });
        drop(runtime);

        drop(cleanup);
        assert!(server.is_closed());
        assert!(server.routing.try_lock().unwrap().routes.is_empty());
        assert!(server.active_turns.try_lock().unwrap().is_empty());
        assert!(close_signal.is_closed());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_thread_spawn_failure_falls_back_despite_lock_contention() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-runtime-shutdown-cleanup");
        std::fs::write(&stub, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (server, close_signal, mut request_rx) = runtime.block_on(async {
            let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
            let route = server.subscribe("root").await.unwrap();
            let close_signal = route.rx.close_signal();
            let (request_tx, request_rx) = oneshot::channel();
            server.pending.lock().await.insert(99, request_tx);
            let routing_guard = server.routing.lock().await;
            server.invalidate_transport_now_with(|_| {
                Err(std::io::Error::other("forced cleanup thread spawn failure"))
            });
            server.invalidate_transport_now_with(|_| {
                panic!("cleanup ownership must remain with the first invalidation")
            });
            assert!(
                !close_signal.is_closed(),
                "synchronous best-effort cleanup must observe the contended routing lock"
            );
            drop(routing_guard);
            (server, close_signal, request_rx)
        });
        drop(runtime);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !close_signal.is_closed() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(close_signal.is_closed());
        assert!(matches!(
            request_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(server.is_closed());
        assert!(server.transport_cleanup_started.load(Ordering::Acquire));
        assert!(
            server.child.lock().unwrap().try_wait().unwrap().is_some(),
            "runtime fallback must reap the app-server process"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn terminating_app_server_reaps_its_descendant_process_tree() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-process-tree");
        let descendant_path = temp.path().join("descendant.pid");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nsleep 60 &\necho $! > '{}'\ncat > /dev/null\n",
                descendant_path.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = AppServer::spawn(stub.to_str().unwrap()).await.unwrap();
        let descendant = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&descendant_path)
                    && let Ok(pid) = text.trim().parse::<u32>()
                {
                    break pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("app-server stub did not publish its descendant pid");

        server.terminate_transport().await.unwrap();
        assert!(
            server
                .child
                .lock()
                .unwrap()
                .try_wait_tree()
                .unwrap()
                .is_some()
        );
        assert!(
            !std::path::Path::new(&format!("/proc/{descendant}")).exists(),
            "app-server descendant survived transport termination"
        );
    }

    #[tokio::test]
    async fn turn_route_burst_is_lossless_and_does_not_block_stdout_eof_cleanup() {
        let deadline = std::time::Duration::from_secs(1);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let routing: Routing = Arc::new(Mutex::new(RoutingState::default()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (route_tx, mut route_rx) = route_channel();
        {
            let mut routing = routing.lock().await;
            routing.subscribe("thread-1", route_tx.clone());
            routing.activate_route("thread-1", "turn-1", &route_tx);
        }
        drop(route_tx);
        let (retired_response_tx, _retired_response_rx) = mpsc::channel(ROUTE_EVENT_BUDGET);

        let (mut writer, reader) = tokio::io::duplex(256);
        let task = tokio::spawn(read_stdout(
            reader,
            pending.clone(),
            routing.clone(),
            ReaderTurnState {
                active_turns: None,
                completed_turns: None,
                turn_lifecycles: None,
            },
            retired_response_tx,
            closed.clone(),
            None,
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
        assert!(routing.lock().await.routes.is_empty());
        assert!(routing.lock().await.buffered.is_empty());
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
            ("untrusted", "danger-full-access", "dangerFullAccess")
        );
        assert_eq!(
            sandbox_policy(crate::BackendPermission::ReadOnly, "readOnly"),
            json!({ "type": "readOnly", "networkAccess": false })
        );
        assert_eq!(
            sandbox_policy(crate::BackendPermission::Ask, "dangerFullAccess"),
            json!({ "type": "dangerFullAccess" })
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
            headers: vec![("Authorization".into(), "Bearer bridge-secret".into())],
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
        assert_eq!(
            servers["trouve"]["http_headers"]["Authorization"],
            "Bearer bridge-secret"
        );
        assert!(servers["trouve"]["command"].is_null());

        // User servers alone (no bridge) still produce an override.
        turn.mcp_bridge = None;
        let config = codex_config_override(&turn);
        assert!(config["mcp_servers"]["jira"].is_object());
        assert!(config["mcp_servers"]["trouve"].is_null());
    }

    #[test]
    fn loaded_threads_are_reused_until_their_thread_settings_change() {
        let first = json!({ "trouve": { "url": "http://127.0.0.1/thread-1" } });
        let changed =
            json!({ "trouve": { "url": "http://127.0.0.1/thread-1?catalog_revision=2" } });
        let loaded = HashMap::from([(
            "thread-1".to_string(),
            LoadedThreadSettings {
                mcp_config: first.clone(),
                developer_instructions: Some("mode prompt".into()),
            },
        )]);

        assert!(loaded_thread_settings_match(
            &loaded,
            "thread-1",
            &first,
            Some("mode prompt")
        ));
        assert!(!loaded_thread_settings_match(
            &loaded,
            "thread-1",
            &changed,
            Some("mode prompt")
        ));
        assert!(!loaded_thread_settings_match(
            &loaded,
            "thread-1",
            &first,
            Some("updated mode prompt")
        ));
        assert!(!loaded_thread_settings_match(
            &loaded,
            "thread-2",
            &first,
            Some("mode prompt")
        ));
    }

    #[test]
    fn loaded_thread_cache_evicts_the_oldest_entry_at_capacity() {
        let mut loaded = LoadedThreadCache::default();
        for index in 0..=THREAD_CACHE_CAP {
            loaded.remember(
                &format!("thread-{index}"),
                json!({ "index": index }),
                Some(format!("instructions-{index}")),
            );
        }

        assert_eq!(loaded.settings.len(), THREAD_CACHE_CAP);
        assert!(!loaded.settings.contains_key("thread-0"));
        assert!(loaded.settings.contains_key("thread-1"));
        assert!(
            loaded
                .settings
                .contains_key(&format!("thread-{THREAD_CACHE_CAP}"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn releasing_a_thread_unsubscribes_and_forces_the_next_turn_to_resume() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-thread-unsubscribe");
        let methods = std::path::PathBuf::from(format!("{}.methods", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, sys
methods = sys.argv[0] + ".methods"
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialized":
        continue
    if method != "initialize":
        with open(methods, "a") as output:
            output.write(method + "\n")
            output.flush()
    response = {"jsonrpc": "2.0", "id": message.get("id"), "result": {}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = AppServer::spawn(stub.to_str().unwrap()).await.unwrap();
        let config = json!({ "mcp_servers": {} });
        server
            .mark_thread_loaded("thread-1", config.clone(), Some("instructions".into()))
            .await;
        server.release_thread("thread-1").await.unwrap();

        assert_eq!(
            std::fs::read_to_string(methods).unwrap(),
            "thread/unsubscribe\n"
        );
        assert!(
            !server
                .thread_settings_match("thread-1", &config, Some("instructions"))
                .await,
            "released thread was still treated as loaded"
        );
        server.terminate_transport().await.unwrap();
    }

    #[test]
    fn optimized_config_disables_product_surface_but_keeps_native_tools() {
        let mut turn = bare_turn();
        turn.mcp_bridge = Some(crate::McpBridgeConfig {
            url: "http://127.0.0.1:1/internal/threads/th_1/mcp?approval=0".into(),
            headers: Vec::new(),
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
        sync.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"refreshed");

        std::fs::write(&source, b"new-login").unwrap();
        std::fs::write(&isolated, b"stale-refresh").unwrap();
        sync.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");

        // Once an external login wins, later isolated changes remain unable
        // to overwrite it.
        std::fs::write(&isolated, b"later-stale-refresh").unwrap();
        sync.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");

        std::fs::remove_file(&isolated).unwrap();
        let missing = AuthSync::new(source, isolated, b"old".to_vec());
        assert_eq!(
            missing.sync().unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn auth_sync_preserves_login_interleaved_after_refresh_staging() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("auth.json");
        let isolated = temp.path().join("isolated-auth.json");
        std::fs::write(&source, b"old").unwrap();
        std::fs::write(&isolated, b"refreshed").unwrap();
        let sync = AuthSync::new(source.clone(), isolated.clone(), b"old".to_vec());

        sync.sync_with_publish_hook(|| {
            // Simulate a separately launched vendor CLI, which cannot know
            // about Trouve's lock file, completing after the refresh was
            // staged but before its atomic publication.
            std::fs::write(&source, b"new-login").unwrap();
        })
        .unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");

        std::fs::write(&isolated, b"later-stale-refresh").unwrap();
        sync.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");
    }

    #[test]
    fn auth_sync_never_clobbers_login_after_source_is_claimed() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("auth.json");
        let isolated = temp.path().join("isolated-auth.json");
        std::fs::write(&source, b"old").unwrap();
        std::fs::write(&isolated, b"refreshed").unwrap();
        let sync = AuthSync::new(source.clone(), isolated.clone(), b"old".to_vec());

        sync.sync_with_publish_hooks(
            || {},
            || {
                // The claim temporarily removes auth.json. A direct vendor
                // login can recreate it without participating in our lock.
                std::fs::write(&source, b"new-login").unwrap();
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");
        assert!(!auth_publication_backup(&source).unwrap().exists());

        std::fs::write(&isolated, b"later-stale-refresh").unwrap();
        sync.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"new-login");
    }

    #[test]
    fn auth_sync_yields_when_the_shared_auth_lock_is_busy() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("auth.json");
        let isolated_a = temp.path().join("isolated-a.json");
        let isolated_b = temp.path().join("isolated-b.json");
        std::fs::write(&source, b"old").unwrap();
        std::fs::write(&isolated_a, b"refresh-a").unwrap();
        std::fs::write(&isolated_b, b"refresh-b").unwrap();
        let sync_a = Arc::new(AuthSync::new(source.clone(), isolated_a, b"old".to_vec()));
        let sync_b = Arc::new(AuthSync::new(
            source.clone(),
            isolated_b.clone(),
            b"old".to_vec(),
        ));

        let (a_staged_tx, a_staged_rx) = std::sync::mpsc::channel();
        let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();
        let a = {
            let sync = sync_a.clone();
            std::thread::spawn(move || {
                sync.sync_with_publish_hook(|| {
                    a_staged_tx.send(()).unwrap();
                    release_a_rx.recv().unwrap();
                })
            })
        };
        a_staged_rx.recv().unwrap();

        let (b_started_tx, b_started_rx) = std::sync::mpsc::channel();
        let (b_done_tx, b_done_rx) = std::sync::mpsc::channel();
        let b = {
            let sync = sync_b.clone();
            std::thread::spawn(move || {
                b_started_tx.send(()).unwrap();
                let result = sync.sync();
                b_done_tx.send(()).unwrap();
                result
            })
        };
        b_started_rx.recv().unwrap();
        b_done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("a pending login must not stall a competing refresh");
        assert_eq!(std::fs::read(&source).unwrap(), b"old");

        release_a_tx.send(()).unwrap();
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"refresh-a");

        // B kept its old baseline when it yielded. Its retry observes A's
        // committed source and stands down instead of overwriting it.
        std::fs::write(&isolated_b, b"later-refresh-b").unwrap();
        sync_b.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"refresh-a");
    }

    #[test]
    fn auth_snapshot_fails_fast_while_an_interactive_login_owns_the_lock() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("auth.json");
        let isolated_home = temp.path().join("isolated-home");
        std::fs::write(&source, b"old").unwrap();
        std::fs::create_dir(&isolated_home).unwrap();
        let login_lock = AuthFileLock::acquire(&source).unwrap();

        let started = std::time::Instant::now();
        let error = match stage_auth_snapshot_from(source.clone(), &isolated_home) {
            Err(error) => error,
            Ok(_) => panic!("snapshot unexpectedly acquired the interactive login lock"),
        };
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "snapshot waited for an interactive login instead of yielding"
        );

        drop(login_lock);
        let snapshot = stage_auth_snapshot_from(source, &isolated_home)
            .unwrap()
            .expect("snapshot should succeed after login releases the lock");
        assert_eq!(std::fs::read(snapshot.isolated).unwrap(), b"old");
    }

    #[test]
    fn auth_sync_retries_when_atomic_publication_does_not_complete() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("auth.json");
        let isolated = temp.path().join("isolated-auth.json");
        std::fs::write(&source, b"old").unwrap();
        std::fs::write(&isolated, b"refreshed").unwrap();
        let sync = AuthSync::new(source.clone(), isolated, b"old".to_vec());

        let error = sync
            .sync_with_publish_hook(|| {
                std::fs::remove_file(&source).unwrap();
                std::fs::create_dir(&source).unwrap();
            })
            .unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);

        std::fs::remove_dir(&source).unwrap();
        std::fs::write(&source, b"old").unwrap();
        sync.sync().unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"refreshed");
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
        assert_eq!(u.input_tokens, 200);
        assert_eq!(u.cached_input_tokens, 1000);
        assert_eq!(u.context_input_tokens, Some(1250));
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.context_window, Some(272000));

        // Older flat shape still parses; no window reported means None.
        let flat = json!({ "usage": { "inputTokens": 7, "outputTokens": 3 } });
        let u = parse_usage(&flat);
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.context_input_tokens, Some(7));
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

    #[tokio::test]
    async fn listing_models_is_static_and_does_not_spawn_app_server() {
        let backend = CodexBackend::new("codex", Some("definitely-not-a-command".into()));
        let models = backend.list_models().await;
        assert_eq!(models.len(), 7);
        assert!(backend.server.lock().await.is_none());
    }

    #[test]
    fn splits_effort_suffix() {
        assert_eq!(split_effort("gpt-5.5@high"), ("gpt-5.5", Some("high")));
        assert_eq!(split_effort("gpt-5.5"), ("gpt-5.5", None));
        assert_eq!(split_effort(""), ("", None));
        assert_eq!(split_effort("gpt@"), ("gpt@", None));
    }

    #[test]
    fn trouve_catalog_owns_codex_roster_metadata_and_settings() {
        let backend = CodexBackend::new("codex", None);
        let models = backend.models();
        assert_eq!(models.len(), 7);
        let sol = models
            .iter()
            .find(|model| model.id == "codex/gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol.display_name, "GPT-5.6 Sol");
        assert_eq!(sol.context_window, 500_000);
        assert_eq!(sol.input_price_per_mtok, None);
        assert_eq!(sol.output_price_per_mtok, None);
        assert_eq!(
            sol.options_schema
                .pointer("/properties/reasoning_effort/enum")
                .unwrap(),
            &json!(["low", "medium", "high", "xhigh", "max", "ultra"])
        );
        assert_eq!(
            sol.options_schema
                .pointer("/properties/reasoning_effort/default")
                .and_then(Value::as_str),
            Some("low")
        );

        let gpt_55 = models
            .iter()
            .find(|model| model.id == "codex/gpt-5.5")
            .unwrap();
        assert_eq!(gpt_55.context_window, 400_000);
        let luna = models
            .iter()
            .find(|model| model.id == "codex/gpt-5.6-luna")
            .unwrap();
        assert_eq!(
            luna.options_schema
                .pointer("/properties/reasoning_effort/enum"),
            Some(&json!(["low", "medium", "high", "xhigh", "max"]))
        );
    }

    #[test]
    fn effect_ids_reject_blank_values() {
        assert!(thread_id_of(&json!({ "thread": { "id": "  " } }), "thread/start").is_err());
        assert!(turn_id_of(&json!({ "turn": { "id": "\n" } })).is_err());
        assert!(steered_turn_id_of(&json!({ "turnId": "\t" })).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn aborting_hanging_handshake_reaps_app_server() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-hanging-initialize");
        let marker = std::path::PathBuf::from(format!("{}.pid", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys, time
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        marker_path = sys.argv[0] + ".pid"
        pending_path = marker_path + ".pending"
        with open(pending_path, "w") as marker:
            marker.write(str(os.getpid()))
            marker.flush()
        os.replace(pending_path, marker_path)
        time.sleep(60)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CodexBackend::new(
            "codex",
            Some(stub.to_string_lossy().into_owned()),
        ));
        let startup = tokio::spawn({
            let backend = backend.clone();
            async move { backend.server().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("initialize did not reach app-server");
        let pid = std::fs::read_to_string(&marker)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        startup.abort();
        assert!(matches!(startup.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("dropped startup future left app-server alive");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelling_hanging_handshake_reaps_app_server_before_returning() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-cancelled-initialize");
        let marker = std::path::PathBuf::from(format!("{}.pid", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys, time
for line in sys.stdin:
    if json.loads(line).get("method") == "initialize":
        marker_path = sys.argv[0] + ".pid"
        pending_path = marker_path + ".pending"
        with open(pending_path, "w") as marker:
            marker.write(str(os.getpid()))
            marker.flush()
        os.replace(pending_path, marker_path)
        time.sleep(60)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CodexBackend::new(
            "codex",
            Some(stub.to_string_lossy().into_owned()),
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        let startup = tokio::spawn({
            let backend = backend.clone();
            let cancel = cancel.clone();
            async move { backend.server_cancellable(&cancel).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("initialize did not reach app-server");
        let pid = std::fs::read_to_string(&marker)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), startup)
                .await
                .expect("startup cancellation did not acknowledge cleanup")
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "startup returned cancellation before reaping app-server"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn malformed_steer_acknowledgements_reap_and_replace_app_server() {
        use std::os::unix::fs::PermissionsExt;

        for (name, steer_result) in [
            ("missing", json!({})),
            ("wrong", json!({ "turnId": "other-turn" })),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let stub = temp.path().join(format!("codex-steer-{name}"));
            let starts = std::path::PathBuf::from(format!("{}.starts", stub.display()));
            let script = r#"#!/usr/bin/env python3
import json, os, sys, time
starts_path = sys.argv[0] + ".starts"
try:
    with open(starts_path) as starts:
        first = len(starts.readlines()) == 0
except FileNotFoundError:
    first = True
with open(starts_path, "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    mid = message.get("id")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {}
    elif method == "turn/steer" and first:
        result = __STEER_RESULT__
    elif method == "account/rateLimits/read":
        result = {"replacement": True}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#
            .replace("__STEER_RESULT__", &steer_result.to_string());
            std::fs::write(&stub, script).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

            let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
            let first = backend.server().await.unwrap();
            first
                .active_turns
                .lock()
                .await
                .insert("thread-1".into(), "turn-1".into());
            assert!(matches!(
                first
                    .steer_turn(
                        "thread-1",
                        vec![json!({ "type": "text", "text": "hi" })],
                        &Default::default()
                    )
                    .await,
                Err(BackendError::Protocol(_))
            ));
            assert!(first.is_closed());
            assert!(
                first
                    .child
                    .lock()
                    .unwrap()
                    .try_wait_tree()
                    .unwrap()
                    .is_some()
            );

            let replacement = backend.server().await.unwrap();
            assert_eq!(
                replacement
                    .request("account/rateLimits/read", Value::Null)
                    .await
                    .unwrap()["replacement"],
                true
            );
            assert_eq!(std::fs::read_to_string(starts).unwrap().lines().count(), 2);
            replacement.terminate_transport().await.unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn malformed_thread_successes_fail_closed() {
        use std::os::unix::fs::PermissionsExt;

        for (name, session, result) in [
            ("fresh", None, json!({ "thread": {} })),
            (
                "resume",
                Some("persisted-thread".to_string()),
                json!({ "thread": { "id": "wrong-thread" } }),
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let stub = temp.path().join(format!("codex-malformed-{name}"));
            let pid_marker = std::path::PathBuf::from(format!("{}.pid", stub.display()));
            let script = r#"#!/usr/bin/env python3
import json, os, sys, time
with open(sys.argv[0] + ".pid", "w") as marker:
    marker.write(str(os.getpid()))
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    mid = message.get("id")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {}
    elif method in ("thread/start", "thread/resume"):
        result = __THREAD_RESULT__
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
    time.sleep(0.01)
"#
            .replace("__THREAD_RESULT__", &result.to_string());
            std::fs::write(&stub, script).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

            let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
            let mut turn = bare_turn();
            turn.worktree = temp.path().to_path_buf();
            turn.session = session;
            assert!(matches!(
                backend.run_turn(turn).await,
                Err(BackendError::Protocol(_))
            ));
            let pid = std::fs::read_to_string(pid_marker)
                .unwrap()
                .parse::<u32>()
                .unwrap();
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fresh_thread_is_reported_before_post_response_cancellation() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-fresh-cancel");
        let marker = std::path::PathBuf::from(format!("{}.started", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    mid = message.get("id")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {}
    elif method == "thread/start":
        result = {"thread": {"id": "fresh-thread"}}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
    if method == "thread/start":
        open(sys.argv[0] + ".started", "w").close()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CodexBackend::new(
            "codex",
            Some(stub.to_string_lossy().into_owned()),
        ));
        let server = backend.server().await.unwrap();
        let lifecycle = server.lock_turn_lifecycle("fresh-thread").await;
        let mut turn = bare_turn();
        turn.worktree = temp.path().to_path_buf();
        let cancel = turn.cancel.clone();
        let running = tokio::spawn({
            let backend = backend.clone();
            async move { backend.run_turn(turn).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("thread/start did not complete");
        cancel.cancel();
        let mut events = running.await.unwrap().unwrap();
        drop(lifecycle);
        assert!(matches!(
            events.next().await,
            Some(Ok(BackendEvent::SessionStarted { session_id })) if session_id == "fresh-thread"
        ));
        assert!(matches!(
            events.next().await,
            Some(Err(BackendError::Cancelled))
        ));
        server.terminate_transport().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fresh_thread_is_reported_before_interrupt_setup_failure() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-fresh-interrupt-error");
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    mid = message.get("id")
    if method == "initialized":
        continue
    if method == "initialize":
        response = {"jsonrpc": "2.0", "id": mid, "result": {}}
    elif method == "thread/start":
        response = {"jsonrpc": "2.0", "id": mid, "result": {"thread": {"id": "fresh-thread"}}}
    elif method == "turn/interrupt":
        response = {"jsonrpc": "2.0", "id": mid, "error": {"code": -32000, "message": "interrupt rejected"}}
    else:
        response = {"jsonrpc": "2.0", "id": mid, "result": {}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
        let server = backend.server().await.unwrap();
        server
            .active_turns
            .lock()
            .await
            .insert("fresh-thread".into(), "stale-turn".into());
        let mut turn = bare_turn();
        turn.worktree = temp.path().to_path_buf();
        let mut events = backend.run_turn(turn).await.unwrap();
        assert!(matches!(
            events.next().await,
            Some(Ok(BackendEvent::SessionStarted { session_id })) if session_id == "fresh-thread"
        ));
        assert!(matches!(
            events.next().await,
            Some(Err(BackendError::Protocol(error))) if error.contains("interrupt rejected")
        ));
        server.terminate_transport().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_resume_reacquires_replacement_before_starting_fresh() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-resume-replacement");
        let starts = std::path::PathBuf::from(format!("{}.starts", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys
starts_path = sys.argv[0] + ".starts"
try:
    with open(starts_path) as starts:
        first = len(starts.readlines()) == 0
except FileNotFoundError:
    first = True
with open(starts_path, "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    mid = message.get("id")
    if method == "initialized":
        continue
    if method == "thread/resume" and first:
        os._exit(0)
    if method == "initialize":
        result = {}
    elif method == "thread/start":
        result = {"thread": {"id": "replacement-thread"}}
    elif method == "turn/start":
        result = {"turn": {"id": "replacement-turn"}}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
    if method == "turn/start":
        completed = {
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "replacement-thread",
                "turn": {"id": "replacement-turn", "status": "completed"},
            },
        }
        sys.stdout.write(json.dumps(completed) + "\n")
        sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
        let mut turn = bare_turn();
        turn.worktree = temp.path().to_path_buf();
        turn.session = Some("persisted-thread".into());
        let mut events =
            tokio::time::timeout(std::time::Duration::from_secs(3), backend.run_turn(turn))
                .await
                .expect("resume fallback stalled")
                .unwrap();
        assert!(matches!(
            events.next().await,
            Some(Ok(BackendEvent::SessionStarted { session_id })) if session_id == "replacement-thread"
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match events.next().await {
                    Some(Ok(BackendEvent::Completed { .. })) => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => panic!("replacement turn failed: {error}"),
                    None => panic!("replacement turn ended without completion"),
                }
            }
        })
        .await
        .expect("replacement turn did not complete");
        assert_eq!(std::fs::read_to_string(starts).unwrap().lines().count(), 2);
        if let Some(server) = backend.server.lock().await.as_ref() {
            server.terminate_transport().await.unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_poison_keeps_closed_cached_server_and_denies_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-cleanup-poison");
        let starts = std::path::PathBuf::from(format!("{}.starts", stub.display()));
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env python3
import json, os, sys
with open(sys.argv[0] + ".starts", "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialized":
        continue
    response = {"jsonrpc": "2.0", "id": message["id"], "result": {}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = CodexBackend::new("codex", Some(stub.to_string_lossy().into_owned()));
        let server = backend.server().await.unwrap();
        server.closed.store(true, Ordering::Relaxed);
        let child = server.child.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = child.lock().unwrap();
            panic!("inject app-server child-lock poison");
        });

        let error = match backend.server().await {
            Ok(_) => panic!("cleanup failure must deny replacement startup"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("poisoned"), "{error}");
        assert_eq!(std::fs::read_to_string(&starts).unwrap().lines().count(), 1);
        let retained = backend.server.lock().await.as_ref().cloned().unwrap();
        assert!(Arc::ptr_eq(&server, &retained));

        server.child.clear_poison();
        server.terminate_transport().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_during_partial_effect_write_reaps_before_returning() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let stub = temp.path().join("codex-partial-effect-write");
        std::fs::write(&stub, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = Arc::new(AppServer::spawn(stub.to_str().unwrap()).await.unwrap());
        let cancel = tokio_util::sync::CancellationToken::new();
        let request = tokio::spawn({
            let server = server.clone();
            let cancel = cancel.clone();
            async move {
                server
                    .request_effect_cancellable(
                        "thread/start",
                        json!({ "padding": "x".repeat(4 * 1024 * 1024) }),
                        &cancel,
                    )
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if server.stdin.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("effect request never entered its transport write");
        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(3), request)
                .await
                .expect("partial-write cancellation did not acknowledge cleanup")
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        assert!(server.is_closed());
        assert!(
            server
                .child
                .lock()
                .unwrap()
                .try_wait_tree()
                .unwrap()
                .is_some()
        );
    }
}
