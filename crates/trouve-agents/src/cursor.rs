//! Cursor backend, driving `cursor-agent acp` (Agent Client Protocol).
//!
//! One `cursor-agent acp` child is spawned lazily **per worktree** (JSON-RPC
//! over stdio, like Codex's app-server) and shared by that worktree's
//! threads. Each trouve thread maps to an ACP session; turns run
//! `session/prompt` and stream `session/update` notifications.
//!
//! The child's process cwd is pinned to the worktree (`current_dir`), not
//! just passed as the ACP session `cwd`: cursor-agent has resolved relative
//! paths and run shell commands against its process cwd (ignoring the
//! session cwd), which silently edited whatever checkout trouve happened to
//! be launched from. With cwd pinned per worktree, even those fallback
//! paths land inside the session's checkout. The pool is bounded like the
//! Claude backend's (LRU cap + idle reaping); killing an idle child loses
//! nothing because sessions resume via `session/load`.
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
//!
//! Subscription usage: the CLI has no usage surface (no subcommand, no ACP
//! method), but the token it stores in `auth.json` is accepted by the
//! dashboard's Connect-RPC endpoint
//! (`aiserver.v1.DashboardService/GetCurrentPeriodUsage`), the same call
//! Cursor's own dashboard makes. Like the direct-Codex provider, this is
//! tolerated, not contracted: the endpoint is undocumented and may break or
//! be restricted at any time, and we never refresh the token ourselves —
//! when it goes stale the user runs any `cursor-agent` command, which
//! refreshes `auth.json` through the sanctioned path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, oneshot};
use trouve_protocol::{ModelInfo, Usage};
use trouve_providers::models_dev::{ModelsDevCatalog, OptionsDialect};

use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventSender, BackendEventStream, BackendLogin,
    BackendPermission, BackendStatus, BackendTurn, async_stream, binary_on_path, format_reset,
    model,
    route::{ROUTE_EVENT_BUDGET, RouteReceiver, RouteSendError, RouteSender, route_channel},
    spawn_login,
};

pub struct CursorBackend {
    id: String,
    command: String,
    api_key: Option<String>,
    pool: Arc<ServerPool>,
    catalog: Arc<ModelsDevCatalog>,
    /// Raw `cursor/list_available_models` adapter records, cached for
    /// [`MODELS_TTL`]. Catalog-covered records are canonicalized on every
    /// read so a models.dev refresh takes effect immediately.
    models_cache: Mutex<Option<(std::time::Instant, Vec<ModelInfo>)>>,
    /// The CLI's credential file, read (never written) for the usage query.
    auth_file: std::path::PathBuf,
    /// Dashboard Connect-RPC origin (overridable for tests).
    dashboard_base: String,
}

/// How long a fetched vendor model list stays fresh.
const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Most live `cursor-agent` children kept at once (one per worktree); the
/// least recently used idle one is evicted first.
const SERVER_CAP: usize = 3;
/// Idle time after which a pooled child is reaped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How often the reaper scans the pool.
const REAP_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSPORT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CANCEL_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Live `cursor-agent acp` children keyed by worktree path.
#[derive(Default)]
struct ServerPool {
    servers: Mutex<HashMap<PathBuf, Arc<AcpServer>>>,
    reaper_started: AtomicBool,
}

/// Where the dashboard Connect-RPC services live (same origin the CLI and
/// IDE talk to).
const DASHBOARD_BASE: &str = "https://api2.cursor.sh";

/// End-to-end budget for one dashboard usage query.
const USAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl CursorBackend {
    pub fn new(id: impl Into<String>, command: Option<String>, api_key: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "cursor-agent".into()),
            api_key,
            pool: Arc::new(ServerPool::default()),
            catalog: Arc::new(ModelsDevCatalog::embedded()),
            models_cache: Mutex::new(None),
            auth_file: cli_auth_file(),
            dashboard_base: DASHBOARD_BASE.into(),
        }
    }

    /// Point the usage query at a different credential file and dashboard
    /// origin (tests).
    pub fn with_dashboard(
        mut self,
        auth_file: std::path::PathBuf,
        base: impl Into<String>,
    ) -> Self {
        self.auth_file = auth_file;
        self.dashboard_base = base.into();
        self
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelsDevCatalog>) -> Self {
        self.catalog = catalog;
        self
    }

    fn canonicalize_models(&self, models: Vec<ModelInfo>) -> Vec<ModelInfo> {
        models
            .into_iter()
            .filter_map(|live| canonicalize_cursor_model(&self.catalog, &self.id, live))
            .collect()
    }

    /// The pooled child for this worktree, spawned (cwd-pinned) on first
    /// use. Dead children are dropped, and the least recently used idle one
    /// is evicted while over [`SERVER_CAP`]; busy children may overflow the
    /// cap rather than being killed mid-turn.
    async fn server_for(&self, worktree: &Path) -> Result<Arc<AcpServer>, BackendError> {
        self.server_for_with_cancel(worktree, None).await
    }

    async fn server_for_cancellable(
        &self,
        worktree: &Path,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Arc<AcpServer>, BackendError> {
        self.server_for_with_cancel(worktree, Some(cancel)).await
    }

    async fn server_for_with_cancel(
        &self,
        worktree: &Path,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<Arc<AcpServer>, BackendError> {
        self.start_reaper();
        let mut servers = match cancel {
            Some(cancel) => tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(BackendError::Cancelled),
                servers = self.pool.servers.lock() => servers,
            },
            None => self.pool.servers.lock().await,
        };
        let closed = servers
            .iter()
            .filter_map(|(path, server)| server.is_closed().then_some(path.clone()))
            .collect::<Vec<_>>();
        for path in closed {
            if let Some(server) = servers.get(&path).cloned() {
                match server.terminate().await {
                    Ok(()) => {
                        servers.remove(&path);
                    }
                    Err(error) if path == worktree => return Err(error),
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            "cursor: retaining unrelated closed server after unacknowledged cleanup: {error}"
                        );
                    }
                }
            }
        }
        if let Some(s) = servers.get(worktree) {
            if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                return Err(BackendError::Cancelled);
            }
            s.touch();
            return Ok(s.clone());
        }
        while servers.len() >= SERVER_CAP {
            let mut lru: Option<(PathBuf, Instant)> = None;
            for (path, s) in servers.iter() {
                // The pool's Arc must be the only owner. This also covers
                // the short setup window before a turn subscribes its route.
                if Arc::strong_count(s) != 1 || !s.is_idle() {
                    continue;
                }
                let used = *s.last_used.lock().unwrap();
                if lru.as_ref().is_none_or(|(_, t)| used < *t) {
                    lru = Some((path.clone(), used));
                }
            }
            let Some((path, _)) = lru else { break }; // all busy: allow overflow
            if let Some(server) = servers.get(&path).cloned() {
                match server.terminate().await {
                    Ok(()) => {
                        servers.remove(&path);
                    }
                    Err(error) => {
                        // This process remains quarantined under its own key.
                        // Let the requested key overflow the soft pool cap
                        // instead of turning an unrelated cleanup fault into
                        // a process-wide availability failure.
                        tracing::warn!(
                            path = %path.display(),
                            "cursor: retaining LRU server after unacknowledged cleanup: {error}"
                        );
                        break;
                    }
                }
            }
        }
        let s = Arc::new(AcpServer::spawn(&self.command, self.api_key.as_deref(), worktree).await?);
        let handshake = match cancel {
            Some(cancel) => tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(BackendError::Cancelled),
                result = s.handshake() => result,
            },
            None => s.handshake().await,
        };
        if let Err(error) = handshake {
            if let Err(cleanup_error) = s.terminate().await {
                servers.insert(worktree.to_path_buf(), s);
                return Err(cleanup_error);
            }
            return Err(error);
        }
        servers.insert(worktree.to_path_buf(), s.clone());
        Ok(s)
    }

    /// Any live child, for worktree-independent requests (model listing);
    /// spawns one in a neutral directory when the pool is empty.
    async fn any_server(&self) -> Result<Arc<AcpServer>, BackendError> {
        {
            let servers = self.pool.servers.lock().await;
            if let Some(s) = servers.values().find(|s| !s.is_closed()) {
                s.touch();
                return Ok(s.clone());
            }
        }
        self.server_for(&std::env::temp_dir()).await
    }

    fn start_reaper(&self) {
        if self.pool.reaper_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let pool = Arc::downgrade(&self.pool);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_INTERVAL).await;
                let Some(pool) = pool.upgrade() else { break };
                let mut servers = pool.servers.lock().await;
                let paths = servers
                    .iter()
                    .filter_map(|(path, server)| {
                        (server.is_closed()
                            || Arc::strong_count(server) == 1
                                && server.is_idle()
                                && server.last_used.lock().unwrap().elapsed() > IDLE_TIMEOUT)
                            .then_some(path.clone())
                    })
                    .collect::<Vec<_>>();
                for path in paths {
                    let Some(server) = servers.get(&path).cloned() else {
                        continue;
                    };
                    match server.terminate().await {
                        Ok(()) => {
                            servers.remove(&path);
                        }
                        Err(error) => {
                            tracing::warn!(
                                "cursor: retaining pooled server after unacknowledged cleanup: {error}"
                            );
                        }
                    }
                }
            }
        });
    }
}

#[async_trait::async_trait]
impl AgentBackend for CursorBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Cursor is a distinct serving surface, like Codex: a trouve-owned
        // static roster inherits public metadata where possible and owns
        // Cursor-only models. ACP discovery adds availability and transport
        // controls without being required to resolve configured defaults.
        self.catalog
            .provider_models("cursor", &self.id, OptionsDialect::ClaudeCli)
            .into_iter()
            .map(|mut model| {
                model.input_price_per_mtok = None;
                model.output_price_per_mtok = None;
                model
            })
            .collect()
    }

    async fn list_models(&self) -> Vec<ModelInfo> {
        let stale = {
            let cache = self.models_cache.lock().await;
            if let Some((at, models)) = cache.as_ref()
                && at.elapsed() < MODELS_TTL
            {
                return self.canonicalize_models(models.clone());
            }
            cache.as_ref().map(|(_, models)| models.clone())
        };
        let fetched = async {
            let server = self.any_server().await?;
            server
                .request("cursor/list_available_models", json!({}))
                .await
        }
        .await;
        match fetched {
            Ok(result) => {
                if !result["models"].is_array() {
                    return stale
                        .map(|models| self.canonicalize_models(models))
                        .unwrap_or_else(|| self.models());
                }
                let models = parse_acp_models(&self.id, &result);
                *self.models_cache.lock().await = Some((std::time::Instant::now(), models.clone()));
                self.canonicalize_models(models)
            }
            Err(e) => {
                tracing::debug!(
                    "cursor/list_available_models failed: {e}; using stale/static list"
                );
                stale
                    .map(|models| self.canonicalize_models(models))
                    .unwrap_or_else(|| self.models())
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

    async fn subscription_health(&self) -> Option<trouve_protocol::SubscriptionHealth> {
        // API-key providers are usage-billed per request; there is no
        // subscription allowance to meter.
        if self.api_key.is_some() {
            return Some(trouve_protocol::SubscriptionHealth {
                provider_id: self.id.clone(),
                status: "unsupported".into(),
                plan: String::new(),
                windows: Vec::new(),
                credits: String::new(),
                note: "usage-billed via API key — no subscription allowance to report.".into(),
            });
        }
        Some(match self.query_dashboard_usage().await {
            Ok((usage, plan_info)) => parse_dashboard_usage(&self.id, &usage, plan_info.as_ref()),
            Err(e) => trouve_protocol::SubscriptionHealth {
                provider_id: self.id.clone(),
                status: "unavailable".into(),
                plan: String::new(),
                windows: Vec::new(),
                credits: String::new(),
                note: format!("could not read usage from Cursor's dashboard API: {e}"),
            },
        })
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        let cancel = turn.cancel.clone();
        let server = self.server_for_cancellable(&turn.worktree, &cancel).await?;

        // Resume the ACP session for this thread, or start a fresh one. A
        // failed load (e.g. server restarted and lost it) degrades to fresh.
        let mut fresh_session = false;
        let desired_mcp_fingerprint = acp_mcp_fingerprint(&acp_mcp_servers(
            &turn.mcp_servers,
            turn.mcp_bridge.as_ref(),
            server.mcp_http.load(Ordering::Relaxed),
        )?)?;
        let known_session = match &turn.session {
            Some(sid) => tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(BackendError::Cancelled),
                known = server.session_settings_match(sid, desired_mcp_fingerprint) => known,
            },
            None => false,
        };
        let session_id = match &turn.session {
            Some(sid) if known_session => sid.clone(),
            Some(sid) => match server
                .load_session(
                    sid,
                    &turn.worktree,
                    &turn.mcp_servers,
                    turn.mcp_bridge.as_ref(),
                    &cancel,
                )
                .await
            {
                Ok(()) => sid.clone(),
                Err(e) if server.is_closed() => return Err(e),
                Err(e) => {
                    tracing::warn!("cursor session/load failed ({e}); starting fresh");
                    fresh_session = true;
                    server
                        .new_session(
                            &turn.worktree,
                            &turn.mcp_servers,
                            turn.mcp_bridge.as_ref(),
                            &cancel,
                        )
                        .await?
                }
            },
            None => {
                fresh_session = true;
                server
                    .new_session(
                        &turn.worktree,
                        &turn.mcp_servers,
                        turn.mcp_bridge.as_ref(),
                        &cancel,
                    )
                    .await?
            }
        };

        // ACP has no system-instruction update primitive. Include Trouve's
        // current rules on every prompt so resumed Cursor sessions cannot
        // retain a stale mode, skill catalog, or AGENTS.md snapshot.
        let text = match &turn.instructions {
            Some(instr) => format!(
                "<trouve-instructions>\n{instr}\n</trouve-instructions>\n\n{}",
                turn.prompt
            ),
            None => turn.prompt.clone(),
        };

        // Mode + model config, then the prompt, under the config lock:
        // cursor applies model selection process-wide (all sessions sync to
        // the current model), so racing turns must not interleave their
        // set-model and prompt-start.
        let (route, prompt_rx) = {
            let _config = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return session_setup_failure(
                        fresh_session,
                        &session_id,
                        BackendError::Cancelled,
                    );
                }
                config = server.config_lock.lock() => config,
            };

            // Cursor ACP has no true tool-free mode. Its Ask mode keeps
            // read-only turns useful with search tools while withholding edit
            // and command execution. Approval-gated and Yolo turns retain the
            // full agent surface and are governed by trouve's permission gate.
            let mode = match turn.permission {
                BackendPermission::ReadOnly => "ask",
                BackendPermission::Ask | BackendPermission::Yolo => "agent",
            };
            if let Err(e) = server
                .set_config_option(&session_id, "mode", mode, &cancel)
                .await
            {
                if matches!(e, BackendError::Cancelled) || server.is_closed() {
                    return session_setup_failure(fresh_session, &session_id, e);
                }
                tracing::warn!("cursor set mode {mode} failed: {e}");
            }

            if !turn.model.is_empty()
                && !matches!(turn.model.as_str(), "auto" | "default")
                && let Err(error) = apply_model_config(&server, &session_id, &turn, &cancel).await
            {
                return session_setup_failure(fresh_session, &session_id, error);
            }

            // ACP image content blocks carry base64 data inline.
            let mut prompt_blocks = vec![json!({ "type": "text", "text": text })];
            for att in &turn.attachments {
                prompt_blocks.push(json!({
                    "type": "image",
                    "mimeType": att.mime,
                    "data": att.base64(),
                }));
            }

            // Subscribe after session setup so a session/load's history
            // replay doesn't re-emit old text into the thread.
            let route = server.subscribe(&session_id).await;
            let prompt_rx = match server
                .request_deferred(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": prompt_blocks,
                    }),
                )
                .await
            {
                Ok(prompt_rx) => prompt_rx,
                Err(error) => {
                    server.unsubscribe(&session_id).await;
                    return session_setup_failure(fresh_session, &session_id, error);
                }
            };
            (route, prompt_rx)
        };

        let stream = turn_stream(
            server.clone(),
            session_id.clone(),
            route,
            prompt_rx,
            fresh_session,
            cancel,
        );
        Ok(stream.boxed())
    }
}

impl CursorBackend {
    /// Ask the dashboard for the current billing period's usage (and, best
    /// effort, the plan name) using the CLI's stored login token.
    async fn query_dashboard_usage(&self) -> Result<(Value, Option<Value>), BackendError> {
        let token = read_cli_token(&self.auth_file)?;
        let http = reqwest::Client::builder()
            .timeout(USAGE_TIMEOUT)
            .build()
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        // The client timeout is per request, so two sequential RPCs could
        // take ~2x the budget. Give the optional plan lookup only whatever
        // the usage call left over; when that runs out, degrade to no plan
        // name rather than stretching the deadline or failing the query.
        let started = std::time::Instant::now();
        let usage = self
            .dashboard_rpc(&http, &token, "GetCurrentPeriodUsage")
            .await?;
        let remaining = USAGE_TIMEOUT.saturating_sub(started.elapsed());
        let plan_info =
            tokio::time::timeout(remaining, self.dashboard_rpc(&http, &token, "GetPlanInfo"))
                .await
                .ok()
                .and_then(Result::ok);
        Ok((usage, plan_info))
    }

    /// One unary Connect-RPC call (JSON encoding) on the DashboardService.
    async fn dashboard_rpc(
        &self,
        http: &reqwest::Client,
        token: &str,
        method: &str,
    ) -> Result<Value, BackendError> {
        let url = format!(
            "{}/aiserver.v1.DashboardService/{method}",
            self.dashboard_base
        );
        let response = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .bearer_auth(token)
            .body("{}")
            .send()
            .await
            .map_err(|e| BackendError::Protocol(format!("{method}: {e}")))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(BackendError::Auth(
                "Cursor rejected the CLI's stored login — run any cursor-agent \
                 command (e.g. `cursor-agent status`) to refresh it, or log in again"
                    .into(),
            ));
        }
        if !status.is_success() {
            return Err(BackendError::Protocol(format!(
                "{method}: HTTP {status}: {}",
                body["message"].as_str().unwrap_or("")
            )));
        }
        Ok(body)
    }
}

/// The CLI's `auth.json` path, mirroring its own per-platform resolution
/// (Windows: `%APPDATA%\Cursor`, macOS: `~/.cursor`, else XDG config).
fn cli_auth_file() -> std::path::PathBuf {
    match std::env::consts::OS {
        "windows" => std::env::var_os("APPDATA")
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join("AppData").join("Roaming")))
            .unwrap_or_default()
            .join("Cursor")
            .join("auth.json"),
        "macos" => dirs::home_dir()
            .unwrap_or_default()
            .join(".cursor")
            .join("auth.json"),
        _ => std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap_or_default()
            .join("cursor")
            .join("auth.json"),
    }
}

/// Read the login token from the CLI's auth file, failing with actionable
/// messages. Deliberately never refreshed here (see module docs).
fn read_cli_token(path: &std::path::Path) -> Result<String, BackendError> {
    let raw = std::fs::read_to_string(path).map_err(|_| {
        BackendError::Auth(format!(
            "no Cursor CLI credentials at {} — run `cursor-agent login` first",
            path.display()
        ))
    })?;
    let auth: Value = serde_json::from_str(&raw)
        .map_err(|e| BackendError::Auth(format!("unreadable cursor auth.json: {e}")))?;
    auth["accessToken"]
        .as_str()
        .or_else(|| auth["apiKey"].as_str())
        .map(str::to_string)
        .ok_or_else(|| BackendError::Auth("cursor auth.json has no access token".into()))
}

/// Turn a `GetCurrentPeriodUsage` response (plus an optional `GetPlanInfo`
/// one) into subscription health.
///
/// Cursor's plans are dollar-metered: an included allowance per billing
/// cycle (with per-bucket percentages for the Auto tier and named/API
/// models), plus an optional on-demand spend limit on top. Amounts are USD
/// cents; int64 fields arrive as JSON strings.
fn parse_dashboard_usage(
    provider_id: &str,
    usage: &Value,
    plan_info: Option<&Value>,
) -> trouve_protocol::SubscriptionHealth {
    let plan = plan_info
        .and_then(|p| p["planInfo"]["planName"].as_str())
        .unwrap_or("")
        .to_string();
    let resets = i64_flex(&usage["billingCycleEnd"])
        .map(format_reset)
        .unwrap_or_default();

    let mut windows = Vec::new();
    let mut push = |label: &str, pct: f64| {
        windows.push(trouve_protocol::SubscriptionWindow {
            label: label.to_string(),
            used_percent: (pct.round() as i64).clamp(0, 100),
            resets: resets.clone(),
        });
    };

    let plan_usage = &usage["planUsage"];
    if let Some(pct) = plan_usage["totalPercentUsed"].as_f64() {
        push("Included usage", pct);
    }
    if let Some(pct) = plan_usage["apiPercentUsed"].as_f64() {
        push("Included (API models)", pct);
    }
    if let Some(pct) = plan_usage["autoPercentUsed"].as_f64() {
        push("Included (Auto)", pct);
    }

    // On-demand spend rides on top of the included allowance; individual
    // limits take precedence, team-pooled ones are the fallback.
    let spend = &usage["spendLimitUsage"];
    let on_demand = [
        ("individualUsed", "individualLimit"),
        ("pooledUsed", "pooledLimit"),
    ]
    .iter()
    .find_map(|(used_key, limit_key)| {
        let used = i64_flex(&spend[*used_key])?;
        let limit = i64_flex(&spend[*limit_key]).filter(|l| *l > 0)?;
        Some((used, limit))
    });
    let mut credits = String::new();
    if let Some((used, limit)) = on_demand {
        push("On-demand spend", used as f64 / limit as f64 * 100.0);
        credits = format!(
            "on-demand: ${:.2} of ${:.2}",
            used as f64 / 100.0,
            limit as f64 / 100.0
        );
    }

    if windows.is_empty() {
        return trouve_protocol::SubscriptionHealth {
            provider_id: provider_id.to_string(),
            status: "unavailable".into(),
            plan,
            windows,
            credits,
            note: "the dashboard reported no usage data — is cursor-agent logged in?".into(),
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

/// Protobuf int64 fields serialize as JSON strings in Connect's JSON
/// encoding; accept both shapes.
fn i64_flex(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}

/// Set the session's model and its config options (thinking/context/effort/
/// fast), translating trouve's stored model + options into ACP config calls.
async fn apply_model_config(
    server: &AcpServer,
    session_id: &str,
    turn: &BackendTurn,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), BackendError> {
    // Threads from before the ACP migration may still store a variant id
    // like "claude-opus-4-8-high"; peel the level back off.
    let (base, legacy_level, legacy_fast) = split_variant(&turn.model);

    let result = server
        .set_config_option(session_id, "model", base, cancel)
        .await
        .map_err(|error| match error {
            BackendError::Cancelled => BackendError::Cancelled,
            error => BackendError::Protocol(format!(
                "cursor-agent rejected model {base}: {error} \
                 (if this persists, update the CLI in Settings → Vendor CLIs)"
            )),
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
        push_cursor_model_option(&mut options, key, value);
    }
    if let Some(level) = legacy_level {
        push_cursor_model_option(&mut options, "effort", level.to_string());
    }
    if legacy_fast {
        options.push(("fast".into(), "true".into()));
    }

    // Unknown options are expected (effort vs reasoning depends on the
    // model); failures are logged, not fatal.
    for (key, value) in options {
        if let Err(e) = server
            .set_config_option(session_id, &key, &value, cancel)
            .await
        {
            if matches!(e, BackendError::Cancelled) {
                return Err(e);
            }
            tracing::debug!("cursor set_config_option {key}={value}: {e}");
        }
    }
    Ok(())
}

fn push_cursor_model_option(options: &mut Vec<(String, String)>, key: &str, value: String) {
    match key {
        // Pre-ACP threads stored the thinking dropdown under
        // thinking_level (cursor) or reasoning_effort (codex-style).
        // Static Cursor models use effort across upstream providers, so
        // normalize it through both ACP spellings as well.
        "thinking_level" | "reasoning_effort" | "effort" => {
            options.push(("effort".into(), value.clone()));
            options.push(("reasoning".into(), value));
        }
        _ => options.push((key.to_string(), value)),
    }
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
    mut route: RouteReceiver<ServerMsg>,
    mut prompt_rx: oneshot::Receiver<Result<Value, String>>,
    fresh_session: bool,
    cancel: tokio_util::sync::CancellationToken,
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
        let mut cancelled = false;
        let mut route_overloaded = false;
        let mut overload_signal = route.overload_signal();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
            }
            _ = overload_signal.wait() => {
                route_overloaded = true;
            }
            _ = async {
                loop {
                    tokio::select! {
                        msg = route.recv() => {
                            let Some(msg) = msg else { break };
                            if handle_msg(&server, msg, &tx).await.is_err() {
                                // Receiver dropped (turn cancelled): stop cursor's
                                // generation instead of letting it run headless.
                                client_gone = true;
                                break;
                            }
                        }
                        result = &mut prompt_rx => {
                            // Reader delivers in wire order, so any updates sent
                            // before the response are already queued; drain them.
                            while let Ok(msg) = route.try_recv() {
                                if handle_msg(&server, msg, &tx).await.is_err() {
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
            } => {}
        }
        if cancelled || client_gone || route_overloaded {
            // ACP cancellation itself is only a notification. Treat the
            // outstanding session/prompt response as the acknowledgement
            // that Cursor has actually stopped this turn. If it never
            // arrives, recycle the shared process before releasing the turn;
            // otherwise a replacement prompt could race stale mutation.
            let acknowledged = match server
                .notify("session/cancel", json!({ "sessionId": session_id }))
                .await
            {
                Ok(()) => cancellation_acknowledged(&mut prompt_rx).await,
                Err(error) => {
                    tracing::warn!("cursor cancellation transport cleanup failed: {error}");
                    false
                }
            };
            if !acknowledged {
                tracing::warn!(
                    "cursor did not acknowledge cancellation within {}s; recycling cursor-agent",
                    CANCEL_ACK_TIMEOUT.as_secs(),
                );
                if let Err(error) = server.terminate().await {
                    let _ = tx.send(Err(error)).await;
                    server.unsubscribe(&session_id).await;
                    return;
                }
            }
        }
        if route_overloaded {
            // Cancellation was acknowledged above (or the shared process was
            // recycled) before this failure becomes visible to the caller.
            // That prevents a replacement turn from overlapping stale work
            // from the overloaded session.
            let _ = tx
                .send(Err(BackendError::Protocol(format!(
                    "cursor-agent event backlog exceeded the per-turn limit of \
                     {ROUTE_EVENT_BUDGET} messages"
                ))))
                .await;
        }
        if cancelled || client_gone {
            // Best effort; the vendor process keeps running for other threads.
            tracing::debug!("cursor turn for {session_id} cancelled by client");
        }
        server.unsubscribe(&session_id).await;
    })
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

async fn cancellation_acknowledged(
    prompt_rx: &mut oneshot::Receiver<Result<Value, String>>,
) -> bool {
    // A closed sender means the reader cleared pending requests after EOF;
    // only an actual JSON-RPC response proves Cursor stopped the turn.
    matches!(
        tokio::time::timeout(CANCEL_ACK_TIMEOUT, prompt_rx).await,
        Ok(Ok(_))
    )
}

/// Map one routed ACP message to backend events. `Err(())` means the
/// receiving stream is gone.
async fn handle_msg(server: &AcpServer, msg: ServerMsg, tx: &BackendEventSender) -> Result<(), ()> {
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
            let mut tool_call = params["toolCall"].clone();
            let (ok_tx, ok_rx) = oneshot::channel();
            let call_id = tool_call["toolCallId"].as_str().unwrap_or("").to_string();
            // ACP permission requests are allowed to omit rawInput. Recover
            // it from the preceding tool_call update so the engine can
            // validate file targets (and show the actual arguments).
            if let Some((_, update)) = server.calls.lock().await.get(&call_id)
                && let (Some(dst), Some(src)) = (tool_call.as_object_mut(), update.as_object())
            {
                for key in ["rawInput", "locations"] {
                    if !dst.contains_key(key)
                        && let Some(value) = src.get(key)
                    {
                        dst.insert(key.to_string(), value.clone());
                    }
                }
            }
            tx.send(Ok(BackendEvent::ApprovalNeeded {
                call_id,
                tool: tool_call["kind"]
                    .as_str()
                    .or_else(|| tool_call["title"].as_str())
                    .unwrap_or("tool")
                    .to_string(),
                args: tool_call,
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
    tx: &BackendEventSender,
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
                            Some(trouve_protocol::CommandInfo {
                                usage: format!("/{name}"),
                                name,
                                description,
                                kind: trouve_protocol::CommandKind::Prompt,
                            })
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
        context_input_tokens: None,
        cost_usd: None,
        context_window: None,
    }
}

// --- model catalog -----------------------------------------------------------

/// Replace metadata for statically catalogued Cursor and public vendor models
/// while preserving live Cursor execution controls. Recognizable public ids
/// absent from the catalog are omitted rather than accepting ACP metadata as a
/// second source of truth. Brand-new Cursor-owned ids may remain additive live
/// records until they are promoted into the checked-in serving-surface roster.
fn canonicalize_cursor_model(
    catalog: &ModelsDevCatalog,
    backend_id: &str,
    live: ModelInfo,
) -> Option<ModelInfo> {
    let raw_id = live
        .id
        .strip_prefix(backend_id)
        .and_then(|id| id.strip_prefix('/'))
        .unwrap_or(&live.id)
        .to_string();
    let canonical = catalog
        .model("cursor", backend_id, &raw_id, OptionsDialect::ClaudeCli)
        .or_else(|| {
            [
                ("anthropic", OptionsDialect::ClaudeCli),
                ("openai", OptionsDialect::CodexCli),
                ("google", OptionsDialect::Gemini),
                ("xai", OptionsDialect::OpenAi),
            ]
            .into_iter()
            .find_map(|(provider, dialect)| catalog.model(provider, backend_id, &raw_id, dialect))
        });
    let Some(mut canonical) = canonical else {
        return (!looks_like_public_cursor_model(&raw_id)).then_some(live);
    };

    // The account-visible spelling is what Cursor accepts at execution time.
    canonical.id = live.id;
    canonical.input_price_per_mtok = None;
    canonical.output_price_per_mtok = None;

    // Context selection and fast mode are Cursor transport controls, not
    // public model capabilities. Cursor-owned models have no upstream option
    // schema, so ACP remains authoritative for all of their controls. Generic
    // reasoning settings on public models remain catalog-owned even when ACP
    // advertises a conflicting list/default.
    if let (Some(canonical_properties), Some(live_properties)) = (
        canonical
            .options_schema
            .pointer_mut("/properties")
            .and_then(Value::as_object_mut),
        live.options_schema
            .pointer("/properties")
            .and_then(Value::as_object),
    ) {
        if raw_id == "default" || raw_id.starts_with("composer-") {
            for (key, property) in live_properties {
                canonical_properties.insert(key.clone(), property.clone());
            }
        } else {
            for key in ["context", "fast"] {
                if let Some(property) = live_properties.get(key) {
                    canonical_properties.insert(key.into(), property.clone());
                }
            }
        }
    }
    Some(canonical)
}

fn looks_like_public_cursor_model(id: &str) -> bool {
    id.starts_with("claude-")
        || id.starts_with("gemini-")
        || id.starts_with("gpt-")
        || id.starts_with("grok-")
        || id.starts_with("chatgpt-")
        || id.starts_with("codex-")
        || id.starts_with("computer-use-")
        || ["o1", "o3", "o4"].iter().any(|family| {
            id == *family
                || id
                    .strip_prefix(*family)
                    .is_some_and(|suffix| suffix.starts_with('-'))
        })
}

/// Map a `cursor/list_available_models` result to ModelInfos: one entry per
/// model with its config options as an adapter schema. A later canonicalization
/// pass replaces public vendor metadata/settings from models.dev.
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

        let mut info = model(backend_id, id, display, context_window.unwrap_or(0));
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
type Routes = Arc<Mutex<HashMap<String, RouteSender<ServerMsg>>>>;

async fn write_reply(stdin: &Mutex<ChildStdin>, reply: Value) {
    let mut line = serde_json::to_vec(&reply).expect("serializable");
    line.push(b'\n');
    let mut stdin = stdin.lock().await;
    let _ = stdin.write_all(&line).await;
    let _ = stdin.flush().await;
}

fn acp_mcp_fingerprint(servers: &Value) -> Result<[u8; 32], BackendError> {
    use sha2::{Digest as _, Sha256};

    let encoded = serde_json::to_vec(servers).map_err(|error| {
        BackendError::Protocol(format!("serializing Cursor MCP config: {error}"))
    })?;
    Ok(Sha256::digest(encoded).into())
}

struct AcpServer {
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: Pending,
    routes: Routes,
    /// Effective MCP configuration for sessions this process created or
    /// loaded. Rotating bridge tickets and edited MCP servers must trigger a
    /// fresh session/load before the next prompt.
    sessions: Mutex<HashMap<String, [u8; 32]>>,
    /// Serializes model/mode config + prompt start: cursor applies model
    /// selection process-wide, so concurrent turns must not interleave.
    config_lock: Mutex<()>,
    /// Plan-mode plans by tool call id: `cursor/create_plan` arrives as a
    /// session-less request (answered by the reader); the stashed content
    /// becomes the plan tool's result when its completion update lands.
    plans: Arc<Mutex<HashMap<String, Value>>>,
    /// Tool call id → (session id, full update), recorded from
    /// `session/update` notifications: session-less requests like
    /// `cursor/ask_question` find their route here, and permission requests
    /// recover rawInput when cursor omits it.
    calls: Arc<Mutex<HashMap<String, (String, Value)>>>,
    /// Negotiated during initialize; an HTTP bridge is mandatory whenever a
    /// BackendTurn includes one.
    mcp_http: AtomicBool,
    /// Held so the complete process tree lives as long as the server handle;
    /// mutable ownership also lets a blocked transport be terminated and
    /// reaped. The reader holds only a `Weak`, so it cannot keep the child
    /// alive after the pool and active turns release the server.
    child: Arc<Mutex<crate::process_env::ProcessTreeChild>>,
    closed: Arc<AtomicBool>,
    transport_cleanup_started: AtomicBool,
    #[cfg(test)]
    injected_terminate_failure: AtomicBool,
    /// When the pool last handed this child to a turn; feeds idle reaping.
    last_used: std::sync::Mutex<Instant>,
}

impl Drop for AcpServer {
    fn drop(&mut self) {
        self.invalidate_transport_now();
    }
}

fn cleanup_cursor_transport_blocking(
    pending: Pending,
    routes: Routes,
    child: Arc<Mutex<crate::process_env::ProcessTreeChild>>,
) -> std::io::Result<()> {
    pending.blocking_lock().clear();
    routes.blocking_lock().clear();
    let mut child = child.blocking_lock();
    let mut terminate_error = child.terminate_now().err();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
                            terminate_error.get_or_insert(error);
                        }
                    }
                }
                if std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(terminate_error.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "cursor-agent process tree did not exit within 5s",
                    )
                }));
            }
            Err(error) => return Err(error),
        }
    }
}

impl AcpServer {
    fn invalidate_transport_now(&self) {
        self.invalidate_transport_now_with(|cleanup| {
            std::thread::Builder::new()
                .name("cursor-transport-cleanup".into())
                .spawn(cleanup)
                .map(|_| ())
        });
    }

    fn invalidate_transport_now_with<F>(&self, spawn_cleanup: F)
    where
        F: FnOnce(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>,
    {
        self.closed.store(true, Ordering::Relaxed);
        // Cancellation may drop the future that is handshaking a newly
        // spawned server. Signal it synchronously, then release waiters and
        // reap it from an OS thread that does not depend on Tokio surviving.
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.terminate_now();
        }
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.clear();
        }
        if let Ok(mut routes) = self.routes.try_lock() {
            routes.clear();
        }
        if self.transport_cleanup_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let child = self.child.clone();
        let pending = self.pending.clone();
        let routes = self.routes.clone();
        if let Err(error) = spawn_cleanup(Box::new(move || {
            let _ = cleanup_cursor_transport_blocking(pending, routes, child);
        })) {
            tracing::error!("cursor: failed to start transport cleanup thread: {error}");
            let child = self.child.clone();
            let pending = self.pending.clone();
            let routes = self.routes.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let _ = cleanup_cursor_transport_blocking(pending, routes, child);
                });
            } else {
                let _ = cleanup_cursor_transport_blocking(pending, routes, child);
            }
        }
    }

    async fn spawn(command: &str, api_key: Option<&str>, cwd: &Path) -> Result<Self, BackendError> {
        let mut cmd = crate::process_env::tokio_command(command);
        cmd.arg("acp");
        // The ACP session `cwd` should govern, but cursor-agent falls back
        // to the process cwd for some path resolution — pin it so those
        // fallbacks stay inside the worktree instead of wherever trouve was
        // launched from.
        cmd.current_dir(cwd);
        if let Some(key) = api_key {
            cmd.env("CURSOR_API_KEY", key);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child =
            crate::process_env::spawn_process_tree(&mut cmd).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BackendError::NotInstalled(command.to_string()),
                _ => BackendError::Io(e),
            })?;
        let stdin = child.take_stdin().expect("stdin piped");
        let stdout = child.take_stdout().expect("stdout piped");

        let server = Self {
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            routes: Arc::new(Mutex::new(HashMap::new())),
            sessions: Mutex::new(HashMap::new()),
            config_lock: Mutex::new(()),
            plans: Arc::new(Mutex::new(HashMap::new())),
            calls: Arc::new(Mutex::new(HashMap::new())),
            mcp_http: AtomicBool::new(false),
            child: Arc::new(Mutex::new(child)),
            closed: Arc::new(AtomicBool::new(false)),
            transport_cleanup_started: AtomicBool::new(false),
            #[cfg(test)]
            injected_terminate_failure: AtomicBool::new(false),
            last_used: std::sync::Mutex::new(Instant::now()),
        };
        server.start_reader(stdout);
        Ok(server)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    /// No turn is streaming from this child (turns hold a route for their
    /// whole duration). `try_lock` failing means someone is mid-(un)subscribe,
    /// which counts as busy.
    fn is_idle(&self) -> bool {
        self.routes
            .try_lock()
            .map(|r| r.is_empty())
            .unwrap_or(false)
    }

    fn start_reader(&self, stdout: tokio::process::ChildStdout) {
        let routes = self.routes.clone();
        let closed = self.closed.clone();
        let pending = self.pending.clone();
        let plans = self.plans.clone();
        let calls = self.calls.clone();
        let stdin = self.stdin.clone();
        let child = Arc::downgrade(&self.child);
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
                        write_reply(stdin.as_ref(), reply).await;
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
                        let update = params["update"].clone();
                        let mut calls = calls.lock().await;
                        if calls.len() >= 4096 && !calls.contains_key(call_id) {
                            calls.clear(); // bound old calls; keep this live one below
                        }
                        let entry = calls
                            .entry(call_id.to_string())
                            .or_insert_with(|| (session_id.clone(), json!({})));
                        entry.0.clone_from(&session_id);
                        // Preserve rawInput from the initial tool_call when
                        // later in-progress updates omit it.
                        if let (Some(stored), Some(incoming)) =
                            (entry.1.as_object_mut(), update.as_object())
                        {
                            for (key, value) in incoming {
                                stored.insert(key.clone(), value.clone());
                            }
                        } else {
                            entry.1 = update;
                        }
                    }
                    if session_id.is_empty()
                        && let Some(call_id) = params["toolCallId"].as_str()
                        && let Some((owner, _)) = calls.lock().await.get(call_id)
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
                        // This reader serves every session in the worktree,
                        // including their JSON-RPC responses. A slow session
                        // must fail independently rather than wedge the
                        // entire shared ACP transport.
                        if let Err(error) = tx.try_send(m) {
                            if error == RouteSendError::Overloaded {
                                tracing::warn!(
                                    "cursor acp: dropping {session_id} event route: \
                                     event backlog limit exceeded"
                                );
                            }
                            let mut routes = routes.lock().await;
                            if routes
                                .get(&session_id)
                                .is_some_and(|active| active.same_channel(&tx))
                            {
                                routes.remove(&session_id);
                            }
                            drop(routes);
                            if has_id {
                                // The agent blocks its turn on server requests.
                                // Reject a request that could not reach its
                                // session instead of leaving it unresolved.
                                let reply = json!({
                                    "jsonrpc": "2.0", "id": msg["id"],
                                    "error": { "code": -32603,
                                               "message": "session event route unavailable" },
                                });
                                write_reply(stdin.as_ref(), reply).await;
                            }
                        }
                    } else if has_id {
                        // A request nobody can answer must still get a
                        // response — the agent blocks its turn on it.
                        tracing::warn!("cursor acp: refusing unroutable request {method}");
                        let reply = json!({
                            "jsonrpc": "2.0", "id": msg["id"],
                            "error": { "code": -32603,
                                       "message": "session event route unavailable" },
                        });
                        write_reply(stdin.as_ref(), reply).await;
                    }
                }
            }
            // Release every waiter the dead transport left behind.
            closed.store(true, Ordering::Relaxed);
            pending.lock().await.clear();
            routes.lock().await.clear();
            if let Some(child) = child.upgrade() {
                let _ = child.lock().await.terminate_and_reap().await;
            }
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
        self.mcp_http.store(
            result["agentCapabilities"]["mcpCapabilities"]["http"]
                .as_bool()
                .unwrap_or(false),
            Ordering::Relaxed,
        );
        Ok(())
    }

    async fn new_session(
        &self,
        worktree: &std::path::Path,
        mcp_servers: &[crate::McpServerLaunch],
        bridge: Option<&crate::McpBridgeConfig>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<String, BackendError> {
        let mcp_servers =
            acp_mcp_servers(mcp_servers, bridge, self.mcp_http.load(Ordering::Relaxed))?;
        let mcp_fingerprint = acp_mcp_fingerprint(&mcp_servers)?;
        let result = self
            .request_cancellable(
                "session/new",
                json!({ "cwd": worktree, "mcpServers": mcp_servers }),
                cancel,
            )
            .await
            .map_err(auth_hint)?;
        let id = match result["sessionId"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
        {
            Some(id) => id.to_string(),
            None => {
                // A successful session/new with no usable identity may have
                // created resources that no future request can address.
                self.terminate().await?;
                return Err(BackendError::Protocol(
                    "session/new result missing sessionId".into(),
                ));
            }
        };
        self.sessions
            .lock()
            .await
            .insert(id.clone(), mcp_fingerprint);
        Ok(id)
    }

    async fn load_session(
        &self,
        session_id: &str,
        worktree: &std::path::Path,
        mcp_servers: &[crate::McpServerLaunch],
        bridge: Option<&crate::McpBridgeConfig>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), BackendError> {
        let mcp_servers =
            acp_mcp_servers(mcp_servers, bridge, self.mcp_http.load(Ordering::Relaxed))?;
        let mcp_fingerprint = acp_mcp_fingerprint(&mcp_servers)?;
        self.request_cancellable(
            "session/load",
            json!({
                "sessionId": session_id,
                "cwd": worktree,
                "mcpServers": mcp_servers,
            }),
            cancel,
        )
        .await
        .map_err(auth_hint)?;
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), mcp_fingerprint);
        Ok(())
    }

    async fn session_settings_match(
        &self,
        session_id: &str,
        desired_mcp_fingerprint: [u8; 32],
    ) -> bool {
        self.sessions.lock().await.get(session_id) == Some(&desired_mcp_fingerprint)
    }

    async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value, BackendError> {
        self.request_cancellable(
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": config_id, "value": value }),
            cancel,
        )
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, BackendError> {
        self.request_cancellable(method, params, &tokio_util::sync::CancellationToken::new())
            .await
    }

    async fn request_cancellable(
        &self,
        method: &str,
        params: Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value, BackendError> {
        self.request_cancellable_with_timeout(method, params, cancel, REQUEST_RESPONSE_TIMEOUT)
            .await
    }

    async fn request_cancellable_with_timeout(
        &self,
        method: &str,
        params: Value,
        cancel: &tokio_util::sync::CancellationToken,
        response_timeout: Duration,
    ) -> Result<Value, BackendError> {
        if cancel.is_cancelled() {
            return Err(BackendError::Cancelled);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if let Err(error) = self
            .write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        enum Response {
            Received(std::result::Result<Result<Value, String>, oneshot::error::RecvError>),
            Cancelled,
            TimedOut,
        }
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => Response::Cancelled,
            response = tokio::time::timeout(response_timeout, rx) => match response {
                Ok(response) => Response::Received(response),
                Err(_) => Response::TimedOut,
            },
        };
        match response {
            Response::Received(Ok(Ok(value))) => Ok(value),
            Response::Received(Ok(Err(error))) => {
                Err(BackendError::Protocol(format!("{method}: {error}")))
            }
            Response::Received(Err(_)) => {
                // EOF cleanup normally owns this already; await idempotent
                // process-tree cleanup before the caller releases setup state.
                self.terminate().await?;
                Err(BackendError::Protocol(format!(
                    "{method}: cursor-agent closed before responding"
                )))
            }
            Response::Cancelled => {
                // The complete request was flushed, so its session/config
                // mutation may be applied later. Recycle the shared process
                // and acknowledge reaping before another setup can proceed.
                self.terminate().await?;
                Err(BackendError::Cancelled)
            }
            Response::TimedOut => {
                self.terminate().await?;
                Err(BackendError::Protocol(format!(
                    "{method}: no response within {}s",
                    response_timeout.as_secs_f64()
                )))
            }
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
        if let Err(error) = self
            .write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        Ok(rx)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), BackendError> {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
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
        let result = tokio::time::timeout(TRANSPORT_WRITE_TIMEOUT, async {
            stdin.write_all(&line).await?;
            stdin.flush().await
        })
        .await;
        drop(stdin);
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.terminate().await?;
                Err(BackendError::Io(error))
            }
            Err(_) => {
                self.terminate().await?;
                Err(BackendError::Protocol(format!(
                    "cursor-agent stdin blocked for {}s",
                    TRANSPORT_WRITE_TIMEOUT.as_secs()
                )))
            }
        }
    }

    async fn terminate(&self) -> Result<(), BackendError> {
        self.closed.store(true, Ordering::Relaxed);
        #[cfg(test)]
        if self.injected_terminate_failure.load(Ordering::Relaxed) {
            return Err(BackendError::Protocol(
                "injected cursor-agent cleanup failure".into(),
            ));
        }
        let cleanup = self.child.lock().await.terminate_and_reap().await;
        self.pending.lock().await.clear();
        self.routes.lock().await.clear();
        cleanup.map(|_| ()).map_err(BackendError::Io)
    }

    async fn subscribe(&self, session_id: &str) -> RouteReceiver<ServerMsg> {
        let (tx, rx) = route_channel();
        self.routes.lock().await.insert(session_id.to_string(), tx);
        rx
    }

    async fn unsubscribe(&self, session_id: &str) {
        self.routes.lock().await.remove(session_id);
        // Idle time counts from the end of the last turn.
        self.touch();
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
fn acp_mcp_servers(
    servers: &[crate::McpServerLaunch],
    bridge: Option<&crate::McpBridgeConfig>,
    supports_http: bool,
) -> Result<Value, BackendError> {
    let mut values: Vec<Value> = servers
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
        .collect();
    if let Some(bridge) = bridge {
        if !supports_http {
            return Err(BackendError::Protocol(
                "cursor-agent did not advertise ACP HTTP MCP support; refusing to drop Trouve's tool bridge"
                    .into(),
            ));
        }
        values.push(json!({
            "type": "http",
            "name": "trouve",
            "url": bridge.url,
            "headers": bridge.headers
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect::<Vec<_>>(),
        }));
    }
    Ok(Value::Array(values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_prompt_channel_is_not_a_cancellation_acknowledgement() {
        let (sender, mut receiver) = oneshot::channel::<Result<Value, String>>();
        drop(sender); // Equivalent to the ACP reader clearing pending on EOF.

        assert!(!cancellation_acknowledged(&mut receiver).await);
    }

    #[test]
    fn parses_dashboard_usage() {
        // Field shapes from a real GetCurrentPeriodUsage response: cents
        // for money, int64s as strings, percentages precomputed.
        let cycle_end_ms = (chrono::Utc::now().timestamp() + 9 * 86_400 + 600) * 1000;
        let usage = json!({
            "billingCycleStart": "1782696817000",
            "billingCycleEnd": cycle_end_ms.to_string(),
            "planUsage": {
                "totalSpend": 53573,
                "includedSpend": 40000,
                "bonusSpend": 13573,
                "limit": 40000,
                "autoPercentUsed": 1.525,
                "apiPercentUsed": 100,
                "totalPercentUsed": 35.715333333333334,
            },
            "spendLimitUsage": {
                "totalSpend": 241122,
                "individualLimit": 250000,
                "individualUsed": 241122,
                "individualRemaining": 8878,
                "limitType": "user",
            },
            "enabled": true,
            "displayMessage": "You've used 97% of your included usage",
        });
        let plan_info = json!({
            "planInfo": { "planName": "Ultra", "includedAmountCents": 40000, "price": "$200/mo" },
        });
        let health = parse_dashboard_usage("cursor", &usage, Some(&plan_info));
        assert_eq!(health.status, "ok");
        assert_eq!(health.plan, "Ultra");
        assert_eq!(health.credits, "on-demand: $2411.22 of $2500.00");
        let windows: Vec<(&str, i64)> = health
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.used_percent))
            .collect();
        assert_eq!(
            windows,
            vec![
                ("Included usage", 36),
                ("Included (API models)", 100),
                ("Included (Auto)", 2),
                ("On-demand spend", 96),
            ]
        );
        // billingCycleEnd is millis-as-string; all meters share the reset.
        assert!(health.windows[0].resets.starts_with("resets in 9d"));
        assert!(
            health
                .windows
                .iter()
                .all(|w| w.resets == health.windows[0].resets)
        );
    }

    #[test]
    fn dashboard_usage_pooled_spend_and_missing_pieces() {
        // Team accounts pool the on-demand limit; missing plan buckets and
        // plan info must not produce windows or a plan name.
        let usage = json!({
            "planUsage": { "totalPercentUsed": 12.4 },
            "spendLimitUsage": { "pooledLimit": 100000, "pooledUsed": 25000 },
        });
        let health = parse_dashboard_usage("cursor", &usage, None);
        assert_eq!(health.status, "ok");
        assert_eq!(health.plan, "");
        let windows: Vec<(&str, i64)> = health
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.used_percent))
            .collect();
        assert_eq!(
            windows,
            vec![("Included usage", 12), ("On-demand spend", 25)]
        );
        assert_eq!(health.credits, "on-demand: $250.00 of $1000.00");
        assert_eq!(health.windows[0].resets, "", "no cycle end reported");

        // Nothing usable at all → unavailable.
        let health = parse_dashboard_usage("cursor", &json!({}), None);
        assert_eq!(health.status, "unavailable");
        assert!(health.note.contains("logged in"));
    }

    #[test]
    fn reads_cli_token_from_auth_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");

        std::fs::write(&path, r#"{"accessToken":"tok-1","refreshToken":"r"}"#).unwrap();
        assert_eq!(read_cli_token(&path).unwrap(), "tok-1");

        // API-key logins store only apiKey.
        std::fs::write(&path, r#"{"apiKey":"key-1"}"#).unwrap();
        assert_eq!(read_cli_token(&path).unwrap(), "key-1");

        std::fs::write(&path, r#"{}"#).unwrap();
        let err = read_cli_token(&path).unwrap_err().to_string();
        assert!(err.contains("no access token"), "{err}");

        let err = read_cli_token(&dir.path().join("missing.json"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cursor-agent login"), "{err}");
    }

    #[test]
    fn acp_mcp_servers_shape() {
        let servers = vec![crate::McpServerLaunch {
            name: "jira".into(),
            command: "jira-mcp".into(),
            args: vec!["--stdio".into()],
            env: vec![("TOKEN".into(), "sekrit".into())],
        }];
        let value = acp_mcp_servers(&servers, None, false).unwrap();
        assert_eq!(
            value,
            json!([{
                "name": "jira",
                "command": "jira-mcp",
                "args": ["--stdio"],
                "env": [{ "name": "TOKEN", "value": "sekrit" }],
            }])
        );
        assert_eq!(acp_mcp_servers(&[], None, false).unwrap(), json!([]));

        let bridge = crate::McpBridgeConfig {
            url: "http://127.0.0.1:7433/internal/threads/th_1/mcp?approval=0".into(),
            headers: vec![("Authorization".into(), "Bearer bridge-secret".into())],
        };
        assert_eq!(
            acp_mcp_servers(&[], Some(&bridge), true).unwrap(),
            json!([{
                "type": "http",
                "name": "trouve",
                "url": bridge.url,
                "headers": [{
                    "name": "Authorization",
                    "value": "Bearer bridge-secret",
                }],
            }])
        );
        assert!(acp_mcp_servers(&[], Some(&bridge), false).is_err());
    }

    #[test]
    fn acp_session_fingerprint_tracks_rotating_bridge_configuration() {
        let first = json!([{
            "type": "http",
            "name": "trouve",
            "url": "http://127.0.0.1/mcp?ticket=first",
            "headers": [],
        }]);
        let second = json!([{
            "type": "http",
            "name": "trouve",
            "url": "http://127.0.0.1/mcp?ticket=second",
            "headers": [],
        }]);
        assert_ne!(
            acp_mcp_fingerprint(&first).unwrap(),
            acp_mcp_fingerprint(&second).unwrap()
        );
        assert_eq!(
            acp_mcp_fingerprint(&first).unwrap(),
            acp_mcp_fingerprint(&first).unwrap()
        );
    }

    #[test]
    fn static_cursor_catalog_is_authoritative_and_live_models_filter_availability() {
        let backend = CursorBackend::new("cursor", None, None);
        let static_models = backend.models();
        let static_ids: Vec<_> = static_models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(
            static_ids,
            vec![
                "cursor/claude-fable-5",
                "cursor/claude-opus-5",
                "cursor/claude-sonnet-5",
                "cursor/composer-2.5",
                "cursor/default",
                "cursor/gemini-3.1-pro",
                "cursor/gemini-3.7-flash",
                "cursor/gpt-5.6-luna",
                "cursor/gpt-5.6-sol",
                "cursor/gpt-5.6-terra",
                "cursor/grok-4.5",
                "cursor/grok-4.6",
            ]
        );
        assert!(static_models.iter().all(|model| {
            model.input_price_per_mtok.is_none() && model.output_price_per_mtok.is_none()
        }));

        let live = parse_acp_models(
            "cursor",
            &json!({ "models": [
                { "value": "claude-fable-5", "name": "Live override", "configOptions": [
                    { "id": "context", "currentValue": "300k",
                      "options": [ { "value": "300k" }, { "value": "1m" } ] }
                ]},
                { "value": "cursor-next", "name": "Cursor Next", "configOptions": [] }
            ]}),
        );
        let merged = backend.canonicalize_models(live);
        assert_eq!(merged.len(), 2);
        let fable = merged
            .iter()
            .find(|model| model.id == "cursor/claude-fable-5")
            .unwrap();
        assert_eq!(fable.display_name, "Claude Fable 5");
        assert_eq!(
            fable.options_schema.pointer("/properties/context/enum"),
            Some(&json!(["300k", "1m"]))
        );
        assert!(merged.iter().any(|model| model.id == "cursor/cursor-next"));
    }

    #[test]
    fn effort_options_try_both_cursor_reasoning_dialects() {
        let mut options = Vec::new();
        push_cursor_model_option(&mut options, "effort", "high".into());
        assert_eq!(
            options,
            vec![
                ("effort".to_string(), "high".to_string()),
                ("reasoning".to_string(), "high".to_string()),
            ]
        );
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
        assert_eq!(composer.context_window, 0); // no context option
        assert_eq!(
            composer.options_schema.pointer("/properties/fast/default"),
            Some(&json!(true))
        );
    }

    #[test]
    fn models_dev_canonicalizes_public_cursor_models_only() {
        let catalog = ModelsDevCatalog::embedded();
        let result = json!({ "models": [
            { "value": "claude-fable-5", "name": "Vendor Override", "configOptions": [
                { "id": "context", "description": "Context size",
                  "currentValue": "300k",
                  "options": [ { "value": "300k" }, { "value": "1m" } ] },
                { "id": "effort", "description": "Vendor effort",
                  "currentValue": "high",
                  "options": [ { "value": "low" }, { "value": "high" } ] }
            ]},
            { "value": "composer-2.5", "name": "Composer 2.5", "configOptions": [
                { "id": "fast", "description": "Faster",
                  "currentValue": "true",
                  "options": [ { "value": "false" }, { "value": "true" } ] }
            ]},
            { "value": "gpt-future", "name": "Uncatalogued public model",
              "configOptions": [] },
            { "value": "grok-future", "name": "Uncatalogued public Grok",
              "configOptions": [] }
        ]});
        let live = parse_acp_models("cursor", &result);
        let models: Vec<_> = live
            .into_iter()
            .filter_map(|model| canonicalize_cursor_model(&catalog, "cursor", model))
            .collect();

        assert_eq!(models.len(), 2, "unknown public ids are not guessed");
        let fable = &models[0];
        assert_eq!(fable.display_name, "Claude Fable 5");
        assert_eq!(fable.context_window, 1_000_000);
        assert_eq!(
            fable.options_schema.pointer("/properties/effort/enum"),
            Some(&json!(["low", "medium", "high", "xhigh", "max"]))
        );
        assert_eq!(
            fable.options_schema.pointer("/properties/effort/default"),
            Some(&json!("medium"))
        );
        assert_eq!(
            fable.options_schema.pointer("/properties/context/enum"),
            Some(&json!(["300k", "1m"]))
        );

        let composer = &models[1];
        assert_eq!(composer.display_name, "Composer 2.5");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn post_write_setup_cancellation_recycles_cursor_before_returning() {
        use std::os::unix::fs::PermissionsExt as _;

        for method in ["session/new", "session/load", "session/set_config_option"] {
            let directory = tempfile::tempdir().unwrap();
            let script_path = directory.path().join("fake-cursor-agent");
            let marker_path = directory.path().join("request.method");
            let script = format!(
                r#"#!/usr/bin/env python3
import json, sys, time
marker = {marker:?}
for line in sys.stdin:
    message = json.loads(line)
    with open(marker, "w") as output:
        output.write(message.get("method", ""))
        output.flush()
    time.sleep(60)
"#,
                marker = marker_path.to_string_lossy(),
            );
            std::fs::write(&script_path, script).unwrap();
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let server = Arc::new(
                AcpServer::spawn(&script_path.to_string_lossy(), None, directory.path())
                    .await
                    .unwrap(),
            );
            let cancel = tokio_util::sync::CancellationToken::new();
            let request = tokio::spawn({
                let server = server.clone();
                let cancel = cancel.clone();
                let worktree = directory.path().to_path_buf();
                async move {
                    match method {
                        "session/new" => server
                            .new_session(&worktree, &[], None, &cancel)
                            .await
                            .map(|_| ()),
                        "session/load" => {
                            server
                                .load_session("session-1", &worktree, &[], None, &cancel)
                                .await
                        }
                        "session/set_config_option" => server
                            .set_config_option("session-1", "model", "test", &cancel)
                            .await
                            .map(|_| ()),
                        _ => unreachable!(),
                    }
                }
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                while !marker_path.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{method} did not reach cursor-agent"));

            cancel.cancel();
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(2), request)
                    .await
                    .unwrap_or_else(|_| panic!("{method} cancellation did not reap cursor-agent"))
                    .unwrap(),
                Err(BackendError::Cancelled)
            ));
            assert!(
                server.is_closed(),
                "{method} left its shared process reusable"
            );
            assert!(server.child.lock().await.try_wait_tree().unwrap().is_some());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn model_config_post_write_cancellation_remains_cancelled() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("fake-cursor-agent");
        let marker_path = directory.path().join("request.method");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, sys, time
marker = {marker:?}
for line in sys.stdin:
    message = json.loads(line)
    with open(marker, "w") as output:
        output.write(message.get("method", ""))
        output.flush()
    time.sleep(60)
"#,
            marker = marker_path.to_string_lossy(),
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = Arc::new(
            AcpServer::spawn(&script_path.to_string_lossy(), None, directory.path())
                .await
                .unwrap(),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let turn = BackendTurn {
            cancel: cancel.clone(),
            thread_id: "thread-1".into(),
            worktree: directory.path().to_path_buf(),
            session: Some("session-1".into()),
            model: "test-model".into(),
            model_options: serde_json::Map::new(),
            prompt: String::new(),
            attachments: Vec::new(),
            instructions: None,
            permission: BackendPermission::ReadOnly,
            tool_free: false,
            mcp_bridge: None,
            mcp_servers: Vec::new(),
        };
        let configuring = tokio::spawn({
            let server = server.clone();
            let cancel = cancel.clone();
            async move { apply_model_config(&server, "session-1", &turn, &cancel).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while std::fs::read_to_string(&marker_path).ok().as_deref()
                != Some("session/set_config_option")
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("model configuration did not reach cursor-agent");
        assert_eq!(
            std::fs::read_to_string(&marker_path).unwrap(),
            "session/set_config_option"
        );

        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), configuring)
                .await
                .expect("model configuration cancellation did not reap cursor-agent")
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        assert!(server.is_closed());
        assert!(server.child.lock().await.try_wait_tree().unwrap().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_write_setup_timeout_recycles_cursor_before_returning() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("fake-cursor-agent");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nIFS= read -r request\nIFS= read -r block\n",
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = AcpServer::spawn(&script_path.to_string_lossy(), None, directory.path())
            .await
            .unwrap();

        let error = server
            .request_cancellable_with_timeout(
                "session/set_config_option",
                json!({"sessionId": "session-1", "configId": "model", "value": "test"}),
                &tokio_util::sync::CancellationToken::new(),
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            BackendError::Protocol(message) if message.contains("no response")
        ));
        assert!(server.is_closed());
        assert!(server.child.lock().await.try_wait_tree().unwrap().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn overloaded_route_waits_for_cursor_cancellation_acknowledgement() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("fake-cursor-agent");
        let marker_path = directory.path().join("cancelled");
        let script = r#"#!/usr/bin/env python3
import json, os, sys, time
prompt_id = None
marker = os.path.join(os.path.dirname(__file__), "cancelled")
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "session/prompt":
        prompt_id = message["id"]
        session_id = message["params"]["sessionId"]
        for index in range(__EVENT_COUNT__):
            update = {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": str(index)},
                    },
                },
            }
            sys.stdout.write(json.dumps(update) + "\n")
        sys.stdout.flush()
    elif method == "session/cancel":
        time.sleep(0.15)
        with open(marker, "w") as output:
            output.write("acknowledged")
        response = {"jsonrpc": "2.0", "id": prompt_id, "result": {"usage": {}}}
        sys.stdout.write(json.dumps(response) + "\n")
        sys.stdout.flush()
"#
        .replace(
            "__EVENT_COUNT__",
            &(ROUTE_EVENT_BUDGET.saturating_add(1)).to_string(),
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = Arc::new(
            AcpServer::spawn(&script_path.to_string_lossy(), None, directory.path())
                .await
                .unwrap(),
        );
        let session_id = "overloaded-session".to_string();
        let route = server.subscribe(&session_id).await;
        let mut overloaded = route.overload_signal();
        let prompt_rx = server
            .request_deferred(
                "session/prompt",
                json!({"sessionId": session_id, "prompt": []}),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), overloaded.wait())
            .await
            .expect("fake cursor route did not overload");

        let mut events = Box::pin(turn_stream(
            server.clone(),
            session_id,
            route,
            prompt_rx,
            false,
            tokio_util::sync::CancellationToken::new(),
        ));
        let event = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .expect("overloaded stream did not finish cancellation cleanup")
            .expect("overloaded stream ended without its protocol error");
        assert!(matches!(
            event,
            Err(BackendError::Protocol(ref error))
                if error.contains("event backlog exceeded")
        ));
        assert!(
            marker_path.exists(),
            "overload failure was published before cursor acknowledged cancellation"
        );
        server.terminate().await.unwrap();
    }

    fn bare_cursor_turn(worktree: &Path) -> BackendTurn {
        BackendTurn {
            cancel: Default::default(),
            thread_id: "thread-1".into(),
            worktree: worktree.to_path_buf(),
            session: None,
            model: "default".into(),
            model_options: serde_json::Map::new(),
            prompt: "hello".into(),
            attachments: Vec::new(),
            instructions: None,
            permission: BackendPermission::ReadOnly,
            tool_free: false,
            mcp_bridge: None,
            mcp_servers: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn aborting_hanging_handshake_reaps_cursor_agent() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("cursor-hanging-initialize");
        let marker = directory.path().join("cursor.pid");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, sys, time
for line in sys.stdin:
    if json.loads(line).get("method") == "initialize":
        with open({marker:?}, "w") as output:
            output.write(str(os.getpid()))
            output.flush()
        time.sleep(60)
"#,
            marker = marker.to_string_lossy(),
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CursorBackend::new(
            "cursor",
            Some(script_path.to_string_lossy().into_owned()),
            None,
        ));
        let worktree = directory.path().to_path_buf();
        let startup = tokio::spawn({
            let backend = backend.clone();
            async move { backend.server_for(&worktree).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("initialize did not reach cursor-agent");
        let pid = std::fs::read_to_string(&marker)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        startup.abort();
        assert!(matches!(startup.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(Duration::from_secs(2), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("dropped startup future left cursor-agent alive");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelling_hanging_handshake_reaps_cursor_agent_before_returning() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("cursor-cancelled-initialize");
        let marker = directory.path().join("cursor.pid");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, sys, time
for line in sys.stdin:
    if json.loads(line).get("method") == "initialize":
        with open({marker:?}, "w") as output:
            output.write(str(os.getpid()))
            output.flush()
        time.sleep(60)
"#,
            marker = marker.to_string_lossy(),
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CursorBackend::new(
            "cursor",
            Some(script_path.to_string_lossy().into_owned()),
            None,
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        let worktree = directory.path().to_path_buf();
        let startup = tokio::spawn({
            let backend = backend.clone();
            let cancel = cancel.clone();
            async move { backend.server_for_cancellable(&worktree, &cancel).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("initialize did not reach cursor-agent");
        let pid = std::fs::read_to_string(&marker)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), startup)
                .await
                .expect("startup cancellation did not acknowledge cleanup")
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "startup returned cancellation before reaping cursor-agent"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_new_session_success_reaps_cursor_agent() {
        use std::os::unix::fs::PermissionsExt as _;

        for (name, result) in [
            ("missing", json!({})),
            ("blank", json!({ "sessionId": "  " })),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let script_path = directory.path().join(format!("cursor-new-{name}"));
            let script = r#"#!/usr/bin/env python3
import json, sys, time
for line in sys.stdin:
    message = json.loads(line)
    response = {"jsonrpc": "2.0", "id": message["id"], "result": __RESULT__}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
    time.sleep(60)
"#
            .replace("__RESULT__", &result.to_string());
            std::fs::write(&script_path, script).unwrap();
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let server = AcpServer::spawn(&script_path.to_string_lossy(), None, directory.path())
                .await
                .unwrap();
            assert!(matches!(
                server
                    .new_session(
                        directory.path(),
                        &[],
                        None,
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await,
                Err(BackendError::Protocol(_))
            ));
            assert!(server.is_closed());
            assert!(server.child.lock().await.try_wait_tree().unwrap().is_some());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fresh_cursor_session_is_reported_before_setup_cancellation() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("cursor-fresh-cancel");
        let marker = directory.path().join("session.started");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        result = {{}}
    elif method == "session/new":
        result = {{"sessionId": "fresh-session"}}
    else:
        result = {{}}
    sys.stdout.write(json.dumps({{"jsonrpc": "2.0", "id": message["id"], "result": result}}) + "\n")
    sys.stdout.flush()
    if method == "session/new":
        open({marker:?}, "w").close()
"#,
            marker = marker.to_string_lossy(),
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = Arc::new(CursorBackend::new(
            "cursor",
            Some(script_path.to_string_lossy().into_owned()),
            None,
        ));
        let server = backend.server_for(directory.path()).await.unwrap();
        let config = server.config_lock.lock().await;
        let turn = bare_cursor_turn(directory.path());
        let cancel = turn.cancel.clone();
        let running = tokio::spawn({
            let backend = backend.clone();
            async move { backend.run_turn(turn).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("session/new did not complete");
        cancel.cancel();
        let mut events = running.await.unwrap().unwrap();
        drop(config);
        assert!(matches!(
            events.next().await,
            Some(Ok(BackendEvent::SessionStarted { session_id })) if session_id == "fresh-session"
        ));
        assert!(matches!(
            events.next().await,
            Some(Err(BackendError::Cancelled))
        ));
        server.terminate().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_failure_keeps_closed_pooled_server_and_denies_replacement() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("cursor-cleanup-failure");
        let starts = directory.path().join("starts.txt");
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, sys
with open({starts:?}, "a") as starts:
    starts.write(str(os.getpid()) + "\n")
    starts.flush()
for line in sys.stdin:
    message = json.loads(line)
    response = {{"jsonrpc": "2.0", "id": message["id"], "result": {{}}}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#,
            starts = starts.to_string_lossy(),
        );
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = CursorBackend::new(
            "cursor",
            Some(script_path.to_string_lossy().into_owned()),
            None,
        );
        let server = backend.server_for(directory.path()).await.unwrap();
        server.closed.store(true, Ordering::Relaxed);
        server
            .injected_terminate_failure
            .store(true, Ordering::Relaxed);

        let error = match backend.server_for(directory.path()).await {
            Ok(_) => panic!("cleanup failure must deny replacement startup"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("injected cursor-agent cleanup failure"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&starts).unwrap().lines().count(), 1);
        let retained = backend
            .pool
            .servers
            .lock()
            .await
            .get(directory.path())
            .cloned()
            .unwrap();
        assert!(Arc::ptr_eq(&server, &retained));

        let unrelated_directory = tempfile::tempdir().unwrap();
        let unrelated = backend
            .server_for(unrelated_directory.path())
            .await
            .expect("one quarantined key must not block an unrelated worktree");
        assert_eq!(
            std::fs::read_to_string(&starts).unwrap().lines().count(),
            2,
            "unrelated worktree did not receive its own server"
        );

        server
            .injected_terminate_failure
            .store(false, Ordering::Relaxed);
        server.terminate().await.unwrap();
        unrelated.terminate().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_thread_spawn_failure_falls_back_for_cursor() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let script_path = directory.path().join("cursor-cleanup-fallback");
        std::fs::write(&script_path, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = AcpServer::spawn(&script_path.to_string_lossy(), None, directory.path())
            .await
            .unwrap();
        let routes = server.routes.lock().await;
        server.invalidate_transport_now_with(|_| {
            Err(std::io::Error::other("forced cleanup thread spawn failure"))
        });
        drop(routes);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if server.child.lock().await.try_wait_tree().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("runtime cleanup fallback did not reap cursor-agent");
        assert!(server.is_closed());
        assert!(server.transport_cleanup_started.load(Ordering::Acquire));
    }
}
