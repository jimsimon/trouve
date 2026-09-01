//! Cursor backend driven by the standalone Cursor Agent SDK Bridge.
//!
//! One credential-bound backend owns one warm Bridge process and one callback
//! router. Cursor's local SQLite store holds every agent for that backend; the
//! router maps each callback's exact `agent_id` and ingress generation to one
//! turn-scoped MCP route.
//! Cursor's native tools are replaced with the single SDK `mcp` capability;
//! concrete tool schemas and calls are proxied to trouve's internal,
//! thread-scoped MCP endpoint and therefore still pass through `ToolExecutor`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock, Weak};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use bytes::BytesMut;
use futures::{StreamExt as _, TryStreamExt as _};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, BufReader};
use tokio::sync::{
    Mutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, RwLock, RwLockReadGuard, Semaphore, watch,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use trouve_protocol::{ModelInfo, Usage};
use trouve_providers::models_dev::{ModelsDevCatalog, OptionsDialect};

use crate::process_env::{ProcessTreeChild, spawn_process_tree};
use crate::{
    AgentBackend, BackendError, BackendEvent, BackendEventSender, BackendEventStream, BackendLogin,
    BackendPermission, BackendStartupActivity, BackendStatus, BackendTurn, async_stream,
    binary_on_path, format_reset,
};

const DASHBOARD_BASE: &str = "https://api2.cursor.sh";
const USAGE_TIMEOUT: Duration = Duration::from_secs(10);
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_ACK_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const CALLBACK_ROUTE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const CALLBACK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RPC_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONNECT_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIAGNOSTIC_LINES: usize = 40;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;
const MAX_CALLBACK_RECORDS: usize = 128;
// Historical IDs use fixed-size hashes, but still need a separate hard ceiling
// so a defective authenticated Bridge cannot grow one turn without bound.
const MAX_CALLBACKS_PER_TURN: usize = 4 * 1024;
// Retain exact call ownership for the life of a shared Bridge so a delayed
// retry from an earlier turn cannot bind to a replacement route. Recycle the
// process at the bound instead of evicting authorization tombstones.
const MAX_CALLBACK_IDENTITIES_PER_PROCESS: usize = 16 * 1024;
// The process-wide callback endpoint has no vendor-visible turn nonce. Never
// route the same durable agent id twice through one endpoint; recycle at this
// bound even when a long-lived process sees only distinct, tool-free agents.
const MAX_RETIRED_AGENT_IDS_PER_PROCESS: usize = 16 * 1024;
const MAX_CALLBACK_REPLAY_RECORDS: usize = 64;
const MAX_CALLBACK_REPLAY_BYTES: usize = 16 * 1024 * 1024;
const MAX_CALLBACK_CONCURRENCY: usize = 8;
const MAX_CALLBACK_HTTP_CONCURRENCY: usize = MAX_CONCURRENT_TURNS * MAX_CALLBACK_CONCURRENCY;
const MAX_LEGACY_SESSION_MARKER_BYTES: u64 = 4 * 1024;
const READY_PREFIX: &str = "cursor-sdk-bridge ready ";
const CALLBACK_PATH: &str = "/sdk.v1.SdkCustomToolCallbackService/CallCustomTool";
/// Cursor-native tool vocabulary in the pinned Agent SDK (1.0.28), excluding
/// the sole `mcp` transport capability that Trouve intentionally exposes.
/// `tools.names` is the primary allowlist; this denylist makes confinement
/// fail closed if a Bridge release ever broadens that field's interpretation.
const CURSOR_NATIVE_TOOL_DENYLIST: &[&str] = &[
    "shell",
    "read",
    "edit",
    "grep",
    "glob",
    "ls",
    "task",
    "webSearch",
    "delete",
    "readLints",
    "webFetch",
    "semSearch",
    "updateTodos",
    "readTodos",
    "askQuestion",
    "await",
    "generateImage",
    "applyAgentDiff",
];
/// Concurrent turns admitted to one shared Cursor Bridge process.
const MAX_CONCURRENT_TURNS: usize = 3;
/// The shared Bridge is expensive enough to reap when the backend stays idle.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const REAP_INTERVAL: Duration = Duration::from_secs(60);

pub struct CursorBackend {
    id: String,
    command: String,
    api_key: Option<String>,
    state_root: PathBuf,
    pool: Arc<BridgePool>,
    catalog: Arc<ModelsDevCatalog>,
    /// Cursor API and Dashboard Connect-RPC origin (overridable for tests).
    dashboard_base: String,
    legacy_cli_migration_required: bool,
}

impl CursorBackend {
    pub fn new(id: impl Into<String>, command: Option<String>, api_key: Option<String>) -> Self {
        let state_root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("trouve")
            .join("cursor-sdk");
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "cursor-sdk-bridge".into()),
            api_key,
            state_root,
            pool: Arc::new(BridgePool::default()),
            catalog: Arc::new(ModelsDevCatalog::embedded()),
            dashboard_base: DASHBOARD_BASE.into(),
            legacy_cli_migration_required: false,
        }
    }

    /// Preserve old `cursor-cli` configurations as an explicit recovery
    /// state. CLI login credentials cannot authenticate the Agent SDK, so the
    /// user must deliberately save an SDK API key before this backend runs.
    pub fn requiring_legacy_cli_migration(mut self) -> Self {
        self.legacy_cli_migration_required = true;
        self
    }

    /// Put durable SDK state under trouve's application data directory.
    pub fn with_state_root(mut self, state_root: impl Into<PathBuf>) -> Self {
        self.state_root = state_root.into();
        self
    }

    /// Point the API-key exchange and usage query at a different origin
    /// (tests).
    pub fn with_dashboard(mut self, base: impl Into<String>) -> Self {
        self.dashboard_base = base.into();
        self
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelsDevCatalog>) -> Self {
        self.catalog = catalog;
        self
    }

    fn start_reaper(&self) {
        if self.pool.reaper_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let pool = Arc::downgrade(&self.pool);
        let closing = self.pool.closing.clone();
        tokio::spawn(reap_idle_until_closed(pool, closing));
    }

    fn effective_api_key(&self) -> Option<String> {
        self.api_key
            .clone()
            .or_else(|| std::env::var("CURSOR_API_KEY").ok())
            .filter(|key| !key.trim().is_empty())
    }

    /// Ask the dashboard for the current billing period's usage (and, best
    /// effort, the plan name) using only the configured Cursor API key.
    async fn query_dashboard_usage(&self) -> Result<(Value, Option<Value>), BackendError> {
        let api_key = self.effective_api_key().ok_or_else(|| {
            BackendError::Auth(
                "no Cursor API key is configured; save a Cursor user or service API key in Providers"
                    .into(),
            )
        })?;
        let http = reqwest::Client::builder()
            .timeout(USAGE_TIMEOUT)
            .build()
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        let started = std::time::Instant::now();
        let token = tokio::time::timeout(USAGE_TIMEOUT, self.exchange_api_key(&http, &api_key))
            .await
            .map_err(|_| BackendError::Protocol("Cursor API-key exchange timed out".into()))??;
        let remaining = USAGE_TIMEOUT.saturating_sub(started.elapsed());
        let usage = tokio::time::timeout(
            remaining,
            self.dashboard_rpc(&http, &token, "GetCurrentPeriodUsage"),
        )
        .await
        .map_err(|_| BackendError::Protocol("Cursor usage query timed out".into()))??;
        let remaining = USAGE_TIMEOUT.saturating_sub(started.elapsed());
        let plan_info =
            tokio::time::timeout(remaining, self.dashboard_rpc(&http, &token, "GetPlanInfo"))
                .await
                .ok()
                .and_then(Result::ok);
        Ok((usage, plan_info))
    }

    /// Exchange a Cursor user/service API key for the ephemeral access token
    /// used by the SDK's authenticated Connect clients.
    async fn exchange_api_key(
        &self,
        http: &reqwest::Client,
        api_key: &str,
    ) -> Result<String, BackendError> {
        let url = format!(
            "{}/auth/exchange_user_api_key",
            self.dashboard_base.trim_end_matches('/')
        );
        let response = http
            .post(url)
            .header("Content-Type", "application/json")
            .bearer_auth(api_key)
            .body("{}")
            .send()
            .await
            .map_err(|e| BackendError::Protocol(format!("API-key exchange: {e}")))?;
        let (status, bytes) =
            read_bounded_response(response, "Cursor API-key exchange", MAX_RPC_BODY_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(BackendError::Auth(
                "Cursor rejected the configured API key".into(),
            ));
        }
        if !status.is_success() {
            let message = body
                .pointer("/error/message")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("");
            return Err(BackendError::Protocol(format!(
                "API-key exchange: HTTP {status}: {message}"
            )));
        }
        body["accessToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                BackendError::Protocol("Cursor API-key exchange returned no access token".into())
            })
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
            self.dashboard_base.trim_end_matches('/')
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
        let (status, bytes) = read_bounded_response(response, method, MAX_RPC_BODY_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(BackendError::Auth(
                "Cursor rejected the access token exchanged from the configured API key".into(),
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

async fn reap_idle_until_closed(pool: Weak<BridgePool>, closing: CancellationToken) {
    loop {
        tokio::select! {
            biased;
            _ = closing.cancelled() => break,
            _ = tokio::time::sleep(REAP_INTERVAL) => {}
        }
        let Some(pool) = pool.upgrade() else {
            break;
        };
        if !pool.is_open() {
            break;
        }
        pool.reap_idle().await;
    }
}

#[async_trait::async_trait]
impl AgentBackend for CursorBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
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

    fn status(&self) -> BackendStatus {
        BackendStatus {
            installed: binary_on_path(&self.command),
            has_credentials: !self.legacy_cli_migration_required
                && self.effective_api_key().is_some(),
        }
    }

    fn supports_tool_free_turns(&self) -> bool {
        true
    }

    fn confines_read_only_turns(&self) -> bool {
        true
    }

    async fn subscription_health(&self) -> Option<trouve_protocol::SubscriptionHealth> {
        if self.legacy_cli_migration_required {
            return Some(trouve_protocol::SubscriptionHealth {
                provider_id: self.id.clone(),
                status: "unavailable".into(),
                plan: String::new(),
                windows: Vec::new(),
                credits: String::new(),
                note: legacy_cursor_migration_message().into(),
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

    async fn shutdown(&self) -> Result<(), BackendError> {
        self.pool.shutdown().await
    }

    async fn startup_activity(&self, turn: &BackendTurn) -> Option<BackendStartupActivity> {
        (!turn.tool_free).then_some(BackendStartupActivity::ConnectingTools)
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        if self.legacy_cli_migration_required {
            return Err(BackendError::Auth(legacy_cursor_migration_message().into()));
        }
        Err(BackendError::Auth(
            "Cursor Agent SDK authentication uses an API key; save it in Provider settings".into(),
        ))
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        if self.legacy_cli_migration_required {
            return Err(BackendError::Auth(legacy_cursor_migration_message().into()));
        }
        self.start_reaper();
        if !binary_on_path(&self.command) {
            return Err(BackendError::NotInstalled(self.command.clone()));
        }
        let api_key = self.effective_api_key().ok_or_else(|| {
            BackendError::Auth("Cursor Agent SDK requires a Cursor user or service API key".into())
        })?;
        if !turn.tool_free
            && !turn
                .mcp_bridge
                .as_ref()
                .is_some_and(|bridge| bridge.bridge_tools)
        {
            return Err(BackendError::Protocol(
                "Cursor Agent SDK requires trouve's full tool bridge for non-tool-free turns"
                    .into(),
            ));
        }

        let command = self.command.clone();
        let state_root = self.state_root.clone();
        let provider_id = self.id.clone();
        let pool = self.pool.clone();
        let stream = async_stream(move |events| async move {
            let result = run_sdk_turn(
                &pool,
                &provider_id,
                &command,
                &api_key,
                &state_root,
                turn,
                &events,
            )
            .await;
            match result {
                Ok(TurnTerminal::Finished(usage)) => {
                    let _ = events.send(Ok(BackendEvent::Completed { usage })).await;
                }
                Ok(TurnTerminal::ConsumerClosed) => {}
                Ok(TurnTerminal::Cancelled) => {
                    if !events.is_closed() {
                        let _ = events.send(Err(BackendError::Cancelled)).await;
                    }
                }
                Err(error) => {
                    if !events.is_closed() {
                        let _ = events.send(Err(error)).await;
                    }
                }
            }
        });
        Ok(stream.boxed())
    }
}

fn legacy_cursor_migration_message() -> &'static str {
    "this provider still uses the retired cursor-cli transport; open Provider settings, select Cursor (Agent SDK), and save a Cursor user or service API key"
}

enum TurnTerminal {
    Finished(Usage),
    Cancelled,
    ConsumerClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPublicationStop {
    PoolClosing,
    Cancelled,
    ConsumerClosed,
}

async fn publish_session_started(
    pool_closing: &CancellationToken,
    cancel: &CancellationToken,
    events: &BackendEventSender,
    session_id: String,
) -> Result<(), SessionPublicationStop> {
    tokio::select! {
        biased;
        _ = pool_closing.cancelled() => Err(SessionPublicationStop::PoolClosing),
        _ = cancel.cancelled() => Err(SessionPublicationStop::Cancelled),
        _ = events.closed() => Err(SessionPublicationStop::ConsumerClosed),
        result = events.send(Ok(BackendEvent::SessionStarted { session_id })) => {
            result.map_err(|()| SessionPublicationStop::ConsumerClosed)
        }
    }
}

struct BridgePool {
    process: Mutex<Option<Arc<PooledBridge>>>,
    spawn_gate: Mutex<()>,
    thread_gates: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    lifecycle: RwLock<()>,
    closed: AtomicBool,
    closing: CancellationToken,
    turn_admission: Arc<Semaphore>,
    available: Arc<Notify>,
    reaper_started: AtomicBool,
}

impl Default for BridgePool {
    fn default() -> Self {
        Self {
            process: Mutex::new(None),
            spawn_gate: Mutex::new(()),
            thread_gates: Mutex::new(HashMap::new()),
            lifecycle: RwLock::new(()),
            closed: AtomicBool::new(false),
            closing: CancellationToken::new(),
            turn_admission: Arc::new(Semaphore::new(MAX_CONCURRENT_TURNS)),
            available: Arc::new(Notify::new()),
            reaper_started: AtomicBool::new(false),
        }
    }
}

struct BridgeLease {
    process: Option<Arc<PooledBridge>>,
    thread_guard: Option<OwnedMutexGuard<()>>,
    available: Arc<Notify>,
}

/// Admission to one thread's serial Bridge lane, ordered with pool shutdown.
struct ThreadBridgeAdmission<'a> {
    _lifecycle: RwLockReadGuard<'a, ()>,
    thread_guard: OwnedMutexGuard<()>,
}

impl BridgeLease {
    fn pooled(&self) -> &Arc<PooledBridge> {
        self.process
            .as_ref()
            .expect("a live Cursor Bridge lease owns its process")
    }
}

impl std::ops::Deref for BridgeLease {
    type Target = PooledBridge;

    fn deref(&self) -> &Self::Target {
        self.pooled()
    }
}

impl Drop for BridgeLease {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            process.release_lease();
        }
        self.thread_guard.take();
        notify_available(&self.available);
    }
}

struct BridgeProcessRequest<'a> {
    command: &'a str,
    worktree: &'a Path,
    state_dir: &'a Path,
    resume_agent_id: Option<&'a str>,
    api_key: &'a str,
    cancel: &'a CancellationToken,
    events: &'a BackendEventSender,
}

impl BridgePool {
    async fn process_for(
        &self,
        admission: ThreadBridgeAdmission<'_>,
        request: BridgeProcessRequest<'_>,
    ) -> Result<BridgeLease, BackendError> {
        let ThreadBridgeAdmission {
            _lifecycle,
            thread_guard,
        } = admission;
        if !self.is_open() {
            return Err(BackendError::Protocol(
                "Cursor SDK Bridge pool is shutting down".into(),
            ));
        }
        if request.cancel.is_cancelled() || request.events.is_closed() {
            return Err(BackendError::Cancelled);
        }
        loop {
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            let existing = {
                let process = self.process.lock().await;
                process.as_ref().map(|process| {
                    // The callback wire has no turn nonce. A durable agent id
                    // already routed through this listener requires a fresh
                    // process-wide URL and bearer before ResumeAgent.
                    let agent_route_available = request
                        .resume_agent_id
                        .is_none_or(|agent_id| process.callback.accepts_agent_id(agent_id));
                    let leased = process.is_reusable()
                        && process.state_dir == request.state_dir
                        && agent_route_available;
                    if leased {
                        process.acquire_lease();
                    }
                    (process.clone(), leased)
                })
            };
            if let Some((process, leased)) = existing {
                if !leased {
                    self.quarantine(&process).await;
                    if self.recycle_if_unleased(&process, None).await? {
                        continue;
                    }
                    tokio::select! {
                        biased;
                        _ = self.closing.cancelled() => return Err(Self::closed_error()),
                        _ = request.cancel.cancelled() => return Err(BackendError::Cancelled),
                        _ = request.events.closed() => return Err(BackendError::Cancelled),
                        _ = self.available.notified() => {}
                    }
                    continue;
                }
                let alive = tokio::select! {
                    biased;
                    _ = self.closing.cancelled() => {
                        process.release_lease();
                        notify_available(&self.available);
                        return Err(Self::closed_error());
                    },
                    _ = request.cancel.cancelled() => {
                        process.release_lease();
                        notify_available(&self.available);
                        return Err(BackendError::Cancelled);
                    },
                    _ = request.events.closed() => {
                        process.release_lease();
                        notify_available(&self.available);
                        return Err(BackendError::Cancelled);
                    },
                    alive = process.is_alive() => alive,
                };
                // Incrementing the lease under the pool slot above is the
                // admission boundary. Revalidate after the asynchronous
                // liveness probe so quarantine during that probe releases the
                // reservation before any Bridge RPC. Quarantine after this
                // check is concurrent with an already-admitted turn and, by
                // design, drains without revoking active leases.
                let reusable = if alive {
                    let current = self.process.lock().await;
                    process.is_reusable()
                        && current
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &process))
                } else {
                    false
                };
                if reusable {
                    process.touch();
                    return Ok(BridgeLease {
                        process: Some(process),
                        thread_guard: Some(thread_guard),
                        available: self.available.clone(),
                    });
                }
                self.quarantine(&process).await;
                process.release_lease();
                notify_available(&self.available);
                if self.recycle_if_unleased(&process, None).await? {
                    continue;
                }
                tokio::select! {
                    biased;
                    _ = self.closing.cancelled() => return Err(Self::closed_error()),
                    _ = request.cancel.cancelled() => return Err(BackendError::Cancelled),
                    _ = request.events.closed() => return Err(BackendError::Cancelled),
                    _ = self.available.notified() => {}
                }
                continue;
            }

            let _spawn = tokio::select! {
                biased;
                _ = self.closing.cancelled() => return Err(Self::closed_error()),
                _ = request.cancel.cancelled() => return Err(BackendError::Cancelled),
                _ = request.events.closed() => return Err(BackendError::Cancelled),
                gate = self.spawn_gate.lock() => gate,
            };
            if self.process.lock().await.is_some() {
                continue;
            }
            if !self.is_open() {
                return Err(Self::closed_error());
            }
            let callback = Arc::new(CallbackRouter::start(local_http_client()?).await?);
            let bridge = BridgeProcess::start(&request, &callback, &self.closing).await;
            let mut bridge = match bridge {
                Ok(bridge) => bridge,
                Err(error) => return cleanup_error(error, callback.stop().await),
            };
            if !self.is_open() {
                let process_cleanup = bridge.shutdown().await;
                let callback_cleanup = callback.stop().await;
                return merge_io_cleanup_errors(
                    Self::closed_error(),
                    process_cleanup,
                    callback_cleanup,
                );
            }
            let process = Arc::new(PooledBridge {
                client: bridge.client.clone(),
                bridge: Mutex::new(bridge),
                callback,
                reusable: Arc::new(AtomicBool::new(true)),
                active_leases: std::sync::atomic::AtomicUsize::new(1),
                state_dir: request.state_dir.to_path_buf(),
                last_used: StdMutex::new(Instant::now()),
            });
            *self.process.lock().await = Some(process.clone());
            return Ok(BridgeLease {
                process: Some(process),
                thread_guard: Some(thread_guard),
                available: self.available.clone(),
            });
        }
    }

    async fn reap_idle(&self) {
        if !self.is_open() {
            return;
        }
        let candidate = self.process.lock().await.clone();
        let Some(candidate) = candidate else {
            return;
        };
        if !bridge_cleanup_is_due(
            candidate.is_reusable(),
            *candidate.last_used.lock().unwrap(),
            Some(IDLE_TIMEOUT),
        ) {
            return;
        }
        if let Err(error) = self
            .recycle_if_unleased(&candidate, Some(IDLE_TIMEOUT))
            .await
        {
            tracing::warn!(
                backend_state_dir = %candidate.state_dir.display(),
                %error,
                "cursor: retaining shared Bridge after cleanup failed"
            );
        }
    }

    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }

    fn closed_error() -> BackendError {
        BackendError::Protocol("Cursor SDK Bridge pool is shutting down".into())
    }

    async fn shutdown(&self) -> Result<(), BackendError> {
        // Close new admission, drain admitted turns, then wake shutdown waiters.
        let drain_deadline = tokio::time::Instant::now() + SHUTDOWN_DRAIN_TIMEOUT;
        self.closed.store(true, Ordering::Release);
        self.turn_admission.close();
        notify_available(&self.available);
        let _lifecycle = match tokio::time::timeout_at(drain_deadline, self.lifecycle.write()).await
        {
            Ok(lifecycle) => lifecycle,
            Err(_) => {
                self.closing.cancel();
                self.lifecycle.write().await
            }
        };
        let gates = self
            .thread_gates
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        let mut guards = Vec::with_capacity(gates.len());
        for gate in gates {
            let guard =
                match tokio::time::timeout_at(drain_deadline, gate.clone().lock_owned()).await {
                    Ok(guard) => guard,
                    Err(_) => {
                        self.closing.cancel();
                        gate.lock_owned().await
                    }
                };
            guards.push(guard);
        }
        let _spawn = self.spawn_gate.lock().await;
        let process = self.process.lock().await.take();
        let result = match process {
            Some(process) => match process.terminate().await {
                Ok(()) => Ok(()),
                Err(error) => {
                    // A failed process-tree cleanup remains retryable even
                    // though the pool is permanently closed to new turns.
                    self.restore_if_vacant(process).await;
                    Err(error)
                }
            },
            None => Ok(()),
        };
        self.thread_gates.lock().await.clear();
        drop(guards);
        self.closing.cancel();
        notify_available(&self.available);
        result
    }

    async fn thread_gate(&self, thread_id: &str) -> Arc<Mutex<()>> {
        let mut gates = self.thread_gates.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(thread_id).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(thread_id.to_string(), Arc::downgrade(&gate));
        gate
    }

    async fn acquire_thread_admission<'a>(
        &'a self,
        thread_id: &str,
        cancel: &CancellationToken,
        events: &BackendEventSender,
    ) -> Result<ThreadBridgeAdmission<'a>, BackendError> {
        // Shutdown owns the write side until every retained process is reaped.
        // Publish closure before taking that writer wakes all of the selects
        // below, including same-thread queues that hold lifecycle readers.
        let lifecycle = tokio::select! {
            biased;
            _ = self.closing.cancelled() => return Err(Self::closed_error()),
            _ = cancel.cancelled() => return Err(BackendError::Cancelled),
            _ = events.closed() => return Err(BackendError::Cancelled),
            lifecycle = self.lifecycle.read() => lifecycle,
        };
        if !self.is_open() {
            return Err(Self::closed_error());
        }
        if cancel.is_cancelled() || events.is_closed() {
            return Err(BackendError::Cancelled);
        }
        let gate = self.thread_gate(thread_id).await;
        let thread_guard = tokio::select! {
            biased;
            _ = self.closing.cancelled() => return Err(Self::closed_error()),
            _ = cancel.cancelled() => return Err(BackendError::Cancelled),
            _ = events.closed() => return Err(BackendError::Cancelled),
            guard = gate.lock_owned() => guard,
        };
        if !self.is_open() {
            return Err(Self::closed_error());
        }
        Ok(ThreadBridgeAdmission {
            _lifecycle: lifecycle,
            thread_guard,
        })
    }

    async fn acquire_turn_admission(
        &self,
        cancel: &CancellationToken,
        events: &BackendEventSender,
    ) -> Result<OwnedSemaphorePermit, BackendError> {
        if !self.is_open() {
            return Err(Self::closed_error());
        }
        tokio::select! {
            biased;
            _ = self.closing.cancelled() => Err(Self::closed_error()),
            _ = cancel.cancelled() => Err(BackendError::Cancelled),
            _ = events.closed() => Err(BackendError::Cancelled),
            permit = self.turn_admission.clone().acquire_owned() => {
                let permit = permit.map_err(|_| Self::closed_error())?;
                if !self.is_open() {
                    drop(permit);
                    Err(Self::closed_error())
                } else {
                    Ok(permit)
                }
            }
        }
    }

    async fn take_if_unleased(
        &self,
        candidate: &Arc<PooledBridge>,
        idle_for: Option<Duration>,
    ) -> Option<Arc<PooledBridge>> {
        let mut process = self.process.lock().await;
        if candidate.active_leases.load(Ordering::Acquire) != 0
            || !bridge_cleanup_is_due(
                candidate.is_reusable(),
                *candidate.last_used.lock().unwrap(),
                idle_for,
            )
            || !process
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, candidate))
        {
            return None;
        }
        process.take()
    }

    async fn recycle_if_unleased(
        &self,
        candidate: &Arc<PooledBridge>,
        idle_for: Option<Duration>,
    ) -> Result<bool, BackendError> {
        // The spawn gate covers the interval where the pool slot is empty but
        // the previous process tree is not yet reaped. This keeps the backend's
        // process count at one during idle eviction and quarantine recovery.
        let _spawn = self.spawn_gate.lock().await;
        let Some(process) = self.take_if_unleased(candidate, idle_for).await else {
            return Ok(false);
        };
        if let Err(error) = process.terminate().await {
            self.restore_if_vacant(process).await;
            notify_available(&self.available);
            return Err(error);
        }
        notify_available(&self.available);
        Ok(true)
    }

    async fn quarantine(&self, candidate: &Arc<PooledBridge>) {
        let process = self.process.lock().await;
        if process
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, candidate))
        {
            candidate.quarantine();
        }
    }

    async fn restore_if_vacant(&self, process: Arc<PooledBridge>) {
        let mut current = self.process.lock().await;
        if current.is_none() {
            *current = Some(process);
        }
    }
}

fn bridge_cleanup_is_due(reusable: bool, last_used: Instant, idle_for: Option<Duration>) -> bool {
    // A quarantined process needs cleanup retried immediately: applying the
    // ordinary idle threshold would let a failed cleanup retain a pool permit
    // without any path to capacity recovery.
    !reusable || idle_for.is_none_or(|idle_for| last_used.elapsed() > idle_for)
}

fn notify_available(available: &Notify) {
    available.notify_waiters();
    // `notify_waiters` is intentionally broad but does not retain a permit.
    // Store one notification as well so a release racing just before a waiter
    // registers cannot strand admission at a full pool.
    available.notify_one();
}

struct PooledBridge {
    client: BridgeClient,
    bridge: Mutex<BridgeProcess>,
    callback: Arc<CallbackRouter>,
    reusable: Arc<AtomicBool>,
    active_leases: std::sync::atomic::AtomicUsize,
    state_dir: PathBuf,
    last_used: StdMutex<Instant>,
}

impl PooledBridge {
    fn acquire_lease(&self) {
        self.active_leases.fetch_add(1, Ordering::AcqRel);
    }

    fn release_lease(&self) {
        let previous = self.active_leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "Cursor Bridge lease count underflowed");
    }

    fn is_reusable(&self) -> bool {
        self.reusable.load(Ordering::Acquire) && self.callback.listener_is_running()
    }

    fn quarantine(&self) {
        self.reusable.store(false, Ordering::Release);
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    async fn is_alive(&self) -> bool {
        let mut bridge = self.bridge.lock().await;
        match bridge.child.try_wait_leader() {
            Ok(status) => status.is_none(),
            Err(error) => {
                tracing::debug!(
                    backend_state_dir = %self.state_dir.display(),
                    bridge_pid = ?bridge.child.id(),
                    %error,
                    "cursor: failed to inspect shared Bridge"
                );
                false
            }
        }
    }

    async fn terminate(&self) -> Result<(), BackendError> {
        self.quarantine();
        let process = self.bridge.lock().await.shutdown().await;
        let callback = self.callback.stop().await;
        merge_io_cleanup_results(process, callback)
    }
}

async fn run_sdk_turn(
    pool: &BridgePool,
    provider_id: &str,
    command: &str,
    api_key: &str,
    state_root: &Path,
    turn: BackendTurn,
    events: &BackendEventSender,
) -> Result<TurnTerminal, BackendError> {
    let local_http = local_http_client()?;
    let mcp_url = (!turn.tool_free)
        .then(|| turn.mcp_bridge.as_ref().map(|bridge| bridge.url.clone()))
        .flatten();

    // Admit the thread lane first. Same-thread queues must not consume every
    // provider-wide permit while waiting for a serial Bridge they cannot yet
    // use, or unrelated threads would starve behind them.
    let thread_admission = match pool
        .acquire_thread_admission(&turn.thread_id, &turn.cancel, events)
        .await
    {
        Ok(admission) => admission,
        Err(BackendError::Cancelled) if events.is_closed() => {
            return Ok(TurnTerminal::ConsumerClosed);
        }
        Err(BackendError::Cancelled) => return Ok(TurnTerminal::Cancelled),
        Err(error) => return Err(error),
    };
    // Bound tool discovery along with every other turn-scoped resource. Tool
    // lists may be large, so queued turns must not fetch and retain one before
    // they own both their thread lane and a pool-wide admission slot.
    let _turn_admission = match pool.acquire_turn_admission(&turn.cancel, events).await {
        Ok(permit) => permit,
        Err(BackendError::Cancelled) if events.is_closed() => {
            return Ok(TurnTerminal::ConsumerClosed);
        }
        Err(BackendError::Cancelled) => return Ok(TurnTerminal::Cancelled),
        Err(error) => return Err(error),
    };
    let custom_tools = match mcp_url.as_deref() {
        Some(url) => tokio::select! {
            biased;
            _ = pool.closing.cancelled() => return Err(BridgePool::closed_error()),
            _ = turn.cancel.cancelled() => return Ok(TurnTerminal::Cancelled),
            _ = events.closed() => return Ok(TurnTerminal::ConsumerClosed),
            tools = load_custom_tools(&local_http, url) => tools?,
        },
        None => Map::new(),
    };
    if !turn.tool_free && custom_tools.is_empty() {
        return Err(BackendError::Protocol(
            "trouve's Cursor tool bridge returned no tools".into(),
        ));
    }
    let allowed_tools = custom_tools.keys().cloned().collect::<HashSet<_>>();
    let state_dir = backend_state_dir(state_root, provider_id);
    let session = select_backend_session(
        state_root,
        provider_id,
        &turn.thread_id,
        turn.session.as_deref(),
    )
    .await?;
    let process = match pool
        .process_for(
            thread_admission,
            BridgeProcessRequest {
                command,
                worktree: &turn.worktree,
                state_dir: &state_dir,
                resume_agent_id: session.resume.as_deref(),
                api_key,
                cancel: &turn.cancel,
                events,
            },
        )
        .await
    {
        Ok(process) => process,
        Err(BackendError::Cancelled) if events.is_closed() => {
            return Ok(TurnTerminal::ConsumerClosed);
        }
        Err(BackendError::Cancelled) => return Ok(TurnTerminal::Cancelled),
        Err(error) => return Err(error),
    };
    let client = process.client.clone();
    let options = agent_options(&turn, api_key, custom_tools);
    let setup = tokio::select! {
        biased;
        _ = pool.closing.cancelled() => Err(BridgePool::closed_error()),
        _ = turn.cancel.cancelled() => Err(BackendError::Cancelled),
        _ = events.closed() => Err(BackendError::Cancelled),
        setup = create_or_resume_agent(&client, session.resume.as_deref(), &options) => setup,
    };
    let (agent_id, fresh) = match setup {
        Ok(value) => value,
        Err(error) => {
            // A cancelled or failed CreateAgent/ResumeAgent request can still
            // have committed inside the Bridge after the HTTP future was
            // dropped. Quarantine the process so its shared store is reopened
            // cleanly once already-active turns have drained.
            pool.quarantine(process.pooled()).await;
            if pool.closing.is_cancelled() {
                return Err(BridgePool::closed_error());
            }
            if matches!(error, BackendError::Cancelled) {
                return Ok(if events.is_closed() {
                    TurnTerminal::ConsumerClosed
                } else {
                    TurnTerminal::Cancelled
                });
            }
            return Err(error);
        }
    };
    if let Some(marker) = session.legacy_marker.as_ref()
        && marker.recorded_agent_id.as_deref() != Some(agent_id.as_str())
        && let Err(error) = record_legacy_session_marker(marker, &agent_id).await
    {
        pool.quarantine(process.pooled()).await;
        let release = close_agent(&client, &agent_id).await;
        return finish_shared_turn(Err(error), release);
    }
    let mut route = match process
        .callback
        .register(
            agent_id.clone(),
            mcp_url,
            allowed_tools,
            turn.cancel.child_token(),
            Some(Arc::downgrade(&process.reusable)),
        )
        .await
    {
        Ok(route) => route,
        Err(error) => {
            // A duplicate or retired agent id cannot safely bind here. Closing
            // an active duplicate would interrupt that turn, and a retired id
            // needs a new callback boundary. Recycle after current routes drain.
            pool.quarantine(process.pooled()).await;
            return Err(error);
        }
    };

    if (fresh || turn.session.as_deref() != Some(agent_id.as_str()))
        && let Err(stop) =
            publish_session_started(&pool.closing, &turn.cancel, events, agent_id.clone()).await
    {
        let callbacks_settled = route.stop().await;
        let release = close_agent(&client, &agent_id).await;
        if !callbacks_settled {
            tracing::warn!(
                "cursor: callback route for agent {agent_id} did not settle after interrupted session publication; quarantining shared Bridge"
            );
        }
        let outcome = match stop {
            SessionPublicationStop::PoolClosing => Err(BridgePool::closed_error()),
            SessionPublicationStop::Cancelled => Ok(TurnTerminal::Cancelled),
            SessionPublicationStop::ConsumerClosed => Ok(TurnTerminal::ConsumerClosed),
        };
        if outcome.is_err() || release.is_err() || !callbacks_settled {
            pool.quarantine(process.pooled()).await;
        }
        return finish_shared_turn(outcome, release);
    }

    let outcome = tokio::select! {
        biased;
        _ = pool.closing.cancelled() => {
            route.supervisor.cancel.cancel();
            Err(BridgePool::closed_error())
        }
        outcome = stream_turn(
            &client,
            &agent_id,
            &turn,
            events,
            route.route.clone(),
        ) => outcome,
    };
    // Cursor can publish a terminal Send frame while an already-admitted
    // callback is still waiting on MCP. Every terminal path therefore uses
    // the same bounded, acknowledged drain before releasing the shared lease.
    let callbacks_settled = route.stop().await;
    let release = close_agent(&client, &agent_id).await;
    if !callbacks_settled {
        tracing::warn!(
            "cursor: callback route for agent {agent_id} did not settle; quarantining shared Bridge"
        );
    }
    if outcome.is_err() || release.is_err() || !callbacks_settled {
        pool.quarantine(process.pooled()).await;
    } else {
        process.touch();
    }
    finish_shared_turn(outcome, release)
}

async fn close_agent(client: &BridgeClient, agent_id: &str) -> Result<(), BackendError> {
    client
        .unary_with_timeout(
            "SdkAgentService",
            "CloseAgent",
            json!({ "agentId": agent_id }),
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
}

fn finish_shared_turn(
    outcome: Result<TurnTerminal, BackendError>,
    release: Result<(), BackendError>,
) -> Result<TurnTerminal, BackendError> {
    let mut error = outcome.as_ref().err().map(ToString::to_string);
    if let Err(release) = release {
        let release = format!("Cursor SDK agent release was not acknowledged: {release}");
        error = Some(error.map_or(release.clone(), |error| format!("{error}; {release}")));
    }
    match error {
        Some(error) => Err(BackendError::Protocol(error)),
        None => outcome,
    }
}

fn cleanup_error<T>(
    primary: BackendError,
    cleanup: std::io::Result<()>,
) -> Result<T, BackendError> {
    match cleanup {
        Ok(()) => Err(primary),
        Err(error) => Err(BackendError::Protocol(format!(
            "{primary}; Cursor SDK Bridge process cleanup was not acknowledged: {error}"
        ))),
    }
}

fn merge_io_cleanup_results(
    process: std::io::Result<()>,
    callback: std::io::Result<()>,
) -> Result<(), BackendError> {
    match (process, callback) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(BackendError::Io(error)),
        (Ok(()), Err(error)) => Err(BackendError::Protocol(format!(
            "Cursor callback router cleanup was not acknowledged: {error}"
        ))),
        (Err(process), Err(callback)) => Err(BackendError::Protocol(format!(
            "Cursor SDK Bridge process cleanup was not acknowledged: {process}; callback router cleanup was not acknowledged: {callback}"
        ))),
    }
}

fn merge_io_cleanup_errors<T>(
    primary: BackendError,
    process: std::io::Result<()>,
    callback: std::io::Result<()>,
) -> Result<T, BackendError> {
    match merge_io_cleanup_results(process, callback) {
        Ok(()) => Err(primary),
        Err(cleanup) => Err(BackendError::Protocol(format!("{primary}; {cleanup}"))),
    }
}

fn local_http_client() -> Result<reqwest::Client, BackendError> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| BackendError::Protocol(format!("local HTTP client: {error}")))
}

fn backend_state_dir(root: &Path, provider_id: &str) -> PathBuf {
    use base64::Engine as _;
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
    root.join(key)
}

fn legacy_thread_state_key(provider_id: &str, thread_id: &str) -> String {
    use base64::Engine as _;
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    hasher.update([0]);
    hasher.update(thread_id.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn legacy_thread_state_dir(root: &Path, provider_id: &str, thread_id: &str) -> PathBuf {
    root.join(legacy_thread_state_key(provider_id, thread_id))
}

fn legacy_session_marker_path(root: &Path, provider_id: &str, thread_id: &str) -> PathBuf {
    backend_state_dir(root, provider_id)
        .join(".trouve-legacy-sessions")
        .join(legacy_thread_state_key(provider_id, thread_id))
}

struct BackendSessionSelection {
    resume: Option<String>,
    legacy_marker: Option<LegacySessionMarker>,
}

struct LegacySessionMarker {
    path: PathBuf,
    recorded_agent_id: Option<String>,
}

async fn select_backend_session(
    root: &Path,
    provider_id: &str,
    thread_id: &str,
    persisted_agent_id: Option<&str>,
) -> Result<BackendSessionSelection, BackendError> {
    let marker_path = legacy_session_marker_path(root, provider_id, thread_id);
    match tokio::fs::metadata(&marker_path).await {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.len() > MAX_LEGACY_SESSION_MARKER_BYTES {
                return Err(BackendError::Protocol(format!(
                    "Cursor legacy session marker is invalid: {}",
                    marker_path.display()
                )));
            }
            let agent_id = tokio::fs::read_to_string(&marker_path)
                .await
                .map_err(BackendError::Io)?;
            if agent_id.is_empty() {
                return Err(BackendError::Protocol(format!(
                    "Cursor legacy session marker is empty: {}",
                    marker_path.display()
                )));
            }
            return Ok(BackendSessionSelection {
                resume: Some(agent_id.clone()),
                legacy_marker: Some(LegacySessionMarker {
                    path: marker_path,
                    recorded_agent_id: Some(agent_id),
                }),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(BackendError::Io(error)),
    }

    let legacy_state = legacy_thread_state_dir(root, provider_id, thread_id);
    match tokio::fs::metadata(&legacy_state).await {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(BackendError::Protocol(format!(
                    "Cursor legacy thread state is not a directory: {}",
                    legacy_state.display()
                )));
            }
            tracing::info!(
                "cursor: resetting legacy per-thread SDK session into the shared backend store"
            );
            Ok(BackendSessionSelection {
                resume: None,
                legacy_marker: Some(LegacySessionMarker {
                    path: marker_path,
                    recorded_agent_id: None,
                }),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BackendSessionSelection {
            resume: persisted_agent_id.map(str::to_string),
            legacy_marker: None,
        }),
        Err(error) => Err(BackendError::Io(error)),
    }
}

async fn record_legacy_session_marker(
    marker: &LegacySessionMarker,
    agent_id: &str,
) -> Result<(), BackendError> {
    if agent_id.is_empty() || agent_id.len() as u64 > MAX_LEGACY_SESSION_MARKER_BYTES {
        return Err(BackendError::Protocol(
            "Cursor agent id cannot be recorded in the legacy session marker".into(),
        ));
    }
    let parent = marker.path.parent().ok_or_else(|| {
        BackendError::Protocol("Cursor legacy session marker has no parent directory".into())
    })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(BackendError::Io)?;
    let temporary = marker
        .path
        .with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    if let Err(error) = tokio::fs::write(&temporary, agent_id).await {
        return Err(BackendError::Io(error));
    }
    let temporary_for_commit = temporary.clone();
    let destination = marker.path.clone();
    let replacing_existing = marker.recorded_agent_id.is_some();
    let commit = tokio::task::spawn_blocking(move || {
        commit_legacy_session_marker(
            &temporary_for_commit,
            &destination,
            replacing_existing,
            crate::install::sync_path_for_durability,
        )
    })
    .await;
    let commit = match commit {
        Ok(commit) => commit,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(BackendError::Protocol(format!(
                "Cursor legacy session marker commit task failed: {error}"
            )));
        }
    };
    if let Err(error) = commit {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(BackendError::Io(error));
    }
    Ok(())
}

fn commit_legacy_session_marker(
    temporary: &Path,
    destination: &Path,
    replacing_existing: bool,
    mut sync_path: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    // The marker becomes the durable authority for resetting one legacy
    // per-thread store. Flush its contents before publication, then flush both
    // the containing directory and its parent so first-time directory creation
    // cannot disappear independently after a power loss.
    sync_path(temporary)?;
    crate::install::replace_file_atomically(temporary, destination, replacing_existing)?;
    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Cursor legacy session marker has no parent directory",
        )
    })?;
    sync_path(parent)?;
    if let Some(grandparent) = parent.parent() {
        sync_path(grandparent)?;
    }
    Ok(())
}

fn agent_options(turn: &BackendTurn, api_key: &str, custom_tools: Map<String, Value>) -> Value {
    let model_id = match turn.model.as_str() {
        "" | "default" => "auto",
        model => model,
    };
    let params = turn
        .model_options
        .iter()
        .map(|(id, value)| {
            let value = match value {
                Value::String(value) => value.clone(),
                Value::Bool(value) => value.to_string(),
                value => value.to_string(),
            };
            json!({ "id": id, "value": value })
        })
        .collect::<Vec<_>>();
    let tool_names = if turn.tool_free {
        Vec::<String>::new()
    } else {
        vec!["mcp".to_string()]
    };
    json!({
        "model": { "id": model_id, "params": params },
        "apiKey": api_key,
        "name": format!("Trouve thread {}", turn.thread_id),
        "mode": sdk_mode(turn.permission),
        "tools": { "names": tool_names },
        "disallowedTools": CURSOR_NATIVE_TOOL_DENYLIST,
        "mcpServers": {},
        "agents": {},
        "local": {
            "cwd": [turn.worktree.to_string_lossy()],
            "settingSources": [],
            "sandboxOptions": { "enabled": false },
            "store": { "type": "sqlite" },
            "autoReview": false,
            "customTools": custom_tools,
        },
    })
}

fn sdk_mode(permission: BackendPermission) -> &'static str {
    match permission {
        BackendPermission::ReadOnly => "AGENT_MODE_OPTION_PLAN",
        BackendPermission::Ask | BackendPermission::Yolo => "AGENT_MODE_OPTION_AGENT",
    }
}

async fn create_or_resume_agent(
    client: &BridgeClient,
    session: Option<&str>,
    options: &Value,
) -> Result<(String, bool), BackendError> {
    if let Some(agent_id) = session {
        match client
            .unary_detailed(
                "SdkAgentService",
                "ResumeAgent",
                json!({ "agentId": agent_id, "options": options }),
            )
            .await
        {
            Ok(response) => {
                let resumed = required_string(&response, "agentId", "ResumeAgent")?;
                if resumed != agent_id {
                    return Err(BackendError::Protocol(
                        "Cursor ResumeAgent returned a different agent id".into(),
                    ));
                }
                return Ok((resumed, false));
            }
            Err(error) if error.is_not_found() => {
                tracing::warn!(
                    "Cursor SDK could not resume agent {agent_id}; creating a replacement"
                );
            }
            Err(error) => return Err(error.error),
        }
    }

    let response = client
        .unary(
            "SdkAgentService",
            "CreateAgent",
            json!({ "options": options }),
        )
        .await?;
    Ok((required_string(&response, "agentId", "CreateAgent")?, true))
}

fn required_string(value: &Value, key: &str, method: &str) -> Result<String, BackendError> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| BackendError::Protocol(format!("{method} omitted {key}")))
}

async fn load_custom_tools(
    http: &reqwest::Client,
    mcp_url: &str,
) -> Result<Map<String, Value>, BackendError> {
    let response = mcp_request(http, mcp_url, "tools/list", json!({}), Some(RPC_TIMEOUT)).await?;
    let tools = response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| BackendError::Protocol("MCP tools/list returned no tool array".into()))?;
    let mut definitions = Map::new();
    for tool in tools {
        let name = tool["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                BackendError::Protocol("MCP tools/list returned an unnamed tool".into())
            })?;
        let input_schema = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        if !input_schema.is_object() {
            return Err(BackendError::Protocol(format!(
                "MCP tool {name} has a non-object input schema"
            )));
        }
        definitions.insert(
            name.to_string(),
            json!({
                "description": tool["description"].as_str().unwrap_or(""),
                "inputSchema": input_schema,
            }),
        );
    }
    Ok(definitions)
}

async fn read_bounded_response(
    response: reqwest::Response,
    label: &str,
    limit: usize,
) -> Result<(reqwest::StatusCode, bytes::Bytes), BackendError> {
    let status = response.status();
    let mut stream = response.bytes_stream();
    let mut bytes = BytesMut::with_capacity(limit.min(64 * 1024));
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|error| BackendError::Protocol(format!("{label}: {error}")))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(BackendError::Protocol(format!(
                "{label} response exceeded {limit} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, bytes.freeze()))
}

async fn mcp_request(
    http: &reqwest::Client,
    mcp_url: &str,
    method: &str,
    params: Value,
    timeout: Option<Duration>,
) -> Result<Value, BackendError> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let exchange = async {
        let response = http
            .post(mcp_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .map_err(|error| BackendError::Protocol(format!("MCP {method}: {error}")))?;
        let label = format!("MCP {method}");
        let (status, bytes) = read_bounded_response(response, &label, MAX_RPC_BODY_BYTES).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            BackendError::Protocol(format!("MCP {method} returned invalid JSON: {error}"))
        })?;
        if !status.is_success() {
            return Err(BackendError::Protocol(format!(
                "MCP {method} returned HTTP {status}: {}",
                bounded_json(&value)
            )));
        }
        if let Some(error) = value.get("error") {
            return Err(BackendError::Protocol(format!(
                "MCP {method} failed: {}",
                bounded_json(error)
            )));
        }
        Ok(value)
    };
    match timeout {
        // One deadline covers the complete logical RPC, including a peer that
        // sends headers and then stalls while streaming the response body.
        Some(timeout) => tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| BackendError::Protocol(format!("MCP {method} timed out")))?,
        // ToolExecutor owns tool-specific deadlines and cancellation. The
        // turn-scoped CallbackSupervisor cancels and joins this request when
        // the turn or output consumer ends. A second adapter wall-clock
        // timeout would break interactive questions and valid long-running
        // shell/MCP operations.
        None => exchange.await,
    }
}

#[derive(Clone)]
struct CallbackState {
    bearer: Arc<str>,
    routes: Arc<StdRwLock<HashMap<String, Arc<CallbackRoute>>>>,
    retired_agent_ids: Arc<StdRwLock<HashSet<CallbackKey>>>,
    route_generation: Arc<AtomicU64>,
    identities: Arc<StdMutex<CallbackIdentities>>,
    request_slots: Arc<Semaphore>,
}

struct CallbackRoute {
    agent_id: String,
    generation: u64,
    mcp_url: Option<Arc<str>>,
    allowed_tools: Arc<HashSet<String>>,
    http: reqwest::Client,
    supervisor: Arc<CallbackSupervisor>,
    request_slots: Arc<Semaphore>,
    identities: Arc<StdMutex<CallbackIdentities>>,
    streamed_call_ids: StdMutex<HashSet<CallbackKey>>,
    owner_reusable: Option<Weak<AtomicBool>>,
    accepting: AtomicBool,
}

type CallbackKey = [u8; 32];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CallbackIdentityKey {
    agent_id: CallbackKey,
    call_id: CallbackKey,
}

#[derive(Default)]
struct CallbackIdentities {
    generations: HashMap<CallbackIdentityKey, u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallbackIdentityClaim {
    Accepted,
    Stale,
    Exhausted,
}

impl CallbackIdentities {
    fn claim(
        &mut self,
        identity: CallbackIdentityKey,
        generation: u64,
    ) -> (CallbackIdentityClaim, bool) {
        if let Some(owner) = self.generations.get(&identity) {
            return (
                if *owner == generation {
                    CallbackIdentityClaim::Accepted
                } else {
                    CallbackIdentityClaim::Stale
                },
                false,
            );
        }
        if self.generations.len() >= MAX_CALLBACK_IDENTITIES_PER_PROCESS {
            return (CallbackIdentityClaim::Exhausted, true);
        }
        self.generations.insert(identity, generation);
        (
            CallbackIdentityClaim::Accepted,
            self.generations.len() == MAX_CALLBACK_IDENTITIES_PER_PROCESS,
        )
    }
}

impl CallbackRoute {
    fn quarantine_owner(&self) {
        if let Some(reusable) = self.owner_reusable.as_ref().and_then(Weak::upgrade) {
            reusable.store(false, Ordering::Release);
        }
    }

    fn claim_identity(&self, call_id: CallbackKey) -> CallbackIdentityClaim {
        let identity = CallbackIdentityKey {
            agent_id: callback_key(&self.agent_id),
            call_id,
        };
        let (claim, retire) = self
            .identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claim(identity, self.generation);
        if retire || claim != CallbackIdentityClaim::Accepted {
            self.quarantine_owner();
        }
        claim
    }

    fn observe_stream_call_id(&self, call_id: &str) -> Result<(), String> {
        let call_id = callback_key(call_id);
        match self.claim_identity(call_id) {
            CallbackIdentityClaim::Accepted => {
                self.streamed_call_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(call_id);
                Ok(())
            }
            CallbackIdentityClaim::Stale => {
                Err("Cursor reused a custom-tool call id owned by an earlier route".into())
            }
            CallbackIdentityClaim::Exhausted => {
                Err("Cursor callback identity history reached its process bound".into())
            }
        }
    }

    async fn identities_correlated(&self) -> bool {
        // Direct router unit tests that do not own a pooled process exercise
        // HTTP routing in isolation. Production routes always carry the owner
        // flag and require the Send stream to corroborate every callback id.
        if self.owner_reusable.is_none() {
            return true;
        }
        let callback_ids = self
            .supervisor
            .calls
            .lock()
            .await
            .seen
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        let streamed_call_ids = self
            .streamed_call_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        callback_ids == streamed_call_ids
    }
}

#[derive(Clone)]
struct CallbackRecord {
    outcome: watch::Receiver<Option<CallbackOutcome>>,
}

#[derive(Default)]
struct CallbackCalls {
    records: HashMap<CallbackKey, CallbackRecord>,
    seen: HashMap<CallbackKey, CallbackKey>,
    completed: VecDeque<(CallbackKey, usize)>,
    replay_bytes: usize,
}

impl CallbackCalls {
    fn admit(
        &mut self,
        call_id: CallbackKey,
        fingerprint: CallbackKey,
        outcome: watch::Receiver<Option<CallbackOutcome>>,
    ) -> bool {
        if self.records.len() >= MAX_CALLBACK_RECORDS || self.seen.len() >= MAX_CALLBACKS_PER_TURN {
            return false;
        }
        self.seen.insert(call_id, fingerprint);
        self.records.insert(call_id, CallbackRecord { outcome });
        true
    }

    fn mark_completed(&mut self, call_id: CallbackKey, replay_bytes: usize) {
        self.completed.push_back((call_id, replay_bytes));
        self.replay_bytes = self.replay_bytes.saturating_add(replay_bytes);
        while self.completed.len() > MAX_CALLBACK_REPLAY_RECORDS
            || self.replay_bytes > MAX_CALLBACK_REPLAY_BYTES
        {
            let Some((expired, bytes)) = self.completed.pop_front() else {
                break;
            };
            self.replay_bytes = self.replay_bytes.saturating_sub(bytes);
            self.records.remove(&expired);
        }
    }

    fn forget(&mut self, call_id: &CallbackKey) {
        self.records.remove(call_id);
        self.seen.remove(call_id);
    }
}

struct CallbackSupervisor {
    calls: Mutex<CallbackCalls>,
    slots: Arc<Semaphore>,
    cancel: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
}

impl CallbackSupervisor {
    fn new(cancel: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(CallbackCalls::default()),
            slots: Arc::new(Semaphore::new(MAX_CALLBACK_CONCURRENCY)),
            cancel,
            tasks: Mutex::new(JoinSet::new()),
        })
    }

    async fn spawn(
        self: &Arc<Self>,
        call_id: CallbackKey,
        http: reqwest::Client,
        mcp_url: Arc<str>,
        request: CustomToolRequest,
        sender: watch::Sender<Option<CallbackOutcome>>,
    ) -> bool {
        let mut tasks = self.tasks.lock().await;
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!("Cursor callback task failed: {error}");
            }
        }
        if self.cancel.is_cancelled() {
            return false;
        }
        let cancel = self.cancel.clone();
        let slots = self.slots.clone();
        let supervisor = Arc::downgrade(self);
        tasks.spawn(async move {
            let permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                permit = slots.acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return,
                },
            };
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                outcome = execute_custom_tool(http, mcp_url, request) => outcome,
            };
            drop(permit);
            let replay_bytes = outcome.replay_bytes();
            let _ = sender.send(Some(outcome));
            if let Some(supervisor) = supervisor.upgrade() {
                supervisor
                    .calls
                    .lock()
                    .await
                    .mark_completed(call_id, replay_bytes);
            }
        });
        true
    }

    async fn stop(&self) {
        self.cancel.cancel();
        let mut tasks = self.tasks.lock().await;
        tasks.abort_all();
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!("Cursor callback task failed during shutdown: {error}");
            }
        }
    }
}

#[derive(Clone)]
struct CallbackOutcome {
    status: StatusCode,
    body: Value,
}

impl CallbackOutcome {
    fn replay_bytes(&self) -> usize {
        serde_json::to_vec(&self.body)
            .map(|body| body.len())
            .unwrap_or(MAX_RPC_BODY_BYTES)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomToolRequest {
    tool_name: String,
    #[serde(default)]
    tool_call_id: Option<String>,
    agent_id: String,
    args: Value,
}

#[derive(Clone, Copy)]
struct CallbackIngressGeneration(u64);

struct CallbackRouter {
    url: String,
    bearer: String,
    state: CallbackState,
    http: reqwest::Client,
    shutdown: CancellationToken,
    task: StdMutex<Option<tokio::task::JoinHandle<std::io::Result<()>>>>,
}

struct CallbackRouteLease {
    agent_id: String,
    route: Arc<CallbackRoute>,
    routes: Arc<StdRwLock<HashMap<String, Arc<CallbackRoute>>>>,
    retired_agent_ids: Arc<StdRwLock<HashSet<CallbackKey>>>,
    supervisor: Arc<CallbackSupervisor>,
    active: bool,
}

impl CallbackRouter {
    async fn start(http: reqwest::Client) -> Result<Self, BackendError> {
        let bearer = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let state = CallbackState {
            bearer: Arc::from(bearer.as_str()),
            routes: Arc::new(StdRwLock::new(HashMap::new())),
            retired_agent_ids: Arc::new(StdRwLock::new(HashSet::new())),
            route_generation: Arc::new(AtomicU64::new(0)),
            identities: Arc::new(StdMutex::new(CallbackIdentities::default())),
            request_slots: Arc::new(Semaphore::new(MAX_CALLBACK_HTTP_CONCURRENCY)),
        };
        let router = Router::new()
            .route(CALLBACK_PATH, post(custom_tool_callback))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                authenticate_callback,
            ))
            .layer(DefaultBodyLimit::max(MAX_RPC_BODY_BYTES))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(BackendError::Io)?;
        let address = listener.local_addr().map_err(BackendError::Io)?;
        let shutdown = CancellationToken::new();
        let shutdown_signal = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal.cancelled_owned())
                .await
        });
        Ok(Self {
            url: format!("http://127.0.0.1:{}", address.port()),
            bearer,
            state,
            http,
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }

    fn listener_is_running(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }

    fn accepts_agent_id(&self, agent_id: &str) -> bool {
        let routes = self
            .state
            .routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !routes.contains_key(agent_id)
            && !self
                .state
                .retired_agent_ids
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&callback_key(agent_id))
    }

    async fn register(
        &self,
        agent_id: String,
        mcp_url: Option<String>,
        allowed_tools: HashSet<String>,
        cancel: CancellationToken,
        owner_reusable: Option<Weak<AtomicBool>>,
    ) -> Result<CallbackRouteLease, BackendError> {
        if agent_id.is_empty() {
            return Err(BackendError::Protocol(
                "Cursor callback route omitted its agent id".into(),
            ));
        }
        let mut routes = self
            .state
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if routes.contains_key(&agent_id) {
            return Err(BackendError::Protocol(format!(
                "Cursor callback route for agent {agent_id} is already active"
            )));
        }
        if self
            .state
            .retired_agent_ids
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&callback_key(&agent_id))
        {
            return Err(BackendError::Protocol(format!(
                "Cursor callback route for agent {agent_id} was already retired by this Bridge"
            )));
        }
        let generation = self
            .state
            .route_generation
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or_else(|| {
                BackendError::Protocol("Cursor callback route generation exhausted".into())
            })?;
        let supervisor = CallbackSupervisor::new(cancel);
        let route = Arc::new(CallbackRoute {
            agent_id: agent_id.clone(),
            generation,
            mcp_url: mcp_url.map(Arc::from),
            allowed_tools: Arc::new(allowed_tools),
            http: self.http.clone(),
            supervisor: supervisor.clone(),
            request_slots: Arc::new(Semaphore::new(MAX_CALLBACK_CONCURRENCY)),
            identities: self.state.identities.clone(),
            streamed_call_ids: StdMutex::new(HashSet::new()),
            owner_reusable,
            accepting: AtomicBool::new(true),
        });
        routes.insert(agent_id.clone(), route.clone());
        // Publish the generation only after the matching route is present,
        // while the route-table writer still excludes handler lookup. An
        // ingress racing first registration therefore snapshots either the
        // prior generation and is rejected, or the fully published route.
        self.state
            .route_generation
            .store(generation, Ordering::Release);
        Ok(CallbackRouteLease {
            agent_id,
            route,
            routes: self.state.routes.clone(),
            retired_agent_ids: self.state.retired_agent_ids.clone(),
            supervisor,
            active: true,
        })
    }

    async fn stop(&self) -> std::io::Result<()> {
        self.stop_until(tokio::time::Instant::now() + CALLBACK_SHUTDOWN_TIMEOUT)
            .await
    }

    async fn stop_until(&self, deadline: tokio::time::Instant) -> std::io::Result<()> {
        let routes = self
            .state
            .routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(agent_id, route)| (agent_id.clone(), route.clone()))
            .collect::<Vec<_>>();
        for (_, route) in &routes {
            route.accepting.store(false, Ordering::Release);
            route.supervisor.cancel.cancel();
        }
        let mut timed_out = false;
        for (agent_id, route) in routes {
            if tokio::time::timeout_at(deadline, route.supervisor.stop())
                .await
                .is_err()
            {
                timed_out = true;
                continue;
            }
            let mut routes = self
                .state
                .routes
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if routes
                .get(&agent_id)
                .is_some_and(|current| Arc::ptr_eq(current, &route))
            {
                routes.remove(&agent_id);
            }
        }
        let route_error = timed_out.then(|| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Cursor callback routes did not settle before shutdown",
            )
        });
        self.shutdown.cancel();
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let listener_result = if let Some(mut task) = task {
            match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => Err(std::io::Error::other(format!(
                    "Cursor callback router task failed: {error}"
                ))),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Ok(())
                }
            }
        } else {
            Ok(())
        };
        match (route_error, listener_result) {
            (None, result) => result,
            (Some(route_error), Ok(())) => Err(route_error),
            (Some(route_error), Err(listener_error)) => Err(std::io::Error::new(
                route_error.kind(),
                format!("{route_error}; callback listener shutdown failed: {listener_error}"),
            )),
        }
    }
}

impl Drop for CallbackRouter {
    fn drop(&mut self) {
        for route in self
            .state
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, route)| route)
        {
            route.accepting.store(false, Ordering::Release);
            route.supervisor.cancel.cancel();
        }
        self.shutdown.cancel();
        if let Ok(mut task) = self.task.try_lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

impl CallbackRouteLease {
    async fn stop(&mut self) -> bool {
        self.stop_until(tokio::time::Instant::now() + CALLBACK_ROUTE_SHUTDOWN_TIMEOUT)
            .await
    }

    async fn stop_until(&mut self, deadline: tokio::time::Instant) -> bool {
        self.route.accepting.store(false, Ordering::Release);
        self.supervisor.cancel.cancel();
        let settled = tokio::time::timeout_at(deadline, self.supervisor.stop())
            .await
            .is_ok();
        let identities_correlated = settled && self.route.identities_correlated().await;
        if identities_correlated {
            self.detach();
        } else {
            // Relinquish removal ownership while leaving the stopped route in
            // the process router. Process quarantine can then retry joining
            // its supervisor or reject an uncorroborated callback identity
            // before admitting a replacement Bridge. Mark the owner
            // fail-closed before any later cleanup await can be dropped.
            self.route.quarantine_owner();
            self.active = false;
        }
        identities_correlated
    }

    fn detach(&mut self) {
        if !self.active {
            return;
        }
        let mut routes = self
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if routes
            .get(&self.agent_id)
            .is_some_and(|route| Arc::ptr_eq(route, &self.route))
        {
            let retire_at_capacity = {
                let mut retired = self
                    .retired_agent_ids
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                retired.insert(callback_key(&self.agent_id));
                retired.len() >= MAX_RETIRED_AGENT_IDS_PER_PROCESS
            };
            routes.remove(&self.agent_id);
            if retire_at_capacity {
                self.route.quarantine_owner();
            }
        }
        self.active = false;
    }
}

impl Drop for CallbackRouteLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.route.accepting.store(false, Ordering::Release);
        self.supervisor.cancel.cancel();
        self.route.quarantine_owner();
        // Drop cannot await supervisor settlement. Keep the stopped route in
        // the process-owned router and fail the process closed; BridgeLease
        // drop will wake pool recovery after releasing the final active lease.
        self.active = false;
    }
}

async fn authenticate_callback(
    State(state): State<CallbackState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Ok(_permit) = state.request_slots.clone().try_acquire_owned() else {
        return callback_error(
            StatusCode::TOO_MANY_REQUESTS,
            "resource_exhausted",
            "too many custom-tool callback requests are active",
        );
    };
    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", state.bearer);
    if !secure_text_eq(presented, &expected) {
        return callback_error(StatusCode::UNAUTHORIZED, "unauthenticated", "Unauthorized");
    }
    // Capture the route-table generation before body extraction can yield. A
    // request already inside the callback server must never bind to a route
    // registered later for the same durable agent id.
    request.extensions_mut().insert(CallbackIngressGeneration(
        state.route_generation.load(Ordering::Acquire),
    ));
    next.run(request).await
}

async fn custom_tool_callback(
    State(state): State<CallbackState>,
    Extension(ingress_generation): Extension<CallbackIngressGeneration>,
    Json(request): Json<CustomToolRequest>,
) -> Response {
    let route = state
        .routes
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&request.agent_id)
        .cloned();
    let Some(route) = route else {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "callback agent id has no active Cursor route",
        );
    };
    if route.generation > ingress_generation.0 {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "callback request predates the active Cursor route",
        );
    }
    if !route.accepting.load(Ordering::Acquire) {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "callback agent route is shutting down",
        );
    }
    let Ok(_route_permit) = route.request_slots.clone().try_acquire_owned() else {
        return callback_error(
            StatusCode::TOO_MANY_REQUESTS,
            "resource_exhausted",
            "too many custom-tool callback requests are active for this Cursor route",
        );
    };
    if request.tool_name.is_empty() || !request.args.is_object() {
        return callback_error(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "custom-tool callback is malformed",
        );
    }
    if !route.allowed_tools.contains(&request.tool_name) {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "custom tool is not enabled for this Cursor agent",
        );
    }
    let Some(mcp_url) = route.mcp_url.as_deref() else {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "tool calls are disabled for this turn",
        );
    };
    let Some(call_id) = request.tool_call_id.as_deref().filter(|id| !id.is_empty()) else {
        return callback_error(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "custom-tool callback omitted its tool-call id",
        );
    };
    let call_key = callback_key(call_id);
    match route.claim_identity(call_key) {
        CallbackIdentityClaim::Accepted => {}
        CallbackIdentityClaim::Stale => {
            return callback_error(
                StatusCode::FORBIDDEN,
                "permission_denied",
                "tool-call id belongs to an earlier Cursor route",
            );
        }
        CallbackIdentityClaim::Exhausted => {
            return callback_error(
                StatusCode::TOO_MANY_REQUESTS,
                "resource_exhausted",
                "Cursor callback identity history reached its process bound",
            );
        }
    }
    let fingerprint = callback_fingerprint(&request);
    let (mut outcome, execute) = {
        let mut calls = route.supervisor.calls.lock().await;
        if let Some(expected) = calls.seen.get(&call_key) {
            if expected != &fingerprint {
                return callback_error(
                    StatusCode::CONFLICT,
                    "already_exists",
                    "tool-call id was reused with different arguments",
                );
            }
            let Some(existing) = calls.records.get(&call_key) else {
                return callback_error(
                    StatusCode::CONFLICT,
                    "already_exists",
                    "tool-call result expired; refusing duplicate execution",
                );
            };
            (existing.outcome.clone(), None)
        } else {
            let (sender, receiver) = watch::channel(None);
            if !calls.admit(call_key, fingerprint, receiver.clone()) {
                return callback_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "resource_exhausted",
                    "too many custom-tool callbacks were admitted for this turn",
                );
            }
            (receiver, Some(sender))
        }
    };
    if let Some(sender) = execute {
        let http = route.http.clone();
        let mcp_url = Arc::<str>::from(mcp_url);
        if !route
            .supervisor
            .spawn(call_key, http, mcp_url, request, sender)
            .await
        {
            route.supervisor.calls.lock().await.forget(&call_key);
            return callback_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "cancelled",
                "the custom-tool callback turn is shutting down",
            );
        }
    }
    loop {
        if let Some(outcome) = outcome.borrow().clone() {
            return callback_outcome_response(&outcome);
        }
        if outcome.changed().await.is_err() {
            return callback_error(
                StatusCode::BAD_GATEWAY,
                "internal",
                "trouve could not execute the requested tool",
            );
        }
    }
}

async fn execute_custom_tool(
    http: reqwest::Client,
    mcp_url: Arc<str>,
    request: CustomToolRequest,
) -> CallbackOutcome {
    // The internal MCP endpoint persists the canonical requested/approval/
    // started/completed lifecycle around ToolExecutor. Do not mirror a
    // second BackendEvent tool card from the SDK callback.
    let result = mcp_request(
        &http,
        &mcp_url,
        "tools/call",
        json!({
            "name": request.tool_name,
            "arguments": request.args,
        }),
        None,
    )
    .await
    .and_then(|response| {
        response
            .get("result")
            .filter(|result| result.is_object())
            .cloned()
            .ok_or_else(|| {
                BackendError::Protocol("MCP tools/call returned no result object".into())
            })
    });

    match result {
        Ok(result) => CallbackOutcome {
            status: StatusCode::OK,
            body: json!({ "result": result }),
        },
        Err(error) => {
            tracing::warn!("Cursor custom-tool callback failed: {error}");
            CallbackOutcome {
                status: StatusCode::BAD_GATEWAY,
                body: json!({
                    "code": "internal",
                    "message": "trouve could not execute the requested tool",
                }),
            }
        }
    }
}

fn callback_outcome_response(outcome: &CallbackOutcome) -> Response {
    (outcome.status, Json(outcome.body.clone())).into_response()
}

fn callback_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "code": code, "message": message }))).into_response()
}

fn callback_key(call_id: &str) -> CallbackKey {
    Sha256::digest(call_id.as_bytes()).into()
}

fn callback_fingerprint(request: &CustomToolRequest) -> CallbackKey {
    let mut digest = Sha256::new();
    digest.update((request.tool_name.len() as u64).to_le_bytes());
    digest.update(request.tool_name.as_bytes());
    serde_json::to_writer(DigestWriter(&mut digest), &request.args)
        .expect("a deserialized Cursor callback argument remains serializable");
    digest.finalize().into()
}

struct DigestWriter<'a>(&'a mut Sha256);

impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn secure_text_eq(left: &str, right: &str) -> bool {
    let left = Sha256::digest(left.as_bytes());
    let right = Sha256::digest(right.as_bytes());
    left == right
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadyPayload {
    schema_version: u64,
    transport: String,
    protocol: String,
    url: String,
    #[serde(default)]
    auth_token: Option<String>,
    #[serde(default)]
    auth_token_file: Option<String>,
}

struct BridgeProcess {
    child: ProcessTreeChild,
    client: BridgeClient,
    stderr_task: tokio::task::JoinHandle<()>,
    _runtime_dir: tempfile::TempDir,
}

impl BridgeProcess {
    async fn start(
        request: &BridgeProcessRequest<'_>,
        callback: &CallbackRouter,
        closing: &CancellationToken,
    ) -> Result<Self, BackendError> {
        let command = request.command;
        let worktree = request.worktree;
        let state_dir = request.state_dir;
        let api_key = request.api_key;
        let cancel = request.cancel;
        let events = request.events;
        let http = local_http_client()?;
        create_private_dir(state_dir)?;
        // The Bridge contract puts its per-process bearer token in the OS
        // temporary directory, independently of its durable state root. Point
        // every supported temp variable at an adapter-owned private directory
        // so the readiness path can still be validated fail-closed.
        let runtime_dir = tempfile::Builder::new()
            .prefix("bridge-runtime-")
            .tempdir_in(state_dir)
            .map_err(BackendError::Io)?;
        let mut process = crate::process_env::tokio_command(command);
        process
            .current_dir(worktree)
            .env("CURSOR_API_KEY", api_key)
            .env("CURSOR_SDK_BRIDGE_STATE_ROOT", state_dir)
            .env("CURSOR_SDK_BRIDGE_WORKSPACE", worktree)
            .env("CURSOR_SDK_CLIENT_LANGUAGE", "rust")
            .env("CURSOR_SDK_TOOL_CALLBACK_AUTH_TOKEN", &callback.bearer)
            .env("CURSOR_SDK_TOOL_CALLBACK_URL", &callback.url)
            .env("TMPDIR", runtime_dir.path())
            .env("TEMP", runtime_dir.path())
            .env("TMP", runtime_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = spawn_process_tree(&mut process).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(command.to_string()),
            _ => BackendError::Io(error),
        })?;
        let stderr = child.take_stderr().ok_or_else(|| {
            BackendError::Protocol("Cursor SDK Bridge stderr was not piped".into())
        })?;
        let mut stderr = BufReader::new(stderr);
        let mut diagnostics = VecDeque::new();
        let ready_result = tokio::select! {
            biased;
            _ = closing.cancelled() => Err(BridgePool::closed_error()),
            _ = cancel.cancelled() => Err(BackendError::Cancelled),
            _ = events.closed() => Err(BackendError::Cancelled),
            result = tokio::time::timeout(STARTUP_TIMEOUT, async {
                loop {
                    let line = read_bounded_line(&mut stderr, MAX_DIAGNOSTIC_LINE_BYTES)
                        .await
                        .map_err(BackendError::Io)?;
                    let Some(line) = line else {
                        return Err(BackendError::Protocol(
                            "Cursor SDK Bridge exited before reporting readiness".into(),
                        ));
                    };
                    if !line.truncated && let Some(payload) = line.text.strip_prefix(READY_PREFIX) {
                        return serde_json::from_str::<ReadyPayload>(payload).map_err(|error| {
                            BackendError::Protocol(format!(
                                "Cursor SDK Bridge emitted an invalid readiness payload: {error}"
                            ))
                        });
                    }
                    push_diagnostic(
                        &mut diagnostics,
                        redact(&line.display(), &[api_key, &callback.bearer]),
                    );
                }
            }) => result.map_err(|_| {
                BackendError::Protocol("Cursor SDK Bridge startup timed out".into())
            })?,
        };
        let ready = match ready_result {
            Ok(ready) => ready,
            Err(error) => {
                let error = startup_error_with_diagnostics(error, &diagnostics);
                let cleanup = child.terminate_and_reap().await;
                return cleanup_error(error, cleanup.map(|_| ()));
            }
        };
        if ready.schema_version != 1 || ready.transport != "tcp" || ready.protocol != "connect" {
            let error = startup_error_with_diagnostics(
                BackendError::Protocol(
                    "Cursor SDK Bridge returned an unsupported discovery payload".into(),
                ),
                &diagnostics,
            );
            let cleanup = child.terminate_and_reap().await;
            return cleanup_error(error, cleanup.map(|_| ()));
        }
        if let Err(error) = validate_loopback_bridge_url(&ready.url) {
            let error = startup_error_with_diagnostics(error, &diagnostics);
            let cleanup = child.terminate_and_reap().await;
            return cleanup_error(error, cleanup.map(|_| ()));
        }
        let token = match bridge_token(&ready, runtime_dir.path()) {
            Ok(token) => token,
            Err(error) => {
                let error = startup_error_with_diagnostics(error, &diagnostics);
                let cleanup = child.terminate_and_reap().await;
                return cleanup_error(error, cleanup.map(|_| ()));
            }
        };
        let process_secrets = vec![api_key.to_string(), callback.bearer.clone(), token.clone()];
        redact_diagnostics(&mut diagnostics, &process_secrets);
        let secrets = Arc::new(StdMutex::new(process_secrets));
        let shared_diagnostics = Arc::new(tokio::sync::Mutex::new(diagnostics));
        let drain_diagnostics = shared_diagnostics.clone();
        let drain_secrets = secrets.clone();
        let stderr_task = tokio::spawn(async move {
            while let Ok(Some(line)) =
                read_bounded_line(&mut stderr, MAX_DIAGNOSTIC_LINE_BYTES).await
            {
                let line = {
                    let secrets = drain_secrets.lock().unwrap();
                    redact(
                        &line.display(),
                        &secrets.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                };
                let mut diagnostics = drain_diagnostics.lock().await;
                push_diagnostic(&mut diagnostics, line);
            }
        });
        Ok(Self {
            child,
            client: BridgeClient {
                http,
                base_url: ready.url.trim_end_matches('/').to_string(),
                token,
                secrets,
                diagnostics: shared_diagnostics,
            },
            stderr_task,
            _runtime_dir: runtime_dir,
        })
    }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        if let Err(error) = self
            .client
            .unary_with_timeout(
                "SdkBridgeControlService",
                "Shutdown",
                json!({ "graceSeconds": 1 }),
                Duration::from_secs(5),
            )
            .await
        {
            tracing::debug!("Cursor SDK Bridge shutdown RPC failed: {error}");
        }
        let cleanup = self.child.terminate_and_reap().await.map(|_| ());
        self.stderr_task.abort();
        cleanup
    }
}

fn create_private_dir(path: &Path) -> Result<(), BackendError> {
    std::fs::create_dir_all(path).map_err(BackendError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(BackendError::Io)?;
    }
    Ok(())
}

fn validate_loopback_bridge_url(url: &str) -> Result<(), BackendError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| BackendError::Protocol(format!("invalid Bridge URL: {error}")))?;
    if parsed.scheme() != "http"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.host_str().is_some_and(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        })
    {
        return Err(BackendError::Protocol(
            "Cursor SDK Bridge must advertise an uncredentialed loopback HTTP URL".into(),
        ));
    }
    Ok(())
}

fn bridge_token(ready: &ReadyPayload, state_dir: &Path) -> Result<String, BackendError> {
    if let Some(token) = ready
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        return Ok(token.to_string());
    }
    let file = ready.auth_token_file.as_deref().ok_or_else(|| {
        BackendError::Protocol("Cursor SDK Bridge returned no bearer token".into())
    })?;
    let file = PathBuf::from(file);
    let file = if file.is_absolute() {
        file
    } else {
        state_dir.join(file)
    };
    let root = std::fs::canonicalize(state_dir).map_err(BackendError::Io)?;
    let file = std::fs::canonicalize(file).map_err(BackendError::Io)?;
    if !file.starts_with(&root) {
        return Err(BackendError::Protocol(
            "Cursor SDK Bridge bearer-token file escaped its private runtime directory".into(),
        ));
    }
    let token = std::fs::read_to_string(file).map_err(BackendError::Io)?;
    let token = token.trim();
    if token.is_empty() {
        return Err(BackendError::Protocol(
            "Cursor SDK Bridge returned an empty bearer token".into(),
        ));
    }
    Ok(token.to_string())
}

struct BoundedLine {
    text: String,
    truncated: bool,
}

impl BoundedLine {
    fn display(&self) -> String {
        if self.truncated {
            "[oversized diagnostic line omitted]".into()
        } else {
            self.text.clone()
        }
    }
}

/// Read and drain one newline-delimited record while retaining at most
/// `limit` bytes. `AsyncBufReadExt::lines` retains an entire unterminated
/// line internally, which would let a defective child bypass the diagnostics
/// cap.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<BoundedLine>> {
    let mut bytes = Vec::with_capacity(limit.min(4096));
    let mut truncated = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() && !truncated {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        let retained = content_len.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&available[..retained]);
        truncated |= retained < content_len;
        let consumed = newline.map_or(available.len(), |position| position + 1);
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if truncated {
        // Never persist a prefix of an oversized line: a configured secret
        // could cross the retention boundary and evade exact-value redaction.
        bytes.clear();
    } else if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(BoundedLine {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    }))
}

fn push_diagnostic(lines: &mut VecDeque<String>, line: String) {
    if line.trim().is_empty() {
        return;
    }
    if lines.len() == MAX_DIAGNOSTIC_LINES {
        lines.pop_front();
    }
    lines.push_back(bounded_text(&line));
}

fn redact_diagnostics(lines: &mut VecDeque<String>, secrets: &[String]) {
    let secrets = secrets.iter().map(String::as_str).collect::<Vec<_>>();
    for line in lines {
        *line = redact(line, &secrets);
    }
}

fn startup_error_with_diagnostics(
    error: BackendError,
    diagnostics: &VecDeque<String>,
) -> BackendError {
    if matches!(error, BackendError::Cancelled) || diagnostics.is_empty() {
        error
    } else {
        BackendError::Protocol(format!(
            "{error}; Bridge diagnostics: {}",
            diagnostics.iter().cloned().collect::<Vec<_>>().join(" | ")
        ))
    }
}

fn redact(value: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(value.to_string(), |value, secret| {
            value.replace(secret, "[REDACTED]")
        })
}

#[derive(Clone)]
struct BridgeClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    secrets: Arc<StdMutex<Vec<String>>>,
    diagnostics: Arc<tokio::sync::Mutex<VecDeque<String>>>,
}

struct BridgeRpcFailure {
    code: Option<String>,
    error: BackendError,
}

impl BridgeRpcFailure {
    fn is_not_found(&self) -> bool {
        self.code.as_deref() == Some("not_found")
    }
}

impl From<BackendError> for BridgeRpcFailure {
    fn from(error: BackendError) -> Self {
        Self { code: None, error }
    }
}

impl BridgeClient {
    async fn unary(&self, service: &str, method: &str, body: Value) -> Result<Value, BackendError> {
        self.unary_with_timeout(service, method, body, RPC_TIMEOUT)
            .await
    }

    async fn unary_detailed(
        &self,
        service: &str,
        method: &str,
        body: Value,
    ) -> Result<Value, BridgeRpcFailure> {
        self.unary_detailed_with_timeout(service, method, body, RPC_TIMEOUT)
            .await
    }

    async fn unary_with_timeout(
        &self,
        service: &str,
        method: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, BackendError> {
        self.unary_detailed_with_timeout(service, method, body, timeout)
            .await
            .map_err(|failure| failure.error)
    }

    async fn unary_detailed_with_timeout(
        &self,
        service: &str,
        method: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeRpcFailure> {
        let url = format!("{}/sdk.v1.{service}/{method}", self.base_url);
        let deadline = tokio::time::Instant::now() + timeout;
        let response = tokio::time::timeout_at(
            deadline,
            self.http
                .post(url)
                .bearer_auth(&self.token)
                .header("Content-Type", "application/json")
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| BackendError::Protocol(format!("{method} timed out")))?
        .map_err(|error| BackendError::Protocol(format!("{method}: {error}")))?;
        let (status, bytes) = tokio::time::timeout_at(
            deadline,
            read_bounded_response(response, method, MAX_RPC_BODY_BYTES),
        )
        .await
        .map_err(|_| BackendError::Protocol(format!("{method} response timed out")))??;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            BackendError::Protocol(format!("{method} returned invalid JSON: {error}"))
        })?;
        if !status.is_success() || (value.get("code").is_some() && value.get("message").is_some()) {
            let detail = self.redact(&bounded_json(&value));
            let diagnostics = self.diagnostic_suffix().await;
            return Err(BridgeRpcFailure {
                code: value
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                error: BackendError::Protocol(format!(
                    "{method} failed (HTTP {status}): {detail}{diagnostics}"
                )),
            });
        }
        Ok(value)
    }

    fn redact(&self, value: &str) -> String {
        let secrets = self.secrets.lock().unwrap();
        redact(
            value,
            &secrets.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }

    async fn diagnostic_suffix(&self) -> String {
        let diagnostics = self
            .diagnostics
            .lock()
            .await
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        if diagnostics.is_empty() {
            String::new()
        } else {
            format!("; Bridge diagnostics: {}", self.redact(&diagnostics))
        }
    }
}

async fn next_send_chunk<S>(
    stream: &mut S,
    idle_timeout: Duration,
) -> Result<Option<S::Ok>, BackendError>
where
    S: futures::TryStream + Unpin,
    S::Error: std::fmt::Display,
{
    tokio::time::timeout(idle_timeout, stream.try_next())
        .await
        .map_err(|_| {
            BackendError::Protocol(format!(
                "Cursor Send stream made no progress for {} seconds",
                idle_timeout.as_secs()
            ))
        })?
        .map_err(|error| BackendError::Protocol(format!("Cursor Send stream failed: {error}")))
}

async fn stream_turn(
    client: &BridgeClient,
    agent_id: &str,
    turn: &BackendTurn,
    events: &BackendEventSender,
    callback_route: Arc<CallbackRoute>,
) -> Result<TurnTerminal, BackendError> {
    let callback_cancel = callback_route.supervisor.cancel.clone();
    let text = match turn.instructions.as_deref() {
        Some(instructions) => format!(
            "<mode-instructions>\n{instructions}\n</mode-instructions>\n\n{}",
            turn.prompt
        ),
        None => turn.prompt.clone(),
    };
    let images = turn
        .attachments
        .iter()
        .map(|attachment| {
            let (width, height) = image_dimensions(&attachment.bytes);
            json!({
                "data": {
                    "data": attachment.base64(),
                    "mimeType": attachment.mime,
                },
                "dimension": { "width": width, "height": height },
            })
        })
        .collect::<Vec<_>>();
    let request = json!({
        "agentId": agent_id,
        "message": {
            "text": text,
            "images": images,
        },
        "options": {
            "enableDeltas": false,
            "enableSteps": true,
            "mode": sdk_mode(turn.permission),
        },
    });
    let mut body = Vec::new();
    let payload = serde_json::to_vec(&request)
        .map_err(|error| BackendError::Protocol(format!("encoding Cursor Send: {error}")))?;
    let length = u32::try_from(payload.len())
        .map_err(|_| BackendError::Protocol("Cursor Send request is too large".into()))?;
    body.push(0);
    body.extend_from_slice(&length.to_be_bytes());
    body.extend_from_slice(&payload);

    let url = format!("{}/sdk.v1.SdkAgentService/Send", client.base_url);
    let response = tokio::select! {
        biased;
        _ = turn.cancel.cancelled() => return Ok(TurnTerminal::Cancelled),
        _ = events.closed() => {
            callback_cancel.cancel();
            return Ok(TurnTerminal::ConsumerClosed);
        }
        response = tokio::time::timeout(
            RPC_TIMEOUT,
            client.http
                .post(url)
                .bearer_auth(&client.token)
                .header("Connect-Protocol-Version", "1")
                .header("Content-Type", "application/connect+json")
                .body(body)
                .send(),
        ) => response
            .map_err(|_| BackendError::Protocol(
                "Cursor Send timed out waiting for response headers".into()
            ))?
            .map_err(|error| BackendError::Protocol(format!("Cursor Send: {error}")))?,
    };
    if !response.status().is_success() {
        let (status, detail) = tokio::select! {
            biased;
            _ = turn.cancel.cancelled() => return Ok(TurnTerminal::Cancelled),
            _ = events.closed() => {
                callback_cancel.cancel();
                return Ok(TurnTerminal::ConsumerClosed);
            }
            detail = tokio::time::timeout(
                RPC_TIMEOUT,
                read_bounded_response(response, "Cursor Send", MAX_RPC_BODY_BYTES),
            ) => detail
                .map_err(|_| BackendError::Protocol(
                    "Cursor Send error response timed out".into()
                ))??,
        };
        let detail = String::from_utf8_lossy(&detail);
        return Err(BackendError::Protocol(format!(
            "Cursor Send failed (HTTP {status}): {}",
            client.redact(&bounded_text(&detail))
        )));
    }

    let mut stream = Box::pin(response.bytes_stream());
    let mut buffered = BytesMut::new();
    let mut projection = RunProjection {
        callback_route: Some(callback_route),
        ..RunProjection::default()
    };
    let mut connect_state = ConnectStreamState::Active;
    let mut stop = None;
    let mut stop_deadline: Option<tokio::time::Instant> = None;
    let mut cancel_sent = false;

    'stream: loop {
        if stop.is_some()
            && !cancel_sent
            && let Some(run_id) = projection.run_id.as_deref()
        {
            let cancel_timeout = stop_deadline
                .expect("a stopped Cursor stream owns its cancellation deadline")
                .saturating_duration_since(tokio::time::Instant::now());
            if cancel_timeout.is_zero() {
                tracing::debug!(
                    "Cursor run identity arrived after the CancelRun deadline; process cleanup will stop the turn"
                );
            } else if let Err(error) = client
                .unary_with_timeout(
                    "SdkAgentService",
                    "CancelRun",
                    json!({ "runId": run_id, "agentId": agent_id }),
                    cancel_timeout,
                )
                .await
            {
                tracing::debug!(
                    "Cursor CancelRun failed; process cleanup will stop the turn: {error}"
                );
            }
            cancel_sent = true;
        }

        let next = if let Some(deadline) = stop_deadline {
            match tokio::time::timeout_at(deadline, stream.try_next()).await {
                Ok(next) => next.map_err(|error| {
                    BackendError::Protocol(format!("Cursor Send stream failed: {error}"))
                }),
                Err(_) => break,
            }
        } else {
            tokio::select! {
                biased;
                _ = turn.cancel.cancelled() => {
                    stop = Some(StreamStop::Cancelled);
                    stop_deadline = Some(tokio::time::Instant::now() + CANCEL_ACK_TIMEOUT);
                    continue;
                }
                _ = events.closed() => {
                    callback_cancel.cancel();
                    stop = Some(StreamStop::ConsumerClosed);
                    stop_deadline = Some(tokio::time::Instant::now() + CANCEL_ACK_TIMEOUT);
                    continue;
                }
                next = next_send_chunk(&mut stream, SEND_IDLE_TIMEOUT) => next,
            }
        };
        let chunk = match next {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if stop.is_some() => {
                tracing::debug!("Cursor Send ended while cancellation was pending: {error}");
                break;
            }
            Err(error) => return Err(error),
        };
        if stop.is_none() && events.is_closed() {
            callback_cancel.cancel();
            stop = Some(StreamStop::ConsumerClosed);
            stop_deadline = Some(tokio::time::Instant::now() + CANCEL_ACK_TIMEOUT);
        }
        buffered.extend_from_slice(&chunk);
        while buffered.len() >= 5 {
            let flags = buffered[0];
            let length =
                u32::from_be_bytes([buffered[1], buffered[2], buffered[3], buffered[4]]) as usize;
            if length > MAX_CONNECT_FRAME_BYTES {
                if stop.is_some() {
                    break 'stream;
                }
                return Err(BackendError::Protocol(format!(
                    "Cursor Connect frame exceeded {MAX_CONNECT_FRAME_BYTES} bytes"
                )));
            }
            if buffered.len() < 5 + length {
                break;
            }
            let frame = buffered.split_to(5 + length);
            if flags & 0x01 != 0 {
                if stop.is_some() {
                    continue;
                }
                return Err(BackendError::Protocol(
                    "compressed Cursor Connect frames are not supported".into(),
                ));
            }
            let is_connect_end = if stop.is_none() {
                connect_state.observe_frame(flags)?
            } else {
                flags & 0x02 != 0
            };
            let value = if length == 0 {
                json!({})
            } else {
                match serde_json::from_slice::<Value>(&frame[5..]) {
                    Ok(value) => value,
                    Err(_) if stop.is_some() => continue,
                    Err(error) => {
                        return Err(BackendError::Protocol(format!(
                            "Cursor Send emitted invalid Connect JSON: {error}"
                        )));
                    }
                }
            };
            if is_connect_end {
                if stop.is_none()
                    && let Some(error) = value.get("error")
                {
                    return Err(BackendError::Protocol(format!(
                        "Cursor Send failed: {}",
                        client.redact(&bounded_json(error))
                    )));
                }
            } else if stop.is_some() {
                projection.capture_frame_run_id(&value);
            } else {
                if let Err(reason) = projection.process(value, events, &turn.cancel).await {
                    if reason == StreamStop::ConsumerClosed {
                        callback_cancel.cancel();
                    }
                    stop = Some(reason);
                    stop_deadline = Some(tokio::time::Instant::now() + CANCEL_ACK_TIMEOUT);
                }
            }
        }
    }

    if let Some(stop) = stop {
        return Ok(match stop {
            StreamStop::Cancelled => TurnTerminal::Cancelled,
            StreamStop::ConsumerClosed => TurnTerminal::ConsumerClosed,
        });
    }
    if !buffered.is_empty() {
        return Err(BackendError::Protocol(
            "Cursor Send ended with a partial Connect frame".into(),
        ));
    }
    if connect_state != ConnectStreamState::Ended {
        return Err(BackendError::Protocol(
            "Cursor Send omitted the Connect end-stream frame".into(),
        ));
    }
    projection.finish(events, &turn.cancel).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectStreamState {
    Active,
    Ended,
}

impl ConnectStreamState {
    fn observe_frame(&mut self, flags: u8) -> Result<bool, BackendError> {
        if *self == Self::Ended {
            return Err(BackendError::Protocol(
                "Cursor Send emitted a frame after its Connect end-stream envelope".into(),
            ));
        }
        let ended = flags & 0x02 != 0;
        if ended {
            *self = Self::Ended;
        }
        Ok(ended)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamStop {
    Cancelled,
    ConsumerClosed,
}

async fn send_projected_event(
    events: &BackendEventSender,
    cancel: &CancellationToken,
    event: BackendEvent,
) -> Result<(), StreamStop> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(StreamStop::Cancelled),
        _ = events.closed() => Err(StreamStop::ConsumerClosed),
        result = events.send(Ok(event)) => result.map_err(|()| StreamStop::ConsumerClosed),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DoneState {
    #[default]
    Missing,
    Seen,
    Malformed,
}

#[derive(Default)]
struct RunProjection {
    run_id: Option<String>,
    final_text: Option<String>,
    usage: Usage,
    emitted_assistant_text: bool,
    last_status_message: Option<String>,
    terminal_status: Option<Value>,
    terminal_error_code: Option<String>,
    done: DoneState,
    callback_route: Option<Arc<CallbackRoute>>,
    protocol_error: Option<String>,
}

impl RunProjection {
    async fn process(
        &mut self,
        frame: Value,
        events: &BackendEventSender,
        cancel: &CancellationToken,
    ) -> Result<(), StreamStop> {
        self.capture_frame_run_id(&frame);
        if let Some(sdk) = frame.get("sdkMessage") {
            let payload = sdk.get("message").unwrap_or(&Value::Null);
            let kind = payload
                .get("type")
                .or_else(|| sdk.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "assistant" => {
                    for text in message_text_blocks(payload) {
                        if !text.is_empty() {
                            self.emitted_assistant_text = true;
                            send_projected_event(events, cancel, BackendEvent::TextDelta(text))
                                .await?;
                        }
                    }
                }
                "thinking" => {
                    if let Some(text) = message_text(payload)
                        && !text.is_empty()
                    {
                        send_projected_event(events, cancel, BackendEvent::ThinkingDelta(text))
                            .await?;
                        send_projected_event(events, cancel, BackendEvent::ThinkingCompleted)
                            .await?;
                    }
                }
                "task" => {
                    if let Some(text) = message_text(payload)
                        && !text.is_empty()
                    {
                        send_projected_event(events, cancel, BackendEvent::ProgressDelta(text))
                            .await?;
                    }
                }
                "status" => {
                    self.last_status_message = payload
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                "usage" => {
                    if let Some(usage) = payload.get("usage").map(parse_usage) {
                        self.usage = usage.clone();
                        send_projected_event(events, cancel, BackendEvent::UsageUpdated { usage })
                            .await?;
                    }
                }
                // Concrete tool lifecycle events are emitted by the
                // authenticated callback, which knows the actual trouve tool
                // name and arguments. SDK messages expose only the dispatcher
                // name (`mcp`) on some Bridge versions, but their call id is a
                // turn-specific identity fence for delayed callback retries.
                "tool_call" => {
                    if let Some(route) = self.callback_route.as_ref() {
                        let observed = payload
                            .get("call_id")
                            .or_else(|| payload.get("callId"))
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| {
                                "Cursor tool lifecycle event omitted its call id".to_string()
                            })
                            .and_then(|call_id| route.observe_stream_call_id(call_id));
                        if let Err(error) = observed {
                            route.quarantine_owner();
                            self.protocol_error.get_or_insert(error);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(result) = frame.get("result") {
            self.terminal_status = result.get("status").cloned();
            self.terminal_error_code = result
                .get("errorCode")
                .or_else(|| result.get("error_code"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if let Some(run_result) = result.get("result") {
                self.final_text = run_result
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(usage) = run_result.get("usage") {
                    self.usage = parse_usage(usage);
                }
            }
        }
        if let Some(done) = frame.get("done") {
            self.done = match (self.done, done.is_object()) {
                (DoneState::Missing, true) => DoneState::Seen,
                _ => DoneState::Malformed,
            };
        }
        Ok(())
    }

    fn capture_frame_run_id(&mut self, frame: &Value) {
        if let Some(message) = frame.pointer("/sdkMessage/message") {
            self.capture_run_id(message);
        }
        if let Some(result) = frame.get("result") {
            self.capture_run_id(result);
        }
        if let Some(done) = frame.get("done") {
            self.capture_run_id(done);
        }
    }

    fn capture_run_id(&mut self, value: &Value) {
        if self.run_id.is_none() {
            self.run_id = value
                .get("runId")
                .or_else(|| value.get("run_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
    }

    async fn finish(
        mut self,
        events: &BackendEventSender,
        cancel: &CancellationToken,
    ) -> Result<TurnTerminal, BackendError> {
        if cancel.is_cancelled() {
            return Ok(TurnTerminal::Cancelled);
        }
        if events.is_closed() {
            return Ok(TurnTerminal::ConsumerClosed);
        }
        if let Some(error) = self.protocol_error.take() {
            return Err(BackendError::Protocol(error));
        }
        match self.done {
            DoneState::Seen => {}
            DoneState::Missing => {
                return Err(BackendError::Protocol(
                    "Cursor Send omitted its done envelope".into(),
                ));
            }
            DoneState::Malformed => {
                return Err(BackendError::Protocol(
                    "Cursor Send emitted an invalid or duplicate done envelope".into(),
                ));
            }
        }
        let status = self.terminal_status.as_ref().ok_or_else(|| {
            BackendError::Protocol("Cursor Send omitted its terminal result".into())
        })?;
        if status_is_finished(status) {
            if !self.emitted_assistant_text
                && let Some(text) = self.final_text
                && !text.is_empty()
                && let Err(stop) =
                    send_projected_event(events, cancel, BackendEvent::TextDelta(text)).await
            {
                return Ok(match stop {
                    StreamStop::Cancelled => TurnTerminal::Cancelled,
                    StreamStop::ConsumerClosed => TurnTerminal::ConsumerClosed,
                });
            }
            return Ok(TurnTerminal::Finished(self.usage));
        }
        if status_is_cancelled(status) {
            return Ok(TurnTerminal::Cancelled);
        }
        let message = self
            .last_status_message
            .or(self.terminal_error_code)
            .unwrap_or_else(|| format!("terminal status {}", bounded_json(status)));
        Err(BackendError::Protocol(format!(
            "Cursor agent run failed: {message}"
        )))
    }
}

fn message_text_blocks(payload: &Value) -> Vec<String> {
    let content = payload
        .pointer("/message/content")
        .or_else(|| payload.get("content"))
        .and_then(Value::as_array);
    let mut text = Vec::new();
    if let Some(content) = content {
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("text")
                && let Some(value) = block.get("text").and_then(Value::as_str)
            {
                text.push(value.to_string());
            }
        }
    }
    if text.is_empty()
        && let Some(value) = payload.get("text").and_then(Value::as_str)
    {
        text.push(value.to_string());
    }
    text
}

fn message_text(payload: &Value) -> Option<String> {
    let direct = payload
        .get("text")
        .or_else(|| payload.get("message").filter(|value| value.is_string()))
        .and_then(Value::as_str);
    direct.map(str::to_string).or_else(|| {
        let blocks = message_text_blocks(payload);
        (!blocks.is_empty()).then(|| blocks.join(""))
    })
}

fn parse_usage(value: &Value) -> Usage {
    let input = u64_flex(
        value
            .get("inputTokens")
            .or_else(|| value.get("input_tokens"))
            .unwrap_or(&Value::Null),
    )
    .unwrap_or(0);
    let output = u64_flex(
        value
            .get("outputTokens")
            .or_else(|| value.get("output_tokens"))
            .unwrap_or(&Value::Null),
    )
    .unwrap_or(0);
    let cached = u64_flex(
        value
            .get("cacheReadTokens")
            .or_else(|| value.get("cache_read_tokens"))
            .unwrap_or(&Value::Null),
    )
    .unwrap_or(0);
    Usage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
        context_input_tokens: Some(input.saturating_add(cached)),
        cost_usd: None,
        context_window: None,
    }
}

fn u64_flex(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .or_else(|| {
            value
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value as u64)
        })
}

fn status_is_finished(status: &Value) -> bool {
    status.as_u64() == Some(3)
        || matches!(status.as_str(), Some("3" | "RUN_LIFECYCLE_STATUS_FINISHED"))
}

fn status_is_cancelled(status: &Value) -> bool {
    status.as_u64() == Some(5)
        || matches!(
            status.as_str(),
            Some("5" | "RUN_LIFECYCLE_STATUS_CANCELLED")
        )
}

fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        if width > 0 && height > 0 {
            return (width, height);
        }
    }
    if bytes.len() >= 10 && matches!(&bytes[..6], b"GIF87a" | b"GIF89a") {
        let width = u16::from_le_bytes(bytes[6..8].try_into().unwrap()) as u32;
        let height = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as u32;
        if width > 0 && height > 0 {
            return (width, height);
        }
    }
    if let Some(dimensions) = jpeg_dimensions(bytes) {
        return dimensions;
    }
    // The SDK requires non-zero dimensions. Unknown image formats retain
    // their original bytes and MIME type; this conservative placeholder is
    // only metadata for the local request.
    (1, 1)
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut offset = 2;
    while offset < bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        // JPEG permits any number of 0xff fill bytes before a marker code.
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        // Stuffed zero bytes and standalone markers carry no segment length.
        if marker == 0x00 || marker == 0x01 || matches!(marker, 0xd0..=0xd9) {
            continue;
        }
        let length = u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if length < 2 || offset + length > bytes.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) && length >= 7
        {
            let height = u16::from_be_bytes(bytes[offset + 3..offset + 5].try_into().ok()?) as u32;
            let width = u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().ok()?) as u32;
            return (width > 0 && height > 0).then_some((width, height));
        }
        offset += length;
    }
    None
}

fn bounded_json(value: &Value) -> String {
    bounded_text(&value.to_string())
}

fn bounded_text(value: &str) -> String {
    const LIMIT: usize = 4096;
    if value.len() <= LIMIT {
        value.to_string()
    } else {
        format!("{}…", &value[..value.floor_char_boundary(LIMIT)])
    }
}

/// Turn a `GetCurrentPeriodUsage` response (plus an optional `GetPlanInfo`
/// one) into subscription health.
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

    let spend = &usage["spendLimitUsage"];
    let on_demand = [
        ("individualUsed", "individualLimit"),
        ("pooledUsed", "pooledLimit"),
    ]
    .iter()
    .find_map(|(used_key, limit_key)| {
        let used = i64_flex(&spend[*used_key])?;
        let limit = i64_flex(&spend[*limit_key]).filter(|limit| *limit > 0)?;
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
            note: "the dashboard reported no usage data for the configured Cursor API key".into(),
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
fn i64_flex(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_url_requires_a_literal_loopback_address() {
        for url in ["http://127.0.0.1:43123", "http://[::1]:43123"] {
            assert!(validate_loopback_bridge_url(url).is_ok(), "{url}");
        }
        for url in [
            "http://localhost:43123",
            "http://192.168.1.2:43123",
            "https://127.0.0.1:43123",
        ] {
            assert!(validate_loopback_bridge_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn jpeg_dimensions_accepts_marker_fill_bytes() {
        let jpeg = [
            0xff, 0xd8, // SOI
            0xff, 0xff, 0xc0, // fill byte + baseline SOF
            0x00, 0x11, // segment length
            0x08, // sample precision
            0x00, 0x18, // height: 24
            0x00, 0x20, // width: 32
            0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
        ];

        assert_eq!(jpeg_dimensions(&jpeg), Some((32, 24)));
    }

    #[test]
    fn resume_replacement_requires_a_structured_not_found_code() {
        let prose = BridgeRpcFailure {
            code: None,
            error: BackendError::Protocol("agent was not found in a diagnostic".into()),
        };
        assert!(!prose.is_not_found());
        let structured = BridgeRpcFailure {
            code: Some("not_found".into()),
            error: BackendError::Protocol("opaque failure".into()),
        };
        assert!(structured.is_not_found());
    }

    #[tokio::test]
    async fn missing_legacy_state_recovers_with_a_shared_store_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let selection = select_backend_session(
            temporary.path(),
            "cursor-provider",
            "thread-with-removed-legacy-state",
            Some("legacy-agent"),
        )
        .await
        .unwrap();
        assert_eq!(selection.resume.as_deref(), Some("legacy-agent"));
        assert!(selection.legacy_marker.is_none());

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let resume_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let create_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = tokio::spawn({
            let resume_calls = resume_calls.clone();
            let create_calls = create_calls.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route(
                            "/sdk.v1.SdkAgentService/ResumeAgent",
                            post(move || {
                                resume_calls.fetch_add(1, Ordering::Relaxed);
                                async {
                                    (
                                        StatusCode::NOT_FOUND,
                                        axum::Json(json!({
                                            "code": "not_found",
                                            "message": "agent is absent from the shared store",
                                        })),
                                    )
                                }
                            }),
                        )
                        .route(
                            "/sdk.v1.SdkAgentService/CreateAgent",
                            post(move || {
                                create_calls.fetch_add(1, Ordering::Relaxed);
                                async { axum::Json(json!({ "agentId": "replacement-agent" })) }
                            }),
                        ),
                )
                .await
            }
        });
        let client = BridgeClient {
            http: reqwest::Client::new(),
            base_url: format!("http://{address}"),
            token: "fixture-token".into(),
            secrets: Arc::new(StdMutex::new(vec!["fixture-token".into()])),
            diagnostics: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
        };

        let (agent_id, fresh) =
            create_or_resume_agent(&client, selection.resume.as_deref(), &json!({}))
                .await
                .unwrap();
        assert_eq!(agent_id, "replacement-agent");
        assert!(fresh, "replacement must publish a new persisted session id");
        assert_eq!(resume_calls.load(Ordering::Relaxed), 1);
        assert_eq!(create_calls.load(Ordering::Relaxed), 1);
        server.abort();
    }

    #[tokio::test]
    async fn stderr_reader_bounds_and_drains_unterminated_diagnostics() {
        let secret = "crsr_boundary_secret";
        let mut input = vec![b'x'; MAX_DIAGNOSTIC_LINE_BYTES - secret.len() + 1];
        input.extend_from_slice(secret.as_bytes());
        input.extend(std::iter::repeat_n(b'x', MAX_DIAGNOSTIC_LINE_BYTES));
        input.extend_from_slice(b"\nnext\n");
        let mut reader = BufReader::new(input.as_slice());
        let first = read_bounded_line(&mut reader, MAX_DIAGNOSTIC_LINE_BYTES)
            .await
            .unwrap()
            .unwrap();
        assert!(first.truncated);
        assert!(first.text.is_empty());
        let diagnostic = redact(&first.display(), &[secret]);
        assert_eq!(diagnostic, "[oversized diagnostic line omitted]");
        assert!(!diagnostic.contains(&secret[..secret.len() - 1]));
        let second = read_bounded_line(&mut reader, MAX_DIAGNOSTIC_LINE_BYTES)
            .await
            .unwrap()
            .unwrap();
        assert!(!second.truncated);
        assert_eq!(second.text, "next");
    }

    #[tokio::test]
    async fn projection_stops_after_the_event_consumer_closes() {
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        drop(stream);
        assert!(events.is_closed());

        let mut projection = RunProjection::default();
        let cancel = CancellationToken::new();
        let result = projection
            .process(
                json!({
                    "sdkMessage": {
                        "message": { "type": "assistant", "text": "late output" }
                    }
                }),
                &events,
                &cancel,
            )
            .await;
        assert_eq!(result, Err(StreamStop::ConsumerClosed));
    }

    #[test]
    fn connect_stream_rejects_frames_after_end_stream() {
        let mut state = ConnectStreamState::Active;
        assert!(!state.observe_frame(0).unwrap());
        assert!(state.observe_frame(0x02).unwrap());
        let error = state.observe_frame(0).unwrap_err();
        assert!(error.to_string().contains("after its Connect end-stream"));
    }

    #[tokio::test]
    async fn idle_reaper_exits_when_the_pool_closes() {
        let pool = Arc::new(BridgePool::default());
        let reaper = tokio::spawn(reap_idle_until_closed(
            Arc::downgrade(&pool),
            pool.closing.clone(),
        ));
        tokio::task::yield_now().await;
        assert!(!reaper.is_finished());

        pool.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), reaper)
            .await
            .expect("idle reaper remained scheduled after pool shutdown")
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pool_closure_interrupts_bridge_startup() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let started = state.join("started");
        let script = temp.path().join("bridge-that-never-becomes-ready");
        std::fs::write(
            &script,
            "#!/bin/sh\ntouch \"$CURSOR_SDK_BRIDGE_STATE_ROOT/started\"\nexec sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = CancellationToken::new();
        let closing = CancellationToken::new();
        let callback = CallbackRouter::start(local_http_client().unwrap())
            .await
            .unwrap();
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        let error = {
            let request = BridgeProcessRequest {
                command: script.to_str().unwrap(),
                worktree: temp.path(),
                state_dir: &state,
                resume_agent_id: None,
                api_key: "secret",
                cancel: &cancel,
                events: &events,
            };
            let startup = BridgeProcess::start(&request, &callback, &closing);
            tokio::pin!(startup);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        result = &mut startup => {
                            match result {
                                Ok(_) => panic!("fixture Bridge unexpectedly reported readiness"),
                                Err(error) => panic!("fixture Bridge exited before shutdown: {error}"),
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {
                            if started.is_file() {
                                break;
                            }
                        }
                    }
                }
            })
            .await
            .expect("fixture Bridge never started");

            closing.cancel();
            match tokio::time::timeout(Duration::from_secs(5), startup).await {
                Ok(Err(error)) => error,
                Ok(Ok(_)) => panic!("fixture Bridge unexpectedly reported readiness"),
                Err(_) => panic!("pool closure did not interrupt Bridge startup"),
            }
        };
        assert!(error.to_string().contains("pool is shutting down"));
        callback.stop().await.unwrap();
    }

    #[tokio::test]
    async fn thread_admission_cancellation_does_not_wait_for_the_lifecycle_writer() {
        let pool = BridgePool::default();
        let writer = pool.lifecycle.write().await;
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        let cancel = CancellationToken::new();
        let admission = pool.acquire_thread_admission("blocked", &cancel, &events);
        tokio::pin!(admission);
        assert!(futures::poll!(admission.as_mut()).is_pending());

        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), admission)
                .await
                .unwrap(),
            Err(BackendError::Cancelled)
        ));
        drop(writer);
    }

    #[tokio::test]
    async fn turn_admission_bounds_callback_owners_and_wakes_on_shutdown() {
        let pool = Arc::new(BridgePool::default());
        let _permits = (0..MAX_CONCURRENT_TURNS)
            .map(|_| pool.turn_admission.clone().try_acquire_owned().unwrap())
            .collect::<Vec<_>>();
        assert!(pool.turn_admission.clone().try_acquire_owned().is_err());

        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        let cancel = CancellationToken::new();
        let waiting_pool = pool.clone();
        let waiter =
            tokio::spawn(
                async move { waiting_pool.acquire_turn_admission(&cancel, &events).await },
            );
        tokio::task::yield_now().await;

        pool.shutdown().await.unwrap();
        let error = waiter.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("pool is shutting down"));
    }

    #[tokio::test]
    async fn same_thread_queues_do_not_consume_global_turn_admission() {
        let pool = BridgePool::default();
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        let cancel = CancellationToken::new();

        let _active_thread = pool
            .acquire_thread_admission("busy-thread", &cancel, &events)
            .await
            .unwrap();
        let _active_turn = pool.acquire_turn_admission(&cancel, &events).await.unwrap();
        let mut same_thread_queue = futures::stream::FuturesUnordered::new();
        for _ in 0..MAX_CONCURRENT_TURNS {
            same_thread_queue.push(pool.acquire_thread_admission("busy-thread", &cancel, &events));
        }
        assert!(futures::poll!(same_thread_queue.next()).is_pending());
        assert_eq!(
            pool.turn_admission.available_permits(),
            MAX_CONCURRENT_TURNS - 1,
            "same-thread waiters reserved provider-wide permits"
        );

        let _other_thread = tokio::time::timeout(
            Duration::from_secs(1),
            pool.acquire_thread_admission("other-thread", &cancel, &events),
        )
        .await
        .expect("an unrelated thread could not enter its Bridge lane")
        .unwrap();
        let _other_turn = tokio::time::timeout(
            Duration::from_secs(1),
            pool.acquire_turn_admission(&cancel, &events),
        )
        .await
        .expect("same-thread queues starved an unrelated turn")
        .unwrap();
    }

    #[tokio::test]
    async fn queued_turn_does_not_discover_tools_before_admission() {
        let pool = BridgePool::default();
        let _permits = (0..MAX_CONCURRENT_TURNS)
            .map(|_| pool.turn_admission.clone().try_acquire_owned().unwrap())
            .collect::<Vec<_>>();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_counter = requests.clone();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/mcp",
                    post(move || {
                        let request_counter = request_counter.clone();
                        async move {
                            request_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": { "tools": [] },
                            }))
                        }
                    }),
                ),
            )
            .await
        });
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        let cancel = CancellationToken::new();
        let turn = BackendTurn {
            cancel: cancel.clone(),
            thread_id: "queued-turn".into(),
            worktree: "/tmp".into(),
            session: None,
            model: "composer-2".into(),
            model_options: Map::new(),
            prompt: "not reached".into(),
            attachments: Vec::new(),
            instructions: None,
            permission: BackendPermission::Ask,
            tool_free: false,
            attach_background: false,
            mcp_bridge: Some(crate::McpBridgeConfig {
                url: format!("http://{address}/mcp"),
                bridge_tools: true,
                disallowed_tools: Vec::new(),
            }),
            mcp_servers: Vec::new(),
        };
        let state = tempfile::tempdir().unwrap();
        let turn = run_sdk_turn(
            &pool,
            "cursor",
            "not-reached",
            "secret",
            state.path(),
            turn,
            &events,
        );
        tokio::pin!(turn);

        assert!(
            tokio::time::timeout(Duration::from_millis(50), turn.as_mut())
                .await
                .is_err()
        );
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "queued turn fetched a tool catalog before admission"
        );
        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), turn)
                .await
                .unwrap()
                .unwrap(),
            TurnTerminal::Cancelled
        ));
        server.abort();
    }

    #[test]
    fn buffered_startup_diagnostics_are_reredacted_after_token_discovery() {
        let token = "bridge-secret-token".to_string();
        let mut diagnostics = VecDeque::from([format!("startup echoed {token}")]);
        redact_diagnostics(&mut diagnostics, std::slice::from_ref(&token));
        assert_eq!(diagnostics[0], "startup echoed [REDACTED]");
    }

    #[test]
    fn callback_fingerprints_are_stable_and_cover_arguments() {
        let request = |value| CustomToolRequest {
            tool_name: "trouve_test".into(),
            tool_call_id: Some("call".into()),
            agent_id: "agent".into(),
            args: json!({ "value": value }),
        };
        assert_eq!(
            callback_fingerprint(&request(1)),
            callback_fingerprint(&request(1))
        );
        assert_ne!(
            callback_fingerprint(&request(1)),
            callback_fingerprint(&request(2))
        );
    }

    #[tokio::test]
    async fn projection_rejects_done_false_as_completion() {
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();
        let cancel = CancellationToken::new();
        let mut projection = RunProjection::default();
        projection
            .process(
                json!({
                    "result": {
                        "status": "RUN_LIFECYCLE_STATUS_FINISHED",
                        "result": { "result": "not complete" }
                    }
                }),
                &events,
                &cancel,
            )
            .await
            .unwrap();
        projection
            .process(json!({ "done": false }), &events, &cancel)
            .await
            .unwrap();

        let error = match projection.finish(&events, &cancel).await {
            Err(error) => error,
            Ok(_) => panic!("done:false unexpectedly completed the projection"),
        };
        assert!(
            error
                .to_string()
                .contains("invalid or duplicate done envelope")
        );
    }

    #[tokio::test]
    async fn projection_backpressure_remains_cancellable() {
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();

        let mut accepted = 0usize;
        loop {
            let event = BackendEvent::SessionStarted {
                session_id: format!("fill-{accepted}"),
            };
            match tokio::time::timeout(Duration::from_millis(100), events.send(Ok(event))).await {
                Ok(Ok(())) => accepted += 1,
                Ok(Err(())) => panic!("event stream closed while filling its buffer"),
                Err(_) => break,
            }
        }
        assert!(
            accepted >= crate::BACKEND_STREAM_CAPACITY,
            "fixture did not reach real event-stream backpressure"
        );

        let cancel = CancellationToken::new();
        let mut projection = RunProjection::default();
        let publish = projection.process(
            json!({
                "sdkMessage": {
                    "message": { "type": "assistant", "text": "blocked output" }
                }
            }),
            &events,
            &cancel,
        );
        tokio::pin!(publish);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), publish.as_mut())
                .await
                .is_err(),
            "projection unexpectedly published through a full event buffer"
        );
        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), publish)
                .await
                .expect("cancellation did not release projection backpressure"),
            Err(StreamStop::Cancelled)
        );
    }

    #[tokio::test]
    async fn session_publication_backpressure_observes_cancellation_and_pool_shutdown() {
        let (sender_tx, sender_rx) = tokio::sync::oneshot::channel();
        let _stream = async_stream(move |events| async move {
            let _ = sender_tx.send(events);
            std::future::pending::<()>().await;
        });
        let events = sender_rx.await.unwrap();

        loop {
            let event = BackendEvent::SessionStarted {
                session_id: "fill".into(),
            };
            match tokio::time::timeout(Duration::from_millis(100), events.send(Ok(event))).await {
                Ok(Ok(())) => {}
                Ok(Err(())) => panic!("event stream closed while filling its buffer"),
                Err(_) => break,
            }
        }

        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                publish_session_started(
                    &CancellationToken::new(),
                    &cancel,
                    &events,
                    "cancelled".into(),
                ),
            )
            .await
            .expect("cancellation did not release session publication backpressure"),
            Err(SessionPublicationStop::Cancelled)
        );

        let closing = CancellationToken::new();
        closing.cancel();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                publish_session_started(
                    &closing,
                    &CancellationToken::new(),
                    &events,
                    "closing".into(),
                ),
            )
            .await
            .expect("pool shutdown did not release session publication backpressure"),
            Err(SessionPublicationStop::PoolClosing)
        );
    }

    #[tokio::test]
    async fn bounded_response_stops_at_the_configured_limit() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/oversized", axum::routing::get(|| async { "12345" })),
            )
            .await
        });
        let response = reqwest::Client::new()
            .get(format!("http://{address}/oversized"))
            .send()
            .await
            .unwrap();
        let error = read_bounded_response(response, "test", 4)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeded 4 bytes"));
        server.abort();
    }

    #[tokio::test]
    async fn unary_rpc_uses_one_aggregate_header_and_body_deadline() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/sdk.v1.FixtureService/Slow",
                    post(|| async {
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        Response::builder()
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from_stream(futures::stream::once(
                                async {
                                    tokio::time::sleep(Duration::from_millis(60)).await;
                                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(
                                        b"{}",
                                    ))
                                },
                            )))
                            .unwrap()
                    }),
                ),
            )
            .await
        });
        let client = BridgeClient {
            http: reqwest::Client::new(),
            base_url: format!("http://{address}"),
            token: "fixture-token".into(),
            secrets: Arc::new(StdMutex::new(vec!["fixture-token".into()])),
            diagnostics: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
        };

        let error = client
            .unary_with_timeout(
                "FixtureService",
                "Slow",
                json!({}),
                Duration::from_millis(100),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("response timed out"));
        server.abort();
    }

    #[tokio::test]
    async fn timed_mcp_request_bounds_a_stalled_response_body() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/mcp",
                    post(|| async {
                        Response::builder()
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from_stream(futures::stream::pending::<
                                Result<bytes::Bytes, std::convert::Infallible>,
                            >(
                            )))
                            .unwrap()
                    }),
                ),
            )
            .await
        });
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            mcp_request(
                &reqwest::Client::new(),
                &format!("http://{address}/mcp"),
                "tools/list",
                json!({}),
                Some(Duration::from_millis(25)),
            ),
        )
        .await
        .expect("MCP response-body deadline was not enforced")
        .unwrap_err();
        assert!(error.to_string().contains("MCP tools/list timed out"));
        server.abort();
    }

    #[tokio::test]
    async fn callback_authentication_precedes_json_deserialization() {
        let callback = CallbackRouter::start(reqwest::Client::new()).await.unwrap();
        let response = reqwest::Client::new()
            .post(format!("{}{}", callback.url, CALLBACK_PATH))
            .header("Content-Type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        callback.stop().await.unwrap();
    }

    #[tokio::test]
    async fn callback_listener_failure_revokes_bridge_reuse_health() {
        let callback = CallbackRouter::start(reqwest::Client::new()).await.unwrap();
        assert!(callback.listener_is_running());
        callback
            .task
            .lock()
            .unwrap()
            .as_ref()
            .expect("callback listener task is present")
            .abort();

        tokio::time::timeout(Duration::from_secs(1), async {
            while callback.listener_is_running() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted callback listener still appeared reusable");
        assert!(callback.stop().await.is_err());
    }

    #[tokio::test]
    async fn shared_callback_router_isolates_agents_tools_and_cancellation() {
        let blocked_started = Arc::new(Semaphore::new(0));
        let handler_started = blocked_started.clone();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let mcp_server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/a",
                        post(move || {
                            let handler_started = handler_started.clone();
                            async move {
                                handler_started.add_permits(1);
                                std::future::pending::<Json<Value>>().await
                            }
                        }),
                    )
                    .route(
                        "/b",
                        post(|| async {
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": "fixture",
                                "result": {
                                    "content": [{ "type": "text", "text": "agent-b" }]
                                }
                            }))
                        }),
                    ),
            )
            .await
        });
        let callback = CallbackRouter::start(reqwest::Client::new()).await.unwrap();
        let mut route_a = callback
            .register(
                "agent-a".into(),
                Some(format!("http://{address}/a")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let mut route_b = callback
            .register(
                "agent-b".into(),
                Some(format!("http://{address}/b")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let collision = callback
            .register(
                "agent-a".into(),
                Some(format!("http://{address}/b")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                None,
            )
            .await;
        assert!(matches!(collision, Err(BackendError::Protocol(_))));

        let http = reqwest::Client::new();
        let callback_url = format!("{}{}", callback.url, CALLBACK_PATH);
        let bearer = callback.bearer.clone();
        let blocked_call = tokio::spawn({
            let http = http.clone();
            let callback_url = callback_url.clone();
            let bearer = bearer.clone();
            async move {
                http.post(callback_url)
                    .bearer_auth(bearer)
                    .json(&json!({
                        "toolName": "shared_tool",
                        "toolCallId": "same-call-id",
                        "agentId": "agent-a",
                        "args": {},
                    }))
                    .send()
                    .await
            }
        });
        blocked_started.acquire().await.unwrap().forget();

        let b_response = http
            .post(&callback_url)
            .bearer_auth(&bearer)
            .json(&json!({
                "toolName": "shared_tool",
                "toolCallId": "same-call-id",
                "agentId": "agent-b",
                "args": {},
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(b_response.status(), StatusCode::OK);
        let b_body: Value = b_response.json().await.unwrap();
        assert_eq!(
            b_body
                .pointer("/result/content/0/text")
                .and_then(Value::as_str),
            Some("agent-b")
        );

        route_a.stop().await;
        let a_response = tokio::time::timeout(Duration::from_secs(1), blocked_call)
            .await
            .expect("cancelling agent A did not settle its callback")
            .unwrap()
            .unwrap();
        assert_eq!(a_response.status(), StatusCode::BAD_GATEWAY);

        let stale = http
            .post(&callback_url)
            .bearer_auth(&bearer)
            .json(&json!({
                "toolName": "shared_tool",
                "toolCallId": "stale-call",
                "agentId": "agent-a",
                "args": {},
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::FORBIDDEN);
        let disallowed = http
            .post(&callback_url)
            .bearer_auth(&bearer)
            .json(&json!({
                "toolName": "not_enabled",
                "toolCallId": "wrong-tool",
                "agentId": "agent-b",
                "args": {},
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(disallowed.status(), StatusCode::FORBIDDEN);
        let surviving = http
            .post(&callback_url)
            .bearer_auth(&bearer)
            .json(&json!({
                "toolName": "shared_tool",
                "toolCallId": "surviving-call",
                "agentId": "agent-b",
                "args": {},
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(surviving.status(), StatusCode::OK);

        route_b.stop().await;
        callback.stop().await.unwrap();
        mcp_server.abort();
    }

    #[tokio::test]
    async fn shared_callback_router_reserves_ingress_for_every_admitted_route() {
        let callbacks_started = Arc::new(Semaphore::new(0));
        let callbacks_release = Arc::new(Semaphore::new(0));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let mcp_server = tokio::spawn({
            let callbacks_started = callbacks_started.clone();
            let callbacks_release = callbacks_release.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new().route(
                        "/mcp",
                        post(move || {
                            let callbacks_started = callbacks_started.clone();
                            let callbacks_release = callbacks_release.clone();
                            async move {
                                callbacks_started.add_permits(1);
                                callbacks_release.acquire().await.unwrap().forget();
                                Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": "fixture",
                                    "result": { "content": [] }
                                }))
                            }
                        }),
                    ),
                )
                .await
            }
        });
        let callback = CallbackRouter::start(reqwest::Client::new()).await.unwrap();
        let mut route_a = callback
            .register(
                "agent-a".into(),
                Some(format!("http://{address}/mcp")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let mut route_b = callback
            .register(
                "agent-b".into(),
                Some(format!("http://{address}/mcp")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let mut route_c = callback
            .register(
                "agent-c".into(),
                Some(format!("http://{address}/mcp")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let http = reqwest::Client::new();
        let callback_url = format!("{}{}", callback.url, CALLBACK_PATH);
        let bearer = callback.bearer.clone();
        let send_callback = |agent_id: &'static str, call_id: String| {
            let http = http.clone();
            let callback_url = callback_url.clone();
            let bearer = bearer.clone();
            tokio::spawn(async move {
                http.post(callback_url)
                    .bearer_auth(bearer)
                    .json(&json!({
                        "toolName": "shared_tool",
                        "toolCallId": call_id,
                        "agentId": agent_id,
                        "args": {},
                    }))
                    .send()
                    .await
            })
        };

        let mut blocked = Vec::new();
        for agent_id in ["agent-a", "agent-b"] {
            for index in 0..MAX_CALLBACK_CONCURRENCY {
                blocked.push(send_callback(agent_id, format!("{agent_id}-{index}")));
            }
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            callbacks_started.acquire_many(u32::try_from(2 * MAX_CALLBACK_CONCURRENCY).unwrap()),
        )
        .await
        .expect("the first two routes did not fill their callback reservations")
        .unwrap()
        .forget();

        let overloaded = send_callback("agent-a", "agent-a-over-capacity".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(overloaded.status(), StatusCode::TOO_MANY_REQUESTS);

        let third = send_callback("agent-c", "agent-c-0".into());
        tokio::time::timeout(Duration::from_secs(1), callbacks_started.acquire())
            .await
            .expect("two routes exhausted the third route's callback capacity")
            .unwrap()
            .forget();
        callbacks_release.add_permits(2 * MAX_CALLBACK_CONCURRENCY + 1);

        let third_response = tokio::time::timeout(Duration::from_secs(1), third)
            .await
            .expect("the third route callback did not settle")
            .unwrap()
            .unwrap();
        assert_eq!(third_response.status(), StatusCode::OK);
        for request in blocked {
            let response = tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .expect("a saturated route callback did not settle")
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        assert!(route_a.stop().await);
        assert!(route_b.stop().await);
        assert!(route_c.stop().await);
        callback.stop().await.unwrap();
        mcp_server.abort();
    }

    #[tokio::test]
    async fn retired_agent_id_cannot_bind_to_a_replacement_route() {
        let mcp_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let mcp_server = tokio::spawn({
            let mcp_calls = mcp_calls.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new().route(
                        "/mcp",
                        post(move || {
                            let mcp_calls = mcp_calls.clone();
                            async move {
                                mcp_calls.fetch_add(1, Ordering::AcqRel);
                                Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": "fixture",
                                    "result": { "content": [] }
                                }))
                            }
                        }),
                    ),
                )
                .await
            }
        });
        let callback = CallbackRouter::start(reqwest::Client::new()).await.unwrap();
        let reusable = Arc::new(AtomicBool::new(true));
        let mut previous = callback
            .register(
                "agent-reused".into(),
                Some(format!("http://{address}/mcp")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                Some(Arc::downgrade(&reusable)),
            )
            .await
            .unwrap();
        assert!(previous.stop().await);
        assert!(!callback.accepts_agent_id("agent-reused"));

        let error = match callback
            .register(
                "agent-reused".into(),
                Some(format!("http://{address}/mcp")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                Some(Arc::downgrade(&reusable)),
            )
            .await
        {
            Ok(_) => panic!("a retired agent id was routed twice through one Bridge"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already retired"), "{error}");

        let delayed = reqwest::Client::new()
            .post(format!("{}{}", callback.url, CALLBACK_PATH))
            .bearer_auth(&callback.bearer)
            .json(&json!({
                "toolName": "shared_tool",
                "toolCallId": "previously-unseen-late-call",
                "agentId": "agent-reused",
                "args": {},
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(delayed.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            mcp_calls.load(Ordering::Acquire),
            0,
            "a callback for a retired agent reached a replacement MCP route"
        );

        callback.stop().await.unwrap();
        mcp_server.abort();
    }

    #[tokio::test]
    async fn dropped_callback_route_quarantines_its_bridge_until_cleanup() {
        let blocked_started = Arc::new(Semaphore::new(0));
        let handler_started = blocked_started.clone();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let mcp_server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/mcp",
                    post(move || {
                        let handler_started = handler_started.clone();
                        async move {
                            handler_started.add_permits(1);
                            std::future::pending::<Json<Value>>().await
                        }
                    }),
                ),
            )
            .await
        });
        let callback = CallbackRouter::start(reqwest::Client::new()).await.unwrap();
        let reusable = Arc::new(AtomicBool::new(true));
        let route = callback
            .register(
                "agent-dropped".into(),
                Some(format!("http://{address}/mcp")),
                HashSet::from(["shared_tool".into()]),
                CancellationToken::new(),
                Some(Arc::downgrade(&reusable)),
            )
            .await
            .unwrap();
        let callback_request = tokio::spawn({
            let url = format!("{}{}", callback.url, CALLBACK_PATH);
            let bearer = callback.bearer.clone();
            async move {
                reqwest::Client::new()
                    .post(url)
                    .bearer_auth(bearer)
                    .json(&json!({
                        "toolName": "shared_tool",
                        "toolCallId": "dropped-call",
                        "agentId": "agent-dropped",
                        "args": {},
                    }))
                    .send()
                    .await
            }
        });
        blocked_started.acquire().await.unwrap().forget();

        drop(route);
        assert!(!reusable.load(Ordering::Acquire));
        assert!(
            callback
                .state
                .routes
                .read()
                .unwrap()
                .contains_key("agent-dropped"),
            "unacknowledged Drop discarded process-owned route cleanup"
        );
        let response = tokio::time::timeout(Duration::from_secs(1), callback_request)
            .await
            .expect("dropped route did not cancel its callback")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        callback.stop().await.unwrap();
        assert!(callback.state.routes.read().unwrap().is_empty());
        mcp_server.abort();
    }

    #[tokio::test]
    async fn stalled_send_chunks_have_an_inactivity_deadline() {
        let mut stream = futures::stream::pending::<Result<bytes::Bytes, std::io::Error>>();
        let error = next_send_chunk(&mut stream, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("made no progress"));
    }

    #[test]
    fn callback_replay_state_is_bounded_without_exhausting_completed_calls() {
        let mut calls = CallbackCalls::default();
        let mut first = None;
        for index in 0..=MAX_CALLBACK_REPLAY_RECORDS {
            let call_id = callback_key(&format!("call-{index}"));
            let fingerprint = callback_key(&format!("fingerprint-{index}"));
            let (_sender, receiver) = watch::channel(None);
            assert!(calls.admit(call_id, fingerprint, receiver));
            calls.mark_completed(call_id, MAX_CALLBACK_REPLAY_BYTES / 2);
            first.get_or_insert((call_id, fingerprint));
        }
        let (first_call, first_fingerprint) = first.unwrap();
        assert_eq!(calls.seen.len(), MAX_CALLBACK_REPLAY_RECORDS + 1);
        assert_eq!(calls.seen.get(&first_call), Some(&first_fingerprint));
        assert!(!calls.records.contains_key(&first_call));
        assert!(calls.replay_bytes <= MAX_CALLBACK_REPLAY_BYTES);
        assert!(calls.completed.len() <= MAX_CALLBACK_REPLAY_RECORDS);

        for index in (MAX_CALLBACK_REPLAY_RECORDS + 1)..MAX_CALLBACK_RECORDS {
            let call_id = callback_key(&format!("completed-{index}"));
            let fingerprint = callback_key(&format!("completed-fingerprint-{index}"));
            let (_sender, receiver) = watch::channel(None);
            assert!(calls.admit(call_id, fingerprint, receiver));
            calls.mark_completed(call_id, MAX_CALLBACK_REPLAY_BYTES / 2);
        }
        assert_eq!(calls.seen.len(), MAX_CALLBACK_RECORDS);

        let (_sender, receiver) = watch::channel(None);
        assert!(calls.admit(
            callback_key("after-completed-cap"),
            callback_key("after-completed-cap-fingerprint"),
            receiver,
        ));
        calls.mark_completed(
            callback_key("after-completed-cap"),
            MAX_CALLBACK_REPLAY_BYTES / 2,
        );
        assert_eq!(calls.seen.len(), MAX_CALLBACK_RECORDS + 1);
        assert_eq!(calls.seen.get(&first_call), Some(&first_fingerprint));

        while calls.seen.len() < MAX_CALLBACKS_PER_TURN {
            let index = calls.seen.len();
            let (_sender, receiver) = watch::channel(None);
            assert!(calls.admit(
                callback_key(&format!("completed-{index}")),
                callback_key(&format!("completed-fingerprint-{index}")),
                receiver,
            ));
            calls.mark_completed(
                callback_key(&format!("completed-{index}")),
                MAX_CALLBACK_REPLAY_BYTES / 2,
            );
        }
        let (_sender, receiver) = watch::channel(None);
        assert!(!calls.admit(
            callback_key("over-turn-capacity"),
            callback_key("over-turn-capacity-fingerprint"),
            receiver,
        ));
        assert_eq!(calls.seen.len(), MAX_CALLBACKS_PER_TURN);
        assert!(calls.records.len() <= MAX_CALLBACK_RECORDS);
        assert!(calls.replay_bytes <= MAX_CALLBACK_REPLAY_BYTES);

        let mut active = CallbackCalls::default();
        while active.records.len() < MAX_CALLBACK_RECORDS {
            let index = active.records.len();
            let (_sender, receiver) = watch::channel(None);
            assert!(active.admit(
                callback_key(&format!("active-{index}")),
                callback_key(&format!("active-fingerprint-{index}")),
                receiver,
            ));
        }
        let (_sender, receiver) = watch::channel(None);
        assert!(!active.admit(
            callback_key("over-active-capacity"),
            callback_key("over-active-capacity-fingerprint"),
            receiver,
        ));
        assert_eq!(active.records.len(), MAX_CALLBACK_RECORDS);
    }

    #[tokio::test]
    async fn callback_supervisor_joins_cancelled_tasks() {
        let supervisor = CallbackSupervisor::new(CancellationToken::new());
        supervisor
            .tasks
            .lock()
            .await
            .spawn(std::future::pending::<()>());
        supervisor.stop().await;
        assert!(supervisor.tasks.lock().await.is_empty());
    }

    #[tokio::test]
    async fn uncorroborated_stream_call_quarantines_before_route_reuse() {
        let callback = CallbackRouter::start(local_http_client().unwrap())
            .await
            .unwrap();
        let reusable = Arc::new(AtomicBool::new(true));
        let mut route = callback
            .register(
                "agent-delayed-before-ingress".into(),
                None,
                HashSet::new(),
                CancellationToken::new(),
                Some(Arc::downgrade(&reusable)),
            )
            .await
            .unwrap();
        route
            .route
            .observe_stream_call_id("call-delayed-before-ingress")
            .unwrap();

        assert!(
            !route.stop().await,
            "a stream call without a correlated callback allowed route reuse"
        );
        assert!(!reusable.load(Ordering::Acquire));
        assert!(
            callback
                .state
                .routes
                .read()
                .unwrap()
                .contains_key("agent-delayed-before-ingress")
        );
        callback.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn callback_route_timeout_retains_noncooperative_supervised_work() {
        let callback = CallbackRouter::start(local_http_client().unwrap())
            .await
            .unwrap();
        let reusable = Arc::new(AtomicBool::new(true));
        let mut route = callback
            .register(
                "agent-timeout".into(),
                None,
                HashSet::new(),
                CancellationToken::new(),
                Some(Arc::downgrade(&reusable)),
            )
            .await
            .unwrap();
        let supervisor = route.supervisor.clone();
        let task_started = Arc::new(Semaphore::new(0));
        let release = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
        supervisor.tasks.lock().await.spawn({
            let task_started = task_started.clone();
            let release = release.clone();
            async move {
                task_started.add_permits(1);
                let (released, wake) = &*release;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
        });
        task_started.acquire().await.unwrap().forget();
        let settled = route
            .stop_until(tokio::time::Instant::now() + Duration::from_millis(10))
            .await;
        assert!(!settled, "a timed-out callback route reported clean reuse");
        assert!(
            !reusable.load(Ordering::Acquire),
            "route timeout did not synchronously quarantine its Bridge"
        );
        assert!(
            callback
                .state
                .routes
                .read()
                .unwrap()
                .contains_key("agent-timeout"),
            "a timed-out route escaped process-owned cleanup"
        );
        let stale = local_http_client()
            .unwrap()
            .post(format!("{}{}", callback.url, CALLBACK_PATH))
            .bearer_auth(&callback.bearer)
            .json(&json!({
                "toolName": "stale-tool",
                "toolCallId": "stale-call",
                "agentId": "agent-timeout",
                "args": {},
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::FORBIDDEN);
        let cleanup = callback
            .stop_until(tokio::time::Instant::now() + Duration::from_millis(10))
            .await;
        assert_eq!(cleanup.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(
            callback.task.lock().unwrap().is_none(),
            "route timeout left the callback listener task running"
        );
        assert!(
            callback
                .state
                .routes
                .read()
                .unwrap()
                .contains_key("agent-timeout"),
            "failed process cleanup discarded the timed-out route"
        );
        {
            let (released, wake) = &*release;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }
        callback.stop().await.unwrap();
        assert!(callback.state.routes.read().unwrap().is_empty());
    }

    #[test]
    fn cursor_terminal_statuses_require_exact_sdk_values() {
        for status in [json!(3), json!("3"), json!("RUN_LIFECYCLE_STATUS_FINISHED")] {
            assert!(status_is_finished(&status));
        }
        for status in [
            json!("NOT_FINISHED"),
            json!("NOT_COMPLETED"),
            json!("COMPLETED"),
            json!(3.5),
        ] {
            assert!(!status_is_finished(&status));
        }
        for status in [
            json!(5),
            json!("5"),
            json!("RUN_LIFECYCLE_STATUS_CANCELLED"),
        ] {
            assert!(status_is_cancelled(&status));
        }
        for status in [json!("NOT_CANCELLED"), json!(5.5)] {
            assert!(!status_is_cancelled(&status));
        }
    }

    #[test]
    fn startup_errors_include_redacted_diagnostics() {
        let diagnostics = VecDeque::from(["configuration rejected [REDACTED]".to_string()]);
        let error = startup_error_with_diagnostics(
            BackendError::Protocol("startup failed".into()),
            &diagnostics,
        );
        let message = error.to_string();
        assert!(message.contains("startup failed"));
        assert!(message.contains("configuration rejected [REDACTED]"));
    }

    #[tokio::test]
    async fn legacy_cursor_cli_requires_explicit_sdk_migration() {
        let backend = CursorBackend::new(
            "cursor",
            Some("cursor-sdk-bridge".into()),
            Some("configured-key".into()),
        )
        .requiring_legacy_cli_migration();
        assert!(!backend.status().has_credentials);
        let health = backend.subscription_health().await.unwrap();
        assert_eq!(health.status, "unavailable");
        assert!(health.note.contains("retired cursor-cli"));
        let error = match backend.start_login().await {
            Ok(_) => panic!("legacy Cursor config unexpectedly started a login flow"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("select Cursor (Agent SDK)"));
    }

    #[test]
    fn quarantined_bridge_cleanup_bypasses_the_idle_threshold() {
        let just_used = Instant::now();
        assert!(!bridge_cleanup_is_due(true, just_used, Some(IDLE_TIMEOUT)));
        assert!(bridge_cleanup_is_due(false, just_used, Some(IDLE_TIMEOUT)));
    }

    #[test]
    fn legacy_session_marker_flushes_before_atomic_publication() {
        let temporary_root = tempfile::tempdir().unwrap();
        let marker_parent = temporary_root.path().join("backend").join("markers");
        std::fs::create_dir_all(&marker_parent).unwrap();
        let temporary = marker_parent.join("candidate");
        let destination = marker_parent.join("thread");
        std::fs::write(&temporary, "new-agent").unwrap();
        std::fs::write(&destination, "old-agent").unwrap();

        let error = commit_legacy_session_marker(&temporary, &destination, true, |path| {
            if path == temporary {
                Err(std::io::Error::other("injected marker flush failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "old-agent");

        let mut synced = Vec::new();
        commit_legacy_session_marker(&temporary, &destination, true, |path| {
            synced.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "new-agent");
        assert_eq!(
            synced,
            vec![
                temporary,
                marker_parent.clone(),
                marker_parent.parent().unwrap().to_path_buf(),
            ]
        );
    }

    #[test]
    fn derives_common_image_dimensions() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0; 8]);
        png.extend_from_slice(&320_u32.to_be_bytes());
        png.extend_from_slice(&200_u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), (320, 200));
        assert_eq!(image_dimensions(b"GIF89a\x02\x00\x03\x00"), (2, 3));
        assert_eq!(image_dimensions(b"unknown"), (1, 1));
    }

    #[test]
    fn parses_cursor_usage_as_non_cached_plus_cached_context() {
        let usage = parse_usage(&json!({
            "inputTokens": "80",
            "outputTokens": 20,
            "cacheReadTokens": "40",
        }));
        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cached_input_tokens, 40);
        assert_eq!(usage.context_input_tokens, Some(120));
    }

    #[test]
    fn backend_state_path_does_not_embed_external_ids() {
        let path = backend_state_dir(Path::new("/state"), "../provider");
        assert_eq!(path.parent(), Some(Path::new("/state")));
        assert!(!path.to_string_lossy().contains("provider"));
    }

    #[tokio::test]
    async fn legacy_per_thread_session_is_reset_once_into_the_shared_store() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let provider_id = "cursor-provider";
        let thread_id = "thread-from-per-thread-store";
        let legacy = legacy_thread_state_dir(root, provider_id, thread_id);
        assert_eq!(
            legacy.file_name().and_then(|name| name.to_str()),
            Some("3X4zI2VSLSnWs8ezqXYPa9LDLz12HET6AyHGPWtrMaM"),
            "legacy detection must preserve the pre-shared-store hash layout"
        );
        tokio::fs::create_dir_all(&legacy).await.unwrap();

        let first = select_backend_session(root, provider_id, thread_id, Some("legacy-agent"))
            .await
            .unwrap();
        assert_eq!(
            first.resume, None,
            "legacy agent cannot resume in shared SQLite"
        );
        let marker = first
            .legacy_marker
            .as_ref()
            .expect("legacy state requires a transition marker");
        assert_eq!(marker.recorded_agent_id, None);
        record_legacy_session_marker(marker, "shared-agent")
            .await
            .unwrap();

        let recover = select_backend_session(root, provider_id, thread_id, Some("legacy-agent"))
            .await
            .unwrap();
        assert_eq!(recover.resume.as_deref(), Some("shared-agent"));
        assert_eq!(
            recover
                .legacy_marker
                .as_ref()
                .and_then(|marker| marker.recorded_agent_id.as_deref()),
            Some("shared-agent")
        );
        let settled = select_backend_session(root, provider_id, thread_id, Some("shared-agent"))
            .await
            .unwrap();
        assert_eq!(settled.resume.as_deref(), Some("shared-agent"));
        let marker = settled.legacy_marker.as_ref().unwrap();
        record_legacy_session_marker(marker, "replacement-agent")
            .await
            .unwrap();
        let replaced =
            select_backend_session(root, provider_id, thread_id, Some("replacement-agent"))
                .await
                .unwrap();
        assert_eq!(replaced.resume.as_deref(), Some("replacement-agent"));
        assert!(
            legacy.is_dir(),
            "safe reset must retain the legacy state for recovery"
        );
    }
}
