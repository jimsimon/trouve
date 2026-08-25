//! Cursor backend driven by the standalone Cursor Agent SDK Bridge.
//!
//! A bounded pool keeps one Bridge warm per recently active Trouve thread.
//! Cursor keeps conversation state in a thread-scoped SQLite store, so a cold
//! replacement can still resume the durable agent. Callback servers and their
//! credentials remain turn-scoped and are registered immediately before each
//! run. Cursor's native tools are replaced with the single SDK `mcp`
//! capability; concrete tool schemas and calls are proxied to trouve's
//! internal, thread-scoped MCP endpoint and therefore still pass through
//! `ToolExecutor`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bytes::BytesMut;
use futures::{StreamExt as _, TryStreamExt as _};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, RwLock, Semaphore, watch};
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
const INTERRUPTED_CALLBACK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const INTERRUPTED_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const CALLBACK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RPC_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONNECT_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIAGNOSTIC_LINES: usize = 40;
const MAX_CALLBACK_RECORDS: usize = 128;
const MAX_CALLBACK_REPLAY_RECORDS: usize = 64;
const MAX_CALLBACK_CONCURRENCY: usize = 8;
const READY_PREFIX: &str = "cursor-sdk-bridge ready ";
const CALLBACK_PATH: &str = "/sdk.v1.SdkCustomToolCallbackService/CallCustomTool";
/// Most warm Cursor Bridge processes retained by one configured backend.
const POOL_CAP: usize = 3;
/// Warm Bridges are inexpensive to resume but large enough to reap when idle.
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
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_INTERVAL).await;
                let Some(pool) = pool.upgrade() else {
                    break;
                };
                pool.reap_idle().await;
            }
        });
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
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
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
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
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

struct BridgePool {
    processes: Mutex<HashMap<String, Arc<PooledBridge>>>,
    thread_gates: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    capacity: Arc<Semaphore>,
    available: Arc<Notify>,
    reaper_started: AtomicBool,
}

impl Default for BridgePool {
    fn default() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            thread_gates: Mutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(POOL_CAP)),
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
        // A capacity waiter may only retry eviction after the lease no longer
        // contributes a process reference and no longer owns the thread gate.
        self.thread_guard.take();
        self.process.take();
        notify_available(&self.available);
    }
}

struct BridgeProcessRequest<'a> {
    command: &'a str,
    worktree: &'a Path,
    state_dir: &'a Path,
    api_key: &'a str,
    callback: &'a CallbackServer,
    cancel: &'a CancellationToken,
    events: &'a BackendEventSender,
}

impl BridgePool {
    async fn process_for(
        &self,
        thread_id: &str,
        request: BridgeProcessRequest<'_>,
    ) -> Result<BridgeLease, BackendError> {
        if request.cancel.is_cancelled() || request.events.is_closed() {
            return Err(BackendError::Cancelled);
        }
        let gate = self.thread_gate(thread_id).await;
        let thread_guard = tokio::select! {
            biased;
            _ = request.cancel.cancelled() => return Err(BackendError::Cancelled),
            _ = request.events.closed() => return Err(BackendError::Cancelled),
            guard = gate.lock_owned() => guard,
        };
        let existing = self.processes.lock().await.get(thread_id).cloned();
        if let Some(process) = existing {
            let alive = if process.is_reusable()
                && process.worktree == request.worktree
                && process.state_dir == request.state_dir
            {
                let mut bridge = tokio::select! {
                    biased;
                    _ = request.cancel.cancelled() => return Err(BackendError::Cancelled),
                    _ = request.events.closed() => return Err(BackendError::Cancelled),
                    bridge = process.bridge.lock() => bridge,
                };
                match bridge.child.try_wait_leader() {
                    Ok(status) => status.is_none(),
                    Err(error) => {
                        tracing::debug!(
                            %thread_id,
                            "cursor: failed to inspect pooled Bridge: {error}"
                        );
                        false
                    }
                }
            } else {
                false
            };
            if alive {
                process.touch();
                return Ok(BridgeLease {
                    process: Some(process),
                    thread_guard: Some(thread_guard),
                    available: self.available.clone(),
                });
            }
            process.quarantine();
            self.remove_if_same(thread_id, &process).await;
            if let Err(error) = process.terminate().await {
                self.restore_if_vacant(thread_id, process).await;
                drop(thread_guard);
                notify_available(&self.available);
                return Err(error);
            }
            drop(process);
            if request.cancel.is_cancelled() || request.events.is_closed() {
                return Err(BackendError::Cancelled);
            }
        }

        let permit = self.acquire_capacity(&request).await?;
        if request.cancel.is_cancelled() || request.events.is_closed() {
            return Err(BackendError::Cancelled);
        }
        let bridge = BridgeProcess::start(
            request.command,
            request.worktree,
            request.state_dir,
            request.api_key,
            request.callback,
            request.cancel,
            request.events,
        )
        .await?;
        let process = Arc::new(PooledBridge {
            bridge: Mutex::new(bridge),
            reusable: AtomicBool::new(true),
            worktree: request.worktree.to_path_buf(),
            state_dir: request.state_dir.to_path_buf(),
            last_used: StdMutex::new(Instant::now()),
            _permit: permit,
        });
        self.processes
            .lock()
            .await
            .insert(thread_id.to_string(), process.clone());
        Ok(BridgeLease {
            process: Some(process),
            thread_guard: Some(thread_guard),
            available: self.available.clone(),
        })
    }

    async fn terminate_and_remove(
        &self,
        thread_id: &str,
        process: &BridgeLease,
    ) -> Result<(), BackendError> {
        process.quarantine();
        self.remove_if_same(thread_id, process.pooled()).await;
        if let Err(error) = process.terminate().await {
            self.restore_if_vacant(thread_id, process.pooled().clone())
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn terminate_and_remove_now_until(
        &self,
        thread_id: &str,
        process: &BridgeLease,
        deadline: tokio::time::Instant,
    ) -> Result<(), BackendError> {
        process.quarantine();
        self.remove_if_same(thread_id, process.pooled()).await;
        if let Err(error) = process.terminate_now_until(deadline).await {
            self.restore_if_vacant(thread_id, process.pooled().clone())
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn reap_idle(&self) {
        while let Some((thread_id, process, guard)) = self.take_evictable(Some(IDLE_TIMEOUT)).await
        {
            match process.terminate().await {
                Ok(()) => {}
                Err(error) => {
                    self.restore_if_vacant(&thread_id, process).await;
                    drop(guard);
                    notify_available(&self.available);
                    tracing::warn!(
                        %thread_id,
                        "cursor: retaining idle Bridge after unacknowledged cleanup: {error}"
                    );
                    break;
                }
            }
        }
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

    async fn remove_if_same(&self, thread_id: &str, process: &Arc<PooledBridge>) {
        let mut processes = self.processes.lock().await;
        if processes
            .get(thread_id)
            .is_some_and(|entry| Arc::ptr_eq(entry, process))
        {
            processes.remove(thread_id);
        }
    }

    async fn restore_if_vacant(&self, thread_id: &str, process: Arc<PooledBridge>) {
        self.processes
            .lock()
            .await
            .entry(thread_id.to_string())
            .or_insert(process);
    }

    async fn acquire_capacity(
        &self,
        request: &BridgeProcessRequest<'_>,
    ) -> Result<OwnedSemaphorePermit, BackendError> {
        loop {
            if let Ok(permit) = self.capacity.clone().try_acquire_owned() {
                return Ok(permit);
            }
            if self.evict_one().await? {
                continue;
            }
            tokio::select! {
                biased;
                _ = request.cancel.cancelled() => return Err(BackendError::Cancelled),
                _ = request.events.closed() => return Err(BackendError::Cancelled),
                permit = self.capacity.clone().acquire_owned() => {
                    return permit.map_err(|_| BackendError::Protocol(
                        "Cursor SDK Bridge pool capacity closed unexpectedly".into()
                    ));
                }
                _ = self.available.notified() => {}
            }
        }
    }

    async fn evict_one(&self) -> Result<bool, BackendError> {
        let Some((thread_id, process, guard)) = self.take_evictable(None).await else {
            return Ok(false);
        };
        if let Err(error) = process.terminate().await {
            self.restore_if_vacant(&thread_id, process).await;
            drop(guard);
            notify_available(&self.available);
            return Err(error);
        }
        Ok(true)
    }

    async fn take_evictable(
        &self,
        idle_for: Option<Duration>,
    ) -> Option<(String, Arc<PooledBridge>, OwnedMutexGuard<()>)> {
        let mut candidates = {
            let processes = self.processes.lock().await;
            processes
                .iter()
                .filter(|(_, process)| bridge_is_evictable(process, idle_for))
                .map(|(thread_id, process)| (thread_id.clone(), *process.last_used.lock().unwrap()))
                .collect::<Vec<_>>()
        };
        candidates.sort_by_key(|(_, last_used)| *last_used);
        for (thread_id, _) in candidates {
            let gate = self.thread_gate(&thread_id).await;
            let Ok(guard) = gate.try_lock_owned() else {
                continue;
            };
            let process = {
                let mut processes = self.processes.lock().await;
                let evictable = processes
                    .get(&thread_id)
                    .is_some_and(|process| bridge_is_evictable(process, idle_for));
                if !evictable {
                    None
                } else {
                    let process = processes.remove(&thread_id).expect("entry checked above");
                    process.quarantine();
                    Some(process)
                }
            };
            if let Some(process) = process {
                return Some((thread_id, process, guard));
            }
        }
        None
    }
}

fn bridge_is_evictable(process: &Arc<PooledBridge>, idle_for: Option<Duration>) -> bool {
    Arc::strong_count(process) == 1
        && process.bridge.try_lock().is_ok()
        && bridge_cleanup_is_due(
            process.is_reusable(),
            *process.last_used.lock().unwrap(),
            idle_for,
        )
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
    bridge: Mutex<BridgeProcess>,
    reusable: AtomicBool,
    worktree: PathBuf,
    state_dir: PathBuf,
    last_used: StdMutex<Instant>,
    _permit: OwnedSemaphorePermit,
}

impl PooledBridge {
    fn is_reusable(&self) -> bool {
        self.reusable.load(Ordering::Acquire)
    }

    fn quarantine(&self) {
        self.reusable.store(false, Ordering::Release);
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    async fn terminate(&self) -> Result<(), BackendError> {
        self.quarantine();
        self.bridge
            .lock()
            .await
            .shutdown()
            .await
            .map_err(BackendError::Io)
    }

    async fn terminate_now_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), BackendError> {
        self.quarantine();
        self.bridge
            .lock()
            .await
            .shutdown_now_until(deadline)
            .await
            .map_err(BackendError::Io)
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
    let custom_tools = match mcp_url.as_deref() {
        Some(url) => tokio::select! {
            biased;
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

    let mut callback = CallbackServer::start(local_http, mcp_url, &turn.cancel).await?;
    if events.is_closed() {
        callback.stop().await;
        return Ok(TurnTerminal::ConsumerClosed);
    }
    let state_dir = thread_state_dir(state_root, provider_id, &turn.thread_id);
    let process = match pool
        .process_for(
            &turn.thread_id,
            BridgeProcessRequest {
                command,
                worktree: &turn.worktree,
                state_dir: &state_dir,
                api_key,
                callback: &callback,
                cancel: &turn.cancel,
                events,
            },
        )
        .await
    {
        Ok(process) => process,
        Err(error) => {
            callback.stop().await;
            return Err(error);
        }
    };
    let mut bridge = tokio::select! {
        biased;
        _ = turn.cancel.cancelled() => {
            callback.stop().await;
            pool.terminate_and_remove(&turn.thread_id, &process).await?;
            return Ok(TurnTerminal::Cancelled);
        }
        _ = events.closed() => {
            callback.stop().await;
            pool.terminate_and_remove(&turn.thread_id, &process).await?;
            return Ok(TurnTerminal::ConsumerClosed);
        }
        bridge = process.bridge.lock() => bridge,
    };
    let process_exited = match bridge.child.try_wait_leader() {
        Ok(status) => status.is_some(),
        Err(error) => {
            drop(bridge);
            callback.stop().await;
            let cleanup = pool.terminate_and_remove(&turn.thread_id, &process).await;
            return merge_cleanup_error(BackendError::Io(error), cleanup);
        }
    };
    if !process.is_reusable() || process_exited {
        drop(bridge);
        callback.stop().await;
        pool.terminate_and_remove(&turn.thread_id, &process).await?;
        return Err(BackendError::Protocol(
            "pooled Cursor SDK Bridge exited before the turn started".into(),
        ));
    }
    if let Err(error) = bridge.set_tool_callback(Some(&callback)).await {
        drop(bridge);
        callback.stop().await;
        let cleanup = pool.terminate_and_remove(&turn.thread_id, &process).await;
        return merge_cleanup_error(error, cleanup);
    }

    let options = agent_options(&turn, api_key, custom_tools);
    let setup = tokio::select! {
        biased;
        _ = turn.cancel.cancelled() => Err(BackendError::Cancelled),
        _ = events.closed() => {
            drop(bridge);
            callback.stop().await;
            pool.terminate_and_remove(&turn.thread_id, &process).await?;
            return Ok(TurnTerminal::ConsumerClosed);
        }
        setup = create_or_resume_agent(&bridge.client, turn.session.as_deref(), &options) => setup,
    };
    let (agent_id, fresh) = match setup {
        Ok(value) => value,
        Err(error) => {
            drop(bridge);
            callback.stop().await;
            let cleanup = pool.terminate_and_remove(&turn.thread_id, &process).await;
            return merge_cleanup_error(error, cleanup);
        }
    };
    *callback.expected_agent_id.write().await = Some(agent_id.clone());

    if fresh
        && events
            .send(Ok(BackendEvent::SessionStarted {
                session_id: agent_id.clone(),
            }))
            .await
            .is_err()
    {
        drop(bridge);
        callback.stop().await;
        pool.terminate_and_remove(&turn.thread_id, &process).await?;
        return Ok(TurnTerminal::ConsumerClosed);
    }

    let outcome = stream_turn(
        &bridge.client,
        &agent_id,
        &turn,
        events,
        &callback.supervisor.cancel,
    )
    .await;
    if matches!(
        &outcome,
        Ok(TurnTerminal::Cancelled | TurnTerminal::ConsumerClosed)
    ) {
        // CancelRun observation, callback shutdown, and process reaping share
        // a five-second budget. Give child reaping its own final interval so a
        // callback server that consumes its allowance cannot strand the warm
        // Bridge or its pool permit.
        let callback_deadline = tokio::time::Instant::now() + INTERRUPTED_CALLBACK_SHUTDOWN_TIMEOUT;
        callback.stop_until(callback_deadline).await;
        drop(bridge);
        let reap_deadline = tokio::time::Instant::now() + INTERRUPTED_REAP_TIMEOUT;
        let process_cleanup = pool
            .terminate_and_remove_now_until(&turn.thread_id, &process, reap_deadline)
            .await;
        return finish_recycled_turn(outcome, Ok(()), process_cleanup);
    }

    // No callback may start after the terminal Send frame. The Bridge mutex
    // stays held until callback tasks are joined and its registration clears.
    callback.stop().await;
    let release = bridge.release_turn(&agent_id).await;
    let keep_warm = matches!(&outcome, Ok(TurnTerminal::Finished(_))) && release.is_ok();
    drop(bridge);

    if keep_warm {
        process.touch();
        return outcome;
    }
    let process_cleanup = pool.terminate_and_remove(&turn.thread_id, &process).await;
    finish_recycled_turn(outcome, release, process_cleanup)
}

fn merge_cleanup_error<T>(
    primary: BackendError,
    cleanup: Result<(), BackendError>,
) -> Result<T, BackendError> {
    match cleanup {
        Ok(()) => Err(primary),
        Err(cleanup) => Err(BackendError::Protocol(format!(
            "{primary}; Cursor SDK Bridge process cleanup was not acknowledged: {cleanup}"
        ))),
    }
}

fn finish_recycled_turn(
    outcome: Result<TurnTerminal, BackendError>,
    release: Result<(), BackendError>,
    process_cleanup: Result<(), BackendError>,
) -> Result<TurnTerminal, BackendError> {
    let mut error = outcome.as_ref().err().map(ToString::to_string);
    if let Err(release) = release {
        let release = format!("Cursor SDK agent release was not acknowledged: {release}");
        error = Some(error.map_or(release.clone(), |error| format!("{error}; {release}")));
    }
    if let Err(cleanup) = process_cleanup {
        let cleanup = format!("Cursor SDK Bridge process cleanup was not acknowledged: {cleanup}");
        error = Some(error.map_or(cleanup.clone(), |error| format!("{error}; {cleanup}")));
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

fn local_http_client() -> Result<reqwest::Client, BackendError> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| BackendError::Protocol(format!("local HTTP client: {error}")))
}

fn thread_state_dir(root: &Path, provider_id: &str, thread_id: &str) -> PathBuf {
    use base64::Engine as _;
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    hasher.update([0]);
    hasher.update(thread_id.as_bytes());
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
    root.join(key)
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
        "disallowedTools": [],
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
            .unary(
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
            Err(error) if missing_agent_error(&error) => {
                tracing::warn!(
                    "Cursor SDK could not resume agent {agent_id}; creating a replacement"
                );
            }
            Err(error) => return Err(error),
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

fn missing_agent_error(error: &BackendError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("not_found")
        || message.contains("not found")
        || message.contains("unknown agent")
        || message.contains("no agent")
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
    let request = http
        .post(mcp_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .send();
    let response = match timeout {
        Some(timeout) => tokio::time::timeout(timeout, request)
            .await
            .map_err(|_| BackendError::Protocol(format!("MCP {method} timed out")))?,
        // ToolExecutor owns tool-specific deadlines and cancellation. A
        // local adapter timeout here would break interactive questions and
        // valid long-running shell/MCP operations.
        None => request.await,
    }
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
}

#[derive(Clone)]
struct CallbackState {
    bearer: Arc<str>,
    expected_agent_id: Arc<RwLock<Option<String>>>,
    mcp_url: Option<Arc<str>>,
    http: reqwest::Client,
    supervisor: Arc<CallbackSupervisor>,
}

#[derive(Clone)]
struct CallbackRecord {
    tool_name: String,
    args: Value,
    outcome: watch::Receiver<Option<CallbackOutcome>>,
}

#[derive(Default)]
struct CallbackCalls {
    records: HashMap<String, CallbackRecord>,
    completed: VecDeque<String>,
}

impl CallbackCalls {
    fn make_room(&mut self) -> bool {
        while self.records.len() >= MAX_CALLBACK_RECORDS {
            let Some(call_id) = self.completed.pop_front() else {
                return false;
            };
            self.records.remove(&call_id);
        }
        true
    }

    fn mark_completed(&mut self, call_id: String) {
        self.completed.push_back(call_id);
        while self.completed.len() > MAX_CALLBACK_REPLAY_RECORDS {
            if let Some(expired) = self.completed.pop_front() {
                self.records.remove(&expired);
            }
        }
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
        call_id: String,
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
            let _ = sender.send(Some(outcome));
            if let Some(supervisor) = supervisor.upgrade() {
                supervisor.calls.lock().await.mark_completed(call_id);
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomToolRequest {
    tool_name: String,
    #[serde(default)]
    tool_call_id: Option<String>,
    agent_id: String,
    args: Value,
}

struct CallbackServer {
    url: String,
    bearer: String,
    expected_agent_id: Arc<RwLock<Option<String>>>,
    supervisor: Arc<CallbackSupervisor>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl CallbackServer {
    async fn start(
        http: reqwest::Client,
        mcp_url: Option<String>,
        turn_cancel: &CancellationToken,
    ) -> Result<Self, BackendError> {
        let bearer = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expected_agent_id = Arc::new(RwLock::new(None));
        let supervisor = CallbackSupervisor::new(turn_cancel.child_token());
        let state = CallbackState {
            bearer: Arc::from(bearer.as_str()),
            expected_agent_id: expected_agent_id.clone(),
            mcp_url: mcp_url.map(Arc::from),
            http,
            supervisor: supervisor.clone(),
        };
        let router = Router::new()
            .route(CALLBACK_PATH, post(custom_tool_callback))
            .layer(DefaultBodyLimit::max(MAX_RPC_BODY_BYTES))
            .with_state(state);
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
            expected_agent_id,
            supervisor,
            shutdown,
            task,
        })
    }

    async fn stop(&mut self) {
        self.stop_until(tokio::time::Instant::now() + CALLBACK_SHUTDOWN_TIMEOUT)
            .await;
    }

    async fn stop_until(&mut self, deadline: tokio::time::Instant) {
        self.supervisor.cancel.cancel();
        self.shutdown.cancel();
        if tokio::time::timeout_at(deadline, &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        self.supervisor.stop().await;
    }
}

async fn custom_tool_callback(
    State(state): State<CallbackState>,
    headers: HeaderMap,
    Json(request): Json<CustomToolRequest>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", state.bearer);
    if !secure_text_eq(presented, &expected) {
        return callback_error(StatusCode::UNAUTHORIZED, "unauthenticated", "Unauthorized");
    }
    let expected_agent = state.expected_agent_id.read().await.clone();
    if expected_agent.as_deref() != Some(request.agent_id.as_str()) {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "callback agent id does not match the active Cursor agent",
        );
    }
    if request.tool_name.is_empty() || !request.args.is_object() {
        return callback_error(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "custom-tool callback is malformed",
        );
    }
    let Some(mcp_url) = state.mcp_url.as_deref() else {
        return callback_error(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "tool calls are disabled for this turn",
        );
    };
    let Some(call_id) = request
        .tool_call_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    else {
        return callback_error(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "custom-tool callback omitted its tool-call id",
        );
    };
    let (mut outcome, execute) = {
        let mut calls = state.supervisor.calls.lock().await;
        if let Some(existing) = calls.records.get(&call_id) {
            if existing.tool_name != request.tool_name || existing.args != request.args {
                return callback_error(
                    StatusCode::CONFLICT,
                    "already_exists",
                    "tool-call id was reused with different arguments",
                );
            }
            (existing.outcome.clone(), None)
        } else {
            if !calls.make_room() {
                return callback_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "resource_exhausted",
                    "too many custom-tool callbacks are active",
                );
            }
            let (sender, receiver) = watch::channel(None);
            calls.records.insert(
                call_id.clone(),
                CallbackRecord {
                    tool_name: request.tool_name.clone(),
                    args: request.args.clone(),
                    outcome: receiver.clone(),
                },
            );
            (receiver, Some(sender))
        }
    };
    if let Some(sender) = execute {
        let http = state.http.clone();
        let mcp_url = Arc::<str>::from(mcp_url);
        if !state
            .supervisor
            .spawn(call_id.clone(), http, mcp_url, request, sender)
            .await
        {
            state.supervisor.calls.lock().await.records.remove(&call_id);
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
        command: &str,
        worktree: &Path,
        state_dir: &Path,
        api_key: &str,
        callback: &CallbackServer,
        cancel: &CancellationToken,
        events: &BackendEventSender,
    ) -> Result<Self, BackendError> {
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
        let mut lines = BufReader::new(stderr).lines();
        let mut diagnostics = VecDeque::new();
        let ready_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BackendError::Cancelled),
            _ = events.closed() => Err(BackendError::Cancelled),
            result = tokio::time::timeout(STARTUP_TIMEOUT, async {
                loop {
                    let line = lines.next_line().await.map_err(BackendError::Io)?;
                    let Some(line) = line else {
                        return Err(BackendError::Protocol(
                            "Cursor SDK Bridge exited before reporting readiness".into(),
                        ));
                    };
                    if let Some(payload) = line.strip_prefix(READY_PREFIX) {
                        return serde_json::from_str::<ReadyPayload>(payload).map_err(|error| {
                            BackendError::Protocol(format!(
                                "Cursor SDK Bridge emitted an invalid readiness payload: {error}"
                            ))
                        });
                    }
                    push_diagnostic(
                        &mut diagnostics,
                        redact(&line, &[api_key, &callback.bearer]),
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
        let secrets = Arc::new(StdMutex::new(vec![
            api_key.to_string(),
            callback.bearer.clone(),
            token.clone(),
        ]));
        let shared_diagnostics = Arc::new(tokio::sync::Mutex::new(diagnostics));
        let drain_diagnostics = shared_diagnostics.clone();
        let drain_secrets = secrets.clone();
        let stderr_task = tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let line = {
                    let secrets = drain_secrets.lock().unwrap();
                    redact(
                        &line,
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

    async fn set_tool_callback(
        &self,
        callback: Option<&CallbackServer>,
    ) -> Result<(), BackendError> {
        if let Some(callback) = callback {
            self.client.remember_secret(&callback.bearer);
        }
        let (url, auth_token) = callback
            .map(|callback| (callback.url.as_str(), callback.bearer.as_str()))
            .unwrap_or(("", ""));
        self.client
            .unary_with_timeout(
                "SdkBridgeControlService",
                "SetToolCallback",
                json!({ "url": url, "authToken": auth_token }),
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }

    async fn release_turn(&self, agent_id: &str) -> Result<(), BackendError> {
        let close = self
            .client
            .unary_with_timeout(
                "SdkAgentService",
                "CloseAgent",
                json!({ "agentId": agent_id }),
                Duration::from_secs(10),
            )
            .await;
        // Registration is process-wide. The turn-scoped callback server is
        // already stopped while the Bridge mutex is still held; clear the
        // stale URL before this process can serve another turn.
        let clear = self.set_tool_callback(None).await;
        match (close, clear) {
            (Ok(_), Ok(())) => Ok(()),
            (Err(close), Ok(())) => Err(close),
            (Ok(_), Err(clear)) => Err(clear),
            (Err(close), Err(clear)) => Err(BackendError::Protocol(format!(
                "{close}; clearing the Cursor SDK tool callback failed: {clear}"
            ))),
        }
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

    async fn shutdown_now_until(&mut self, deadline: tokio::time::Instant) -> std::io::Result<()> {
        let cleanup = self
            .child
            .terminate_and_reap_until(deadline)
            .await
            .map(|_| ());
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
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
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

fn push_diagnostic(lines: &mut VecDeque<String>, line: String) {
    if line.trim().is_empty() {
        return;
    }
    if lines.len() == MAX_DIAGNOSTIC_LINES {
        lines.pop_front();
    }
    lines.push_back(bounded_text(&line));
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

impl BridgeClient {
    async fn unary(&self, service: &str, method: &str, body: Value) -> Result<Value, BackendError> {
        self.unary_with_timeout(service, method, body, RPC_TIMEOUT)
            .await
    }

    async fn unary_with_timeout(
        &self,
        service: &str,
        method: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, BackendError> {
        let url = format!("{}/sdk.v1.{service}/{method}", self.base_url);
        let response = tokio::time::timeout(
            timeout,
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
        let (status, bytes) = tokio::time::timeout(
            timeout,
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
            return Err(BackendError::Protocol(format!(
                "{method} failed (HTTP {status}): {detail}{diagnostics}"
            )));
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

    fn remember_secret(&self, secret: &str) {
        let mut secrets = self.secrets.lock().unwrap();
        if !secrets.iter().any(|known| known == secret) {
            secrets.push(secret.to_string());
        }
    }

    async fn diagnostic_suffix(&self) -> String {
        let diagnostics = self.diagnostics.lock().await;
        if diagnostics.is_empty() {
            String::new()
        } else {
            format!(
                "; Bridge diagnostics: {}",
                diagnostics.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        }
    }
}

async fn stream_turn(
    client: &BridgeClient,
    agent_id: &str,
    turn: &BackendTurn,
    events: &BackendEventSender,
    callback_cancel: &CancellationToken,
) -> Result<TurnTerminal, BackendError> {
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

    let mut stream = response.bytes_stream();
    let mut buffered = BytesMut::new();
    let mut projection = RunProjection::default();
    let mut saw_connect_end = false;
    let mut stop = None;
    let mut stop_deadline: Option<tokio::time::Instant> = None;
    let mut cancel_sent = false;

    loop {
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
                Ok(next) => next,
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
                next = stream.try_next() => next,
            }
        };
        let chunk = match next {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if stop.is_some() => {
                tracing::debug!("Cursor Send ended while cancellation was pending: {error}");
                break;
            }
            Err(error) => {
                return Err(BackendError::Protocol(format!(
                    "Cursor Send stream failed: {error}"
                )));
            }
        };
        buffered.extend_from_slice(&chunk);
        while buffered.len() >= 5 {
            let flags = buffered[0];
            let length =
                u32::from_be_bytes([buffered[1], buffered[2], buffered[3], buffered[4]]) as usize;
            if length > MAX_CONNECT_FRAME_BYTES {
                return Err(BackendError::Protocol(format!(
                    "Cursor Connect frame exceeded {MAX_CONNECT_FRAME_BYTES} bytes"
                )));
            }
            if buffered.len() < 5 + length {
                break;
            }
            let frame = buffered.split_to(5 + length);
            if flags & 0x01 != 0 {
                return Err(BackendError::Protocol(
                    "compressed Cursor Connect frames are not supported".into(),
                ));
            }
            let value = if length == 0 {
                json!({})
            } else {
                serde_json::from_slice::<Value>(&frame[5..]).map_err(|error| {
                    BackendError::Protocol(format!(
                        "Cursor Send emitted invalid Connect JSON: {error}"
                    ))
                })?
            };
            if flags & 0x02 != 0 {
                saw_connect_end = true;
                if let Some(error) = value.get("error") {
                    return Err(BackendError::Protocol(format!(
                        "Cursor Send failed: {}",
                        client.redact(&bounded_json(error))
                    )));
                }
            } else {
                projection.process(value, events).await?;
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
    if !saw_connect_end {
        return Err(BackendError::Protocol(
            "Cursor Send omitted the Connect end-stream frame".into(),
        ));
    }
    projection.finish(events).await
}

#[derive(Clone, Copy)]
enum StreamStop {
    Cancelled,
    ConsumerClosed,
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
    saw_done: bool,
}

impl RunProjection {
    async fn process(
        &mut self,
        frame: Value,
        events: &BackendEventSender,
    ) -> Result<(), BackendError> {
        if let Some(sdk) = frame.get("sdkMessage") {
            let payload = sdk.get("message").unwrap_or(&Value::Null);
            self.capture_run_id(payload);
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
                            if events
                                .send(Ok(BackendEvent::TextDelta(text)))
                                .await
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                    }
                }
                "thinking" => {
                    if let Some(text) = message_text(payload)
                        && !text.is_empty()
                    {
                        let _ = events.send(Ok(BackendEvent::ThinkingDelta(text))).await;
                        let _ = events.send(Ok(BackendEvent::ThinkingCompleted)).await;
                    }
                }
                "task" => {
                    if let Some(text) = message_text(payload)
                        && !text.is_empty()
                    {
                        let _ = events.send(Ok(BackendEvent::ProgressDelta(text))).await;
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
                        let _ = events.send(Ok(BackendEvent::UsageUpdated { usage })).await;
                    }
                }
                // Concrete tool lifecycle events are emitted by the
                // authenticated callback, which knows the actual trouve tool
                // name and arguments. SDK messages expose only the dispatcher
                // name (`mcp`) on some Bridge versions.
                "tool_call" => {}
                _ => {}
            }
        }
        if let Some(result) = frame.get("result") {
            self.capture_run_id(result);
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
            self.capture_run_id(done);
            self.saw_done = true;
        }
        Ok(())
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

    async fn finish(self, events: &BackendEventSender) -> Result<TurnTerminal, BackendError> {
        if !self.saw_done {
            return Err(BackendError::Protocol(
                "Cursor Send omitted its done envelope".into(),
            ));
        }
        let status = self.terminal_status.as_ref().ok_or_else(|| {
            BackendError::Protocol("Cursor Send omitted its terminal result".into())
        })?;
        if status_is_finished(status) {
            if !self.emitted_assistant_text
                && let Some(text) = self.final_text
                && !text.is_empty()
            {
                let _ = events.send(Ok(BackendEvent::TextDelta(text))).await;
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
    u64_flex(status) == Some(3) || status.as_str() == Some("RUN_LIFECYCLE_STATUS_FINISHED")
}

fn status_is_cancelled(status: &Value) -> bool {
    u64_flex(status) == Some(5) || status.as_str() == Some("RUN_LIFECYCLE_STATUS_CANCELLED")
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
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        offset += 2;
        if matches!(marker, 0xd8 | 0xd9) {
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

    #[test]
    fn callback_replay_state_and_admission_are_bounded() {
        let mut calls = CallbackCalls::default();
        for index in 0..=MAX_CALLBACK_REPLAY_RECORDS {
            let call_id = format!("call-{index}");
            let (_sender, receiver) = watch::channel(None);
            calls.records.insert(
                call_id.clone(),
                CallbackRecord {
                    tool_name: "read_file".into(),
                    args: json!({ "path": "README.md" }),
                    outcome: receiver,
                },
            );
            calls.mark_completed(call_id);
        }
        assert_eq!(calls.records.len(), MAX_CALLBACK_REPLAY_RECORDS);
        assert_eq!(calls.completed.len(), MAX_CALLBACK_REPLAY_RECORDS);

        calls.completed.clear();
        while calls.records.len() < MAX_CALLBACK_RECORDS {
            let index = calls.records.len();
            let (_sender, receiver) = watch::channel(None);
            calls.records.insert(
                format!("pending-{index}"),
                CallbackRecord {
                    tool_name: "read_file".into(),
                    args: json!({ "path": "README.md" }),
                    outcome: receiver,
                },
            );
        }
        assert!(!calls.make_room());
        assert_eq!(calls.records.len(), MAX_CALLBACK_RECORDS);
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

    #[test]
    fn cursor_terminal_statuses_require_exact_sdk_values() {
        for status in [json!(3), json!("3"), json!("RUN_LIFECYCLE_STATUS_FINISHED")] {
            assert!(status_is_finished(&status));
        }
        for status in [
            json!("NOT_FINISHED"),
            json!("NOT_COMPLETED"),
            json!("COMPLETED"),
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
        assert!(!status_is_cancelled(&json!("NOT_CANCELLED")));
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
    fn thread_state_path_does_not_embed_external_ids() {
        let path = thread_state_dir(Path::new("/state"), "../provider", "../../thread");
        assert_eq!(path.parent(), Some(Path::new("/state")));
        assert!(!path.to_string_lossy().contains("provider"));
        assert!(!path.to_string_lossy().contains("thread"));
    }
}
