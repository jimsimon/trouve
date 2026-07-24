//! The engine: workspaces, sessions, threads, and the agent loop.
//!
//! One `Engine` backs one server. Turns run as spawned tasks; progress is
//! reported exclusively through the event log. Worktree mutations are
//! serialized per session (threads share the session worktree, ADR 0003).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result, anyhow, bail};
use futures::{FutureExt, StreamExt};
use trouve_agents::{AgentBackend, BackendEvent, BackendPermission, BackendTurn};
use trouve_protocol::{
    AgentMode, ApprovalDecision, BranchList, CreateSessionRequest, CreateThreadRequest, Event,
    ProviderInfo, ProvidersResponse, RestoreDirection, Scope, Session, Thread, ToolStatus,
    TurnAccepted, UpdateSessionRequest, UpdateThreadRequest, UpsertProviderRequest, Usage,
    Workspace,
};
use trouve_providers::{Message, Provider, ProviderEvent, ToolSpec};

use crate::config::{Config, ProviderConfig};
use crate::permissions::{ApprovalHub, Gate, QuestionHub, allow_key, gate};
use crate::store::{CheckpointRow, Store};
use crate::tools::{LocalToolExecutor, ToolCtx, ToolExecutor};
use crate::{context, git, modes, new_id};

/// Safety valve: maximum provider round-trips within a single turn.
const MAX_ITERATIONS: usize = 32;

/// Compact the transcript once its estimated size crosses this share of the
/// model's context window.
const COMPACTION_THRESHOLD: f64 = 0.8;

/// End-to-end budget for refreshing one GitHub host. This bounds how long a
/// stalled GraphQL request can retain the shared dashboard-cache lock.
const GITHUB_DASHBOARD_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

const THINKING_OPTION_KEYS: [&str; 4] =
    ["thinking_level", "reasoning_effort", "effort", "reasoning"];

fn validate_thinking_level(level: Option<&str>) -> Result<(), EngineError> {
    if level.is_some_and(|value| value.trim().is_empty()) {
        return Err(EngineError::BadRequest(
            "default_thinking_level must not be empty".into(),
        ));
    }
    Ok(())
}

fn has_thinking_option(options: &serde_json::Map<String, serde_json::Value>) -> bool {
    THINKING_OPTION_KEYS
        .iter()
        .any(|key| options.contains_key(*key))
}

fn inherit_thinking_option(
    options: &mut serde_json::Map<String, serde_json::Value>,
    mode_level: Option<&str>,
    global_level: Option<&str>,
) {
    if has_thinking_option(options) {
        return;
    }
    if let Some(level) = mode_level.or(global_level) {
        options.insert(
            "thinking_level".into(),
            serde_json::Value::String(level.into()),
        );
    }
}

/// Resolve the canonical inherited `thinking_level` key through a model's
/// advertised options schema. Unknown/unsupported levels fall back to the
/// model's schema default; models without an enum thinking knob drop the
/// inherited option entirely.
fn normalize_thinking_option(
    options: &mut serde_json::Map<String, serde_json::Value>,
    model: Option<&trouve_protocol::ModelInfo>,
) {
    let Some(canonical) = options.get("thinking_level").cloned() else {
        return;
    };
    let property = model.and_then(|model| {
        THINKING_OPTION_KEYS.iter().find_map(|key| {
            let property = model
                .options_schema
                .pointer(&format!("/properties/{key}"))?;
            let values = property["enum"].as_array()?;
            (values.len() > 1).then_some((*key, property, values))
        })
    });
    let Some((key, property, values)) = property else {
        options.remove("thinking_level");
        return;
    };

    // A thread-level selection already stored under the model's native key
    // wins over the inherited canonical value.
    if key != "thinking_level" && options.contains_key(key) {
        options.remove("thinking_level");
        return;
    }

    let selected = canonical
        .as_str()
        .filter(|level| values.iter().any(|value| value.as_str() == Some(*level)))
        .map(String::from)
        .or_else(|| property["default"].as_str().map(String::from));
    options.remove("thinking_level");
    if let Some(selected) = selected {
        options.insert(key.into(), serde_json::Value::String(selected));
    }
}

pub struct Engine {
    pub(crate) store: Store,
    pub(crate) data_dir: PathBuf,
    config_dir: Option<PathBuf>,
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    /// Providers registered programmatically (`with_provider`); preserved
    /// across config-driven registry reloads.
    injected_providers: Mutex<HashMap<String, Arc<dyn Provider>>>,
    /// External agent backends (Codex app-server, cursor-agent, Claude Code
    /// CLI), keyed by provider id like `providers`.
    backends: RwLock<HashMap<String, Arc<dyn AgentBackend>>>,
    /// Backends registered programmatically (`with_backend`); preserved
    /// across config-driven registry reloads.
    injected_backends: Mutex<HashMap<String, Arc<dyn AgentBackend>>>,
    pub(crate) executor: Arc<dyn ToolExecutor>,
    approvals: Arc<ApprovalHub>,
    questions: Arc<QuestionHub>,
    session_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Threads with a dispatcher currently running turns, mapped to their
    /// session. A thread in this map drains its own prompt queue; sends
    /// while present just enqueue. The session ids feed `Session.active`
    /// and the `session.activity` server event.
    active_threads: Mutex<std::collections::HashMap<String, String>>,
    /// Sessions currently being deleted. Dispatch checks this while holding
    /// `active_threads`, making "no active turns" and "no new turns" one
    /// atomic state transition before destructive cleanup begins.
    deleting_sessions: Mutex<std::collections::HashSet<String>>,
    /// Cancellation tokens for in-flight turns, keyed by thread id. Set while
    /// a turn runs; `cancel_turn` trips one to interrupt the turn's provider
    /// stream, tool calls, and approval waits at the next await point.
    turn_cancels: Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    /// Threads where a new prompt arrived after cancellation was requested.
    /// The cancelling dispatcher consumes this marker and resumes the queue
    /// instead of leaving that explicitly submitted follow-up paused.
    resume_after_cancel: Mutex<HashSet<String>>,
    /// Per-host incremental PR snapshots. Holding this asynchronous lock also
    /// coalesces refresh requests from multiple connected clients into one
    /// upstream GitHub poll.
    github_dashboard_caches:
        tokio::sync::Mutex<HashMap<String, crate::github::GitHubDashboardCache>>,
    pub(crate) config: Mutex<Config>,
    /// Where provider configuration changes are persisted. `None` disables
    /// persistence (tests).
    config_file: Option<PathBuf>,
    default_model: RwLock<String>,
    /// Canonical thinking-level token inherited by modes without an
    /// override. It is translated through the selected model's schema when
    /// the turn starts.
    default_thinking_level: RwLock<Option<String>>,
    /// Global default permission mode for new threads, used by modes that
    /// don't set one of their own.
    default_permission_mode: RwLock<trouve_protocol::PermissionMode>,
    pub(crate) secrets: Arc<dyn trouve_providers::secrets::SecretStore>,
    pub(crate) code_review: crate::review::CodeReviewRuntime,
    /// In-flight OAuth logins, keyed by provider id.
    logins: Mutex<HashMap<String, LoginState>>,
    /// In-flight managed vendor-CLI installs, keyed by CLI id.
    cli_installs: Mutex<HashMap<String, CliInstallState>>,
    /// The llama-server sidecar behind the built-in "local" provider.
    local_manager: Arc<crate::local::LlamaManager>,
    /// The built-in "local" provider, kept around so enabling re-injects
    /// the same instance after a disable removed it from the registry.
    local_provider: Arc<dyn Provider>,
    /// In-flight local model (GGUF) downloads, keyed by model id.
    local_downloads: Mutex<HashMap<String, LocalDownloadState>>,
    /// RAM/VRAM probe, run once on first use.
    hardware: std::sync::OnceLock<crate::local::Hardware>,
    /// Latest-version lookups, cached per CLI (network is best-effort).
    cli_latest: Mutex<HashMap<String, (std::time::Instant, Option<String>)>>,
    /// This server's reachable base URL (e.g. "http://127.0.0.1:7433"), set
    /// once the listener binds; the MCP tool bridge dials back through it.
    base_url: RwLock<Option<String>>,
    /// Ephemeral credential appended only to internal MCP bridge URLs.
    bridge_token: RwLock<Option<String>>,
    /// Warm the search index on session creation and GC the shared index
    /// store on archive/delete. Off by default so tests never touch the
    /// embedding model; the server enables it (`with_index_hooks`).
    index_hooks: bool,
    /// Per-server MCP logs, shared with the executor's `McpManager` so both
    /// runtime connections and settings health probes land in one place.
    mcp_logs: crate::mcp::McpLogStore,
    /// Interactive shells (one per session) for the client terminal panel.
    terminals: crate::terminal::TerminalManager,
    /// Whether the server can reach the internet. Defaults to true; only a
    /// configured probe (`with_connectivity_probe`) or `set_online` ever
    /// flips it, so probe-less engines (tests, embedders) never go offline.
    online: std::sync::atomic::AtomicBool,
    /// Reachability check driven by the connectivity monitor. `None`
    /// disables monitoring entirely.
    connectivity_probe: Option<crate::connectivity::Probe>,
}

#[derive(Debug, Clone)]
enum LoginState {
    /// In flight; carries what the user was told to do so a repeated
    /// start_login can re-present it (e.g. after closing the browser tab)
    /// instead of refusing while the flow is still valid.
    Pending {
        started: trouve_protocol::LoginStarted,
        callback_sender: Option<tokio::sync::mpsc::Sender<String>>,
    },
    Success,
    Failed(String),
}

#[derive(Debug, Clone)]
enum CliInstallState {
    Pending {
        /// Version being installed, once discovered.
        version: Option<String>,
        /// Byte progress + cancel flag, shared with the install task.
        progress: Arc<trouve_agents::install::Progress>,
    },
    Success(String),
    Failed(String),
}

#[derive(Debug, Clone)]
enum LocalDownloadState {
    Pending {
        /// Bytes downloaded so far; the task updates the counter.
        bytes: Arc<std::sync::atomic::AtomicU64>,
        /// Set to make the download task stop and clean up its .part file.
        cancel: Arc<std::sync::atomic::AtomicBool>,
    },
    Failed(String),
}

/// Whether a `--version` report refers to the given vendor version. The
fn terminal_info(terminal: &crate::terminal::Terminal) -> trouve_protocol::TerminalInfo {
    let (cols, rows) = terminal.size();
    trouve_protocol::TerminalInfo {
        id: terminal.id.clone(),
        session_id: terminal.session_id.clone(),
        cols,
        rows,
        exited: terminal.exited(),
    }
}

/// CLIs decorate their output differently ("2.1.34 (Claude Code)",
/// "codex-cli 0.143.0", "2026.07.01-41b2de7"), so containment beats
/// equality.
fn cli_version_matches(reported: &str, version: &str) -> bool {
    reported == version
        || reported
            .split([' ', '(', ')'])
            .any(|tok| tok == version || tok.strip_prefix('v') == Some(version))
}

/// The managed CLI serving a backend provider kind, if any.
fn cli_for_kind(kind: &str) -> Option<trouve_agents::install::CliId> {
    use trouve_agents::install::CliId;
    match kind {
        "cursor-cli" => Some(CliId::CursorAgent),
        "claude-cli" => Some(CliId::Claude),
        "codex-app-server" | "codex-responses" => Some(CliId::Codex),
        _ => None,
    }
}

/// Resolve the executable for a CLI-backed provider. An explicit command
/// wins; otherwise a trouve-managed binary takes precedence over PATH.
fn resolved_cli_command(kind: &str, command: Option<String>, data_dir: &Path) -> Option<String> {
    command.or_else(|| {
        cli_for_kind(kind)
            .map(|cli| trouve_agents::install::managed_bin(data_dir, cli))
            .filter(|bin| bin.exists())
            .map(|bin| bin.to_string_lossy().into_owned())
    })
}

/// Config kinds handled by the [`AgentBackend`] seam rather than a Provider.
fn is_backend_kind(kind: &str) -> bool {
    matches!(kind, "codex-app-server" | "cursor-cli" | "claude-cli")
}

/// Config kinds whose auth lives in a vendor CLI (backends plus the
/// experimental direct-Codex provider, which reads `codex login`'s token).
fn is_cli_auth_kind(kind: &str) -> bool {
    is_backend_kind(kind) || kind == "codex-responses"
}

/// Credential style for a configured provider: "cli" for vendor-CLI-backed
/// kinds, "oauth" when subscription endpoints are configured (and no inline
/// key wins), "none" for keyless local endpoints, "api-key" otherwise.
fn provider_auth_kind(pc: &ProviderConfig) -> String {
    if is_cli_auth_kind(&pc.kind) {
        // cursor-cli works both ways: subscription login ("cursor" preset)
        // or an API key ("cursor-api" preset, usage-based billing).
        if pc.kind == "cursor-cli" && (pc.api_key.is_some() || pc.api_key_env.is_some()) {
            "api-key".into()
        } else {
            "cli".into()
        }
    } else if pc.oauth.is_some() && pc.api_key.is_none() {
        "oauth".into()
    } else if pc.api_key.is_none()
        && pc.api_key_env.is_none()
        && pc.base_url.as_deref().is_some_and(is_loopback_base_url)
    {
        "none".into()
    } else {
        "api-key".into()
    }
}

/// Whether a provider endpoint lives on this machine (Ollama, llama.cpp,
/// vLLM, …) and therefore keeps working without internet. Parses the
/// authority and requires the exact host `localhost` or a loopback IP —
/// a substring check would also accept remote hosts like
/// `localhost.attacker.example` and mislabel them as offline-capable
/// keyless endpoints.
fn is_loopback_base_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // IPv6 hosts come back bracketed; IpAddr parsing wants them bare.
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Build the provider registry from config + zero-config env defaults.
fn build_all_providers(
    config: &Config,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
) -> HashMap<String, Arc<dyn Provider>> {
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    for (id, pc) in &config.providers {
        if is_backend_kind(&pc.kind) {
            continue; // handled by build_all_backends
        }
        match build_provider(id, pc, secrets) {
            Ok(p) => {
                providers.insert(id.clone(), p);
            }
            Err(e) => tracing::warn!("provider {id}: {e}; skipping"),
        }
    }
    // Zero-config defaults from conventional env vars.
    if !providers.contains_key("openai")
        && let Ok(p) = trouve_providers::openai_compat::OpenAiCompatProvider::openai_from_env()
    {
        providers.insert("openai".into(), Arc::new(p));
    }
    if !providers.contains_key("anthropic")
        && let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
    {
        providers.insert(
            "anthropic".into(),
            Arc::new(trouve_providers::anthropic::AnthropicProvider::new(
                "anthropic",
                None,
                Arc::new(trouve_providers::auth::StaticToken(key)),
            )),
        );
    }
    providers
}

/// Build the agent-backend registry from config.
fn build_all_backends(
    config: &Config,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
    data_dir: &Path,
) -> HashMap<String, Arc<dyn AgentBackend>> {
    let mut backends: HashMap<String, Arc<dyn AgentBackend>> = HashMap::new();
    for (id, pc) in &config.providers {
        // Explicit command wins; otherwise a trouve-managed install beats
        // whatever is on PATH (distro packages lag behind vendor releases).
        let command = resolved_cli_command(&pc.kind, pc.command.clone(), data_dir);
        let backend: Arc<dyn AgentBackend> = match pc.kind.as_str() {
            "codex-app-server" => Arc::new(trouve_agents::codex::CodexBackend::new(id, command)),
            "cursor-cli" => {
                // Same precedence as native providers: inline key > env var >
                // key saved through settings (secret store). Subscription
                // login via the CLI still works when all are absent.
                let api_key = pc
                    .api_key
                    .clone()
                    .or_else(|| pc.api_key_env.as_ref().and_then(|v| std::env::var(v).ok()))
                    .or_else(|| {
                        secrets
                            .get(&trouve_providers::secrets::api_key_secret(id))
                            .ok()
                            .flatten()
                    });
                Arc::new(trouve_agents::cursor::CursorBackend::new(
                    id, command, api_key,
                ))
            }
            "claude-cli" => Arc::new(trouve_agents::claude::ClaudeBackend::new(id, command)),
            _ => continue,
        };
        backends.insert(id.clone(), backend);
    }
    backends
}

impl Engine {
    pub fn new(store: Store, data_dir: PathBuf, config: &Config) -> Self {
        let secrets: Arc<dyn trouve_providers::secrets::SecretStore> =
            Arc::from(trouve_providers::secrets::default_store(&data_dir));
        let mut providers = build_all_providers(config, &secrets);
        let backends = build_all_backends(config, &secrets, &data_dir);
        let mcp_logs = crate::mcp::McpLogStore::default();
        let config_dir = dirs::config_dir().map(|d| d.join("trouve"));
        // The built-in "local" provider (managed llama-server). Registered
        // unless the user disabled local models — it lists no models until
        // a GGUF is downloaded — and seeded as injected so config-driven
        // reloads keep it.
        // Construction reaps llama-servers leaked by a crashed previous run
        // (they hold VRAM and would starve this run's model loads).
        let local_manager = Arc::new(crate::local::LlamaManager::new(&data_dir));
        let local_provider: Arc<dyn Provider> = Arc::new(crate::local::LocalProvider::new(
            data_dir.clone(),
            config_dir.clone(),
            local_manager.clone(),
        ));
        let mut injected_providers = HashMap::new();
        if config.local_enabled.unwrap_or(true) {
            providers.insert("local".into(), local_provider.clone());
            injected_providers.insert("local".to_string(), local_provider.clone());
        }
        Self {
            store,
            data_dir,
            config_dir,
            providers: RwLock::new(providers),
            injected_providers: Mutex::new(injected_providers),
            backends: RwLock::new(backends),
            injected_backends: Mutex::new(HashMap::new()),
            executor: Arc::new(LocalToolExecutor::with_mcp_logs(mcp_logs.clone())),
            approvals: Arc::new(ApprovalHub::default()),
            questions: Arc::new(QuestionHub::default()),
            session_locks: Mutex::new(HashMap::new()),
            active_threads: Mutex::new(std::collections::HashMap::new()),
            deleting_sessions: Mutex::new(std::collections::HashSet::new()),
            turn_cancels: Mutex::new(std::collections::HashMap::new()),
            resume_after_cancel: Mutex::new(HashSet::new()),
            github_dashboard_caches: tokio::sync::Mutex::new(HashMap::new()),
            config: Mutex::new(config.clone()),
            // No write-back by default: only a caller that loaded `config`
            // from disk should enable persisting to that file (see
            // `with_config_file`). Defaulting to the real config path here
            // let test/embedded engines built from synthetic configs
            // clobber the user's config.toml on any provider change.
            config_file: None,
            default_model: RwLock::new(
                config
                    .default_model
                    .clone()
                    .unwrap_or_else(|| "openai/gpt-4.1-mini".into()),
            ),
            default_thinking_level: RwLock::new(config.default_thinking_level.clone()),
            default_permission_mode: RwLock::new(
                config.default_permission_mode.unwrap_or_default(),
            ),
            secrets,
            code_review: crate::review::CodeReviewRuntime::default(),
            logins: Mutex::new(HashMap::new()),
            cli_installs: Mutex::new(HashMap::new()),
            local_manager,
            local_provider,
            local_downloads: Mutex::new(HashMap::new()),
            hardware: std::sync::OnceLock::new(),
            cli_latest: Mutex::new(HashMap::new()),
            base_url: RwLock::new(None),
            bridge_token: RwLock::new(None),
            index_hooks: false,
            mcp_logs,
            terminals: crate::terminal::TerminalManager::default(),
            online: std::sync::atomic::AtomicBool::new(true),
            connectivity_probe: None,
        }
    }

    /// Enable search-index lifecycle hooks: warm the index when a session is
    /// created (the in-process analogue of the agent plugins' SessionStart
    /// hook) and sweep the shared store when one is archived or deleted.
    pub fn with_index_hooks(mut self) -> Self {
        self.index_hooks = true;
        self
    }

    /// Enable internet-reachability monitoring with the given probe (the
    /// server binary passes [`crate::connectivity::system_probe`]; tests can
    /// inject a scripted one). Without a probe the engine always reports
    /// online and never touches the network.
    pub fn with_connectivity_probe(mut self, probe: crate::connectivity::Probe) -> Self {
        self.connectivity_probe = Some(probe);
        self
    }

    /// Whether the server can currently reach the internet.
    pub fn is_online(&self) -> bool {
        self.online.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Force the connectivity state (tests/embedders). Emits the
    /// `server.connectivity_changed` event on an actual transition, exactly
    /// like the probe-driven monitor.
    pub fn set_online(&self, online: bool) {
        self.transition_connectivity(online);
    }

    fn transition_connectivity(&self, online: bool) {
        let was = self
            .online
            .swap(online, std::sync::atomic::Ordering::Relaxed);
        if was == online {
            return;
        }
        if online {
            tracing::info!("connectivity restored");
        } else {
            tracing::warn!("connectivity lost: model vendors are unreachable");
        }
        let _ = self
            .store
            .append_event(Scope::Server, Event::ConnectivityChanged { online });
    }

    /// Run the first connectivity probe (no-op without one). Called before
    /// the server starts accepting requests so an offline start never serves
    /// a model list it will immediately retract.
    pub async fn init_connectivity(&self) {
        if let Some(probe) = self.connectivity_probe.clone() {
            let online = probe().await;
            self.transition_connectivity(online);
        }
    }

    /// Poll the connectivity probe for the lifetime of the server: slowly
    /// while online (going offline is rarely urgent), quickly while offline
    /// (clients unblock prompt entry off the recovery event). No-op without
    /// a probe.
    pub fn start_connectivity_monitor(self: &Arc<Self>) {
        let Some(probe) = self.connectivity_probe.clone() else {
            return;
        };
        let engine = self.clone();
        tokio::spawn(async move {
            loop {
                let interval = if engine.is_online() {
                    crate::connectivity::ONLINE_POLL
                } else {
                    crate::connectivity::OFFLINE_POLL
                };
                tokio::time::sleep(interval).await;
                let online = probe().await;
                engine.transition_connectivity(online);
            }
        });
    }

    /// Record the server's reachable base URL (enables the MCP tool bridge
    /// for backends configured with `tool_bridge = true`).
    pub fn set_base_url(&self, url: &str) {
        *self.base_url.write().unwrap() = Some(url.trim_end_matches('/').to_string());
    }

    /// Set the server-generated credential vendor children must present to
    /// the internal MCP bridge. `None` keeps in-process open test routers
    /// backwards-compatible.
    pub fn set_bridge_token(&self, token: Option<String>) {
        *self.bridge_token.write().unwrap() = token;
    }

    /// Swap the tool executor (cloud isolation hook, ADR 0004).
    pub fn with_executor(mut self, executor: Arc<dyn ToolExecutor>) -> Self {
        self.executor = executor;
        self
    }

    /// Register (or replace) a provider instance under an id. Survives
    /// config-driven registry reloads.
    pub fn with_provider(self, id: &str, provider: Arc<dyn Provider>) -> Self {
        self.injected_providers
            .lock()
            .unwrap()
            .insert(id.to_string(), provider.clone());
        self.providers
            .write()
            .unwrap()
            .insert(id.to_string(), provider);
        self
    }

    /// Register (or replace) an agent backend instance under an id. Survives
    /// config-driven registry reloads (tests, embedders).
    pub fn with_backend(self, id: &str, backend: Arc<dyn AgentBackend>) -> Self {
        self.injected_backends
            .lock()
            .unwrap()
            .insert(id.to_string(), backend.clone());
        self.backends
            .write()
            .unwrap()
            .insert(id.to_string(), backend);
        self
    }

    /// Override the default model for new threads.
    pub fn with_default_model(self, model: &str) -> Self {
        *self.default_model.write().unwrap() = model.to_string();
        self
    }

    /// Override the global default thinking level for new threads.
    pub fn with_default_thinking_level(self, level: Option<&str>) -> Self {
        *self.default_thinking_level.write().unwrap() = level.map(String::from);
        self
    }

    /// Override the config dir used for mode/AGENTS.md discovery (tests).
    pub fn with_config_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.config_dir = dir;
        self
    }

    /// Override (or disable, with `None`) where provider config changes are
    /// written.
    pub fn with_config_file(mut self, path: Option<PathBuf>) -> Self {
        self.config_file = path;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.providers.read().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// All models available right now: native provider models plus, for each
    /// agent backend that is installed and logged in, the vendor-reported
    /// catalog (cached inside the backend). Backends without credentials are
    /// skipped entirely — their models can't run, so listing them only
    /// clutters the picker.
    ///
    /// While the server is offline, only models that can actually run are
    /// listed: the built-in local provider and loopback endpoints (Ollama
    /// etc.). Remote providers and vendor backends are dropped instead of
    /// degrading to static/fallback catalogs of models every turn would
    /// fail on; clients gate prompt entry on this list being non-empty.
    pub async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let online = self.is_online();
        let offline_capable = if online {
            std::collections::HashSet::new()
        } else {
            self.offline_capable_provider_ids()
        };
        let providers: Vec<_> = self
            .providers
            .read()
            .unwrap()
            .iter()
            .filter(|(id, _)| online || offline_capable.contains(id.as_str()))
            .map(|(_, p)| p.clone())
            .collect();
        let provider_lists =
            futures::future::join_all(providers.iter().map(|p| p.list_models())).await;
        let mut models: Vec<_> = provider_lists.into_iter().flatten().collect();
        let ready: Vec<_> = if online {
            self.backends
                .read()
                .unwrap()
                .values()
                .filter(|b| {
                    let status = b.status();
                    status.installed && status.has_credentials
                })
                .cloned()
                .collect()
        } else {
            Vec::new() // vendor backends all need their cloud
        };
        let listings = futures::future::join_all(ready.iter().map(|b| b.list_models())).await;
        models.extend(listings.into_iter().flatten());
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    /// Provider ids that keep working without internet: the built-in local
    /// provider plus configured loopback endpoints.
    fn offline_capable_provider_ids(&self) -> std::collections::HashSet<String> {
        let mut ids: std::collections::HashSet<String> = ["local".to_string()].into();
        let config = self.config.lock().unwrap();
        for (id, pc) in &config.providers {
            if pc.base_url.as_deref().is_some_and(is_loopback_base_url) {
                ids.insert(id.clone());
            }
        }
        ids
    }

    // --- provider configuration ----------------------------------------------

    /// Well-known provider presets for one-click setup in clients.
    pub fn known_providers(&self) -> Vec<trouve_protocol::KnownProvider> {
        trouve_providers::catalog::known_providers()
    }

    /// Configured providers (secrets elided) plus the default model.
    pub fn list_providers(&self) -> ProvidersResponse {
        let config = self.config.lock().unwrap();
        let registry = self.providers.read().unwrap();
        let mut infos: Vec<ProviderInfo> = config
            .providers
            .iter()
            .map(|(id, pc)| {
                let auth = provider_auth_kind(pc);
                let has_credentials = self.provider_has_credentials(id, pc, &auth, &registry);
                ProviderInfo {
                    id: id.clone(),
                    kind: pc.kind.clone(),
                    base_url: pc.base_url.clone(),
                    has_credentials,
                    category: trouve_providers::catalog::provider_category(
                        id,
                        &auth,
                        pc.base_url.as_deref(),
                    ),
                    auth,
                    experimental: pc.kind == "codex-responses",
                }
            })
            .collect();
        // Zero-config providers (env keys) that aren't in the config file.
        for id in registry.keys() {
            if !config.providers.contains_key(id) {
                let local = id == "local";
                infos.push(ProviderInfo {
                    id: id.clone(),
                    kind: if id == "anthropic" {
                        "anthropic".into()
                    } else {
                        "openai-compat".into()
                    },
                    base_url: None,
                    has_credentials: true,
                    auth: if local { "none" } else { "api-key" }.into(),
                    category: if local { "local" } else { "api" }.into(),
                    experimental: false,
                });
            }
        }
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        ProvidersResponse {
            providers: infos,
            default_model: self.default_model.read().unwrap().clone(),
            default_thinking_level: self.default_thinking_level.read().unwrap().clone(),
            default_permission_mode: *self.default_permission_mode.read().unwrap(),
        }
    }

    /// Best-effort credential presence for one configured provider.
    fn provider_has_credentials(
        &self,
        id: &str,
        pc: &ProviderConfig,
        auth: &str,
        registry: &HashMap<String, Arc<dyn Provider>>,
    ) -> bool {
        match auth {
            // Vendor CLI holds the auth; adapters do cheap fs checks.
            "cli" => {
                if is_backend_kind(&pc.kind) {
                    self.backends
                        .read()
                        .unwrap()
                        .get(id)
                        .map(|b| {
                            let s = b.status();
                            s.installed && s.has_credentials
                        })
                        .unwrap_or(false)
                } else {
                    // codex-responses: needs the Codex CLI's auth file.
                    let home = std::env::var("CODEX_HOME")
                        .map(PathBuf::from)
                        .ok()
                        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")));
                    home.map(|h| h.join("auth.json").exists()).unwrap_or(false)
                }
            }
            // OAuth providers build lazily, so registry membership alone
            // doesn't prove credentials — check for stored tokens.
            "oauth" => self
                .secrets
                .get(&trouve_providers::secrets::oauth_secret(id))
                .ok()
                .flatten()
                .is_some(),
            // Key-authenticated agent backend (cursor-api): not in the
            // provider registry, so check the key channels directly.
            _ if is_backend_kind(&pc.kind) => {
                pc.api_key.is_some()
                    || pc
                        .api_key_env
                        .as_ref()
                        .map(|v| std::env::var(v).is_ok())
                        .unwrap_or(false)
                    || self
                        .secrets
                        .get(&trouve_providers::secrets::api_key_secret(id))
                        .ok()
                        .flatten()
                        .is_some()
            }
            _ => registry.contains_key(id),
        }
    }

    /// Create or update a provider. The API key (when present) goes to the
    /// secret store; the config file only holds non-secret settings.
    pub fn upsert_provider(
        &self,
        id: &str,
        req: &UpsertProviderRequest,
    ) -> Result<ProviderInfo, EngineError> {
        if !matches!(req.kind.as_str(), "openai-compat" | "anthropic")
            && !is_cli_auth_kind(&req.kind)
        {
            return Err(EngineError::BadRequest(format!(
                "unknown provider kind {:?} (expected openai-compat, anthropic, \
                 codex-app-server, cursor-cli, claude-cli, or codex-responses)",
                req.kind
            )));
        }
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(EngineError::BadRequest(
                "provider id must be non-empty ascii alphanumeric/dashes".into(),
            ));
        }
        if let Some(key) = req.api_key.as_deref().filter(|k| !k.is_empty()) {
            self.secrets
                .set(&trouve_providers::secrets::api_key_secret(id), key)
                .map_err(EngineError::Internal)?;
        }
        {
            let mut config = self.config.lock().unwrap();
            let entry = config.providers.entry(id.to_string()).or_default();
            entry.kind = req.kind.clone();
            entry.base_url = req.base_url.clone().filter(|u| !u.is_empty());
            // Known preset: honor the conventional env var as a key fallback.
            if entry.api_key_env.is_none() {
                entry.api_key_env = trouve_providers::catalog::known_providers()
                    .into_iter()
                    .find(|k| k.id == id)
                    .and_then(|k| k.api_key_env);
            }
            self.persist_config(&config);
        }
        self.reload_providers();
        let config = self.config.lock().unwrap();
        let registry = self.providers.read().unwrap();
        let pc = config.providers.get(id).cloned().unwrap_or_default();
        let auth = provider_auth_kind(&pc);
        let has_credentials = self.provider_has_credentials(id, &pc, &auth, &registry);
        Ok(ProviderInfo {
            id: id.to_string(),
            kind: req.kind.clone(),
            base_url: req.base_url.clone().filter(|u| !u.is_empty()),
            has_credentials,
            category: trouve_providers::catalog::provider_category(
                id,
                &auth,
                req.base_url.as_deref(),
            ),
            auth,
            experimental: req.kind == "codex-responses",
        })
    }

    /// Remove a provider from the config and its stored API key.
    pub fn delete_provider(&self, id: &str) -> Result<(), EngineError> {
        {
            let mut config = self.config.lock().unwrap();
            if config.providers.remove(id).is_none() {
                return Err(EngineError::NotFound(format!("provider {id}")));
            }
            self.persist_config(&config);
        }
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::api_key_secret(id));
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::oauth_secret(id));
        self.reload_providers();
        Ok(())
    }

    // --- OAuth login (subscription providers) ---------------------------------

    /// Start an OAuth login for a configured provider. Returns what the user
    /// must do (open a URL, possibly enter a code); the exchange runs in the
    /// background and `login_status` reports how it went.
    pub async fn start_login(
        self: &Arc<Self>,
        id: &str,
    ) -> Result<trouve_protocol::LoginStarted, EngineError> {
        use trouve_providers::auth as oauth_flow;

        // Vendor-CLI logins (subscription backends) go through the vendor's
        // own flow; everything else uses our generic OAuth machinery.
        let cli_kind = {
            let config = self.config.lock().unwrap();
            config
                .providers
                .get(id)
                .map(|pc| (pc.kind.clone(), pc.command.clone()))
                .filter(|(k, _)| is_cli_auth_kind(k))
        };
        if let Some((kind, command)) = cli_kind {
            return self.start_cli_login(id, &kind, command).await;
        }

        // "Sign in with GitHub" (Integrations, not a model provider): id
        // "github" is github.com, "github:<host>" a GitHub Enterprise
        // instance. Device flow against that host; the token lands in the
        // oauth secret github_token() reads, because the login id and the
        // host's secret id are the same string.
        let github_host = if id == "github" {
            Some(crate::github::GITHUB_COM.to_string())
        } else {
            id.strip_prefix("github:").map(str::to_string)
        };
        let oauth = if let Some(host) = github_host {
            let client_id = self
                .github_hosts()
                .into_iter()
                .find(|(h, _)| *h == host)
                .ok_or_else(|| EngineError::NotFound(format!("GitHub host {host}")))?
                .1
                .ok_or_else(|| {
                    EngineError::BadRequest(format!(
                        "GitHub OAuth is not configured for {host}: set a client id of an \
                         OAuth app (device flow enabled) on that host"
                    ))
                })?;
            crate::github::oauth_config(&host, &client_id)
        } else {
            let config = self.config.lock().unwrap();
            config
                .providers
                .get(id)
                .and_then(|pc| pc.oauth.clone())
                .ok_or_else(|| {
                    EngineError::BadRequest(format!("provider {id} has no OAuth configuration"))
                })?
        };
        // A login is already in flight (the user may have closed the
        // browser tab): re-present the same instructions — the URL/code
        // stay valid while the flow waits — instead of refusing.
        if let Some(LoginState::Pending { started, .. }) = self.logins.lock().unwrap().get(id) {
            return Ok(started.clone());
        }

        if oauth.device_authorization_url.is_some() {
            // RFC 8628 device flow: show the code, poll in the background.
            let device = oauth_flow::device_authorize(&oauth)
                .await
                .map_err(|e| EngineError::BadRequest(e.to_string()))?;
            let started = trouve_protocol::LoginStarted {
                verification_url: device
                    .verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| device.verification_uri.clone()),
                user_code: Some(device.user_code.clone()),
            };
            self.logins.lock().unwrap().insert(
                id.to_string(),
                LoginState::Pending {
                    started: started.clone(),
                    callback_sender: None,
                },
            );
            let engine = self.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                let result = oauth_flow::device_poll(&oauth, &device).await;
                engine.finish_login(&id, result);
            });
            Ok(started)
        } else if oauth.authorization_url.is_some() {
            // PKCE browser flow: we listen on localhost for the redirect.
            let listener =
                tokio::net::TcpListener::bind(("127.0.0.1", oauth.redirect_port.unwrap_or(0)))
                    .await
                    .map_err(|e| {
                        EngineError::BadRequest(format!("cannot bind redirect port: {e}"))
                    })?;
            let redirect_uri = format!(
                "http://localhost:{}{}",
                listener.local_addr().map(|a| a.port()).unwrap_or_default(),
                oauth.redirect_path.as_deref().unwrap_or("/callback")
            );
            let challenge = oauth_flow::pkce_challenge();
            let state = uuid::Uuid::new_v4().simple().to_string();
            let url = oauth_flow::pkce_authorize_url(&oauth, &challenge, &redirect_uri, &state)
                .map_err(|e| EngineError::BadRequest(e.to_string()))?;
            let started = trouve_protocol::LoginStarted {
                verification_url: url,
                user_code: None,
            };
            self.logins.lock().unwrap().insert(
                id.to_string(),
                LoginState::Pending {
                    started: started.clone(),
                    callback_sender: None,
                },
            );
            let engine = self.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                let result = async {
                    let code = tokio::time::timeout(
                        std::time::Duration::from_secs(600),
                        oauth_flow::pkce_wait_for_code(listener, &state),
                    )
                    .await
                    .map_err(|_| {
                        trouve_providers::ProviderError::Auth("login timed out".into())
                    })??;
                    oauth_flow::pkce_exchange(&oauth, &code, &challenge.verifier, &redirect_uri)
                        .await
                }
                .await;
                engine.finish_login(&id, result);
            });
            Ok(started)
        } else {
            Err(EngineError::BadRequest(format!(
                "provider {id} OAuth config has neither device_authorization_url \
                 nor authorization_url"
            )))
        }
    }

    /// Login for CLI-auth providers: run the vendor CLI's own login flow and
    /// surface its verification URL; `login_status` reports the outcome.
    async fn start_cli_login(
        self: &Arc<Self>,
        id: &str,
        kind: &str,
        command: Option<String>,
    ) -> Result<trouve_protocol::LoginStarted, EngineError> {
        // The vendor CLI is still waiting on its verification URL (the
        // user may have closed the browser tab): hand the same URL back
        // so the client can reopen it, rather than refusing.
        if let Some(LoginState::Pending { started, .. }) = self.logins.lock().unwrap().get(id) {
            return Ok(started.clone());
        }
        let login = if is_backend_kind(kind) {
            let backend = self
                .backends
                .read()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| EngineError::NotFound(format!("provider {id}")))?;
            backend.start_login().await
        } else {
            // codex-responses: credentials come from the Codex CLI login.
            let cmd = resolved_cli_command(kind, command, &self.data_dir)
                .unwrap_or_else(|| "codex".into());
            trouve_agents::spawn_codex_login(&cmd).await
        }
        .map_err(|e| EngineError::BadRequest(e.to_string()))?;

        let trouve_agents::BackendLogin {
            verification_url,
            user_code,
            callback_sender,
            done,
        } = login;
        let started = trouve_protocol::LoginStarted {
            verification_url: verification_url.unwrap_or_default(),
            user_code,
        };
        self.logins.lock().unwrap().insert(
            id.to_string(),
            LoginState::Pending {
                started: started.clone(),
                callback_sender,
            },
        );
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let state = match done.await {
                Ok(()) => LoginState::Success,
                Err(e) => LoginState::Failed(e.to_string()),
            };
            engine
                .logins
                .lock()
                .unwrap()
                .insert(id_owned.clone(), state);
        });
        Ok(started)
    }

    // --- managed vendor CLIs ---------------------------------------------------

    /// Install state of every vendor CLI trouve can manage: the binary that
    /// would run (managed install beats PATH), its version, and whether the
    /// vendor serves something newer (best-effort network check, cached).
    pub async fn list_clis(&self) -> trouve_protocol::CliList {
        use trouve_agents::install as cli;

        let mut clis = Vec::new();
        for id in cli::ALL_CLIS {
            let explicit = {
                // An explicit per-provider `command` overrides resolution;
                // surface it so the UI doesn't claim "not installed".
                let config = self.config.lock().unwrap();
                config
                    .providers
                    .values()
                    .filter(|pc| cli_for_kind(&pc.kind) == Some(id))
                    .find_map(|pc| pc.command.clone())
            };
            let managed = cli::installed(&self.data_dir, id);
            let (source, path, installed_version) = if let Some(cmd) = explicit {
                let version = cli::binary_version(&cmd).await;
                ("path".to_string(), Some(cmd), version)
            } else if let Some(info) = managed {
                ("managed".into(), Some(info.bin), Some(info.version))
            } else if let Some(found) = cli::find_on_path(id.as_str()) {
                let path = found.to_string_lossy().into_owned();
                let version = cli::binary_version(&path).await;
                ("path".into(), Some(path), version)
            } else {
                ("none".into(), None, None)
            };

            let latest_version = self.cli_latest_version(id).await;
            let update_available = match (&installed_version, &latest_version) {
                (Some(have), Some(latest)) => !cli_version_matches(have, latest),
                (None, Some(_)) => true,
                _ => false,
            };
            clis.push(trouve_protocol::CliInfo {
                id: id.as_str().into(),
                display_name: id.display_name().into(),
                kinds: id.provider_kinds().iter().map(|s| s.to_string()).collect(),
                installed_version,
                source,
                path,
                latest_version,
                update_available,
            });
        }
        trouve_protocol::CliList { clis }
    }

    /// Latest vendor version for one CLI, cached for an hour; None when the
    /// lookup fails (offline is fine — the UI just can't offer updates).
    async fn cli_latest_version(&self, id: trouve_agents::install::CliId) -> Option<String> {
        const TTL: std::time::Duration = std::time::Duration::from_secs(3600);
        {
            let cache = self.cli_latest.lock().unwrap();
            if let Some((at, v)) = cache.get(id.as_str())
                && at.elapsed() < TTL
            {
                return v.clone();
            }
        }
        let fetched = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            trouve_agents::install::latest_version(id),
        )
        .await;
        let latest = match fetched {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::debug!("latest-version check for {} failed: {e}", id.as_str());
                None
            }
            Err(_) => None,
        };
        self.cli_latest.lock().unwrap().insert(
            id.as_str().into(),
            (std::time::Instant::now(), latest.clone()),
        );
        latest
    }

    /// Start downloading the newest build of a vendor CLI into trouve's
    /// managed directory. Progress is reported by `cli_install_status`; on
    /// success the backend registry reloads so new turns use the new binary.
    pub fn start_cli_install(self: &Arc<Self>, id: &str) -> Result<(), EngineError> {
        let cli = trouve_agents::install::CliId::parse(id)
            .ok_or_else(|| EngineError::NotFound(format!("cli {id}")))?;
        let progress = Arc::new(trouve_agents::install::Progress::default());
        {
            let mut installs = self.cli_installs.lock().unwrap();
            if matches!(installs.get(id), Some(CliInstallState::Pending { .. })) {
                return Err(EngineError::Conflict(format!(
                    "an install for {id} is already in progress"
                )));
            }
            installs.insert(
                id.to_string(),
                CliInstallState::Pending {
                    version: None,
                    progress: progress.clone(),
                },
            );
        }
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let result = async {
                let version = trouve_agents::install::latest_version(cli)
                    .await
                    .map_err(|e| e.to_string())?;
                engine.cli_installs.lock().unwrap().insert(
                    id_owned.clone(),
                    CliInstallState::Pending {
                        version: Some(version.clone()),
                        progress: progress.clone(),
                    },
                );
                match trouve_agents::install::install(&engine.data_dir, cli, &version, &progress)
                    .await
                {
                    Ok(_) => Ok(Some(version)),
                    Err(trouve_agents::install::InstallError::Cancelled) => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            }
            .await;
            let mut installs = engine.cli_installs.lock().unwrap();
            match result {
                Ok(Some(version)) => {
                    // The managed binary now exists; rebuild backends so it
                    // takes over from any PATH resolution.
                    engine.reload_providers();
                    engine.cli_latest.lock().unwrap().remove(id_owned.as_str());
                    installs.insert(id_owned, CliInstallState::Success(version));
                }
                // Cancelled: back to "none", like it never started.
                Ok(None) => {
                    installs.remove(&id_owned);
                }
                Err(e) => {
                    installs.insert(id_owned, CliInstallState::Failed(e));
                }
            }
        });
        Ok(())
    }

    /// Ask an in-flight install started with `start_cli_install` to stop.
    /// The task notices at its next chunk and clears the install state.
    pub fn cancel_cli_install(&self, id: &str) -> Result<(), EngineError> {
        match self.cli_installs.lock().unwrap().get(id) {
            Some(CliInstallState::Pending { progress, .. }) => {
                progress
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            _ => Err(EngineError::NotFound(format!(
                "no install for {id} is in progress"
            ))),
        }
    }

    /// Remove the managed install of a CLI (PATH installs are untouched).
    /// For llama-server the sidecar is stopped first.
    pub async fn uninstall_cli(&self, id: &str) -> Result<(), EngineError> {
        let cli = trouve_agents::install::CliId::parse(id)
            .ok_or_else(|| EngineError::NotFound(format!("cli {id}")))?;
        {
            let installs = self.cli_installs.lock().unwrap();
            if matches!(installs.get(id), Some(CliInstallState::Pending { .. })) {
                return Err(EngineError::Conflict(format!(
                    "an install for {id} is in progress — cancel it first"
                )));
            }
        }
        if cli == trouve_agents::install::CliId::LlamaServer {
            self.local_manager.stop().await;
        }
        trouve_agents::install::uninstall(&self.data_dir, cli)
            .map_err(|e| EngineError::Internal(e.into()))?;
        // Drop any stale success/failed state so status reads "none", and
        // rebuild backends so they fall back to PATH resolution (or none).
        self.cli_installs.lock().unwrap().remove(id);
        self.reload_providers();
        Ok(())
    }

    /// Report the state of an install started with `start_cli_install`.
    pub fn cli_install_status(&self, id: &str) -> trouve_protocol::CliInstallStatus {
        match self.cli_installs.lock().unwrap().get(id) {
            None => trouve_protocol::CliInstallStatus {
                status: "none".into(),
                version: None,
                error: None,
                received_bytes: 0,
                total_bytes: 0,
            },
            Some(CliInstallState::Pending { version, progress }) => {
                use std::sync::atomic::Ordering::Relaxed;
                trouve_protocol::CliInstallStatus {
                    status: "pending".into(),
                    version: version.clone(),
                    error: None,
                    received_bytes: progress.received.load(Relaxed),
                    total_bytes: progress.total.load(Relaxed),
                }
            }
            Some(CliInstallState::Success(version)) => trouve_protocol::CliInstallStatus {
                status: "success".into(),
                version: Some(version.clone()),
                error: None,
                received_bytes: 0,
                total_bytes: 0,
            },
            Some(CliInstallState::Failed(e)) => trouve_protocol::CliInstallStatus {
                status: "failed".into(),
                version: None,
                error: Some(e.clone()),
                received_bytes: 0,
                total_bytes: 0,
            },
        }
    }

    // --- automations ------------------------------------------------------------

    /// All automations, in creation order.
    pub fn list_automations(&self) -> Result<Vec<trouve_protocol::Automation>, EngineError> {
        Ok(self.store.list_automations()?)
    }

    pub fn create_automation(
        &self,
        req: trouve_protocol::UpsertAutomationRequest,
    ) -> Result<trouve_protocol::Automation, EngineError> {
        self.validate_automation(&req)?;
        let next_run_at = if req.enabled {
            crate::automations::next_run(&req.schedule, chrono::Local::now())
                .map(|t| t.to_rfc3339())
        } else {
            None
        };
        let automation = trouve_protocol::Automation {
            id: new_id("auto"),
            name: req.name,
            prompt: req.prompt,
            workspace_id: req.workspace_id,
            mode: req.mode,
            model: req.model,
            permission_mode: req.permission_mode,
            schedule: req.schedule,
            enabled: req.enabled,
            next_run_at,
            last_run_at: None,
            last_session_id: None,
            last_error: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.store.insert_automation(&automation)?;
        Ok(automation)
    }

    pub fn update_automation(
        &self,
        id: &str,
        req: trouve_protocol::UpsertAutomationRequest,
    ) -> Result<trouve_protocol::Automation, EngineError> {
        self.validate_automation(&req)?;
        let mut automation = self
            .store
            .automation(id)?
            .ok_or_else(|| EngineError::NotFound(format!("automation {id}")))?;
        automation.name = req.name;
        automation.prompt = req.prompt;
        automation.workspace_id = req.workspace_id;
        automation.mode = req.mode;
        automation.model = req.model;
        automation.permission_mode = req.permission_mode;
        automation.schedule = req.schedule;
        automation.enabled = req.enabled;
        automation.next_run_at = if req.enabled {
            crate::automations::next_run(&automation.schedule, chrono::Local::now())
                .map(|t| t.to_rfc3339())
        } else {
            None
        };
        self.store.update_automation(&automation)?;
        Ok(automation)
    }

    pub fn delete_automation(&self, id: &str) -> Result<(), EngineError> {
        if !self.store.delete_automation(id)? {
            return Err(EngineError::NotFound(format!("automation {id}")));
        }
        Ok(())
    }

    fn validate_automation(
        &self,
        req: &trouve_protocol::UpsertAutomationRequest,
    ) -> Result<(), EngineError> {
        if req.name.trim().is_empty() {
            return Err(EngineError::BadRequest("automations need a name".into()));
        }
        if req.prompt.trim().is_empty() {
            return Err(EngineError::BadRequest("automations need a prompt".into()));
        }
        if self.store.open_workspace(&req.workspace_id)?.is_none() {
            return Err(EngineError::NotFound(format!(
                "workspace {}",
                req.workspace_id
            )));
        }
        if let Some(complaint) = crate::automations::validate(&req.schedule) {
            return Err(EngineError::BadRequest(complaint));
        }
        Ok(())
    }

    /// Fire an automation immediately, in the background (creating the
    /// worktree takes a moment). The outcome lands in `last_*` and an
    /// `automation.fired` event, same as a scheduled run.
    pub fn run_automation_now(self: &Arc<Self>, id: &str) -> Result<(), EngineError> {
        let automation = self
            .store
            .automation(id)?
            .ok_or_else(|| EngineError::NotFound(format!("automation {id}")))?;
        let engine = self.clone();
        tokio::spawn(async move {
            engine.fire_and_record(&automation).await;
        });
        Ok(())
    }

    /// Start the background scheduler (called once when serving). Runs
    /// missed while the server was down are skipped — every enabled
    /// automation's next fire is recomputed from "now" at startup.
    pub fn start_automation_scheduler(self: &Arc<Self>) {
        let engine = self.clone();
        tokio::spawn(async move {
            let now = chrono::Local::now();
            if let Ok(automations) = engine.store.list_automations() {
                for a in automations {
                    let next = a
                        .enabled
                        .then(|| crate::automations::next_run(&a.schedule, now))
                        .flatten()
                        .map(|t| t.to_rfc3339());
                    let _ = engine.store.set_automation_next_run(&a.id, next.as_deref());
                }
            }
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                engine.fire_due_automations().await;
            }
        });
    }

    async fn fire_due_automations(self: &Arc<Self>) {
        let Ok(automations) = self.store.list_automations() else {
            return;
        };
        let now = chrono::Utc::now();
        for automation in automations {
            if !automation.enabled {
                continue;
            }
            // Closing a workspace pauses its scheduled activity without
            // deleting or disabling the persisted automation. Reopening the
            // workspace makes it eligible on a later scheduler tick.
            if self
                .store
                .open_workspace(&automation.workspace_id)
                .ok()
                .flatten()
                .is_none()
            {
                continue;
            }
            let due = automation
                .next_run_at
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|next| next <= now);
            if due {
                self.fire_and_record(&automation).await;
            }
        }
    }

    /// One run: session + thread + prompt, then bookkeeping and the
    /// server-scope event clients refresh on.
    async fn fire_and_record(self: &Arc<Self>, automation: &trouve_protocol::Automation) {
        let next = crate::automations::next_run(&automation.schedule, chrono::Local::now())
            .filter(|_| automation.enabled)
            .map(|t| t.to_rfc3339());
        let ran_at = chrono::Utc::now().to_rfc3339();
        match self.fire_automation(automation).await {
            Ok((session_id, thread_id, turn)) => {
                // Advance the schedule as soon as dispatch succeeds so a
                // long-running or approval-blocked turn cannot fire again on
                // the next scheduler tick. The completion watcher records
                // the actual outcome and only then emits automation.fired.
                let _ = self.store.mark_automation_run(
                    &automation.id,
                    &ran_at,
                    Some(&session_id),
                    "",
                    next.as_deref(),
                );
                let engine = self.clone();
                let automation_id = automation.id.clone();
                let automation_name = automation.name.clone();
                tokio::spawn(async move {
                    engine
                        .monitor_automation_turn(
                            automation_id,
                            automation_name,
                            session_id,
                            thread_id,
                            turn,
                        )
                        .await;
                });
            }
            Err(e) => {
                let error = e.to_string();
                let _ = self.store.mark_automation_run(
                    &automation.id,
                    &ran_at,
                    None,
                    &error,
                    next.as_deref(),
                );
                tracing::warn!("automation {} failed: {error}", automation.name);
                let _ = self.store.append_event(
                    Scope::Server,
                    Event::AutomationFired {
                        automation_id: automation.id.clone(),
                        session_id: None,
                        error,
                    },
                );
            }
        }
    }

    async fn fire_automation(
        self: &Arc<Self>,
        automation: &trouve_protocol::Automation,
    ) -> Result<(String, String, u64), EngineError> {
        let session = self
            .create_session(trouve_protocol::CreateSessionRequest {
                workspace_id: automation.workspace_id.clone(),
                title: Some(format!(
                    "{} — {}",
                    automation.name,
                    chrono::Local::now().format("%b %d %H:%M")
                )),
                base_ref: None,
                checkout_ref: None,
                fetch_latest: true,
            })
            .await?;
        let thread = self.create_thread(trouve_protocol::CreateThreadRequest {
            session_id: session.id.clone(),
            mode: automation.mode.clone(),
            model: automation.model.clone(),
            model_options: Default::default(),
            // Scoped to this fresh run session; it does not change global
            // mode defaults or carry approvals into future runs.
            permission_mode: Some(automation.permission_mode),
        })?;
        let accepted = self.send_message(&thread.id, automation.prompt.clone(), Vec::new())?;
        if accepted.queued || accepted.turn == 0 {
            return Err(EngineError::Conflict(format!(
                "automation thread {} did not dispatch",
                thread.id
            )));
        }
        Ok((session.id, thread.id, accepted.turn))
    }

    async fn monitor_automation_turn(
        self: &Arc<Self>,
        automation_id: String,
        automation_name: String,
        session_id: String,
        thread_id: String,
        turn: u64,
    ) {
        let scope = Scope::Thread(thread_id);
        let mut live = self.store.subscribe();
        let mut after = 0u64;
        let mut replay = std::collections::VecDeque::from(
            self.store.events_after(&scope, after).unwrap_or_default(),
        );
        let error = loop {
            let envelope = match replay.pop_front() {
                Some(envelope) => envelope,
                None => match live.recv().await {
                    Ok(envelope) => envelope,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        replay = std::collections::VecDeque::from(
                            self.store.events_after(&scope, after).unwrap_or_default(),
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break "event stream closed before automation completed".to_string();
                    }
                },
            };
            if envelope.scope != scope || envelope.cursor <= after {
                continue;
            }
            after = envelope.cursor;
            match envelope.event {
                Event::ApprovalRequested {
                    turn: event_turn, ..
                } if event_turn == turn => {
                    // Ask remains the safe default for scheduled work; make
                    // its blocked state explicit instead of reporting a
                    // successful automation before the user responds.
                    let _ = self
                        .store
                        .set_automation_result(&automation_id, "awaiting approval");
                }
                Event::ApprovalResolved { .. } => {
                    let _ = self.store.set_automation_result(&automation_id, "");
                }
                Event::TurnCompleted {
                    turn: event_turn, ..
                } if event_turn == turn => break String::new(),
                Event::TurnFailed {
                    turn: event_turn,
                    error,
                } if event_turn == turn => break error,
                Event::TurnCancelled { turn: event_turn } if event_turn == turn => {
                    break "turn cancelled".to_string();
                }
                _ => {}
            }
        };

        let _ = self.store.set_automation_result(&automation_id, &error);
        if !error.is_empty() {
            tracing::warn!("automation {automation_name} failed: {error}");
        }
        let _ = self.store.append_event(
            Scope::Server,
            Event::AutomationFired {
                automation_id,
                session_id: Some(session_id),
                error,
            },
        );
    }

    // --- local models ---------------------------------------------------------

    /// The hardware probe result, run once (off the async runtime) and
    /// cached for the engine's lifetime.
    async fn hardware(&self) -> crate::local::Hardware {
        if self.hardware.get().is_none() {
            let hw = tokio::task::spawn_blocking(crate::local::probe_hardware)
                .await
                .unwrap_or_default();
            let _ = self.hardware.set(hw);
        }
        self.hardware.get().cloned().unwrap_or_default()
    }

    /// Local inference status for the settings screen: hardware, runtime
    /// install state, the running sidecar, and every model with its
    /// download/fit state.
    pub async fn local_status(&self) -> trouve_protocol::LocalStatus {
        use trouve_agents::install as cli;
        let hw = self.hardware().await;
        let managed = cli::installed(&self.data_dir, cli::CliId::LlamaServer);
        let (runtime_installed, runtime_version, runtime_managed) = match &managed {
            Some(info) => (true, Some(info.version.clone()), true),
            None => match cli::find_on_path("llama-server") {
                Some(_) => (true, Some("system".into()), false),
                None => (false, None, false),
            },
        };
        let runtime_latest_version = self.cli_latest_version(cli::CliId::LlamaServer).await;
        // Only a managed install is ours to update; system builds belong
        // to the user's package manager.
        let runtime_update_available = match (&managed, &runtime_latest_version) {
            (Some(info), Some(latest)) => !cli_version_matches(&info.version, latest),
            _ => false,
        };
        let (running_model, server_status) = match self.local_manager.state() {
            crate::local::ServerState::Stopped => (None, "stopped".to_string()),
            crate::local::ServerState::Starting(m) => (Some(m), "starting".to_string()),
            crate::local::ServerState::Running(m) => (Some(m), "running".to_string()),
        };
        let enabled = self.config.lock().unwrap().local_enabled.unwrap_or(true);
        let downloads = self.local_downloads.lock().unwrap().clone();
        let models = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .map(|entry| {
                let downloaded = crate::local::gguf_path(&self.data_dir, &entry).exists();
                let (download_status, download_bytes, download_error) =
                    match downloads.get(&entry.id) {
                        Some(LocalDownloadState::Pending { bytes, .. }) => (
                            "pending".to_string(),
                            bytes.load(std::sync::atomic::Ordering::Relaxed),
                            String::new(),
                        ),
                        Some(LocalDownloadState::Failed(e)) => ("failed".to_string(), 0, e.clone()),
                        None => ("none".to_string(), 0, String::new()),
                    };
                trouve_protocol::LocalModelInfo {
                    id: entry.id.clone(),
                    display_name: entry.display_name.clone(),
                    repo: entry.repo.clone(),
                    file: entry.file.clone(),
                    size_bytes: entry.size_bytes,
                    params: entry.params.clone(),
                    context_window: crate::local::SERVE_CONTEXT,
                    fit: crate::local::fit(entry.size_bytes, &hw).to_string(),
                    notes: entry.notes.clone(),
                    downloaded,
                    download_status,
                    download_bytes,
                    download_error,
                    custom: entry.custom,
                }
            })
            .collect();
        trouve_protocol::LocalStatus {
            enabled,
            ram_bytes: hw.ram_bytes,
            gpus: hw.gpus,
            runtime_installed,
            runtime_version,
            runtime_managed,
            runtime_latest_version,
            runtime_update_available,
            running_model,
            server_status,
            models,
        }
    }

    /// Turn the built-in "local" provider on or off. Disabling stops the
    /// llama-server sidecar and removes the provider (its models disappear
    /// from pickers); enabling re-registers it. Persisted in config.toml.
    pub async fn set_local_enabled(&self, enabled: bool) -> Result<(), EngineError> {
        {
            let mut config = self.config.lock().unwrap();
            if config.local_enabled.unwrap_or(true) == enabled {
                return Ok(());
            }
            config.local_enabled = Some(enabled);
            self.persist_config(&config);
        }
        if enabled {
            self.injected_providers
                .lock()
                .unwrap()
                .insert("local".into(), self.local_provider.clone());
            self.providers
                .write()
                .unwrap()
                .insert("local".into(), self.local_provider.clone());
        } else {
            self.injected_providers.lock().unwrap().remove("local");
            self.providers.write().unwrap().remove("local");
            self.local_manager.stop().await;
        }
        Ok(())
    }

    /// Start downloading one model's GGUF from HuggingFace into the data
    /// dir. Progress is visible through `local_status`.
    pub fn start_local_model_download(self: &Arc<Self>, id: &str) -> Result<(), EngineError> {
        let entry = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| EngineError::NotFound(format!("local model {id}")))?;
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut downloads = self.local_downloads.lock().unwrap();
            if matches!(downloads.get(id), Some(LocalDownloadState::Pending { .. })) {
                return Err(EngineError::Conflict(format!(
                    "a download for {id} is already in progress"
                )));
            }
            downloads.insert(
                id.to_string(),
                LocalDownloadState::Pending {
                    bytes: counter.clone(),
                    cancel: cancel.clone(),
                },
            );
        }
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let result = download_gguf(&engine.data_dir, &entry, &counter, &cancel).await;
            let mut downloads = engine.local_downloads.lock().unwrap();
            match result {
                // Downloaded state comes from the file's existence;
                // cancelled downloads also just clear (status "none").
                Ok(_) => {
                    downloads.remove(&id_owned);
                }
                Err(e) => {
                    downloads.insert(id_owned, LocalDownloadState::Failed(format!("{e:#}")));
                }
            }
        });
        Ok(())
    }

    /// Ask an in-flight model download to stop; its partial file is
    /// deleted and the model returns to the not-downloaded state.
    pub fn cancel_local_model_download(&self, id: &str) -> Result<(), EngineError> {
        match self.local_downloads.lock().unwrap().get(id) {
            Some(LocalDownloadState::Pending { cancel, .. }) => {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            _ => Err(EngineError::NotFound(format!(
                "no download for {id} is in progress"
            ))),
        }
    }

    /// Register a custom GGUF (HuggingFace repo + filename), validating
    /// that the file exists and reading its size.
    pub async fn add_local_model(
        &self,
        req: trouve_protocol::AddLocalModelRequest,
    ) -> Result<(), EngineError> {
        let config_dir = self
            .config_dir
            .clone()
            .ok_or_else(|| EngineError::Internal(anyhow::anyhow!("no config dir")))?;
        let repo = req.repo.trim().trim_matches('/').to_string();
        let file = req.file.trim().trim_start_matches('/').to_string();
        if repo.is_empty() || !repo.contains('/') || file.is_empty() {
            return Err(EngineError::BadRequest(
                "expected a HuggingFace repo like owner/name and a .gguf filename".into(),
            ));
        }
        if !file.ends_with(".gguf") {
            return Err(EngineError::BadRequest("the file must be a .gguf".into()));
        }
        let id = crate::local::slug_from_file(&file);
        if crate::local::all_entries(Some(&config_dir))
            .iter()
            .any(|e| e.id == id)
        {
            return Err(EngineError::Conflict(format!(
                "a local model with id {id} already exists"
            )));
        }
        // Validate against HF and learn the size for the fit label. Don't
        // follow the CDN redirect: the size lives in `x-linked-size` on the
        // resolve response itself, and a redirect already proves existence.
        let url = crate::local::download_url(&repo, &file);
        let resp = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| EngineError::Internal(e.into()))?
            .head(&url)
            .send()
            .await
            .map_err(|e| EngineError::BadRequest(format!("checking {repo}/{file}: {e}")))?;
        if !resp.status().is_success() && !resp.status().is_redirection() {
            return Err(EngineError::BadRequest(format!(
                "HuggingFace returned {} for {repo}/{file} — check the repo and filename \
                 (gated repos are not supported)",
                resp.status()
            )));
        }
        let size_bytes = resp
            .headers()
            .get("x-linked-size")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .or_else(|| resp.content_length().filter(|n| *n > 0))
            .unwrap_or(0);
        let display_name = req
            .display_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let path = crate::local::custom_models_path(&config_dir);
        let mut models = crate::local::read_custom_models(&path);
        models.push(crate::local::CustomModel {
            id,
            display_name,
            repo,
            file,
            size_bytes,
        });
        crate::local::write_custom_models(&path, &models)
            .map_err(|e| EngineError::Internal(e.into()))?;
        Ok(())
    }

    /// Search HuggingFace for GGUF repos matching `query`, listing each
    /// repo's single-file GGUFs with hardware-fit guidance and a
    /// recommended pick for this machine. Repos without usable files (or
    /// whose file listing fails) are dropped.
    pub async fn search_local_models(
        &self,
        query: &str,
    ) -> Result<Vec<trouve_protocol::LocalSearchResult>, EngineError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let hw = self.hardware().await;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .map_err(|e| EngineError::Internal(e.into()))?;
        let repos = crate::local::search_hf_repos(&client, query, 8)
            .await
            .map_err(|e| EngineError::BadRequest(format!("HuggingFace search failed: {e}")))?;
        // (repo, file) pairs already in the model list, to mark "added".
        let existing: std::collections::HashSet<(String, String)> =
            crate::local::all_entries(self.config_dir.as_deref())
                .into_iter()
                .map(|e| (e.repo.to_ascii_lowercase(), e.file.to_ascii_lowercase()))
                .collect();

        let lookups = repos.into_iter().map(|repo| {
            let client = client.clone();
            async move {
                let files = crate::local::list_gguf_files(&client, &repo.id)
                    .await
                    .ok()?;
                Some((repo, files))
            }
        });
        let mut results = Vec::new();
        for looked_up in futures::future::join_all(lookups).await {
            let Some((repo, mut files)) = looked_up else {
                continue;
            };
            if files.is_empty() {
                continue;
            }
            files.sort_by_key(|(_, size)| *size);
            let files: Vec<trouve_protocol::LocalSearchFile> = files
                .into_iter()
                .map(|(file, size_bytes)| trouve_protocol::LocalSearchFile {
                    quant: crate::local::quant_of(&file),
                    fit: crate::local::fit(size_bytes, &hw).to_string(),
                    added: existing
                        .contains(&(repo.id.to_ascii_lowercase(), file.to_ascii_lowercase())),
                    file,
                    size_bytes,
                })
                .collect();
            let recommended = recommend_gguf(&files) as u32;
            results.push(trouve_protocol::LocalSearchResult {
                repo: repo.id,
                downloads: repo.downloads,
                likes: repo.likes,
                files,
                recommended,
            });
        }
        Ok(results)
    }

    /// Delete a model's downloaded GGUF (stopping the server if it is the
    /// one loaded); custom entries are removed from the list entirely.
    pub async fn delete_local_model(&self, id: &str) -> Result<(), EngineError> {
        let entry = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| EngineError::NotFound(format!("local model {id}")))?;
        if self.local_manager.running_model().as_deref() == Some(id) {
            self.local_manager.stop().await;
        }
        self.local_downloads.lock().unwrap().remove(id);
        let gguf = crate::local::gguf_path(&self.data_dir, &entry);
        let _ = std::fs::remove_file(gguf.with_extension("gguf.part"));
        if gguf.exists() {
            std::fs::remove_file(&gguf).map_err(|e| EngineError::Internal(e.into()))?;
        }
        if entry.custom
            && let Some(config_dir) = &self.config_dir
        {
            let path = crate::local::custom_models_path(config_dir);
            let mut models = crate::local::read_custom_models(&path);
            models.retain(|m| m.id != id);
            crate::local::write_custom_models(&path, &models)
                .map_err(|e| EngineError::Internal(e.into()))?;
        }
        Ok(())
    }

    /// Stop the llama-server sidecar (frees the model's RAM/VRAM; the next
    /// local turn restarts it).
    pub async fn stop_local_server(&self) {
        self.local_manager.stop().await;
    }

    /// Restart the llama-server sidecar with the model it is serving. The
    /// reload happens in the background (large GGUFs take a while);
    /// progress shows in `local_status` as server_status "starting".
    pub async fn restart_local_server(&self) -> Result<(), EngineError> {
        let model = self
            .local_manager
            .running_model()
            .ok_or_else(|| EngineError::Conflict("no local server is running".into()))?;
        let entry = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == model)
            .ok_or_else(|| EngineError::NotFound(format!("local model {model}")))?;
        let bin = crate::local::runtime_bin(&self.data_dir).ok_or_else(|| {
            EngineError::Conflict("the llama.cpp runtime is not installed".into())
        })?;
        let gguf = crate::local::gguf_path(&self.data_dir, &entry);
        let log_path = self.data_dir.join("llama-server.log");
        self.local_manager.stop().await;
        let manager = self.local_manager.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.ensure(&bin, &entry.id, &gguf, &log_path).await {
                tracing::warn!("llama-server restart failed: {e:#}");
            }
        });
        Ok(())
    }

    /// Report the state of an OAuth login started with `start_login`.
    pub fn login_status(&self, id: &str) -> trouve_protocol::LoginStatus {
        match self.logins.lock().unwrap().get(id) {
            None => trouve_protocol::LoginStatus {
                status: "none".into(),
                error: None,
            },
            Some(LoginState::Pending { .. }) => trouve_protocol::LoginStatus {
                status: "pending".into(),
                error: None,
            },
            Some(LoginState::Success) => trouve_protocol::LoginStatus {
                status: "success".into(),
                error: None,
            },
            Some(LoginState::Failed(e)) => trouve_protocol::LoginStatus {
                status: "failed".into(),
                error: Some(e.clone()),
            },
        }
    }

    /// Forward a browser authentication response to an interactive vendor CLI.
    pub async fn complete_login(
        &self,
        id: &str,
        request: trouve_protocol::CompleteLoginRequest,
    ) -> Result<trouve_protocol::LoginStatus, EngineError> {
        let callback = request.callback_url.trim();
        if callback.is_empty() {
            return Err(EngineError::BadRequest(
                "authentication response must not be empty".into(),
            ));
        }
        if callback.len() > 16 * 1024 || callback.chars().any(char::is_control) {
            return Err(EngineError::BadRequest(
                "authentication response is too long or contains control characters".into(),
            ));
        }

        let sender = {
            let mut logins = self.logins.lock().unwrap();
            match logins.get_mut(id) {
                Some(LoginState::Pending {
                    callback_sender, ..
                }) => callback_sender.take().ok_or_else(|| {
                    EngineError::Conflict(format!(
                        "provider {id} login does not accept an authentication response"
                    ))
                })?,
                Some(LoginState::Success) => {
                    return Ok(trouve_protocol::LoginStatus {
                        status: "success".into(),
                        error: None,
                    });
                }
                Some(LoginState::Failed(error)) => {
                    return Err(EngineError::Conflict(format!(
                        "provider {id} login already failed: {error}"
                    )));
                }
                None => {
                    return Err(EngineError::NotFound(format!(
                        "no login is running for provider {id}"
                    )));
                }
            }
        };
        sender.send(callback.to_string()).await.map_err(|_| {
            EngineError::Conflict(format!("provider {id} login is no longer accepting input"))
        })?;
        Ok(self.login_status(id))
    }

    fn finish_login(
        &self,
        id: &str,
        result: Result<trouve_providers::auth::OAuthTokens, trouve_providers::ProviderError>,
    ) {
        let state = match result {
            Ok(tokens) => match serde_json::to_string(&tokens)
                .map_err(anyhow::Error::from)
                .and_then(|raw| {
                    self.secrets
                        .set(&trouve_providers::secrets::oauth_secret(id), &raw)
                }) {
                Ok(()) => {
                    self.reload_providers();
                    LoginState::Success
                }
                Err(e) => LoginState::Failed(format!("storing tokens: {e}")),
            },
            Err(e) => LoginState::Failed(e.to_string()),
        };
        self.logins.lock().unwrap().insert(id.to_string(), state);
    }

    /// Set the default model for new threads (provider-qualified).
    pub fn set_default_model(
        &self,
        model: &str,
        thinking_level: Option<&str>,
    ) -> Result<(), EngineError> {
        if !model.contains('/') {
            return Err(EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            )));
        }
        validate_thinking_level(thinking_level)?;
        {
            let mut config = self.config.lock().unwrap();
            config.default_model = Some(model.to_string());
            if let Some(level) = thinking_level {
                config.default_thinking_level = Some(level.into());
            }
            self.persist_config(&config);
        }
        *self.default_model.write().unwrap() = model.to_string();
        if let Some(level) = thinking_level {
            *self.default_thinking_level.write().unwrap() = Some(level.into());
        }
        Ok(())
    }

    /// Set the global default permission mode for new threads (used by
    /// modes that don't set one of their own).
    pub fn set_default_permission_mode(
        &self,
        mode: trouve_protocol::PermissionMode,
    ) -> Result<(), EngineError> {
        {
            let mut config = self.config.lock().unwrap();
            config.default_permission_mode = Some(mode);
            self.persist_config(&config);
        }
        *self.default_permission_mode.write().unwrap() = mode;
        Ok(())
    }

    pub(crate) fn persist_config(&self, config: &Config) {
        if let Some(path) = &self.config_file
            && let Err(e) = config.save_to(path)
        {
            tracing::warn!("failed to persist config: {e}");
        }
    }

    /// Rebuild the provider registry from the current config (after provider
    /// CRUD), preserving programmatically injected providers.
    fn reload_providers(&self) {
        let config = self.config.lock().unwrap().clone();
        let mut rebuilt = build_all_providers(&config, &self.secrets);
        for (id, p) in self.injected_providers.lock().unwrap().iter() {
            rebuilt.insert(id.clone(), p.clone());
        }
        *self.providers.write().unwrap() = rebuilt;
        let mut backends = build_all_backends(&config, &self.secrets, &self.data_dir);
        for (id, b) in self.injected_backends.lock().unwrap().iter() {
            backends.insert(id.clone(), b.clone());
        }
        *self.backends.write().unwrap() = backends;
    }

    pub fn thread_usage(
        &self,
        thread_id: &str,
    ) -> Result<trouve_protocol::UsageSummary, EngineError> {
        self.get_thread(thread_id)?;
        Ok(self
            .store
            .usage_summary(crate::store::UsageScope::Thread(thread_id))?)
    }

    pub fn session_usage(
        &self,
        session_id: &str,
    ) -> Result<trouve_protocol::UsageSummary, EngineError> {
        self.get_session(session_id)?;
        Ok(self
            .store
            .usage_summary(crate::store::UsageScope::Session(session_id))?)
    }

    /// Agent modes visible for a workspace (built-ins + config + `.agents`).
    pub fn list_modes(&self, workspace_id: Option<&str>) -> Result<Vec<AgentMode>, EngineError> {
        let root = match workspace_id {
            Some(id) => {
                let ws = self
                    .store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
                Some(PathBuf::from(ws.path))
            }
            None => None,
        };
        Ok(modes::resolve_modes(
            self.config_dir.as_deref(),
            root.as_deref(),
        ))
    }

    /// Modes with provenance (builtin / customized / custom / workspace)
    /// for the settings screen.
    pub fn list_mode_infos(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<trouve_protocol::ModeInfo>, EngineError> {
        let root = match workspace_id {
            Some(id) => {
                let ws = self
                    .store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
                Some(PathBuf::from(ws.path))
            }
            None => None,
        };
        Ok(modes::resolve_mode_infos(
            self.config_dir.as_deref(),
            root.as_deref(),
        ))
    }

    /// Create or update a user-level mode. Saving under a built-in id
    /// customizes that built-in; the file lands in `<config>/modes/`.
    pub fn upsert_mode(
        &self,
        id: &str,
        req: trouve_protocol::UpsertModeRequest,
    ) -> Result<(), EngineError> {
        let config_dir = self
            .config_dir
            .as_deref()
            .ok_or_else(|| EngineError::BadRequest("no config dir".into()))?;
        if let Some(model) = req.default_model.as_deref()
            && !model.contains('/')
        {
            return Err(EngineError::BadRequest(format!(
                "default_model must be provider-qualified (\"provider/model\"), got {model}"
            )));
        }
        validate_thinking_level(req.default_thinking_level.as_deref())?;
        let mode = AgentMode {
            id: id.to_string(),
            display_name: req.display_name,
            system_prompt: req.system_prompt,
            allowed_tools: req.allowed_tools,
            read_only: req.read_only,
            default_permission_mode: req.default_permission_mode,
            default_model: req.default_model,
            default_thinking_level: req.default_thinking_level,
        };
        modes::upsert_user_mode(config_dir, &mode)
            .map_err(|e| EngineError::BadRequest(format!("{e:#}")))
    }

    /// Remove a user-level mode file: deletes a custom mode, or resets a
    /// customized built-in to its defaults.
    pub fn delete_mode(&self, id: &str) -> Result<(), EngineError> {
        let config_dir = self
            .config_dir
            .as_deref()
            .ok_or_else(|| EngineError::BadRequest("no config dir".into()))?;
        modes::delete_user_mode(config_dir, id)
            .map_err(|e| EngineError::BadRequest(format!("{e:#}")))
    }

    /// GitHub repository named by the session's origin remote. Routes to
    /// github.com or a configured GitHub Enterprise host based on the URL.
    fn github_repository_for_session(
        &self,
        session: &trouve_protocol::Session,
    ) -> Result<(String, String, String), EngineError> {
        self.github_repository_for_checkout(&PathBuf::from(&session.worktree_path))
    }

    /// GitHub repository named by any checkout's origin remote.
    fn github_repository_for_checkout(
        &self,
        checkout: &Path,
    ) -> Result<(String, String, String), EngineError> {
        let url = git::remote_url(checkout, "origin")
            .ok_or_else(|| EngineError::BadRequest("workspace has no 'origin' remote".into()))?;
        let (host, owner, repo) = crate::github::parse_remote(&url).ok_or_else(|| {
            EngineError::BadRequest(format!("origin is not a GitHub-style remote: {url}"))
        })?;
        if !self.github_hosts().iter().any(|(h, _)| *h == host) {
            return Err(EngineError::BadRequest(format!(
                "origin remote is on {host}, which isn't github.com or a configured \
                 GitHub Enterprise host — add it in Settings → Integrations"
            )));
        }
        Ok((host, owner, repo))
    }

    /// Authenticated GitHub client for the session's origin repository.
    fn github_for_session(
        &self,
        session: &trouve_protocol::Session,
    ) -> Result<crate::github::GitHub, EngineError> {
        self.github_for_checkout(&PathBuf::from(&session.worktree_path))
    }

    /// Authenticated GitHub client for any checkout's origin repository.
    fn github_for_checkout(&self, checkout: &Path) -> Result<crate::github::GitHub, EngineError> {
        let (host, owner, repo) = self.github_repository_for_checkout(checkout)?;
        let token = self.github_token(&host).ok_or_else(|| {
            EngineError::BadRequest(format!(
                "no GitHub OAuth session for {host}; sign in under Settings → Integrations"
            ))
        })?;
        crate::github::GitHub::new(&token, &host, &owner, &repo).map_err(EngineError::Internal)
    }

    /// Every GitHub host the integration knows: github.com first (always),
    /// then the configured enterprise hosts, each with its optional OAuth
    /// app client id.
    fn github_hosts(&self) -> Vec<(String, Option<String>)> {
        let config = self.config.lock().unwrap();
        let mut hosts = vec![(
            crate::github::GITHUB_COM.to_string(),
            // github.com always has an OAuth path: the built-in shared app,
            // unless config overrides it with the user's own client id.
            config
                .github_client_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .or_else(|| Some(crate::github::DEFAULT_CLIENT_ID.to_string())),
        )];
        for e in &config.github_enterprise {
            hosts.push((
                e.host.clone(),
                e.client_id.clone().filter(|id| !id.trim().is_empty()),
            ));
        }
        hosts
    }

    /// Secret-store / login id for a GitHub host. github.com keeps the
    /// plain "github" id (pre-enterprise secrets stay valid); enterprise
    /// hosts get "github:<host>".
    fn github_secret_id(host: &str) -> String {
        if host == crate::github::GITHUB_COM {
            "github".to_string()
        } else {
            format!("github:{host}")
        }
    }

    /// The OAuth access token for a host. GitHub authentication deliberately
    /// has one integration point: the device-flow secret.
    fn github_token(&self, host: &str) -> Option<String> {
        let id = Self::github_secret_id(host);
        if let Ok(Some(raw)) = self
            .secrets
            .get(&trouve_providers::secrets::oauth_secret(&id))
        {
            // Device-flow tokens from classic OAuth apps don't expire; apps
            // configured with expiring tokens just need a fresh sign-in.
            if let Ok(tokens) = serde_json::from_str::<trouve_providers::auth::OAuthTokens>(&raw) {
                return Some(tokens.access_token);
            }
        }
        None
    }

    /// Append durable links for PR numbers not already recorded for a session.
    fn record_session_pr_numbers(
        &self,
        session_id: &str,
        repository: &(String, String, String),
        numbers: impl IntoIterator<Item = u64>,
        recorded: &mut HashSet<u64>,
    ) -> Result<(), EngineError> {
        let (host, owner, repo) = repository;
        for number in numbers {
            if recorded.insert(number) {
                self.store.append_event(
                    Scope::Session(session_id.to_string()),
                    Event::SessionPrOpened {
                        number,
                        url: crate::github::pr_url(host, owner, repo, number),
                    },
                )?;
            }
        }
        Ok(())
    }

    /// PR numbers already linked through persisted session events.
    fn recorded_session_pr_numbers(&self, session_id: &str) -> Result<HashSet<u64>, EngineError> {
        Ok(self
            .store
            .events_after(&Scope::Session(session_id.to_string()), 0)?
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::SessionPrOpened { number, .. } => Some(number),
                _ => None,
            })
            .collect())
    }

    /// Provider-neutral evidence tying GitHub activity to this session.
    /// Explicit PR references work for any integration; successful tool args
    /// and produced commit IDs preserve enough identity to discover a PR that
    /// the user creates later in GitHub's UI.
    fn session_pr_evidence(
        &self,
        session_id: &str,
        host: &str,
        owner: &str,
        repo: &str,
    ) -> Result<SessionPrEvidence, EngineError> {
        let mut evidence = SessionPrEvidence::default();
        for envelope in self
            .store
            .events_after(&Scope::Session(session_id.to_string()), 0)?
        {
            match envelope.event {
                Event::SessionPrOpened { number, .. } => {
                    evidence.numbers.insert(number);
                    evidence.recorded_numbers.insert(number);
                }
                Event::CheckpointCreated { commit, .. } => {
                    evidence.commit_ids.insert(commit.to_ascii_lowercase());
                }
                _ => {}
            }
        }

        for thread in self.store.list_threads(session_id)? {
            let events = self.store.events_after(&Scope::Thread(thread.id), 0)?;
            evidence.extend(pr_evidence_from_events(
                events.into_iter().map(|envelope| envelope.event),
                host,
                owner,
                repo,
            ));
        }
        Ok(evidence)
    }

    /// The open PR associated with this session, if one exists.
    pub async fn session_pr(
        &self,
        session_id: &str,
    ) -> Result<Option<trouve_protocol::PrInfo>, EngineError> {
        Ok(self
            .session_prs(session_id)
            .await?
            .into_iter()
            .find(|pr| pr.state == "open"))
    }

    /// Every PR associated with the session (open first, newest first).
    ///
    /// This includes PRs from the worktree branch, explicitly referenced PRs,
    /// and open PRs whose head branch or commit appears in session activity.
    pub async fn session_prs(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::PrInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let repository = self.github_repository_for_session(&session)?;
        let (host, owner, repo) = &repository;
        let github = self.github_for_session(&session)?;
        let mut evidence = self.session_pr_evidence(session_id, host, owner, repo)?;
        let explicit_numbers = evidence.numbers.iter().copied().collect::<Vec<_>>();
        self.record_session_pr_numbers(
            session_id,
            &repository,
            explicit_numbers,
            &mut evidence.recorded_numbers,
        )?;
        let mut prs = github
            .prs_for_branch(&session.branch)
            .await
            .map_err(EngineError::Internal)?;
        let mut seen: HashSet<u64> = prs.iter().map(|pr| pr.number).collect();
        for pr in github
            .open_prs_referenced_by(&evidence.successful_tool_args, &evidence.commit_ids)
            .await
            .map_err(EngineError::Internal)?
        {
            self.record_session_pr_numbers(
                session_id,
                &repository,
                [pr.number],
                &mut evidence.recorded_numbers,
            )?;
            if seen.insert(pr.number) {
                prs.push(pr);
            }
        }
        for number in evidence.numbers {
            if seen.insert(number) {
                match github.pr(number).await {
                    Ok(pr) => prs.push(pr),
                    Err(error) => tracing::warn!(
                        session_id,
                        pr_number = number,
                        error = %error,
                        "skipping unavailable linked pull request"
                    ),
                }
            }
        }
        prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
        Ok(prs)
    }

    /// Path of the MCP config file for a scope; workspace scope requires
    /// a workspace id.
    fn mcp_config_path(
        &self,
        scope: &str,
        workspace_id: Option<&str>,
    ) -> Result<PathBuf, EngineError> {
        match scope {
            "user" => {
                let dir = self
                    .config_dir
                    .as_deref()
                    .ok_or_else(|| EngineError::BadRequest("no config dir available".into()))?;
                Ok(crate::mcp::user_config_path(dir))
            }
            "workspace" => {
                let id = workspace_id.ok_or_else(|| {
                    EngineError::BadRequest("workspace scope needs workspace_id".into())
                })?;
                let ws = self
                    .store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
                Ok(crate::mcp::workspace_config_path(Path::new(&ws.path)))
            }
            other => Err(EngineError::BadRequest(format!(
                "unknown MCP scope '{other}' (use \"user\" or \"workspace\")"
            ))),
        }
    }

    /// User-managed MCP servers: the config dir's `mcp.json` plus each
    /// workspace's `.agents/.mcp.json` (one workspace when an id is given,
    /// every registered workspace otherwise). With `probe`, every enabled
    /// server is spawned and handshaken concurrently to report health.
    pub async fn list_mcp_servers(
        &self,
        workspace_id: Option<&str>,
        probe: bool,
    ) -> Result<Vec<trouve_protocol::McpServerInfo>, EngineError> {
        // (name, scope, workspace id, workspace name, config)
        type Entry = (String, String, String, String, crate::mcp::McpServerConfig);
        let mut entries: Vec<Entry> = Vec::new();
        if let Some(dir) = self.config_dir.as_deref() {
            for (name, config) in crate::mcp::read_servers(&crate::mcp::user_config_path(dir)) {
                entries.push((name, "user".into(), String::new(), String::new(), config));
            }
        }
        let workspaces = match workspace_id {
            Some(id) => vec![
                self.store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?,
            ],
            None => self.store.list_workspaces()?,
        };
        for ws in workspaces {
            let path = crate::mcp::workspace_config_path(Path::new(&ws.path));
            for (name, config) in crate::mcp::read_servers(&path) {
                entries.push((
                    name,
                    "workspace".into(),
                    ws.id.clone(),
                    ws.name.clone(),
                    config,
                ));
            }
        }
        let probes = futures::future::join_all(entries.iter().map(
            |(name, scope, _, _, config)| async move {
                // Only probe (spawn) user-scope servers: workspace-scope
                // servers live in a repo's .agents/.mcp.json and are never
                // auto-run, so opening settings must not execute them.
                if probe && !config.disabled && scope == "user" {
                    Some(crate::mcp::probe(name, config, &self.mcp_logs).await)
                } else {
                    None
                }
            },
        ))
        .await;
        Ok(entries
            .into_iter()
            .zip(probes)
            .map(
                |((name, scope, workspace_id, workspace_name, config), probed)| {
                    let (health, detail) = if config.disabled {
                        ("disabled".to_string(), "disabled in this scope".to_string())
                    } else if scope == "workspace" {
                        (
                            "untrusted".to_string(),
                            "defined in this repo's .agents/.mcp.json; not auto-run. \
                             Copy it into your own config to trust and enable it."
                                .to_string(),
                        )
                    } else {
                        match probed {
                            Some(Ok(tools)) => ("ok".to_string(), format!("{tools} tools")),
                            Some(Err(e)) => ("error".to_string(), format!("{e:#}")),
                            None => ("unknown".to_string(), String::new()),
                        }
                    };
                    trouve_protocol::McpServerInfo {
                        name,
                        scope,
                        workspace_id,
                        workspace_name,
                        command: config.command,
                        args: config.args,
                        env: config.env,
                        health,
                        detail,
                    }
                },
            )
            .collect())
    }

    /// The effective MCP config for one session: all scopes merged the way
    /// a turn in this session would see them (app-wide < workspace <
    /// branch), each entry tagged with the winning layer. Disabled entries
    /// are kept and flagged so tombstones are visible.
    pub fn session_mcp_servers(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::McpServerInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let workspace_root = self
            .store
            .workspace(&session.workspace_id)?
            .map(|ws| PathBuf::from(ws.path));
        Ok(crate::mcp::discover_with_provenance(
            self.config_dir.as_deref(),
            workspace_root.as_deref(),
            Path::new(&session.worktree_path),
        )
        .into_iter()
        .map(|(name, config, source)| trouve_protocol::McpServerInfo {
            name,
            scope: source.clone(),
            workspace_id: session.workspace_id.clone(),
            workspace_name: String::new(),
            command: config.command,
            args: config.args,
            env: config.env,
            health: if config.disabled {
                "disabled".into()
            } else {
                "unknown".into()
            },
            detail: if config.disabled {
                format!("disabled by the {source} config")
            } else {
                String::new()
            },
        })
        .collect())
    }

    /// Add or replace an MCP server in the scope's config file.
    pub fn upsert_mcp_server(
        &self,
        name: &str,
        req: &trouve_protocol::UpsertMcpServerRequest,
    ) -> Result<(), EngineError> {
        let name = name.trim();
        if name.is_empty() || name.contains("__") || name.contains('/') {
            return Err(EngineError::BadRequest(
                "server name must be non-empty and free of '__' and '/'".into(),
            ));
        }
        if req.command.trim().is_empty() {
            return Err(EngineError::BadRequest("command is required".into()));
        }
        let path = self.mcp_config_path(&req.scope, req.workspace_id.as_deref())?;
        let config = crate::mcp::McpServerConfig {
            command: req.command.trim().to_string(),
            args: req.args.clone(),
            env: req.env.clone(),
            disabled: false,
        };
        crate::mcp::upsert_server(&path, name, &config).map_err(EngineError::Internal)
    }

    /// Remove an MCP server from the scope's config file.
    pub fn delete_mcp_server(
        &self,
        name: &str,
        scope: &str,
        workspace_id: Option<&str>,
    ) -> Result<(), EngineError> {
        let path = self.mcp_config_path(scope, workspace_id)?;
        crate::mcp::remove_server(&path, name).map_err(EngineError::Internal)
    }

    /// Recent log lines (stderr + lifecycle) for one MCP server.
    pub fn mcp_server_logs(&self, name: &str) -> trouve_protocol::McpLogs {
        trouve_protocol::McpLogs {
            lines: self.mcp_logs.lines(name),
        }
    }

    /// Subscription usage for every configured subscription provider.
    /// Codex answers via its app-server, Claude Code via its CLI's
    /// stream-json usage query, and Cursor via the dashboard's undocumented
    /// usage RPC (read with the CLI's stored login). Kimi Code uses the key
    /// stored for its provider preset against the same `/usages` endpoint as
    /// Kimi's open-source CLI.
    pub async fn subscription_health(&self) -> Vec<trouve_protocol::SubscriptionHealth> {
        let backends: Vec<(String, Arc<dyn AgentBackend>)> = {
            let map = self.backends.read().unwrap();
            let mut list: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            list.sort_by(|a, b| a.0.cmp(&b.0));
            list
        };
        let mut out = Vec::new();
        for (id, backend) in backends {
            match backend.subscription_health().await {
                Some(health) => out.push(health),
                None => out.push(trouve_protocol::SubscriptionHealth {
                    provider_id: id,
                    status: "unsupported".into(),
                    plan: String::new(),
                    windows: Vec::new(),
                    credits: String::new(),
                    note: "This vendor does not provide subscription usage to third-party apps."
                        .into(),
                }),
            }
        }
        let kimi_configs: Vec<(String, ProviderConfig)> = {
            let config = self.config.lock().unwrap();
            config
                .providers
                .iter()
                .filter(|(id, provider)| {
                    id.as_str() == "kimi-code"
                        && trouve_providers::kimi_usage::is_kimi_code_base_url(
                            provider.base_url.as_deref(),
                        )
                })
                .map(|(id, provider)| (id.clone(), provider.clone()))
                .collect()
        };
        for (id, provider) in kimi_configs {
            let Some(api_key) = resolved_api_key(&id, &provider, &self.secrets) else {
                out.push(trouve_protocol::SubscriptionHealth {
                    provider_id: id,
                    status: "unavailable".into(),
                    plan: String::new(),
                    windows: Vec::new(),
                    credits: String::new(),
                    note: "Kimi Code usage needs the subscription API key saved in Providers."
                        .into(),
                });
                continue;
            };
            let base_url = provider
                .base_url
                .as_deref()
                .unwrap_or(trouve_providers::kimi_usage::KIMI_CODE_BASE_URL);
            out.push(
                trouve_providers::kimi_usage::subscription_health(&id, base_url, &api_key).await,
            );
        }
        out.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        out
    }

    /// Whether GitHub calls can authenticate, per host. The top-level
    /// fields mirror github.com (`hosts[0]`) for older clients.
    pub fn github_integration(&self) -> trouve_protocol::GithubIntegration {
        let hosts: Vec<trouve_protocol::GithubHostIntegration> = self
            .github_hosts()
            .into_iter()
            .map(|(host, client_id)| {
                let configured = self.github_token(&host).is_some();
                trouve_protocol::GithubHostIntegration {
                    removable: host != crate::github::GITHUB_COM,
                    host,
                    configured,
                    source: if configured {
                        "oauth".into()
                    } else {
                        String::new()
                    },
                    oauth_available: client_id.is_some(),
                }
            })
            .collect();
        trouve_protocol::GithubIntegration {
            configured: hosts[0].configured,
            source: hosts[0].source.clone(),
            oauth_available: hosts[0].oauth_available,
            hosts,
        }
    }

    /// Register a self-hosted GitHub Enterprise instance so remotes on it
    /// resolve and it can hold its own auth.
    pub fn add_github_host(&self, host: &str, client_id: &str) -> Result<(), EngineError> {
        let host = host
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if host.is_empty() || !host.contains('.') || host.contains('/') || host.contains(':') {
            return Err(EngineError::BadRequest(
                "enter a bare hostname, e.g. github.example.com".into(),
            ));
        }
        if host == crate::github::GITHUB_COM {
            return Err(EngineError::BadRequest(
                "github.com is always available; add only enterprise hosts".into(),
            ));
        }
        let mut config = self.config.lock().unwrap();
        if config.github_enterprise.iter().any(|e| e.host == host) {
            return Err(EngineError::Conflict(format!("{host} is already added")));
        }
        if client_id.trim().is_empty() {
            return Err(EngineError::BadRequest(
                "an OAuth app client id is required for a GitHub Enterprise host".into(),
            ));
        }
        config
            .github_enterprise
            .push(crate::config::GithubEnterpriseConfig {
                host,
                client_id: Some(client_id.trim().to_string()),
            });
        let snapshot = config.clone();
        drop(config);
        self.persist_config(&snapshot);
        Ok(())
    }

    /// Remove an enterprise host and forget its stored secrets.
    pub fn remove_github_host(&self, host: &str) -> Result<(), EngineError> {
        let host = host.trim().to_ascii_lowercase();
        let mut config = self.config.lock().unwrap();
        let before = config.github_enterprise.len();
        config.github_enterprise.retain(|e| e.host != host);
        if config.github_enterprise.len() == before {
            return Err(EngineError::NotFound(format!("GitHub host {host}")));
        }
        let snapshot = config.clone();
        drop(config);
        self.persist_config(&snapshot);
        let id = Self::github_secret_id(&host);
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::api_key_secret(&id));
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::oauth_secret(&id));
        self.store.append_event(
            Scope::Server,
            Event::GithubPullRequestsUpdated {
                pull_requests: trouve_protocol::GithubPrList {
                    viewer: String::new(),
                    host,
                    prs: Vec::new(),
                },
            },
        )?;
        Ok(())
    }

    /// Push the session branch and open a PR for it.
    pub async fn create_session_pr(
        &self,
        session_id: &str,
        req: &trouve_protocol::CreatePrRequest,
    ) -> Result<trouve_protocol::PrInfo, EngineError> {
        let session = self.get_session(session_id)?;
        let github = self.github_for_session(&session)?;
        let worktree = PathBuf::from(&session.worktree_path);
        let branch = session.branch.clone();
        let requested_base = req.base.clone();
        let session_base = session.base_ref.clone();
        let base = tokio::task::spawn_blocking(move || -> Result<String> {
            let base = requested_base.unwrap_or_else(|| {
                git::remote_branch_name(&worktree, &session_base)
                    .unwrap_or_else(|| session_base.clone())
            });
            git::push_branch(&worktree, "origin", &branch)?;
            Ok(base)
        })
        .await
        .map_err(|e| EngineError::Internal(anyhow!(e)))?
        .map_err(EngineError::Internal)?;
        let pr = github
            .create_pr(&session.branch, &base, &req.title, &req.body, req.draft)
            .await
            .map_err(EngineError::Internal)?;
        self.store.append_event(
            Scope::Session(session.id.clone()),
            Event::SessionPrOpened {
                number: pr.number,
                url: pr.url.clone(),
            },
        )?;
        Ok(pr)
    }

    /// Merge the session's PR.
    pub async fn merge_session_pr(
        &self,
        session_id: &str,
        method: Option<&str>,
    ) -> Result<(), EngineError> {
        let session = self.get_session(session_id)?;
        let github = self.github_for_session(&session)?;
        let pr = self
            .session_pr(session_id)
            .await?
            .ok_or_else(|| EngineError::NotFound("no open PR for this session".into()))?;
        github
            .merge_pr(pr.number, method.unwrap_or("merge"))
            .await
            .map_err(EngineError::Internal)
    }

    /// Unified diff of the session worktree against its base ref.
    pub async fn session_diff(&self, session_id: &str) -> Result<String, EngineError> {
        let session = self.get_session(session_id)?;
        let wt = PathBuf::from(&session.worktree_path);
        let base = session.base_ref.clone();
        tokio::task::spawn_blocking(move || git::session_diff(&wt, &base))
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)
    }

    /// List a directory inside the session worktree (IDE-style browsing).
    pub async fn session_list_dir(
        &self,
        session_id: &str,
        rel_path: &str,
    ) -> Result<Vec<trouve_protocol::DirEntry>, EngineError> {
        let session = self.get_session(session_id)?;
        let ctx = ToolCtx {
            worktree: PathBuf::from(&session.worktree_path),
            ..Default::default()
        };
        let full = ctx
            .resolve(rel_path)
            .map_err(|e| EngineError::BadRequest(e.to_string()))?;
        let mut rd = tokio::fs::read_dir(&full)
            .await
            .map_err(|e| EngineError::BadRequest(format!("cannot list {rel_path}: {e}")))?;
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(trouve_protocol::DirEntry { name, is_dir });
        }
        entries.sort_by(|a, b| (b.is_dir, &a.name).cmp(&(a.is_dir, &b.name)));
        Ok(entries)
    }

    /// Every path in the session worktree (files, plus directories with a
    /// trailing '/'), worktree-relative, honouring .gitignore — feeds the
    /// composer's "@" file-mention completion. Capped; alphabetical.
    pub async fn session_list_paths(&self, session_id: &str) -> Result<Vec<String>, EngineError> {
        const MAX_PATHS: usize = 5000;
        let session = self.get_session(session_id)?;
        let worktree = PathBuf::from(&session.worktree_path);
        let paths = tokio::task::spawn_blocking(move || {
            let mut paths = Vec::new();
            let walker = ignore::WalkBuilder::new(&worktree)
                .hidden(true)
                .require_git(false)
                .build();
            for entry in walker.flatten() {
                let Ok(rel) = entry.path().strip_prefix(&worktree) else {
                    continue;
                };
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let mut path = rel.to_string_lossy().replace('\\', "/");
                if entry.file_type().is_some_and(|t| t.is_dir()) {
                    path.push('/');
                }
                paths.push(path);
            }
            paths.sort();
            paths.truncate(MAX_PATHS);
            paths
        })
        .await
        .map_err(|e| EngineError::Internal(anyhow!("path walk failed: {e}")))?;
        Ok(paths)
    }

    /// Read a file inside the session worktree.
    pub async fn session_read_file(
        &self,
        session_id: &str,
        rel_path: &str,
    ) -> Result<String, EngineError> {
        let session = self.get_session(session_id)?;
        let ctx = ToolCtx {
            worktree: PathBuf::from(&session.worktree_path),
            ..Default::default()
        };
        let full = ctx
            .resolve(rel_path)
            .map_err(|e| EngineError::BadRequest(e.to_string()))?;
        tokio::fs::read_to_string(&full)
            .await
            .map_err(|e| EngineError::BadRequest(format!("cannot read {rel_path}: {e}")))
    }

    // --- integrated terminal --------------------------------------------

    /// The session's interactive terminal, spawning a shell in its worktree
    /// if none is live. Ephemeral (not persisted, not in the event log).
    pub fn open_terminal(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<trouve_protocol::TerminalInfo, EngineError> {
        let session = self.get_session(session_id)?;
        let terminal = self
            .terminals
            .open(session_id, Path::new(&session.worktree_path), cols, rows)
            .map_err(EngineError::Internal)?;
        Ok(terminal_info(&terminal))
    }

    pub fn terminal_input(&self, terminal_id: &str, bytes: &[u8]) -> Result<(), EngineError> {
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        terminal.write(bytes).map_err(EngineError::Internal)
    }

    pub fn terminal_resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), EngineError> {
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        terminal.resize(cols, rows).map_err(EngineError::Internal)
    }

    /// Kill a terminal (the next open spawns a fresh shell).
    pub fn terminal_kill(&self, terminal_id: &str) -> Result<(), EngineError> {
        // get() first so unknown ids surface as 404 rather than a no-op.
        self.terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        self.terminals.remove(terminal_id);
        Ok(())
    }

    /// Attach to a terminal's output from byte offset `after`: the retained
    /// backlog from there plus a live receiver (empty chunk = shell exited).
    pub fn terminal_subscribe(
        &self,
        terminal_id: &str,
        after: u64,
    ) -> Result<
        (
            u64,
            Vec<u8>,
            tokio::sync::broadcast::Receiver<bytes::Bytes>,
            bool,
        ),
        EngineError,
    > {
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        let (from, replay, rx) = terminal.subscribe(after);
        Ok((from, replay, rx, terminal.exited()))
    }

    fn session_lock(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.session_locks
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .clone()
    }

    // --- workspaces ---------------------------------------------------------

    pub fn register_workspace(
        &self,
        path: &str,
        name: Option<String>,
    ) -> Result<Workspace, EngineError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| EngineError::BadRequest(format!("invalid path {path}: {e}")))?;
        if !git::is_git_repo(&canonical) {
            return Err(EngineError::BadRequest(format!(
                "{} is not a git repository",
                canonical.display()
            )));
        }
        let path_str = canonical.to_string_lossy().to_string();
        if let Some(existing) = self.store.workspace_by_path(&path_str)? {
            if self.store.set_workspace_closed(&existing.id, false)? {
                self.store.append_event(
                    Scope::Server,
                    Event::WorkspaceRegistered {
                        workspace_id: existing.id.clone(),
                        path: path_str,
                    },
                )?;
            }
            return Ok(existing);
        }
        let ws = Workspace {
            id: new_id("ws"),
            name: name.unwrap_or_else(|| {
                canonical
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "workspace".into())
            }),
            path: path_str.clone(),
        };
        self.store.insert_workspace(&ws)?;
        self.store.append_event(
            Scope::Server,
            Event::WorkspaceRegistered {
                workspace_id: ws.id.clone(),
                path: path_str,
            },
        )?;
        Ok(ws)
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, EngineError> {
        Ok(self.store.list_workspaces()?)
    }

    /// Refresh the account-centric PR feed on every signed-in GitHub instance.
    pub async fn refresh_github_prs(&self) -> Result<(), EngineError> {
        let mut dashboard_caches = match self.github_dashboard_caches.try_lock() {
            Ok(caches) => caches,
            Err(_) => {
                tracing::debug!("coalescing concurrent GitHub dashboard refresh");
                return Ok(());
            }
        };
        let merged_since = chrono::Utc::now() - chrono::Duration::hours(24);
        let workspaces = self.store.list_workspaces()?;
        let workspace_repositories = tokio::task::spawn_blocking(move || {
            workspaces
                .into_iter()
                .filter_map(|workspace| {
                    let remote = git::remote_url(Path::new(&workspace.path), "origin")?;
                    let (host, owner, repo) = crate::github::parse_remote(&remote)?;
                    Some((host, format!("{owner}/{repo}"), workspace.id))
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        let mut failures = Vec::new();
        let github_hosts = self.github_hosts();
        let known_hosts = github_hosts
            .iter()
            .map(|(host, _)| host.clone())
            .collect::<HashSet<_>>();
        dashboard_caches.retain(|host, _| known_hosts.contains(host));
        for (host, _) in github_hosts {
            let Some(token) = self.github_token(&host) else {
                dashboard_caches.remove(&host);
                continue;
            };
            let account =
                crate::github::GitHubAccount::new(&token, &host).map_err(EngineError::Internal)?;
            let cache = dashboard_caches.entry(host.clone()).or_default();
            let refresh = tokio::time::timeout(
                GITHUB_DASHBOARD_REFRESH_TIMEOUT,
                account.dashboard_prs(merged_since, cache),
            )
            .await;
            let (viewer, mut prs) = match refresh {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    failures.push(format!("{host}: {error:#}"));
                    continue;
                }
                Err(_) => {
                    failures.push(format!(
                        "{host}: GitHub dashboard refresh timed out after {}s",
                        GITHUB_DASHBOARD_REFRESH_TIMEOUT.as_secs()
                    ));
                    continue;
                }
            };
            for pr in &mut prs {
                pr.workspace_id = workspace_repositories
                    .iter()
                    .find_map(|(host, repository, workspace_id)| {
                        (host == &pr.host && repository == &pr.repository)
                            .then(|| workspace_id.clone())
                    })
                    .unwrap_or_default();
            }
            self.store.append_event(
                Scope::Server,
                Event::GithubPullRequestsUpdated {
                    pull_requests: trouve_protocol::GithubPrList { viewer, host, prs },
                },
            )?;
        }
        if !failures.is_empty() {
            return Err(EngineError::BadRequest(failures.join("; ")));
        }
        Ok(())
    }

    /// Hide a workspace from clients and reject new sessions/automation runs
    /// while retaining existing sessions, worktrees, and automation records.
    /// Registering the same path later reopens it.
    pub fn close_workspace(&self, id: &str) -> Result<(), EngineError> {
        if self.store.workspace(id)?.is_none() {
            return Err(EngineError::NotFound(format!("workspace {id}")));
        }
        if self.store.set_workspace_closed(id, true)? {
            self.store.append_event(
                Scope::Server,
                Event::WorkspaceClosed {
                    workspace_id: id.to_string(),
                },
            )?;
        }
        Ok(())
    }

    /// Local branches of the workspace repo, for base-ref selection.
    pub async fn workspace_branches(&self, id: &str) -> Result<BranchList, EngineError> {
        let ws = self
            .store
            .workspace(id)?
            .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
        let repo = PathBuf::from(&ws.path);
        tokio::task::spawn_blocking(move || -> Result<BranchList> {
            let branches = git::list_branches(&repo)?;
            let head = git::head_ref(&repo)?;
            Ok(BranchList { branches, head })
        })
        .await
        .map_err(|e| EngineError::Internal(anyhow!(e)))?
        .map_err(EngineError::Internal)
    }

    // --- sessions -----------------------------------------------------------

    pub async fn create_session(&self, req: CreateSessionRequest) -> Result<Session, EngineError> {
        let ws = self
            .store
            .open_workspace(&req.workspace_id)?
            .ok_or_else(|| EngineError::NotFound(format!("workspace {}", req.workspace_id)))?;
        let repo = PathBuf::from(&ws.path);
        let title = req.title.unwrap_or_else(|| "New session".into());
        let session_id = new_id("se");
        let slug = git::slugify(&title);
        // Session id suffix keeps branches unique across same-titled sessions.
        let branch = format!("trouve/{slug}-{}", &session_id[3..9]);
        let base_ref = match req.base_ref {
            Some(r) => r,
            None => {
                let repo = repo.clone();
                tokio::task::spawn_blocking(move || git::head_ref(&repo))
                    .await
                    .map_err(|e| EngineError::Internal(anyhow!(e)))?
                    .map_err(EngineError::Internal)?
            }
        };
        let worktree_path = git::worktree_dir(&self.data_dir, &session_id);
        let base_ref = {
            let repo = repo.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            let selected_base = base_ref.clone();
            let fetch_latest = req.fetch_latest;
            let checkout_ref = req.checkout_ref.clone();
            tokio::task::spawn_blocking(move || -> Result<String> {
                let mut session_base = selected_base.clone();
                let worktree_base = if fetch_latest {
                    match git::fetch_upstream_base(&repo, &selected_base)? {
                        Some(fetched) => {
                            session_base = fetched.upstream_ref;
                            fetched.commit
                        }
                        None => selected_base,
                    }
                } else {
                    selected_base
                };
                let checkout_ref = checkout_ref.as_deref().unwrap_or(&worktree_base);
                git::create_worktree(&repo, &worktree_path, &branch, checkout_ref)?;
                Ok(session_base)
            })
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)?
        };

        let session = Session {
            id: session_id.clone(),
            workspace_id: ws.id.clone(),
            title,
            branch: branch.clone(),
            worktree_path: worktree_path.to_string_lossy().to_string(),
            base_ref,
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        self.store.insert_session(&session)?;

        // Checkpoint 0: pristine state, so the first turn can be undone.
        let commit = {
            let wt = worktree_path.clone();
            let sid = session_id.clone();
            tokio::task::spawn_blocking(move || {
                git::checkpoint(&wt, &sid, 0, "trouve: session start")
            })
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)?
        };
        let checkpoint_id = new_id("cp");
        self.store.append_checkpoint(&CheckpointRow {
            id: checkpoint_id.clone(),
            session_id: session_id.clone(),
            thread_id: None,
            turn: 0,
            seq: 0,
            commit_hash: commit.clone(),
        })?;

        self.store.append_event(
            Scope::Server,
            Event::SessionCreated {
                session_id: session_id.clone(),
                workspace_id: ws.id.clone(),
            },
        )?;
        self.store.append_event(
            Scope::Session(session_id.clone()),
            Event::WorktreeCreated {
                path: session.worktree_path.clone(),
                branch,
            },
        )?;
        self.store.append_event(
            Scope::Session(session_id.clone()),
            Event::CheckpointCreated {
                checkpoint_id,
                thread_id: String::new(),
                turn: 0,
                commit,
            },
        )?;
        if self.index_hooks {
            crate::tools::warm_index_in_background(worktree_path);
        }
        Ok(session)
    }

    pub fn list_sessions(&self, workspace_id: Option<&str>) -> Result<Vec<Session>, EngineError> {
        let mut sessions = self.store.list_sessions(workspace_id)?;
        let active = self.active_threads.lock().unwrap();
        for session in &mut sessions {
            session.active = active.values().any(|s| *s == session.id);
        }
        Ok(sessions)
    }

    pub fn get_session(&self, id: &str) -> Result<Session, EngineError> {
        let mut session = self
            .store
            .session(id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {id}")))?;
        session.active = {
            let active = self.active_threads.lock().unwrap();
            active.values().any(|s| *s == session.id)
        };
        Ok(session)
    }

    /// Rename and/or (un)archive a session.
    pub fn update_session(
        &self,
        id: &str,
        req: &UpdateSessionRequest,
    ) -> Result<Session, EngineError> {
        let session = self.get_session(id)?;
        if let Some(title) = req.title.as_deref()
            && title.trim().is_empty()
        {
            return Err(EngineError::BadRequest("title cannot be empty".into()));
        }
        self.store
            .update_session(id, req.title.as_deref(), req.archived)?;
        self.store.append_event(
            Scope::Server,
            Event::SessionUpdated {
                session_id: id.to_string(),
                workspace_id: session.workspace_id.clone(),
            },
        )?;
        if self.index_hooks
            && req.archived == Some(true)
            && !session.archived
            && let Some(ws) = self.store.workspace(&session.workspace_id)?
        {
            crate::tools::gc_index_store_in_background(PathBuf::from(&ws.path));
        }
        self.get_session(id)
    }

    pub async fn delete_session(&self, id: &str) -> Result<(), EngineError> {
        let session = self.get_session(id)?;
        {
            // Lock ordering is always active_threads -> deleting_sessions;
            // dispatch_queue uses the same order. Once the marker is set, an
            // idle session cannot acquire a new dispatcher while deletion is
            // in progress.
            let active = self.active_threads.lock().unwrap();
            if active.values().any(|session_id| session_id == id) {
                return Err(EngineError::Conflict(format!(
                    "session {id} has an active turn"
                )));
            }
            let mut deleting = self.deleting_sessions.lock().unwrap();
            if !deleting.insert(id.to_string()) {
                return Err(EngineError::Conflict(format!(
                    "session {id} is already being deleted"
                )));
            }
        }

        let result = async {
            let ws = self
                .store
                .workspace(&session.workspace_id)?
                .ok_or_else(|| {
                    EngineError::NotFound(format!("workspace {}", session.workspace_id))
                })?;
            // Capture cleanup paths while the relational rows still exist,
            // then commit the database deletion before any irreversible
            // filesystem work. A database error must leave the session and
            // its worktree consistently intact.
            let attachment_paths = self.store.session_attachment_paths(id)?;
            self.store.delete_session(id)?;

            self.terminals.remove_session(id);
            self.executor
                .evict_worktree(Path::new(&session.worktree_path))
                .await;
            for path in attachment_paths {
                let _ = std::fs::remove_file(&path);
            }

            let repo = PathBuf::from(&ws.path);
            let wt = PathBuf::from(&session.worktree_path);
            if wt.exists() {
                let res = tokio::task::spawn_blocking(move || git::remove_worktree(&repo, &wt))
                    .await
                    .map_err(|e| EngineError::Internal(anyhow!(e)))?;
                if let Err(e) = res {
                    tracing::warn!("failed to remove worktree for {id}: {e}");
                }
            }
            // Session-scoped events are deleted with the session for privacy;
            // the persisted server event is the replayable deletion signal.
            self.store.append_event(
                Scope::Server,
                Event::SessionDeleted {
                    session_id: id.to_string(),
                    workspace_id: session.workspace_id.clone(),
                },
            )?;
            if self.index_hooks {
                crate::tools::gc_index_store_in_background(PathBuf::from(&ws.path));
            }
            Ok(())
        }
        .await;

        self.deleting_sessions.lock().unwrap().remove(id);
        result
    }

    // --- threads ------------------------------------------------------------

    pub fn create_thread(&self, req: CreateThreadRequest) -> Result<Thread, EngineError> {
        let session = self.get_session(&req.session_id)?;
        let ws = self.store.workspace(&session.workspace_id)?.unwrap();
        let all_modes = modes::resolve_modes(self.config_dir.as_deref(), Some(Path::new(&ws.path)));
        let mode_id = req.mode.unwrap_or_else(|| "code".into());
        let mode = modes::find_mode(&all_modes, &mode_id)
            .ok_or_else(|| EngineError::BadRequest(format!("unknown mode: {mode_id}")))?;
        // Provider availability is validated when a message is sent, not
        // here: a thread must be creatable before any provider is configured.
        // Model precedence: explicit request > the mode's default model >
        // the global default.
        let model = req
            .model
            .or_else(|| mode.default_model.clone())
            .unwrap_or_else(|| self.default_model.read().unwrap().clone());
        let mut model_options = req.model_options;
        let global_thinking_level = self.default_thinking_level.read().unwrap().clone();
        // `thinking_level` is the canonical inherited key. Before a turn it
        // is resolved to the selected model's advertised key
        // (reasoning_effort, effort, ...), or removed for models that do not
        // expose thinking levels.
        inherit_thinking_option(
            &mut model_options,
            mode.default_thinking_level.as_deref(),
            global_thinking_level.as_deref(),
        );
        let thread = Thread {
            id: new_id("th"),
            session_id: session.id.clone(),
            mode: mode.id.clone(),
            model,
            model_options: model_options.clone(),
            // Permission precedence mirrors the model's: explicit request >
            // the mode's default > the global default.
            permission_mode: req
                .permission_mode
                .or(mode.default_permission_mode)
                .unwrap_or_else(|| *self.default_permission_mode.read().unwrap()),
            created_at: chrono::Utc::now(),
            // Spawn parentage is recorded by the spawn tools after insert;
            // reads recompute this flag from the spawned_threads table.
            spawned: false,
            todos: Vec::new(),
        };
        self.store.insert_thread(&thread, &model_options)?;
        self.store.append_event(
            Scope::Server,
            Event::ThreadCreated {
                thread_id: thread.id.clone(),
                session_id: session.id,
            },
        )?;
        Ok(thread)
    }

    pub fn get_thread(&self, id: &str) -> Result<Thread, EngineError> {
        self.store
            .thread(id)?
            .ok_or_else(|| EngineError::NotFound(format!("thread {id}")))
    }

    pub fn list_threads(&self, session_id: &str) -> Result<Vec<Thread>, EngineError> {
        Ok(self.store.list_threads(session_id)?)
    }

    /// Change thread settings (mode/model/options) between turns. Conflicts
    /// while a turn is running in the thread's session.
    pub fn update_thread(
        &self,
        id: &str,
        req: &UpdateThreadRequest,
    ) -> Result<Thread, EngineError> {
        let thread = self.get_thread(id)?;
        let session = self.get_session(&thread.session_id)?;

        // The session lock is held for the duration of a turn; a locked
        // session means settings would change under a running agent.
        let lock = self.session_lock(&session.id);
        let _guard = lock.try_lock().map_err(|_| {
            EngineError::Conflict("cannot change thread settings while a turn is running".into())
        })?;

        if let Some(mode_id) = req.mode.as_deref() {
            let ws = self.store.workspace(&session.workspace_id)?.unwrap();
            let all_modes =
                modes::resolve_modes(self.config_dir.as_deref(), Some(Path::new(&ws.path)));
            modes::find_mode(&all_modes, mode_id)
                .ok_or_else(|| EngineError::BadRequest(format!("unknown mode: {mode_id}")))?;
        }
        if let Some(model) = req.model.as_deref()
            && !model.contains('/')
        {
            return Err(EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            )));
        }
        self.store.update_thread(
            id,
            req.mode.as_deref(),
            req.model.as_deref(),
            req.model_options.as_ref(),
            req.permission_mode,
        )?;
        self.store.append_event(
            Scope::Server,
            Event::ThreadUpdated {
                thread_id: id.to_string(),
                session_id: session.id,
            },
        )?;
        self.get_thread(id)
    }

    fn resolve_provider(&self, model: &str) -> Result<(Arc<dyn Provider>, String), EngineError> {
        let (provider_id, model_name) = model.split_once('/').ok_or_else(|| {
            EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            ))
        })?;
        let provider = self
            .providers
            .read()
            .unwrap()
            .get(provider_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::BadRequest(format!(
                    "provider {provider_id} is not configured (configured: {})",
                    self.provider_ids().join(", ")
                ))
            })?;
        Ok((provider, model_name.to_string()))
    }

    // --- approvals ------------------------------------------------------------

    pub fn resolve_approval(
        &self,
        call_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), EngineError> {
        if self.approvals.resolve(call_id, decision) {
            Ok(())
        } else {
            Err(EngineError::NotFound(format!("pending approval {call_id}")))
        }
    }

    // --- questions --------------------------------------------------------------

    /// Answer (or skip, `answers: None`) a pending `question.requested`.
    pub fn resolve_question(
        &self,
        request_id: &str,
        answers: Option<Vec<trouve_protocol::QuestionAnswer>>,
    ) -> Result<(), EngineError> {
        if self.questions.resolve(request_id, answers) {
            Ok(())
        } else {
            Err(EngineError::NotFound(format!(
                "pending question {request_id}"
            )))
        }
    }

    /// Pose questions to the user and block until they answer or skip.
    /// Emits `question.requested` / `question.resolved` around the wait.
    async fn ask_user_questions(
        &self,
        thread_id: &str,
        turn: u64,
        request_id: &str,
        title: Option<String>,
        questions: Vec<trouve_protocol::Question>,
    ) -> Result<Option<Vec<trouve_protocol::QuestionAnswer>>> {
        let scope = Scope::Thread(thread_id.to_string());
        let rx = self.questions.request(request_id);
        self.store.append_event(
            scope.clone(),
            Event::QuestionRequested {
                turn,
                request_id: request_id.to_string(),
                title,
                questions,
            },
        )?;
        let answers = rx.await.unwrap_or(None);
        self.store.append_event(
            scope,
            Event::QuestionResolved {
                request_id: request_id.to_string(),
                answers: answers.clone(),
            },
        )?;
        Ok(answers)
    }

    // --- undo/redo --------------------------------------------------------------

    pub async fn undo(self: &Arc<Self>, session_id: &str) -> Result<(), EngineError> {
        self.restore_checkpoint(session_id, RestoreDirection::Undo)
            .await
    }

    pub async fn redo(self: &Arc<Self>, session_id: &str) -> Result<(), EngineError> {
        self.restore_checkpoint(session_id, RestoreDirection::Redo)
            .await
    }

    async fn restore_checkpoint(
        &self,
        session_id: &str,
        direction: RestoreDirection,
    ) -> Result<(), EngineError> {
        let session = self.get_session(session_id)?;
        let lock = self.session_lock(session_id);
        let _guard = lock.lock().await;

        let latest = self
            .store
            .latest_checkpoint_seq(session_id)?
            .ok_or_else(|| EngineError::BadRequest("session has no checkpoints".into()))?;
        let current = self.store.undo_pos(session_id)?.unwrap_or(latest);
        let target = match direction {
            RestoreDirection::Undo => current - 1,
            RestoreDirection::Redo => current + 1,
        };
        if target < 0 || target > latest {
            return Err(EngineError::BadRequest(format!(
                "nothing to {}",
                match direction {
                    RestoreDirection::Undo => "undo",
                    RestoreDirection::Redo => "redo",
                }
            )));
        }
        let cp = self
            .store
            .checkpoint_at(session_id, target)?
            .ok_or_else(|| EngineError::NotFound(format!("checkpoint seq {target}")))?;
        let wt = PathBuf::from(&session.worktree_path);
        let commit = cp.commit_hash.clone();
        tokio::task::spawn_blocking(move || git::restore(&wt, &commit))
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)?;
        self.store.set_undo_pos(
            session_id,
            if target == latest { None } else { Some(target) },
        )?;
        self.store.append_event(
            Scope::Session(session_id.to_string()),
            Event::CheckpointRestored {
                checkpoint_id: cp.id,
                direction,
            },
        )?;
        Ok(())
    }

    // --- turns ---------------------------------------------------------------

    /// Accept a user message. If the thread is idle it runs immediately;
    /// otherwise it joins the thread's persistent prompt queue and runs when
    /// its turn comes. Progress is visible on the thread's event stream.
    /// Attachment uploads are decoded and stored immediately, so queued
    /// prompts reference durable files rather than request payloads.
    pub fn send_message(
        self: &Arc<Self>,
        thread_id: &str,
        content: String,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
    ) -> Result<TurnAccepted, EngineError> {
        self.get_thread(thread_id)?; // 404 for unknown threads
        let attachments = self.save_attachments(thread_id, uploads)?;
        self.store
            .enqueue_prompt(thread_id, &content, &attachments)?;
        self.emit_queue(thread_id)?;
        let turn = self.dispatch_queue(thread_id)?;
        Ok(TurnAccepted {
            thread_id: thread_id.to_string(),
            turn: turn.unwrap_or(0),
            queued: turn.is_none(),
        })
    }

    /// Decode and persist prompt uploads under `data_dir/attachments`,
    /// recording each in the store. Rejects the whole message on a bad or
    /// oversized attachment — better than silently dropping a file the
    /// prompt refers to.
    fn save_attachments(
        &self,
        thread_id: &str,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
    ) -> Result<Vec<trouve_protocol::Attachment>, EngineError> {
        use base64::Engine as _;
        const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
        let mut out = Vec::new();
        let dir = self.data_dir.join("attachments");
        for up in uploads {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(up.data.as_bytes())
                .map_err(|e| {
                    EngineError::BadRequest(format!("attachment {}: invalid base64: {e}", up.name))
                })?;
            if bytes.is_empty() {
                return Err(EngineError::BadRequest(format!(
                    "attachment {} is empty",
                    up.name
                )));
            }
            if bytes.len() > MAX_ATTACHMENT_BYTES {
                return Err(EngineError::BadRequest(format!(
                    "attachment {} exceeds {} MB",
                    up.name,
                    MAX_ATTACHMENT_BYTES / (1024 * 1024)
                )));
            }
            std::fs::create_dir_all(&dir).map_err(anyhow::Error::from)?;
            let id = format!("at_{}", uuid::Uuid::new_v4().simple());
            // Store under the opaque id; keep the (sanitized) extension so
            // tools and vendor CLIs sniff the type naturally.
            let ext = Path::new(&up.name)
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
                .map(|e| format!(".{}", e.to_ascii_lowercase()))
                .unwrap_or_default();
            let path = dir.join(format!("{id}{ext}"));
            std::fs::write(&path, &bytes).map_err(anyhow::Error::from)?;
            let attachment = trouve_protocol::Attachment {
                id,
                name: up.name,
                mime: up.mime,
                size_bytes: bytes.len() as u64,
            };
            self.store
                .add_attachment(thread_id, &attachment, &path.to_string_lossy())?;
            out.push(attachment);
        }
        Ok(out)
    }

    /// Metadata and stored file for one attachment (serves
    /// `GET /v1/attachments/{id}`).
    pub fn attachment(
        &self,
        id: &str,
    ) -> Result<(trouve_protocol::Attachment, PathBuf), EngineError> {
        let (attachment, path) = self
            .store
            .attachment(id)?
            .ok_or_else(|| EngineError::NotFound(format!("attachment {id}")))?;
        Ok((attachment, PathBuf::from(path)))
    }

    /// Look up the stored file behind each attachment; rows that vanished
    /// (e.g. a pruned data dir) are dropped with a warning rather than
    /// failing the turn.
    fn resolve_attachments(
        &self,
        attachments: &[trouve_protocol::Attachment],
    ) -> Vec<(trouve_protocol::Attachment, PathBuf)> {
        attachments
            .iter()
            .filter_map(|a| match self.store.attachment(&a.id) {
                Ok(Some((meta, path))) if Path::new(&path).exists() => {
                    Some((meta, PathBuf::from(path)))
                }
                _ => {
                    tracing::warn!("attachment {} ({}) missing; skipped", a.id, a.name);
                    None
                }
            })
            .collect()
    }

    /// Publish the thread's current queue on its event stream.
    fn emit_queue(&self, thread_id: &str) -> Result<(), EngineError> {
        let prompts = self.store.queued_prompts(thread_id)?;
        self.store.append_event(
            Scope::Thread(thread_id.to_string()),
            Event::QueueUpdated { prompts },
        )?;
        Ok(())
    }

    // --- prompt queue ----------------------------------------------------

    pub fn list_queued_prompts(
        &self,
        thread_id: &str,
    ) -> Result<Vec<trouve_protocol::QueuedPrompt>, EngineError> {
        self.get_thread(thread_id)?;
        Ok(self.store.queued_prompts(thread_id)?)
    }

    pub fn update_queued_prompt(&self, prompt_id: &str, content: &str) -> Result<(), EngineError> {
        let thread_id = self
            .store
            .queued_prompt_thread(prompt_id)?
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;
        if !self.store.update_queued_prompt(prompt_id, content)? {
            return Err(EngineError::NotFound(format!("queued prompt {prompt_id}")));
        }
        self.emit_queue(&thread_id)
    }

    pub fn delete_queued_prompt(&self, prompt_id: &str) -> Result<(), EngineError> {
        let thread_id = self
            .store
            .queued_prompt_thread(prompt_id)?
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;
        if !self.store.delete_queued_prompt(prompt_id)? {
            return Err(EngineError::NotFound(format!("queued prompt {prompt_id}")));
        }
        self.emit_queue(&thread_id)
    }

    /// Apply a full new order for the thread's queue. `ids` must name every
    /// currently queued prompt exactly once.
    pub fn reorder_queue(&self, thread_id: &str, ids: &[String]) -> Result<(), EngineError> {
        self.get_thread(thread_id)?;
        if !self.store.reorder_queued_prompts(thread_id, ids)? {
            return Err(EngineError::Conflict(
                "queue changed while reordering; refresh and retry".into(),
            ));
        }
        self.emit_queue(thread_id)
    }

    /// Start draining the thread's queue if it's idle — the "Send now"
    /// affordance. Deliberately never called automatically at startup: a
    /// crash may have cut a turn short, and running the queue on top of
    /// half-finished work needs a human's judgment. (A failed turn likewise
    /// pauses its queue until the user kicks it.)
    /// Returns the turn number of the dispatched prompt, or `None` when a
    /// turn is already running or the queue is empty.
    pub fn dispatch_queue(self: &Arc<Self>, thread_id: &str) -> Result<Option<u64>, EngineError> {
        let thread = self.get_thread(thread_id)?;
        // Claim the thread and take the queue front atomically so two
        // concurrent sends can't both start a dispatcher.
        let (prompt, session_woke) = {
            let mut active = self.active_threads.lock().unwrap();
            if self
                .deleting_sessions
                .lock()
                .unwrap()
                .contains(&thread.session_id)
            {
                return Err(EngineError::Conflict(format!(
                    "session {} is being deleted",
                    thread.session_id
                )));
            }
            if active.contains_key(thread_id) {
                // A send that races cancellation cleanup is an explicit
                // request to keep working. Remember it while holding the
                // active-thread lock so the cancelling dispatcher either
                // sees this marker or releases the claim before this send
                // retries dispatch below.
                let cancelling = self
                    .turn_cancels
                    .lock()
                    .unwrap()
                    .get(thread_id)
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                if cancelling {
                    self.resume_after_cancel
                        .lock()
                        .unwrap()
                        .insert(thread_id.to_string());
                }
                return Ok(None);
            }
            let Some(p) = self.store.claim_queued_prompt(thread_id)? else {
                return Ok(None);
            };
            let was_active = active.values().any(|s| *s == thread.session_id);
            active.insert(thread_id.to_string(), thread.session_id.clone());
            (p, !was_active)
        };
        if session_woke {
            self.emit_session_activity(&thread.session_id, true);
        }
        // If setup fails after claiming, release the claim — otherwise the
        // thread stays "active" forever and can never dispatch again.
        if let Err(e) = self.emit_queue(thread_id) {
            let _ = self.store.release_queued_prompt(&prompt.id);
            self.release_thread(thread_id);
            return Err(e);
        }
        let turn = match self.store.next_turn(thread_id) {
            Ok(t) => t,
            Err(e) => {
                let _ = self.store.release_queued_prompt(&prompt.id);
                self.release_thread(thread_id);
                return Err(e.into());
            }
        };
        // Register cancellation before returning from dispatch so an
        // immediate cancel cannot race the spawned turn task.
        let cancel = self.register_cancel(thread_id);
        let engine = self.clone();
        tokio::spawn(async move {
            let thread_id = thread.id.clone();
            let prompt_id = prompt.id.clone();
            // Catch a panic in the turn machinery so the claim (and cancel
            // token) are always released and the UI unsticks — tokio would
            // otherwise swallow the panic and leave the thread wedged as
            // "active" with no TurnFailed event.
            let drained =
                std::panic::AssertUnwindSafe(engine.drain_queue(thread, turn, prompt, cancel))
                    .catch_unwind()
                    .await;
            if drained.is_err() {
                tracing::error!("turn dispatcher for {thread_id} panicked");
                let _ = engine.store.release_queued_prompt(&prompt_id);
                let _ = engine.emit_queue(&thread_id);
                let _ = engine.store.append_event(
                    Scope::Thread(thread_id.clone()),
                    Event::TurnFailed {
                        turn,
                        error: "internal error".into(),
                    },
                );
                engine.clear_cancel(&thread_id);
                engine.release_thread(&thread_id);
            }
        });
        Ok(Some(turn))
    }

    /// Run `content` as `turn`, then keep pulling queued prompts until the
    /// queue is empty or a turn fails (a failure pauses the queue so a
    /// persistent error can't burn every queued prompt).
    async fn drain_queue(
        self: &Arc<Self>,
        thread: Thread,
        turn: u64,
        prompt: trouve_protocol::QueuedPrompt,
        first_cancel: tokio_util::sync::CancellationToken,
    ) {
        let mut thread = thread;
        let mut turn = turn;
        let mut prompt = prompt;
        let mut first_cancel = Some(first_cancel);
        loop {
            let cancel = first_cancel
                .take()
                .unwrap_or_else(|| self.register_cancel(&thread.id));
            let result = self.run_turn(&thread, turn, &prompt, cancel.clone()).await;
            let cancelled = cancel.is_cancelled();
            if let Err(e) = result {
                self.clear_cancel(&thread.id);
                tracing::error!("turn {turn} of {} failed: {e}", thread.id);
                let _ = self.store.release_queued_prompt(&prompt.id);
                let _ = self.emit_queue(&thread.id);
                self.release_thread(&thread.id);
                let _ = self.store.append_event(
                    Scope::Thread(thread.id.clone()),
                    Event::TurnFailed {
                        turn,
                        error: e.to_string(),
                    },
                );
                return;
            }
            if cancelled {
                // A user-cancelled turn normally pauses the queue (like a
                // failure, but not an error). A prompt submitted after the
                // cancel request is itself an explicit resume, though. Make
                // that decision atomically with releasing the active claim
                // so the racing send cannot be stranded between the two.
                let resume = self.finish_cancelled_turn(&thread.id);
                let _ = self.store.append_event(
                    Scope::Thread(thread.id.clone()),
                    Event::TurnCancelled { turn },
                );
                if !resume {
                    return;
                }
            } else {
                self.clear_cancel(&thread.id);
            }
            // Pop the next prompt; releasing the claim and inspecting the
            // queue must be atomic against concurrent send_message calls.
            let (next, session_idle) = {
                let mut active = self.active_threads.lock().unwrap();
                match self.store.claim_queued_prompt(&thread.id) {
                    Ok(Some(p)) => (Some(p), false),
                    _ => {
                        active.remove(&thread.id);
                        (None, !active.values().any(|s| *s == thread.session_id))
                    }
                }
            };
            if session_idle {
                self.emit_session_activity(&thread.session_id, false);
            }
            let Some(next) = next else { return };
            let _ = self.emit_queue(&thread.id);
            // Thread settings may have changed between turns.
            if let Ok(t) = self.get_thread(&thread.id) {
                thread = t;
            }
            turn = match self.store.next_turn(&thread.id) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("queue for {} stopped: {e}", thread.id);
                    let _ = self.store.release_queued_prompt(&next.id);
                    let _ = self.emit_queue(&thread.id);
                    self.release_thread(&thread.id);
                    return;
                }
            };
            prompt = next;
        }
    }

    /// Drop a thread's dispatcher claim; when it was the session's last
    /// active thread, announce the session going idle.
    fn release_thread(&self, thread_id: &str) {
        let idle_session = {
            let mut active = self.active_threads.lock().unwrap();
            active
                .remove(thread_id)
                .filter(|session| !active.values().any(|s| s == session))
        };
        if let Some(session_id) = idle_session {
            self.emit_session_activity(&session_id, false);
        }
    }

    /// Register a fresh cancellation token for a turn about to run.
    fn register_cancel(&self, thread_id: &str) -> tokio_util::sync::CancellationToken {
        let token = tokio_util::sync::CancellationToken::new();
        self.turn_cancels
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), token.clone());
        token
    }

    fn clear_cancel(&self, thread_id: &str) {
        self.turn_cancels.lock().unwrap().remove(thread_id);
        self.resume_after_cancel.lock().unwrap().remove(thread_id);
    }

    /// Finish a cancelled turn while coordinating with sends that may be
    /// waiting on the same active-thread claim. Returns true when one of
    /// those sends requested that the queue continue draining.
    fn finish_cancelled_turn(&self, thread_id: &str) -> bool {
        let (resume, idle_session) = {
            // Lock ordering matches `dispatch_queue`: active thread, cancel
            // token, then resume marker.
            let mut active = self.active_threads.lock().unwrap();
            self.turn_cancels.lock().unwrap().remove(thread_id);
            let resume = self.resume_after_cancel.lock().unwrap().remove(thread_id);
            let idle_session = if resume {
                None
            } else {
                active
                    .remove(thread_id)
                    .filter(|session| !active.values().any(|s| s == session))
            };
            (resume, idle_session)
        };
        if let Some(session_id) = idle_session {
            self.emit_session_activity(&session_id, false);
        }
        resume
    }

    /// Interrupt the turn currently running on a thread. Trips its
    /// cancellation token, which stops the provider stream, in-flight tool
    /// call, or approval wait at the next await point. No-op error when the
    /// thread has no running turn.
    pub fn cancel_turn(&self, thread_id: &str) -> Result<(), EngineError> {
        match self.turn_cancels.lock().unwrap().get(thread_id) {
            Some(token) => {
                token.cancel();
                Ok(())
            }
            None => Err(EngineError::BadRequest(format!(
                "no running turn to cancel on thread {thread_id}"
            ))),
        }
    }

    /// Server-scope `session.activity` event — session lists light up (or
    /// dim) their indicator without refetching.
    fn emit_session_activity(&self, session_id: &str, active: bool) {
        let workspace_id = self
            .store
            .session(session_id)
            .ok()
            .flatten()
            .map(|s| s.workspace_id)
            .unwrap_or_default();
        let _ = self.store.append_event(
            Scope::Server,
            Event::SessionActivity {
                session_id: session_id.to_string(),
                workspace_id,
                active,
            },
        );
    }

    async fn run_turn(
        self: &Arc<Self>,
        thread: &Thread,
        turn: u64,
        prompt: &trouve_protocol::QueuedPrompt,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let content = prompt.content.clone();
        let attachments = prompt.attachments.clone();
        let session = self
            .store
            .session(&thread.session_id)?
            .context("session vanished")?;
        let ws = self
            .store
            .workspace(&session.workspace_id)?
            .context("workspace vanished")?;
        let scope = Scope::Thread(thread.id.clone());
        let worktree = PathBuf::from(&session.worktree_path);
        let ctx = ToolCtx {
            worktree: worktree.clone(),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&ws.path)),
        };

        let all_modes = modes::resolve_modes(self.config_dir.as_deref(), Some(Path::new(&ws.path)));
        let mode = modes::find_mode(&all_modes, &thread.mode)
            .cloned()
            .unwrap_or_else(modes::fallback_mode);

        // Serialize worktree mutations across the session's threads — except
        // agent-spawned children in read-only modes: they can't write, and
        // running them concurrently with the parent's turn (which holds this
        // lock) is the whole point of spawn_thread exploration fan-out.
        let concurrent_child = mode.read_only && self.store.spawn_parent(&thread.id)?.is_some();
        let lock = self.session_lock(&session.id);
        let _guard = if concurrent_child {
            None
        } else {
            Some(lock.lock().await)
        };

        // External agent backend? The vendor harness owns the loop; we
        // stream its events and bridge approvals. (Session lock stays held.)
        if let Some((backend_id, backend, model_name)) = self.backend_for(&thread.model) {
            return self
                .run_backend_turn(
                    &session,
                    thread,
                    turn,
                    &mode,
                    &backend_id,
                    backend,
                    model_name,
                    content,
                    attachments,
                    concurrent_child,
                    cancel,
                    &prompt.id,
                )
                .await;
        }

        let (provider, model_name) = self
            .resolve_provider(&thread.model)
            .map_err(|e| anyhow!(e.to_string()))?;
        let mut model_options = self.store.thread_model_options(&thread.id)?;
        let model_catalog = provider.list_models().await;
        normalize_thinking_option(
            &mut model_options,
            model_catalog.iter().find(|m| m.id == thread.model),
        );

        self.store.append_event(
            scope.clone(),
            Event::TurnStarted {
                turn,
                mode: mode.id.clone(),
                model: thread.model.clone(),
            },
        )?;
        // Show the prompt in the UI before any slow pre-turn work:
        // compaction below can block for a while (its model probe may even
        // spawn the local llama-server and load a model).
        self.store.append_event(
            scope.clone(),
            Event::UserMessage {
                turn,
                content: content.clone(),
                attachments: attachments.clone(),
            },
        )?;

        // Compact the transcript when it nears the model's context window,
        // before this turn's user message joins it (the stored transcript —
        // the event above is display-only).
        if let Err(e) = self
            .maybe_compact(thread, turn, &provider, &model_name)
            .await
        {
            // Compaction is best-effort; the turn proceeds with full history.
            tracing::warn!("compaction failed for {}: {e}", thread.id);
        }
        // Native providers speak text-only; every attachment (images
        // included) becomes a path reference the model's file tools can
        // follow. Copy them into the worktree first: the file tools reject
        // absolute paths (the sandbox), so a data-dir path the model can't
        // open is useless — a worktree-relative copy is reachable.
        let resolved = self.resolve_attachments(&attachments);
        let materialized = materialize_attachments(&worktree, &resolved);
        let content = annotate_attachments(content, &materialized);
        self.store
            .append_message(&thread.id, &serde_json::to_value(Message::User(content))?)?;
        if !self.store.finish_queued_prompt(&prompt.id)? {
            bail!("queued prompt {} vanished before turn start", prompt.id);
        }
        self.emit_queue(&thread.id)?;

        // Tool policy: empty allowed_tools = all registered tools. The
        // engine-served ask_question tool always rides along (deferring to
        // the user is an interaction primitive, not a capability).
        let mut specs: Vec<ToolSpec> = self
            .executor
            .specs(&ctx)
            .await
            .into_iter()
            .filter(|s| mode.allowed_tools.is_empty() || mode.allowed_tools.contains(&s.name))
            .collect();
        specs.push(ask_question_spec());
        specs.push(search_transcript_spec());
        // Spawn tools are for top-level agents only: children don't get to
        // spawn grandchildren (also enforced at execution). They also respect
        // the mode's tool policy, so restrictive/read-only modes that don't
        // list them can't create branches or child agents.
        let spawn_allowed = |name: &str| {
            mode.allowed_tools.is_empty() || mode.allowed_tools.iter().any(|t| t == name)
        };
        if self.store.spawn_parent(&thread.id)?.is_none() {
            if spawn_allowed("spawn_thread") {
                specs.push(spawn_thread_spec());
            }
            if spawn_allowed("spawn_session") {
                specs.push(spawn_session_spec());
            }
            if spawn_allowed("spawn_thread") || spawn_allowed("spawn_session") {
                specs.push(spawn_output_spec());
            }
        }

        let system = context::system_prompt(&mode, self.config_dir.as_deref(), Path::new(&ws.path));
        let mut usage_total = Usage::default();
        // The last request's input size — the context-size proxy for
        // compaction. Summing per-iteration inputs (usage_total) would
        // over-count a multi-tool turn many-fold; the final request carries
        // the whole transcript, so its input is what "context size" means.
        let mut context_input_tokens = 0u64;
        // Becomes false when the loop ends because the model stopped calling
        // tools (or was cancelled); stays true only if we exhaust the
        // iteration budget mid-work, which we then surface to the user.
        let mut hit_iteration_limit = true;

        for _iteration in 0..MAX_ITERATIONS {
            if cancel.is_cancelled() {
                hit_iteration_limit = false;
                break;
            }
            // Rebuild the transcript each iteration; the store is the truth.
            let mut messages = vec![Message::System(system.clone())];
            for payload in self.store.messages(&thread.id)? {
                messages.push(serde_json::from_value(payload)?);
            }
            // Repair any tool_calls left without results by a crash/restart
            // mid-turn (and drop empty assistant turns); providers reject a
            // dangling tool_use/tool_call, which would wedge the thread.
            let messages = sanitize_transcript(messages);

            let mut stream = provider
                .stream_chat(&model_name, &messages, &specs, &model_options)
                .await
                .map_err(|e| anyhow!("provider error: {e}"))?;

            let mut text = String::new();
            let mut tool_calls = Vec::new();
            // Provider-native reasoning blocks (Anthropic signed thinking) to
            // persist and replay verbatim — Anthropic rejects a follow-up
            // tool-use turn whose thinking blocks aren't preserved.
            let mut reasoning: Vec<serde_json::Value> = Vec::new();
            loop {
                let ev = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    ev = stream.next() => match ev {
                        Some(ev) => ev,
                        None => break,
                    },
                };
                match ev.map_err(|e| anyhow!("provider stream error: {e}"))? {
                    ProviderEvent::TextDelta(delta) => {
                        text.push_str(&delta);
                        self.store.append_event(
                            scope.clone(),
                            Event::AssistantDelta { turn, text: delta },
                        )?;
                    }
                    // Display-only; never joins the provider transcript.
                    ProviderEvent::ThinkingDelta(delta) => {
                        self.store.append_event(
                            scope.clone(),
                            Event::AssistantThinking { turn, text: delta },
                        )?;
                    }
                    // Kept out of the UI (already streamed as ThinkingDelta);
                    // carried in the transcript for replay only.
                    ProviderEvent::Reasoning(block) => reasoning.push(block),
                    ProviderEvent::ToolCall(call) => tool_calls.push(call),
                    ProviderEvent::Completed { usage } => {
                        usage_total.input_tokens += usage.input_tokens;
                        usage_total.output_tokens += usage.output_tokens;
                        usage_total.cached_input_tokens += usage.cached_input_tokens;
                        context_input_tokens = usage.input_tokens + usage.cached_input_tokens;
                    }
                }
            }

            // Interrupted mid-stream: keep any streamed text for display, but
            // drop the (unexecuted) tool calls so we don't strand tool_use
            // without results, and stop the turn.
            if cancel.is_cancelled() {
                if !text.is_empty() {
                    self.store.append_event(
                        scope.clone(),
                        Event::AssistantMessage {
                            turn,
                            content: text.clone(),
                        },
                    )?;
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning,
                        })?,
                    )?;
                }
                hit_iteration_limit = false;
                break;
            }

            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            // Skip a fully-empty assistant message (no text, no tool calls —
            // e.g. a thinking-only or empty provider response): it serializes
            // to an empty content block that Anthropic rejects on the next
            // request, wedging the thread.
            if !text.is_empty() || !tool_calls.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: tool_calls.clone(),
                        reasoning,
                    })?,
                )?;
            }

            if tool_calls.is_empty() {
                hit_iteration_limit = false;
                break;
            }

            for call in tool_calls {
                let (result_content, images) = self
                    .handle_tool_call(&session, thread, turn, &mode, &ctx, &call, &cancel)
                    .await?;
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::ToolResult {
                        call_id: call.id.clone(),
                        content: result_content,
                        images,
                    })?,
                )?;
            }
        }

        // Truncated mid-work at the iteration budget: make one final
        // tool-free provider pass over the last tool results so the user gets
        // a truthful model-authored progress report rather than a completed
        // turn whose transcript ends at a tool result.
        if hit_iteration_limit {
            let mut messages = vec![Message::System(system.clone())];
            for payload in self.store.messages(&thread.id)? {
                messages.push(serde_json::from_value(payload)?);
            }
            let mut messages = sanitize_transcript(messages);
            messages.push(Message::User(format!(
                "You reached the hard {MAX_ITERATIONS}-step limit for this turn. Do not call any \
                 more tools. Give the user a concise progress report based on the tool results \
                 above, clearly identify unfinished work, and ask them to continue in a new turn."
            )));
            let mut final_text = String::new();
            let mut final_reasoning = Vec::new();
            match provider
                .stream_chat(&model_name, &messages, &[], &model_options)
                .await
            {
                Ok(mut stream) => {
                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(ProviderEvent::TextDelta(delta)) => {
                                final_text.push_str(&delta);
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantDelta { turn, text: delta },
                                )?;
                            }
                            Ok(ProviderEvent::ThinkingDelta(delta)) => {
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantThinking { turn, text: delta },
                                )?;
                            }
                            Ok(ProviderEvent::Reasoning(block)) => final_reasoning.push(block),
                            Ok(ProviderEvent::Completed { usage }) => {
                                usage_total.input_tokens += usage.input_tokens;
                                usage_total.output_tokens += usage.output_tokens;
                                usage_total.cached_input_tokens += usage.cached_input_tokens;
                                context_input_tokens =
                                    usage.input_tokens + usage.cached_input_tokens;
                            }
                            // Tools are deliberately unavailable on this
                            // final pass. Ignore a non-conforming provider's
                            // request and fall back to the explicit note.
                            Ok(ProviderEvent::ToolCall(_)) => {}
                            Err(e) => {
                                tracing::warn!("iteration-limit summary failed: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("iteration-limit summary failed: {e}"),
            }
            if final_text.trim().is_empty() {
                final_text = format!(
                    "Reached the {MAX_ITERATIONS}-step limit for one turn and stopped mid-task. \
                     Send another message to continue."
                );
            }
            self.store.append_event(
                scope.clone(),
                Event::AssistantMessage {
                    turn,
                    content: final_text.clone(),
                },
            )?;
            self.store.append_message(
                &thread.id,
                &serde_json::to_value(Message::Assistant {
                    content: final_text,
                    tool_calls: Vec::new(),
                    reasoning: final_reasoning,
                })?,
            )?;
        }

        // Dollar cost from the model catalog, when pricing is known.
        if let Some(model) = provider.models().iter().find(|m| m.id == thread.model) {
            usage_total.cost_usd = trouve_providers::catalog::cost_usd(
                model,
                usage_total.input_tokens,
                usage_total.output_tokens,
            );
        }
        self.store.record_usage(
            &session.id,
            &thread.id,
            turn,
            &usage_total,
            context_input_tokens,
        )?;

        // Snapshot the worktree when the turn changed it. Lock-free child
        // turns never snapshot: they can't write, so any dirt is the
        // parent's in-flight work — not theirs to checkpoint.
        let checkpoint_id = if concurrent_child {
            None
        } else {
            self.maybe_checkpoint(&session, thread, turn).await?
        };
        self.store.append_event(
            scope,
            Event::TurnCompleted {
                turn,
                usage: usage_total,
                checkpoint_id,
            },
        )?;
        Ok(())
    }

    /// Resolve a provider-qualified model id to a registered agent backend.
    fn backend_for(&self, model: &str) -> Option<(String, Arc<dyn AgentBackend>, String)> {
        let (backend_id, model_name) = model.split_once('/')?;
        let backend = self.backends.read().unwrap().get(backend_id).cloned()?;
        Some((backend_id.to_string(), backend, model_name.to_string()))
    }

    /// MCP tool-bridge config for a backend turn. Claude Code always gets
    /// the bridge (it carries the approval-prompt gate for Ask mode, and
    /// optionally — `tool_bridge = true` — trouve's tools in place of
    /// Claude's built-ins). Codex gets it too, for trouve's semantic search
    /// and question tools; its approvals stay native app-server RPCs.
    fn mcp_bridge_for(
        &self,
        model: &str,
        thread_id: &str,
    ) -> Option<trouve_agents::McpBridgeConfig> {
        let backend_id = model.split_once('/')?.0;
        let (kind, bridge_tools) = {
            let config = self.config.lock().unwrap();
            let pc = config.providers.get(backend_id)?;
            (pc.kind.clone(), pc.tool_bridge.unwrap_or(false))
        };
        if kind != "claude-cli" && kind != "codex-app-server" {
            return None;
        }
        // Full tool bridging (vendor built-ins stand down) is Claude-only.
        let bridge_tools = bridge_tools && kind == "claude-cli";
        let Some(base_url) = self.base_url.read().unwrap().clone() else {
            tracing::warn!(
                "MCP bridge wanted for {backend_id} but the server base URL is unknown; \
                 running without it (approvals will fail in ask mode)"
            );
            return None;
        };
        // Codex approvals are native RPCs; serving Claude's permission-gate
        // tool would only tempt the model to call it.
        let approval = if kind == "codex-app-server" { 0 } else { 1 };
        let mut url = format!(
            "{}/internal/threads/{}/mcp?tools={}&approval={}",
            base_url.trim_end_matches('/'),
            thread_id,
            bridge_tools as u8,
            approval,
        );
        if let Some(token) = self.bridge_token.read().unwrap().as_deref() {
            url.push_str("&bridge_token=");
            url.push_str(token);
        }
        Some(trouve_agents::McpBridgeConfig {
            url,
            bridge_tools,
            // Claude built-ins stand down; trouve's executor is the tool
            // source (reads included, for full permission fidelity).
            disallowed_tools: if bridge_tools {
                [
                    "Bash",
                    "Edit",
                    "Write",
                    "MultiEdit",
                    "NotebookEdit",
                    "WebFetch",
                    "WebSearch",
                    "Read",
                    "Glob",
                    "Grep",
                    "Task",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect()
            } else {
                Vec::new()
            },
        })
    }

    // --- bridged tools (MCP tool bridge, Phase 6) -----------------------------

    /// Tool specs for a thread, as exposed to a bridged vendor agent
    /// (filtered by the thread's mode, same as native turns).
    pub async fn bridged_tool_specs(&self, thread_id: &str) -> Result<Vec<ToolSpec>, EngineError> {
        let (_, _, mode, ctx) = self.bridged_context(thread_id)?;
        let mut specs: Vec<ToolSpec> = self
            .executor
            .specs(&ctx)
            .await
            .into_iter()
            .filter(|s| mode.allowed_tools.is_empty() || mode.allowed_tools.contains(&s.name))
            .collect();
        // Engine-served, always available (see handle_tool_call).
        specs.push(ask_question_spec());
        specs.push(search_transcript_spec());
        // Spawn tools: top-level agents only, same as native turns.
        if self.store.spawn_parent(thread_id)?.is_none() {
            specs.push(spawn_thread_spec());
            specs.push(spawn_session_spec());
            specs.push(spawn_output_spec());
        }
        Ok(specs)
    }

    /// Execute one tool call on behalf of a bridged vendor agent, through
    /// the same gate/approval/event chokepoint as native tool calls.
    pub async fn bridged_tool_call(
        self: &Arc<Self>,
        thread_id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, EngineError> {
        let (session, thread, mode, ctx) = self.bridged_context(thread_id)?;
        let turn = self.store.last_turn(thread_id)?;
        let call = trouve_providers::ToolCallRequest {
            id: new_id("call"),
            name: name.to_string(),
            arguments: arguments.clone(),
        };
        // Bridged responses are text-only (MCP content blocks could carry
        // images, but no bridged vendor consumes them yet); the summary the
        // engine leaves in place of "_images" still tells the model the
        // image was read.
        // Share the running turn's cancellation token when there is one, so a
        // cancel also unblocks a bridged tool's approval wait.
        let cancel = self
            .turn_cancels
            .lock()
            .unwrap()
            .get(thread_id)
            .cloned()
            .unwrap_or_default();
        let (content, _images) = self
            .handle_tool_call(&session, &thread, turn, &mode, &ctx, &call, &cancel)
            .await
            .map_err(EngineError::Internal)?;
        Ok(content)
    }

    /// Gate a vendor-side tool call (Claude Code's `--permission-prompt-tool`
    /// hook) through trouve's permission layer. The vendor executes the tool
    /// itself if allowed; we only decide and record the decision. The
    /// approval attaches to the tool card the vendor's stream already
    /// created (the `tool_use` block precedes the permission request); a
    /// synthetic card is the fallback when no open call matches.
    pub async fn bridged_approval(
        &self,
        thread_id: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<bool, EngineError> {
        let (session, thread, mode, _ctx) = self.bridged_context(thread_id)?;
        let turn = self
            .store
            .last_turn(thread_id)
            .map_err(EngineError::Internal)?;
        let scope = Scope::Thread(thread.id.clone());
        let matched = self.open_vendor_call(&thread.id, turn, tool, args);
        let synthetic = matched.is_none();
        let call_id = matched.unwrap_or_else(|| new_id("appr"));
        if synthetic {
            self.store
                .append_event(
                    scope.clone(),
                    Event::ToolRequested {
                        turn,
                        call_id: call_id.clone(),
                        tool: tool.to_string(),
                        args: args.clone(),
                        requires_approval: true,
                    },
                )
                .map_err(EngineError::Internal)?;
        }
        let approved = self
            .gate_backend_approval(&session, &thread, turn, &mode, &call_id, tool, args)
            .await
            .map_err(EngineError::Internal)?;
        // A matched card gets its completion from the vendor's own
        // tool_result; only the synthetic card needs closing here.
        if synthetic {
            self.store
                .append_event(
                    scope,
                    Event::ToolCompleted {
                        call_id,
                        status: if approved {
                            ToolStatus::Ok
                        } else {
                            ToolStatus::Denied
                        },
                        result: serde_json::json!(if approved { "approved" } else { "denied" }),
                    },
                )
                .map_err(EngineError::Internal)?;
        }
        Ok(approved)
    }

    /// The newest still-open vendor tool call in this turn that a
    /// permission request refers to: same tool, preferring an exact args
    /// match, never one already carrying an approval.
    fn open_vendor_call(
        &self,
        thread_id: &str,
        turn: u64,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<String> {
        let events = self
            .store
            .events_after(&Scope::Thread(thread_id.to_string()), 0)
            .ok()?;
        let mut open: Vec<(String, serde_json::Value)> = Vec::new();
        let mut gated: std::collections::HashSet<String> = Default::default();
        for env in &events {
            match &env.event {
                Event::ToolRequested {
                    turn: t,
                    call_id,
                    tool: name,
                    args: a,
                    ..
                } if *t == turn && name == tool => {
                    open.push((call_id.clone(), a.clone()));
                }
                Event::ToolCompleted { call_id, .. } => {
                    open.retain(|(id, _)| id != call_id);
                }
                Event::ApprovalRequested { call_id, .. } => {
                    gated.insert(call_id.clone());
                }
                _ => {}
            }
        }
        open.retain(|(id, _)| !gated.contains(id));
        // Stored args may carry injected "_line" display hints the vendor's
        // approval request doesn't have; ignore them when matching.
        let strip = |v: &serde_json::Value| {
            let mut v = v.clone();
            if let Some(map) = v.as_object_mut() {
                map.remove("_line");
                if let Some(edits) = map.get_mut("edits").and_then(|e| e.as_array_mut()) {
                    for e in edits {
                        if let Some(m) = e.as_object_mut() {
                            m.remove("_line");
                        }
                    }
                }
            }
            v
        };
        open.iter()
            .rev()
            .find(|(_, a)| strip(a) == *args)
            .or(open.last())
            .map(|(id, _)| id.clone())
    }

    /// Whether a `tool.requested` card already exists for this call in the
    /// current turn.
    fn tool_card_exists(&self, thread_id: &str, turn: u64, call_id: &str) -> bool {
        self.store
            .events_after(&Scope::Thread(thread_id.to_string()), 0)
            .ok()
            .is_some_and(|events| {
                events.iter().any(|env| {
                    matches!(
                        &env.event,
                        Event::ToolRequested {
                            turn: t,
                            call_id: id,
                            ..
                        } if *t == turn && id == call_id
                    )
                })
            })
    }

    /// Normalize a todo list from trouve's canonical shape or a supported
    /// vendor-native tool shape.
    fn parse_todo_snapshot(value: &serde_json::Value) -> Option<Vec<trouve_protocol::TodoItem>> {
        if let Ok(todos) = serde_json::from_value::<Vec<trouve_protocol::TodoItem>>(value.clone()) {
            return Some(todos);
        }

        // Claude's built-in TodoWrite omits ids and adds `activeForm`.
        // Normalize that vendor shape at the core boundary while keeping the
        // protocol's canonical TodoItem strict.
        value
            .as_array()?
            .iter()
            .map(|item| {
                let content = item.get("content")?.as_str()?.to_string();
                let status = serde_json::from_value(item.get("status")?.clone()).ok()?;
                let id = item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("vendor:{content}"));
                Some(trouve_protocol::TodoItem {
                    id,
                    content,
                    status,
                })
            })
            .collect()
    }

    /// Persist a successful todo tool's authoritative result snapshot, or
    /// its paired start arguments when a vendor only returns an acknowledgement.
    fn persist_todos_from_result(
        &self,
        thread_id: &str,
        tool: &str,
        status: ToolStatus,
        result: &serde_json::Value,
        args: Option<&serde_json::Value>,
    ) -> Result<Option<Vec<trouve_protocol::TodoItem>>> {
        let base = tool.rsplit("__").next().unwrap_or(tool);
        if status != ToolStatus::Ok || !matches!(base, "todo_write" | "TodoWrite") {
            return Ok(None);
        }
        let result_todos = result.get("todos").and_then(Self::parse_todo_snapshot);
        let (mut todos, merge) = match result_todos {
            // Native trouve tools return the authoritative full snapshot,
            // including after a merge update.
            Some(todos) => (todos, false),
            // Vendor-native TodoWrite tools commonly return only an
            // acknowledgement. Their started event still carries the
            // requested list, so use that as the snapshot fallback.
            None => {
                let Some(args) = args else {
                    return Ok(None);
                };
                let Some(todos) = args.get("todos").and_then(Self::parse_todo_snapshot) else {
                    return Ok(None);
                };
                (
                    todos,
                    args.get("merge")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                )
            }
        };
        if merge {
            let mut merged = self
                .store
                .thread(thread_id)?
                .map(|thread| thread.todos)
                .unwrap_or_default();
            for todo in todos {
                match merged.iter_mut().find(|existing| existing.id == todo.id) {
                    Some(existing) => *existing = todo,
                    None => merged.push(todo),
                }
            }
            todos = merged;
        }
        self.store.update_thread_todos(thread_id, &todos)?;
        Ok(Some(todos))
    }

    fn bridged_context(
        &self,
        thread_id: &str,
    ) -> Result<(Session, Thread, AgentMode, ToolCtx), EngineError> {
        let thread = self.get_thread(thread_id)?;
        let session = self.get_session(&thread.session_id)?;
        let ws = self
            .store
            .workspace(&session.workspace_id)
            .map_err(EngineError::Internal)?
            .ok_or_else(|| EngineError::NotFound("workspace".into()))?;
        let all_modes = modes::resolve_modes(self.config_dir.as_deref(), Some(Path::new(&ws.path)));
        let mode = modes::find_mode(&all_modes, &thread.mode)
            .cloned()
            .unwrap_or_else(modes::fallback_mode);
        let ctx = ToolCtx {
            worktree: PathBuf::from(&session.worktree_path),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&ws.path)),
        };
        Ok((session, thread, mode, ctx))
    }

    /// User-configured MCP servers for a session's worktree, flattened for
    /// a vendor agent CLI: scopes merged (user < workspace < worktree),
    /// disabled entries dropped, env `${VAR}` references expanded. The name
    /// "trouve" is reserved for the bridge and skipped.
    fn mcp_servers_for(
        &self,
        session: &Session,
    ) -> Result<Vec<trouve_agents::McpServerLaunch>, EngineError> {
        let workspace_root = self
            .store
            .workspace(&session.workspace_id)?
            .map(|ws| PathBuf::from(ws.path));
        // Only trusted (user-config) servers are handed to the vendor CLI:
        // it would otherwise spawn a cloned repo's command with the expanded
        // environment, same RCE/exfiltration risk as the native path.
        let configs = crate::mcp::trusted_configs(
            self.config_dir.as_deref(),
            workspace_root.as_deref(),
            Path::new(&session.worktree_path),
        );
        Ok(configs
            .into_iter()
            .filter(|(name, _)| name != "trouve")
            .map(|(name, config)| trouve_agents::McpServerLaunch {
                name,
                command: config.command,
                args: config.args,
                env: config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), crate::mcp::expand_env(v)))
                    .collect(),
            })
            .collect())
    }

    /// Run one turn through an external agent backend. The vendor harness
    /// plans, calls tools, and edits the worktree; we persist its events,
    /// gate its approval requests through our permission layer, and keep the
    /// checkpoint/usage flow identical to native turns. Compaction and the
    /// system prompt are the vendor's job (the mode prompt rides along as
    /// appended instructions); the local transcript is kept for rendering
    /// and history, not as the model's context.
    #[allow(clippy::too_many_arguments)]
    async fn run_backend_turn(
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentMode,
        backend_id: &str,
        backend: Arc<dyn AgentBackend>,
        model_name: String,
        content: String,
        attachments: Vec<trouve_protocol::Attachment>,
        concurrent_child: bool,
        cancel: tokio_util::sync::CancellationToken,
        queued_prompt_id: &str,
    ) -> Result<()> {
        let scope = Scope::Thread(thread.id.clone());
        // Vendor sessions are per (thread, backend): each vendor keeps its
        // own history, and switching models away and back resumes it.
        // Vendors can't read our transcript, so whatever part of the
        // thread's past this one hasn't seen — everything for a vendor
        // joining mid-conversation, the interleaved turns other models ran
        // for a resumed one — is handed off as a digest in the prompt.
        let resume = self.store.backend_session(&thread.id, backend_id)?;
        let payloads = self.store.messages(&thread.id)?;
        let unseen = match &resume {
            // A compaction can shrink the transcript below the watermark;
            // handing off the fresh summary again covers that.
            Some((_, seen)) => payloads.get(*seen as usize..).unwrap_or(&payloads),
            None => &payloads[..],
        };
        let handoff = {
            let messages: Vec<Message> = unseen
                .iter()
                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                .collect();
            render_history_digest(&messages, resume.is_some())
        };
        let vendor_session = resume.map(|(id, _)| id);
        // After this turn the vendor has seen everything up to and
        // including its own reply (appended below on completion).
        let seen_after = payloads.len() as u64 + 2;
        self.store.append_event(
            scope.clone(),
            Event::TurnStarted {
                turn,
                mode: mode.id.clone(),
                model: thread.model.clone(),
            },
        )?;
        self.store.append_event(
            scope.clone(),
            Event::UserMessage {
                turn,
                content: content.clone(),
                attachments: attachments.clone(),
            },
        )?;
        // Images go to the vendor protocol as native image inputs; other
        // files become path references in the prompt text (vendor agents
        // run on this filesystem and can read them with their tools).
        let resolved = self.resolve_attachments(&attachments);
        let (images, files): (Vec<_>, Vec<_>) = resolved
            .into_iter()
            .partition(|(a, _)| a.mime.starts_with("image/"));
        let content = annotate_attachments(content, &files);
        let turn_attachments: Vec<trouve_agents::TurnAttachment> = images
            .into_iter()
            .map(|(a, path)| trouve_agents::TurnAttachment {
                name: a.name,
                mime: a.mime,
                path,
            })
            .collect();
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::User(content.clone()))?,
        )?;
        if !self.store.finish_queued_prompt(queued_prompt_id)? {
            bail!("queued prompt {queued_prompt_id} vanished before turn start");
        }
        self.emit_queue(&thread.id)?;

        let permission = if mode.read_only {
            BackendPermission::ReadOnly
        } else {
            match thread.permission_mode {
                trouve_protocol::PermissionMode::Yolo => BackendPermission::Yolo,
                _ => BackendPermission::Ask,
            }
        };

        let mcp_bridge = self.mcp_bridge_for(&thread.model, &thread.id);
        // Vendor agents get the mode prompt plus, when the bridge serves
        // trouve's search tools, guidance to prefer them over built-ins
        // (MCP instructions alone are too weak a signal).
        let mut instructions = mode.system_prompt.trim().to_string();
        if mcp_bridge.is_some() {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(crate::tools::VENDOR_SEARCH_GUIDANCE);
        }
        // The digest decorates only the prompt sent to the vendor; the
        // stored transcript keeps the user's words alone.
        let prompt = match &handoff {
            Some(digest) => format!("{digest}\n\n{content}"),
            None => content,
        };
        let mut model_options = self.store.thread_model_options(&thread.id)?;
        let model_catalog = backend.list_models().await;
        normalize_thinking_option(
            &mut model_options,
            model_catalog.iter().find(|m| m.id == thread.model),
        );
        let backend_turn = BackendTurn {
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: vendor_session,
            model: model_name,
            model_options,
            prompt,
            attachments: turn_attachments,
            instructions: (!instructions.is_empty()).then_some(instructions),
            permission,
            mcp_bridge,
            mcp_servers: self.mcp_servers_for(session)?,
        };

        let mut stream = backend
            .run_turn(backend_turn)
            .await
            .map_err(|e| anyhow!("backend error: {e}"))?;

        // `text` records the whole turn for the transcript; `segment` is the
        // current streamed block, flushed (finalized) at each tool boundary
        // so tool cards interleave with the text in the order they happened
        // instead of all text merging into one leading bubble.
        let mut text = String::new();
        let mut segment = String::new();
        let mut usage_total = Usage::default();
        // Vendor-native todo tools are reported as ordinary tool events.
        // Remember their names until completion so their result can update
        // the same persisted snapshot as trouve's bridged/native tool.
        let mut tool_calls = HashMap::<String, (String, serde_json::Value)>::new();
        // Creation tools sometimes stream their final PR URL before the
        // completion payload. Buffer output only for calls whose request is
        // demonstrably creating a PR; list/view output must never associate
        // every PR it happens to mention with this session.
        let mut github_creation_output = HashMap::<String, String>::new();
        // A vendor may use any GitHub client instead of trouve's create-PR
        // endpoint. Turn repository-specific PR references in its output into
        // the same durable session event, independent of the tool name.
        let github_repository = self.github_repository_for_session(session).ok();
        let mut recorded_prs = if github_repository.is_some() {
            self.recorded_session_pr_numbers(&session.id)?
        } else {
            HashSet::new()
        };
        loop {
            let ev = tokio::select! {
                biased;
                // Cancellation drops the backend stream, whose Drop kills the
                // vendor process (kill_on_drop). We stop consuming and finish
                // the turn with whatever streamed so far.
                _ = cancel.cancelled() => break,
                ev = stream.next() => match ev {
                    Some(ev) => ev,
                    None => break,
                },
            };
            match ev.map_err(|e| anyhow!("backend stream error: {e}"))? {
                BackendEvent::SessionStarted { session_id } => {
                    self.store
                        .set_backend_session(&thread.id, backend_id, &session_id)?;
                }
                BackendEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    segment.push_str(&delta);
                    self.store
                        .append_event(scope.clone(), Event::AssistantDelta { turn, text: delta })?;
                }
                BackendEvent::ThinkingDelta(delta) => {
                    // Thinking is a block boundary like a tool call:
                    // finalize the streamed text so far so post-thinking
                    // text starts a new bubble in the right order.
                    if !segment.is_empty() {
                        self.store.append_event(
                            scope.clone(),
                            Event::AssistantMessage {
                                turn,
                                content: std::mem::take(&mut segment),
                            },
                        )?;
                    }
                    self.store.append_event(
                        scope.clone(),
                        Event::AssistantThinking { turn, text: delta },
                    )?;
                }
                BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    mut args,
                } => {
                    tool_calls.insert(call_id.clone(), (tool.clone(), args.clone()));
                    if !segment.is_empty() {
                        self.store.append_event(
                            scope.clone(),
                            Event::AssistantMessage {
                                turn,
                                content: std::mem::take(&mut segment),
                            },
                        )?;
                    }
                    // Snippet edits carry no position; the worktree file is
                    // still un-edited at announcement time, so resolve line
                    // hints now for the UI's diff gutter.
                    annotate_edit_lines(Path::new(&session.worktree_path), &mut args);
                    if !self.tool_card_exists(&thread.id, turn, &call_id) {
                        self.store.append_event(
                            scope.clone(),
                            Event::ToolRequested {
                                turn,
                                call_id: call_id.clone(),
                                tool,
                                args,
                                requires_approval: false,
                            },
                        )?;
                    }
                    self.store
                        .append_event(scope.clone(), Event::ToolStarted { call_id })?;
                }
                BackendEvent::ToolOutput { call_id, chunk } => {
                    if let Some((_, owner, repo)) = &github_repository
                        && let Some((tool, args)) = tool_calls.get(&call_id)
                        && requests_pull_request_creation(tool, args, owner, repo)
                    {
                        github_creation_output
                            .entry(call_id.clone())
                            .or_default()
                            .push_str(&chunk);
                    }
                    self.store
                        .append_event(scope.clone(), Event::ToolOutput { call_id, chunk })?;
                }
                BackendEvent::CommandsUpdated { commands } => {
                    self.store
                        .append_event(scope.clone(), Event::CommandsUpdated { commands })?;
                }
                BackendEvent::ToolCompleted {
                    call_id,
                    ok,
                    result,
                } => {
                    let status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Error
                    };
                    let todos = match tool_calls.get(&call_id) {
                        Some((tool, args)) => self.persist_todos_from_result(
                            &thread.id,
                            tool,
                            status,
                            &result,
                            Some(args),
                        )?,
                        None => None,
                    };
                    if ok
                        && let Some(repository @ (host, owner, repo)) = &github_repository
                        && let Some((tool, args)) = tool_calls.get(&call_id)
                        && requests_pull_request_creation(tool, args, owner, repo)
                    {
                        let mut numbers = pr_numbers_in_value(args, host, owner, repo);
                        numbers.extend(pr_numbers_in_value(&result, host, owner, repo));
                        if let Some(output) = github_creation_output.remove(&call_id) {
                            numbers.extend(crate::github::pr_numbers_in_text(
                                &output, host, owner, repo,
                            ));
                        }
                        self.record_session_pr_numbers(
                            &session.id,
                            repository,
                            numbers,
                            &mut recorded_prs,
                        )?;
                    } else {
                        github_creation_output.remove(&call_id);
                    }
                    self.store.append_event(
                        scope.clone(),
                        Event::ToolCompleted {
                            call_id,
                            status,
                            result,
                        },
                    )?;
                    if let Some(todos) = todos {
                        self.store
                            .append_event(scope.clone(), Event::TodosUpdated { todos })?;
                    }
                }
                BackendEvent::ApprovalNeeded {
                    call_id,
                    tool,
                    args,
                    responder,
                } => {
                    if !segment.is_empty() {
                        self.store.append_event(
                            scope.clone(),
                            Event::AssistantMessage {
                                turn,
                                content: std::mem::take(&mut segment),
                            },
                        )?;
                    }
                    let approved = self
                        .gate_backend_approval(session, thread, turn, mode, &call_id, &tool, &args)
                        .await?;
                    let _ = responder.send(approved);
                }
                BackendEvent::QuestionsNeeded {
                    request_id,
                    title,
                    questions,
                    responder,
                } => {
                    if !segment.is_empty() {
                        self.store.append_event(
                            scope.clone(),
                            Event::AssistantMessage {
                                turn,
                                content: std::mem::take(&mut segment),
                            },
                        )?;
                    }
                    let answers = self
                        .ask_user_questions(&thread.id, turn, &request_id, title, questions)
                        .await?;
                    let _ = responder.send(answers);
                }
                BackendEvent::Completed { usage } => {
                    usage_total.input_tokens += usage.input_tokens;
                    usage_total.output_tokens += usage.output_tokens;
                    usage_total.cached_input_tokens += usage.cached_input_tokens;
                    if let Some(cost) = usage.cost_usd {
                        usage_total.cost_usd = Some(usage_total.cost_usd.unwrap_or(0.0) + cost);
                    }
                    if usage.context_window.is_some() {
                        usage_total.context_window = usage.context_window;
                    }
                }
            }
        }
        // Drop the backend stream promptly so a cancelled turn kills the
        // vendor process now rather than at end of scope.
        drop(stream);

        if cancel.is_cancelled() {
            if !segment.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: segment,
                    },
                )?;
            }
            if !text.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            return Ok(());
        }

        if !segment.is_empty() {
            self.store.append_event(
                scope.clone(),
                Event::AssistantMessage {
                    turn,
                    content: segment,
                },
            )?;
        }
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::Assistant {
                content: text,
                tool_calls: Vec::new(),
                reasoning: Vec::new(),
            })?,
        )?;
        self.store
            .mark_backend_seen(&thread.id, backend_id, seen_after)?;

        // Vendors report one usage per turn, so the totals already reflect
        // the last (only) request — use them as the context-size proxy.
        let context_input_tokens = usage_total.input_tokens + usage_total.cached_input_tokens;
        self.store.record_usage(
            &session.id,
            &thread.id,
            turn,
            &usage_total,
            context_input_tokens,
        )?;
        // Lock-free children (read-only spawned agents) never checkpoint:
        // they hold no session lock, so `git add`/write-tree here would race
        // the parent's concurrent turn and snapshot its half-finished work as
        // the child's checkpoint. Matches the native path (invariant 4).
        let checkpoint_id = if concurrent_child {
            None
        } else {
            self.maybe_checkpoint(session, thread, turn).await?
        };
        self.store.append_event(
            scope,
            Event::TurnCompleted {
                turn,
                usage: usage_total,
                checkpoint_id,
            },
        )?;
        Ok(())
    }

    /// Gate one backend approval request through trouve's permission layer:
    /// allow-list hits auto-approve, read-only modes deny, otherwise ask the
    /// user through the ApprovalHub (same endpoints as native tool calls).
    #[allow(clippy::too_many_arguments)]
    async fn gate_backend_approval(
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentMode,
        call_id: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<bool> {
        // A vendor write aimed outside the session worktree is denied
        // without asking: the vendor executes the tool itself, so this is
        // the only place trouve can stop an edit from escaping into some
        // other checkout, and the approval card may render the path
        // worktree-relative — the user could approve without noticing.
        if let Some(path) =
            crate::permissions::escaping_write_path(tool, args, Path::new(&session.worktree_path))
        {
            tracing::warn!(
                "denied vendor tool {tool}: {path} is outside worktree {}",
                session.worktree_path
            );
            return Ok(false);
        }
        let scope = Scope::Thread(thread.id.clone());
        let key = allow_key(tool, args);
        // Bridged trouve tools are our own: trust the executor's mutability
        // flag so read-only tools (code search) pass even in read-only
        // modes. Anything else the vendor asks about is treated as mutating
        // (it only asks for things it considers mutating).
        let mutates = crate::mcp::split_tool_name(tool)
            .filter(|(server, _)| *server == "trouve")
            .and_then(|(_, name)| self.executor.tool_mutates(name))
            .unwrap_or(true);
        let decision = gate(
            thread.permission_mode,
            mode.read_only,
            mutates,
            &self.approvals.allow_list(&session.id),
            &key,
        );
        match decision {
            Gate::Allow => Ok(true),
            Gate::Deny => Ok(false),
            Gate::NeedsApproval => {
                // Cursor (and occasionally Codex) can ask for permission
                // before the tool_call announcement that normally creates the
                // card. Without a synthetic card the Approve/Deny UI has
                // nowhere to attach and the turn hangs forever.
                if !self.tool_card_exists(&thread.id, turn, call_id) {
                    let mut display_args = args.clone();
                    annotate_edit_lines(Path::new(&session.worktree_path), &mut display_args);
                    self.store.append_event(
                        scope.clone(),
                        Event::ToolRequested {
                            turn,
                            call_id: call_id.to_string(),
                            tool: tool.to_string(),
                            args: display_args,
                            requires_approval: true,
                        },
                    )?;
                }
                let cancel = self
                    .turn_cancels
                    .lock()
                    .unwrap()
                    .get(&thread.id)
                    .cloned()
                    .unwrap_or_default();
                let rx = self.approvals.request(call_id);
                self.store.append_event(
                    scope.clone(),
                    Event::ApprovalRequested {
                        turn,
                        call_id: call_id.to_string(),
                    },
                )?;
                // A cancelled turn must not hang on an unanswered approval.
                let decision = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => ApprovalDecision::Deny,
                    d = rx => d.unwrap_or(ApprovalDecision::Deny),
                };
                self.store.append_event(
                    scope,
                    Event::ApprovalResolved {
                        call_id: call_id.to_string(),
                        decision,
                    },
                )?;
                let unlocks_mcp_server =
                    decision == ApprovalDecision::Approve && key.starts_with("mcp:");
                if decision == ApprovalDecision::AlwaysApprove || unlocks_mcp_server {
                    // MCP approval is first-use per server and session: a
                    // plain approval unlocks this server, matching native MCP
                    // calls without broadening approval to other servers.
                    self.approvals.extend_allow_list(&session.id, key);
                }
                Ok(decision != ApprovalDecision::Deny)
            }
        }
    }

    /// Summarize the transcript into a single message when its estimated
    /// size crosses `COMPACTION_THRESHOLD` of the model's context window.
    async fn maybe_compact(
        &self,
        thread: &Thread,
        turn: u64,
        provider: &Arc<dyn Provider>,
        model_name: &str,
    ) -> Result<()> {
        // The live listing knows gateway models (kilocode, openrouter, ...)
        // the static catalog doesn't; it is cached, so this is cheap. A
        // model absent from both still compacts, against a conservative
        // default window — never compacting would let the transcript grow
        // until requests fail or the model degrades.
        let live = provider.list_models().await;
        let known = provider.models();
        let context_window = live
            .iter()
            .chain(known.iter())
            .find(|m| m.id == thread.model)
            .map(|m| m.context_window)
            .filter(|w| *w > 0)
            .unwrap_or(100_000);
        let payloads = self.store.messages(&thread.id)?;
        if payloads.len() < 2 {
            return Ok(());
        }
        // Prefer the provider-reported size of the last request; fall back
        // to the standard ~4 chars/token estimate over the raw transcript.
        let estimated_tokens = self.store.last_input_tokens(&thread.id)?.unwrap_or(0).max(
            payloads
                .iter()
                .map(|p| p.to_string().len() as u64)
                .sum::<u64>()
                / 4,
        );
        if (estimated_tokens as f64) < COMPACTION_THRESHOLD * context_window as f64 {
            return Ok(());
        }

        let scope = Scope::Thread(thread.id.clone());
        self.store
            .append_event(scope.clone(), Event::CompactionStarted { turn })?;

        let mut messages: Vec<Message> = vec![Message::System(
            "You are compacting an AI coding session transcript. Produce a dense summary \
             that preserves: the user's goals and constraints, decisions made, files \
             created/modified and how, commands run and their outcomes, current state, \
             unresolved problems, and what should happen next. Write it so the assistant \
             can seamlessly continue the session from the summary alone."
                .into(),
        )];
        for payload in &payloads {
            messages.push(serde_json::from_value(payload.clone())?);
        }
        messages = sanitize_transcript(messages);
        messages.push(Message::User(
            "Summarize the conversation so far per your instructions.".into(),
        ));

        let mut stream = provider
            .stream_chat(model_name, &messages, &[], &serde_json::Map::new())
            .await
            .map_err(|e| anyhow!("compaction provider error: {e}"))?;
        let mut summary = String::new();
        while let Some(ev) = stream.next().await {
            if let ProviderEvent::TextDelta(delta) =
                ev.map_err(|e| anyhow!("compaction stream error: {e}"))?
            {
                summary.push_str(&delta);
            }
        }
        if summary.trim().is_empty() {
            anyhow::bail!("compaction produced an empty summary");
        }

        let replacement = serde_json::to_value(Message::User(format!(
            "[Context was compacted. Older turns were summarized below; exact details \
             (error text, file paths, command output) are recoverable with the \
             search_transcript tool.]\n\n{summary}"
        )))?;
        self.store.replace_messages(&thread.id, &[replacement])?;
        self.store.append_event(
            scope,
            Event::CompactionCompleted {
                turn,
                messages_compacted: payloads.len() as u64,
            },
        )?;
        Ok(())
    }

    /// Gate, (maybe) get approval for, and execute one tool call. Returns the
    /// content fed back to the model.
    #[allow(clippy::too_many_arguments)]
    async fn handle_tool_call(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentMode,
        ctx: &ToolCtx,
        call: &trouve_providers::ToolCallRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(String, Vec<trouve_providers::ToolImage>)> {
        let scope = Scope::Thread(thread.id.clone());
        let call_id = if call.id.is_empty() {
            new_id("call")
        } else {
            call.id.clone()
        };

        // ask_question is engine-served (it blocks on the QuestionHub, which
        // tools can't reach) and never gated — asking is how the agent defers
        // to the user, so it works even in read-only modes. No tool card is
        // emitted: the question wizard is its representation in the UI.
        if call.name == "ask_question" {
            let result = match parse_question_args(&call.arguments) {
                Ok((title, questions)) => {
                    let answers = self
                        .ask_user_questions(&thread.id, turn, &call_id, title, questions.clone())
                        .await?;
                    question_result_json(&questions, answers)
                }
                Err(e) => serde_json::json!({ "error": e }),
            };
            return Ok((result.to_string(), Vec::new()));
        }

        // The spawn family and transcript search are engine-served too
        // (child agents and cross-thread history need the store and turn
        // dispatch, which tools can't reach). Unlike ask_question they do
        // get tool cards — these are real, visible actions. Errors become
        // tool results, never turn failures.
        if matches!(
            call.name.as_str(),
            "spawn_thread" | "spawn_session" | "spawn_output" | "search_transcript"
        ) {
            self.store.append_event(
                scope.clone(),
                Event::ToolRequested {
                    turn,
                    call_id: call_id.clone(),
                    tool: call.name.clone(),
                    args: call.arguments.clone(),
                    requires_approval: false,
                },
            )?;
            let outcome = if call.name == "search_transcript" {
                self.handle_search_transcript(session, thread, &call.arguments)
            } else {
                self.handle_spawn_tool(session, thread, mode, &call.name, &call.arguments)
                    .await
            };
            let result = match outcome {
                Ok(v) => v,
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };
            let status = if result.get("error").is_some() {
                ToolStatus::Error
            } else {
                ToolStatus::Ok
            };
            self.store.append_event(
                scope,
                Event::ToolCompleted {
                    call_id,
                    status,
                    result: result.clone(),
                },
            )?;
            return Ok((result.to_string(), Vec::new()));
        }

        let known = self.executor.tool_mutates(&call.name);
        let allowed_by_mode =
            mode.allowed_tools.is_empty() || mode.allowed_tools.contains(&call.name);
        let mutates = known.unwrap_or(true);
        let key = allow_key(&call.name, &call.arguments);
        let decision = if known.is_none() || !allowed_by_mode {
            Gate::Deny
        } else {
            gate(
                thread.permission_mode,
                mode.read_only,
                mutates,
                &self.approvals.allow_list(&session.id),
                &key,
            )
        };

        // Display copy of the args: snippet edits (edit_file) pick up a
        // "_line" hint locating the old text in the pre-edit file, so the
        // UI diff can number its gutter. Stored/executed args stay pristine.
        let mut display_args = call.arguments.clone();
        annotate_edit_lines(Path::new(&session.worktree_path), &mut display_args);
        self.store.append_event(
            scope.clone(),
            Event::ToolRequested {
                turn,
                call_id: call_id.clone(),
                tool: call.name.clone(),
                args: display_args,
                requires_approval: decision == Gate::NeedsApproval,
            },
        )?;

        let decision = match decision {
            Gate::Deny => {
                self.store.append_event(
                    scope.clone(),
                    Event::ToolCompleted {
                        call_id: call_id.clone(),
                        status: ToolStatus::Denied,
                        result: serde_json::json!({
                            "error": "tool not permitted in this mode"
                        }),
                    },
                )?;
                return Ok((
                    "Tool call denied: not permitted in this mode.".into(),
                    Vec::new(),
                ));
            }
            Gate::NeedsApproval => {
                let rx = self.approvals.request(&call_id);
                self.store.append_event(
                    scope.clone(),
                    Event::ApprovalRequested {
                        turn,
                        call_id: call_id.clone(),
                    },
                )?;
                // A cancelled turn must not hang on an unanswered approval:
                // treat cancellation as a denial so the wait unblocks.
                let decision = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => ApprovalDecision::Deny,
                    d = rx => d.unwrap_or(ApprovalDecision::Deny),
                };
                self.store.append_event(
                    scope.clone(),
                    Event::ApprovalResolved {
                        call_id: call_id.clone(),
                        decision,
                    },
                )?;
                let unlocks_mcp_server =
                    decision == ApprovalDecision::Approve && key.starts_with("mcp:");
                if decision == ApprovalDecision::AlwaysApprove || unlocks_mcp_server {
                    // MCP approval is per server per session (first use).
                    self.approvals.extend_allow_list(&session.id, key);
                }
                decision
            }
            Gate::Allow => ApprovalDecision::Approve,
        };

        if decision == ApprovalDecision::Deny {
            self.store.append_event(
                scope.clone(),
                Event::ToolCompleted {
                    call_id: call_id.clone(),
                    status: ToolStatus::Denied,
                    result: serde_json::json!({"error": "denied by user"}),
                },
            )?;
            return Ok(("Tool call denied by the user.".into(), Vec::new()));
        }

        self.store.append_event(
            scope.clone(),
            Event::ToolStarted {
                call_id: call_id.clone(),
            },
        )?;
        let mut outcome = self
            .executor
            .execute(ctx, &call.name, &call.arguments)
            .await;
        // Peel vision content ("_images") out of the result: megabytes of
        // base64 must not land in the event log or the text transcript —
        // it becomes native image input on the tool-result message instead.
        let images = take_tool_images(&mut outcome.result);
        let todos = self.persist_todos_from_result(
            &thread.id,
            &call.name,
            outcome.status,
            &outcome.result,
            Some(&call.arguments),
        )?;
        if matches!(outcome.status, ToolStatus::Ok)
            && let Ok(repository) = self.github_repository_for_session(session)
            && requests_pull_request_creation(
                &call.name,
                &call.arguments,
                &repository.1,
                &repository.2,
            )
        {
            let (host, owner, repo) = &repository;
            let mut recorded_prs = self.recorded_session_pr_numbers(&session.id)?;
            let mut numbers = pr_numbers_in_value(&call.arguments, host, owner, repo);
            numbers.extend(pr_numbers_in_value(&outcome.result, host, owner, repo));
            self.record_session_pr_numbers(&session.id, &repository, numbers, &mut recorded_prs)?;
        }
        self.store.append_event(
            scope.clone(),
            Event::ToolCompleted {
                call_id,
                status: outcome.status,
                result: outcome.result.clone(),
            },
        )?;
        if let Some(todos) = todos {
            self.store
                .append_event(scope, Event::TodosUpdated { todos })?;
        }
        Ok((outcome.result.to_string(), images))
    }

    /// The spawn tool family: `spawn_thread` starts a child agent on a new
    /// thread in the caller's session, `spawn_session` starts one in a fresh
    /// worktree session branched from the caller's branch, and
    /// `spawn_output` reports (and optionally waits for) a child's result.
    /// Guardrails: children never spawn grandchildren, at most
    /// `MAX_CONCURRENT_CHILDREN` children run at once, children inherit the
    /// parent's permission mode, and read-only parents can't escalate a
    /// child into a writing mode.
    async fn handle_spawn_tool(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        mode: &AgentMode,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        const MAX_CONCURRENT_CHILDREN: usize = 4;

        if name == "spawn_output" {
            let child_id = args
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .context("thread_id is required")?;
            // Only the spawner may collect: child output can hold anything
            // the child read, so it stays within the parent's thread.
            if self.store.spawn_parent(child_id)?.as_deref() != Some(thread.id.as_str()) {
                bail!("thread {child_id} is not a child of this thread");
            }
            let wait_ms = args
                .get("wait_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                .min(180_000);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
            loop {
                let status = self.spawn_status(child_id)?;
                let running = status["status"] == "running";
                if !running || std::time::Instant::now() >= deadline {
                    return Ok(status);
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }

        // Depth guard: one level only. Fan-out stays useful; runaway
        // recursive spawning does not. Checked before the mode policy so a
        // child always gets the depth message regardless of its mode.
        if self.store.spawn_parent(&thread.id)?.is_some() {
            bail!("spawned agents cannot spawn further agents");
        }

        // Respect the mode's tool policy: a restrictive/read-only mode that
        // doesn't list the spawn tool can't create branches or child agents
        // (the specs are already filtered, but a model may still emit the
        // call — deny it here too).
        if !(mode.allowed_tools.is_empty() || mode.allowed_tools.iter().any(|t| t == name)) {
            bail!("{name} is not permitted in {} mode", mode.id);
        }
        let children = self.store.spawned_children(&thread.id)?;
        {
            let active = self.active_threads.lock().unwrap();
            let running = children.iter().filter(|c| active.contains_key(*c)).count();
            if running >= MAX_CONCURRENT_CHILDREN {
                bail!("already {running} children running; collect some with spawn_output first");
            }
        }

        let prompt = args
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .context("prompt is required")?;
        let child_mode = args
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&thread.mode)
            .to_string();
        // A read-only parent must not launch an agent that can do what it
        // itself cannot.
        if mode.read_only && child_mode != thread.mode {
            bail!("read-only modes can only spawn children in the same mode");
        }
        let child_model = args
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&thread.model)
            .to_string();
        if !child_model.contains('/') {
            bail!("model must be provider-qualified (e.g. openai/gpt-4.1-mini): {child_model}");
        }
        // Same model: the parent's option choices (thinking level, …) carry
        // over. A different model validates its own options; start clean.
        let model_options = if child_model == thread.model {
            self.store.thread_model_options(&thread.id)?
        } else {
            serde_json::Map::new()
        };

        let (child_session_id, extra) = if name == "spawn_session" {
            let title = args
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .unwrap_or_else(|| {
                    let snippet: String = prompt
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(48)
                        .collect();
                    format!("Agent: {snippet}")
                });
            // Base the child on the parent's latest checkpoint commit, not
            // its branch: turn checkpoints are written to hidden refs and
            // never move the session branch, so basing on the branch would
            // show the child none of the parent's work. Fall back to the
            // branch when there is no checkpoint yet.
            let base_ref = match self.store.latest_checkpoint_seq(&session.id)? {
                Some(seq) => self
                    .store
                    .checkpoint_at(&session.id, seq)?
                    .map(|c| c.commit_hash)
                    .unwrap_or_else(|| session.branch.clone()),
                None => session.branch.clone(),
            };
            let child_session = self
                .create_session(CreateSessionRequest {
                    workspace_id: session.workspace_id.clone(),
                    title: Some(title),
                    base_ref: Some(base_ref.clone()),
                    checkout_ref: None,
                    fetch_latest: true,
                })
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
            let extra = serde_json::json!({
                "branch": child_session.branch,
                "based_on": base_ref,
                "worktree": child_session.worktree_path,
            });
            (child_session.id, Some(extra))
        } else {
            (session.id.clone(), None)
        };

        let child = self
            .create_thread(CreateThreadRequest {
                session_id: child_session_id.clone(),
                mode: Some(child_mode),
                model: Some(child_model),
                model_options,
                permission_mode: Some(thread.permission_mode),
            })
            .map_err(|e| anyhow!(e.to_string()))?;
        let kind = if name == "spawn_session" {
            "session"
        } else {
            "thread"
        };
        self.store.insert_spawned(&child.id, &thread.id, kind)?;
        self.send_message(&child.id, prompt.to_string(), Vec::new())
            .map_err(|e| anyhow!(e.to_string()))?;

        let mut result = serde_json::json!({
            "thread_id": child.id,
            "session_id": child_session_id,
            "note": "child agent started; check on it with spawn_output",
        });
        if let Some(extra) = extra {
            for (k, v) in extra.as_object().unwrap() {
                result[k] = v.clone();
            }
        }
        Ok(result)
    }

    /// A child agent's status, folded from its event log: running (its
    /// dispatcher is live), failed (last turn errored), completed (ran and
    /// idle), or pending (never ran). Includes the latest assistant message
    /// and aggregate token usage so the parent sees what its money bought.
    fn spawn_status(&self, thread_id: &str) -> Result<serde_json::Value> {
        let running = self.active_threads.lock().unwrap().contains_key(thread_id);
        let mut last_message = String::new();
        let mut completed_turns = 0u64;
        let mut failure: Option<String> = None;
        for envelope in self
            .store
            .events_after(&Scope::Thread(thread_id.to_string()), 0)?
        {
            match envelope.event {
                Event::AssistantMessage { content, .. } => last_message = content,
                Event::TurnCompleted { .. } => {
                    completed_turns += 1;
                    failure = None;
                }
                Event::TurnFailed { error, .. } => failure = Some(error),
                _ => {}
            }
        }
        let status = if running {
            "running"
        } else if failure.is_some() {
            "failed"
        } else if completed_turns > 0 {
            "completed"
        } else {
            "pending"
        };
        let usage = self
            .store
            .usage_summary(crate::store::UsageScope::Thread(thread_id))?;
        let mut out = serde_json::json!({
            "thread_id": thread_id,
            "status": status,
            "turns": completed_turns,
            "last_message": last_message,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cost_usd": usage.cost_usd,
            },
        });
        if let Some(error) = failure {
            out["error"] = serde_json::json!(error);
        }
        Ok(out)
    }

    /// The engine-served `search_transcript` tool: recover details that
    /// compaction summarized away or a handoff digest elided. Query mode
    /// returns turn-stamped snippets from the stored event log (user and
    /// assistant messages plus tool results — already image-stripped and
    /// bounded); turn mode reads one turn's messages in full. Scoped to the
    /// current thread by default, opt-in to the session or workspace —
    /// never across workspaces.
    fn handle_search_transcript(
        &self,
        session: &Session,
        thread: &Thread,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        const MAX_MATCHES: usize = 20;
        const SNIPPET_RADIUS: usize = 120;
        const TURN_ITEM_CAP: usize = 2_000;

        // Turn mode: read one turn in full (found via a prior search).
        if let Some(turn) = args.get("turn").and_then(serde_json::Value::as_u64) {
            let target = args
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&thread.id);
            let t = self
                .get_thread(target)
                .map_err(|e| anyhow!(e.to_string()))?;
            let s = self
                .get_session(&t.session_id)
                .map_err(|e| anyhow!(e.to_string()))?;
            if s.workspace_id != session.workspace_id {
                bail!("thread {target} is outside this workspace");
            }
            let mut calls: std::collections::HashMap<String, u64> = Default::default();
            let mut messages = Vec::new();
            for env in self
                .store
                .events_after(&Scope::Thread(target.to_string()), 0)?
            {
                let item = match env.event {
                    Event::UserMessage {
                        turn: t, content, ..
                    } if t == turn => {
                        serde_json::json!({"role": "user", "content": cap_chars(&content, TURN_ITEM_CAP)})
                    }
                    Event::AssistantMessage { turn: t, content } if t == turn => {
                        serde_json::json!({"role": "assistant", "content": cap_chars(&content, TURN_ITEM_CAP)})
                    }
                    Event::ToolRequested {
                        turn: t,
                        call_id,
                        tool,
                        args,
                        ..
                    } => {
                        if t != turn {
                            continue;
                        }
                        calls.insert(call_id, t);
                        serde_json::json!({"role": "tool_call", "tool": tool,
                            "args": cap_chars(&args.to_string(), TURN_ITEM_CAP)})
                    }
                    Event::ToolCompleted {
                        call_id, result, ..
                    } if calls.contains_key(&call_id) => {
                        serde_json::json!({"role": "tool_result",
                            "content": cap_chars(&result.to_string(), TURN_ITEM_CAP)})
                    }
                    Event::TurnFailed { turn: t, error } if t == turn => {
                        serde_json::json!({"role": "error", "content": error})
                    }
                    _ => continue,
                };
                messages.push(item);
            }
            if messages.is_empty() {
                bail!("no messages for turn {turn} of thread {target}");
            }
            return Ok(serde_json::json!({
                "thread_id": target,
                "turn": turn,
                "messages": messages,
            }));
        }

        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .context("query is required (or pass turn to read one turn in full)")?;
        let needle = query.to_lowercase();
        let scope = args
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("thread");
        let thread_ids: Vec<String> = match scope {
            "thread" => vec![thread.id.clone()],
            "session" => self
                .store
                .list_threads(&session.id)?
                .into_iter()
                .map(|t| t.id)
                .collect(),
            "workspace" => {
                let mut ids = Vec::new();
                for s in self.store.list_sessions(Some(&session.workspace_id))? {
                    ids.extend(self.store.list_threads(&s.id)?.into_iter().map(|t| t.id));
                }
                ids
            }
            other => bail!("unknown scope: {other} (thread | session | workspace)"),
        };

        let mut matches = Vec::new();
        let mut truncated = false;
        'threads: for tid in &thread_ids {
            let mut calls: std::collections::HashMap<String, u64> = Default::default();
            for env in self.store.events_after(&Scope::Thread(tid.clone()), 0)? {
                let (turn, role, text) = match &env.event {
                    Event::UserMessage { turn, content, .. } => (*turn, "user", content.clone()),
                    Event::AssistantMessage { turn, content } => {
                        (*turn, "assistant", content.clone())
                    }
                    Event::ToolRequested { turn, call_id, .. } => {
                        calls.insert(call_id.clone(), *turn);
                        continue;
                    }
                    Event::ToolCompleted {
                        call_id, result, ..
                    } => {
                        let Some(turn) = calls.get(call_id) else {
                            continue;
                        };
                        (*turn, "tool", result.to_string())
                    }
                    _ => continue,
                };
                let Some(at) = text.to_lowercase().find(&needle) else {
                    continue;
                };
                if matches.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'threads;
                }
                let start = floor_char_boundary(&text, at.saturating_sub(SNIPPET_RADIUS));
                let end =
                    ceil_char_boundary(&text, (at + needle.len() + SNIPPET_RADIUS).min(text.len()));
                let mut snippet = String::new();
                if start > 0 {
                    snippet.push('…');
                }
                snippet.push_str(&text[start..end]);
                if end < text.len() {
                    snippet.push('…');
                }
                matches.push(serde_json::json!({
                    "thread_id": tid,
                    "turn": turn,
                    "role": role,
                    "ts": env.ts.to_rfc3339(),
                    "snippet": snippet,
                }));
            }
        }
        Ok(serde_json::json!({
            "query": query,
            "scope": scope,
            "matches": matches,
            "truncated": truncated,
            "hint": "pass {thread_id, turn} to read a matched turn in full",
        }))
    }

    async fn maybe_checkpoint(
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
    ) -> Result<Option<String>> {
        let worktree = PathBuf::from(&session.worktree_path);
        let dirty = {
            let wt = worktree.clone();
            tokio::task::spawn_blocking(move || git::has_changes(&wt)).await??
        };
        if !dirty {
            return Ok(None);
        }
        let seq = self.store.latest_checkpoint_seq(&session.id)?.unwrap_or(-1) + 1;
        let commit = {
            let wt = worktree.clone();
            let sid = session.id.clone();
            let msg = format!("trouve: turn {turn} of {}", thread.id);
            tokio::task::spawn_blocking(move || git::checkpoint(&wt, &sid, seq, &msg)).await??
        };
        let checkpoint_id = new_id("cp");
        self.store.append_checkpoint(&CheckpointRow {
            id: checkpoint_id.clone(),
            session_id: session.id.clone(),
            thread_id: Some(thread.id.clone()),
            turn,
            seq,
            commit_hash: commit.clone(),
        })?;
        self.store.append_event(
            Scope::Session(session.id.clone()),
            Event::CheckpointCreated {
                checkpoint_id: checkpoint_id.clone(),
                thread_id: thread.id.clone(),
                turn,
                commit,
            },
        )?;
        Ok(Some(checkpoint_id))
    }
}

/// Annotate snippet-edit tool args (old/new string pairs, as sent by
/// Claude's Edit/MultiEdit and cursor's ACP edit calls) with the 1-based
/// line where each edit applies, resolved by locating the old text in the
/// pre-edit worktree file. Vendor agents apply the edit themselves right
/// after announcing it, so this is the one moment the position is knowable.
/// The hint rides in the args as `"_line"` — display metadata for the
/// client's diff gutter, never model input. Files that can't be read or
/// snippets that don't match (or match ambiguously) just skip the hint.
/// Index of the best file to suggest from a repo's GGUFs: prefer usable
/// quants that fit the GPU over CPU-only over too-large, then the best
/// quality/size trade-off quant (the catalog's Q4_K_M-class default),
/// then the smaller file.
fn recommend_gguf(files: &[trouve_protocol::LocalSearchFile]) -> usize {
    // Sub-3-bit quants are a last resort no matter what they fit on —
    // quality falls off a cliff below ~3 bits.
    fn junk_quant(quant: &str) -> bool {
        quant.starts_with("IQ1") || quant.starts_with("IQ2") || quant.starts_with("Q2")
    }
    fn quant_rank(quant: &str) -> usize {
        const PREF: &[&str] = &[
            "Q4_K_M", "Q4_K_S", "Q5_K_M", "IQ4_XS", "Q4_0", "Q5_K_S", "Q5_0", "Q6_K", "Q3_K_M",
            "Q8_0", "Q3_K_S", "IQ3_XS", "IQ3_M", "Q2_K", "F16", "BF16", "F32",
        ];
        PREF.iter().position(|p| *p == quant).unwrap_or(PREF.len())
    }
    fn fit_rank(fit: &str) -> usize {
        match fit {
            "gpu" => 0,
            "cpu" => 1,
            _ => 2,
        }
    }
    files
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| {
            (
                junk_quant(&f.quant),
                fit_rank(&f.fit),
                quant_rank(&f.quant),
                f.size_bytes,
            )
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Append path references for attachments that can't ride natively in the
/// model input, so the agent can open them with its file tools.
/// Ceiling on the handoff digest, in characters (~6k tokens). Compaction
/// keeps most transcripts under this; anything longer loses its middle —
/// the opening (goals, often a compaction summary) and the recent tail
/// matter most.
const HISTORY_DIGEST_MAX: usize = 24_000;

/// Render stored transcript messages into a handoff preamble for a vendor
/// backend that hasn't seen them: everything, for a vendor joining a
/// thread mid-conversation (`resumed` false); just the interleaved turns
/// other models ran, for one being resumed after a model swap (`resumed`
/// true). Tool results are omitted — their effects live in the worktree,
/// which the vendor can inspect. Returns None when there is nothing to
/// hand off.
fn render_history_digest(messages: &[Message], resumed: bool) -> Option<String> {
    let mut body = String::new();
    for message in messages {
        let block = match message {
            Message::User(text) => format!("User:\n{}", text.trim()),
            Message::Assistant { content, .. } if !content.trim().is_empty() => {
                format!("Assistant:\n{}", content.trim())
            }
            Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                let names: Vec<&str> = tool_calls.iter().map(|c| c.name.as_str()).collect();
                format!("Assistant: [ran tools: {}]", names.join(", "))
            }
            _ => continue,
        };
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&block);
    }
    if body.is_empty() {
        return None;
    }
    if body.len() > HISTORY_DIGEST_MAX {
        let head = floor_char_boundary(&body, HISTORY_DIGEST_MAX / 4);
        let tail = ceil_char_boundary(&body, body.len() - (HISTORY_DIGEST_MAX - head));
        body = format!(
            "{}\n\n[... earlier conversation truncated — recover specifics with the \
             search_transcript tool ...]\n\n{}",
            &body[..head],
            &body[tail..]
        );
    }
    let header = if resumed {
        "[Handoff: since your last turn in this conversation, the turns below were \
         handled by a different assistant or model. Catch up from this digest and \
         continue seamlessly — do not greet the user or restate the history.]"
    } else {
        "[Handoff: you are continuing an existing conversation. Earlier turns may have \
         been handled by a different assistant or model; a digest of the conversation so \
         far follows. Continue seamlessly from it — do not greet the user or restate the \
         history.]"
    };
    Some(format!(
        "{header}\n\n{body}\n\n[End of digest. The user's current message follows.]"
    ))
}

/// Truncate to at most `max` bytes on a char boundary, marking the cut.
fn cap_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = floor_char_boundary(s, max);
    format!("{}… [truncated]", &s[..end])
}

/// Largest index `<= at` that lands on a char boundary.
fn floor_char_boundary(s: &str, mut at: usize) -> usize {
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Smallest index `>= at` that lands on a char boundary.
fn ceil_char_boundary(s: &str, mut at: usize) -> usize {
    while at < s.len() && !s.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// Copy prompt attachments into the session worktree (under a gitignored
/// `.trouve/attachments/` dir) so the native file tools — which only open
/// worktree-relative paths — can read them. Returns each attachment paired
/// with its worktree-relative path. Failures drop that attachment with a
/// warning rather than failing the turn.
fn materialize_attachments(
    worktree: &Path,
    files: &[(trouve_protocol::Attachment, PathBuf)],
) -> Vec<(trouve_protocol::Attachment, PathBuf)> {
    if files.is_empty() {
        return Vec::new();
    }
    let rel_dir = Path::new(".trouve").join("attachments");
    let abs_dir = worktree.join(&rel_dir);
    if let Err(e) = std::fs::create_dir_all(&abs_dir) {
        tracing::warn!("cannot stage attachments in {}: {e}", abs_dir.display());
        return Vec::new();
    }
    // Keep the staged files out of the user's diffs/commits.
    let _ = std::fs::write(worktree.join(".trouve").join(".gitignore"), "*\n");
    let mut out = Vec::new();
    for (meta, src) in files {
        // Prefix with the id so distinct attachments with the same filename
        // don't collide.
        let file_name = format!("{}-{}", meta.id, meta.name);
        let rel = rel_dir.join(&file_name);
        match std::fs::copy(src, worktree.join(&rel)) {
            Ok(_) => out.push((meta.clone(), rel)),
            Err(e) => tracing::warn!("cannot stage attachment {}: {e}", meta.name),
        }
    }
    out
}

fn annotate_attachments(
    content: String,
    files: &[(trouve_protocol::Attachment, PathBuf)],
) -> String {
    if files.is_empty() {
        return content;
    }
    let mut out = content;
    out.push_str(
        "\n\nThe user attached these files (read them with the file tools at the paths shown):",
    );
    for (a, path) in files {
        out.push_str(&format!("\n- {} ({}): {}", a.name, a.mime, path.display()));
    }
    out
}

/// Remove the `_images` vision payload from a tool result, leaving a small
/// summary in its place (the event log and text transcript stay lean; the
/// images travel on the provider message as native vision content).
fn take_tool_images(result: &mut serde_json::Value) -> Vec<trouve_providers::ToolImage> {
    let Some(payload) = result.as_object_mut().and_then(|o| o.remove("_images")) else {
        return Vec::new();
    };
    let images: Vec<trouve_providers::ToolImage> =
        serde_json::from_value(payload).unwrap_or_default();
    if !images.is_empty() {
        result["images"] = serde_json::json!(
            images
                .iter()
                .map(|img| {
                    serde_json::json!({
                        "mime": img.mime,
                        // Base64 expands bytes 4:3; report the real size.
                        "bytes": img.data.len() * 3 / 4,
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    images
}

/// Repository-local PR numbers found recursively in structured tool data.
fn pr_numbers_in_value(
    value: &serde_json::Value,
    host: &str,
    owner: &str,
    repo: &str,
) -> HashSet<u64> {
    let mut numbers = HashSet::new();
    match value {
        serde_json::Value::String(text) => {
            numbers.extend(crate::github::pr_numbers_in_text(text, host, owner, repo));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                numbers.extend(pr_numbers_in_value(item, host, owner, repo));
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                numbers.extend(pr_numbers_in_value(value, host, owner, repo));
            }
        }
        _ => {}
    }
    numbers
}

/// Full SHA-1 or SHA-256 commit IDs present as tokens in text.
fn git_commit_ids_in_text(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_ascii_hexdigit())
        .filter(|token| matches!(token.len(), 40 | 64))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Full git commit IDs found recursively in structured tool data.
fn git_commit_ids_in_value(value: &serde_json::Value) -> HashSet<String> {
    let mut commits = HashSet::new();
    match value {
        serde_json::Value::String(text) => {
            commits.extend(git_commit_ids_in_text(text));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                commits.extend(git_commit_ids_in_value(item));
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                commits.extend(git_commit_ids_in_value(value));
            }
        }
        _ => {}
    }
    commits
}

/// Lowercase words from tool names and arguments, with punctuation treated
/// as separators so CLI commands and snake/camel-ish tool names can be
/// recognized without depending on one provider's schema.
fn activity_words(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

fn compact_activity(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn mentions_exact_path(text: &str, expected_path: &str) -> bool {
    text.split_whitespace().any(|token| {
        let token = token.trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '{' | '}' | '[' | ']' | '(' | ')' | ','
            )
        });
        let path = token
            .split(['?', '#'])
            .next()
            .unwrap_or(token)
            .trim_end_matches('/')
            .to_ascii_lowercase();
        path == expected_path.trim_start_matches('/') || path.ends_with(expected_path)
    })
}

/// Whether a structured HTTP request mutates the expected REST collection.
fn contains_rest_mutation(
    value: &serde_json::Value,
    expected_path: &str,
    methods: &[&str],
    descendants: bool,
) -> bool {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| contains_rest_mutation(item, expected_path, methods, descendants)),
        serde_json::Value::Object(fields) => {
            let direct = fields
                .get("method")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|method| {
                    methods
                        .iter()
                        .any(|expected| method.eq_ignore_ascii_case(expected))
                })
                && fields
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|url| {
                        let path = url
                            .split(['?', '#'])
                            .next()
                            .unwrap_or(url)
                            .trim_end_matches('/')
                            .to_ascii_lowercase();
                        path.ends_with(expected_path)
                            || (descendants && path.contains(&format!("{expected_path}/")))
                    });
            direct
                || fields
                    .values()
                    .any(|item| contains_rest_mutation(item, expected_path, methods, descendants))
        }
        _ => false,
    }
}

/// A successful tool call that actually creates a pull request. Merely
/// listing, viewing, or mentioning a PR must not associate it with a session.
fn requests_pull_request_creation(
    tool: &str,
    args: &serde_json::Value,
    owner: &str,
    repo: &str,
) -> bool {
    let tool_words = activity_words(tool);
    let tool_compact = compact_activity(tool);
    let args_text = args.to_string();
    let args_words = activity_words(&args_text);
    let args_compact = compact_activity(&args_text);
    let shell_like = ["shell", "bash", "command", "terminal", "exec", "gh"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    let browser_like = ["browser", "playwright", "web", "click"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    let graphql_mutation = args_words.split_whitespace().any(|word| word == "mutation")
        && args_compact.contains("createpullrequest");
    let rest_path = format!("/repos/{owner}/{repo}/pulls").to_ascii_lowercase();
    let shell_rest_creation = shell_like
        && mentions_exact_path(&args_text, &rest_path)
        && (format!(" {args_words} ").contains(" post ")
            || args_text.contains(" -f ")
            || args_text.contains(" --field ")
            || args_text.contains(" --raw-field "));

    tool_compact.contains("createpullrequest")
        || tool_compact.ends_with("createpr")
        || (shell_like && args_words.contains("gh pr create"))
        || graphql_mutation
        || (browser_like && args_words.contains("create pull request"))
        || shell_rest_creation
        || contains_rest_mutation(args, &rest_path, &["POST"], false)
}

/// A successful tool call that creates or updates a remote branch. This is
/// the evidence needed to find a PR opened later through github.com.
fn requests_remote_ref_mutation(
    tool: &str,
    args: &serde_json::Value,
    owner: &str,
    repo: &str,
) -> bool {
    let tool_words = activity_words(tool);
    let tool_compact = compact_activity(tool);
    let args_text = args.to_string();
    let args_words = activity_words(&args_text);
    let args_compact = compact_activity(&args_text);
    let shell_like = ["shell", "bash", "command", "terminal", "exec", "gh"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    let graphql_mutation = args_words.split_whitespace().any(|word| word == "mutation")
        && (args_compact.contains("createref") || args_compact.contains("updateref"));
    let rest_path = format!("/repos/{owner}/{repo}/git/refs").to_ascii_lowercase();
    let args_lower = args_text.to_ascii_lowercase();
    let shell_rest_mutation = shell_like
        && (args_lower.contains(&rest_path)
            || args_lower.contains(rest_path.trim_start_matches('/')))
        && [" post ", " patch ", " put "]
            .iter()
            .any(|method| format!(" {args_words} ").contains(method));

    [
        "pushbranch",
        "createbranch",
        "updateref",
        "createref",
        "pushref",
    ]
    .iter()
    .any(|operation| tool_compact.contains(operation))
        || (shell_like && args_words.contains("git push"))
        || graphql_mutation
        || shell_rest_mutation
        || contains_rest_mutation(args, &rest_path, &["POST", "PATCH", "PUT"], true)
}

/// Evidence that associates PRs with a session independently of the client.
#[derive(Default)]
struct SessionPrEvidence {
    numbers: HashSet<u64>,
    recorded_numbers: HashSet<u64>,
    successful_tool_args: Vec<String>,
    commit_ids: HashSet<String>,
}

impl SessionPrEvidence {
    /// Merge evidence collected from another thread into this session.
    fn extend(&mut self, other: Self) {
        self.numbers.extend(other.numbers);
        self.recorded_numbers.extend(other.recorded_numbers);
        self.successful_tool_args.extend(other.successful_tool_args);
        self.commit_ids.extend(other.commit_ids);
    }
}

/// Collect PR references, successful branch activity, and commits from events.
fn pr_evidence_from_events(
    events: impl IntoIterator<Item = Event>,
    host: &str,
    owner: &str,
    repo: &str,
) -> SessionPrEvidence {
    let mut requested = HashMap::new();
    let mut output = HashMap::<String, String>::new();
    let mut evidence = SessionPrEvidence::default();
    for event in events {
        match event {
            Event::ToolRequested {
                call_id,
                tool,
                args,
                ..
            } => {
                requested.insert(call_id, (tool, args));
            }
            Event::ToolOutput { call_id, chunk } => {
                output.entry(call_id).or_default().push_str(&chunk);
            }
            Event::ToolCompleted {
                call_id,
                status,
                result,
            } => {
                let request = requested.remove(&call_id);
                let output = output.remove(&call_id).unwrap_or_default();
                if matches!(status, ToolStatus::Ok)
                    && let Some((tool, args)) = request
                {
                    let creates_pr = requests_pull_request_creation(&tool, &args, owner, repo);
                    let mutates_ref = requests_remote_ref_mutation(&tool, &args, owner, repo);
                    if creates_pr {
                        evidence
                            .numbers
                            .extend(pr_numbers_in_value(&args, host, owner, repo));
                        evidence
                            .numbers
                            .extend(pr_numbers_in_value(&result, host, owner, repo));
                        evidence.numbers.extend(crate::github::pr_numbers_in_text(
                            &output, host, owner, repo,
                        ));
                    }
                    if creates_pr || mutates_ref {
                        evidence.commit_ids.extend(git_commit_ids_in_value(&args));
                        evidence.commit_ids.extend(git_commit_ids_in_value(&result));
                        evidence.commit_ids.extend(git_commit_ids_in_text(&output));
                        evidence.successful_tool_args.push(args.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    evidence
}

/// Make a stored transcript safe to send to a provider. A crash or restart
/// between persisting an assistant message with `tool_calls` and persisting
/// its results (tool execution can take minutes; approval waits are
/// unbounded) leaves a dangling `tool_call`, which both OpenAI and Anthropic
/// reject — permanently wedging the thread. Synthesize an "interrupted"
/// result for every tool call left unanswered, and drop empty assistant
/// messages (they serialize to an empty content block Anthropic rejects).
fn sanitize_transcript(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    while let Some(msg) = iter.next() {
        match msg {
            Message::Assistant {
                content,
                tool_calls,
                reasoning,
            } => {
                if content.trim().is_empty() && tool_calls.is_empty() {
                    continue;
                }
                let ids: Vec<String> = tool_calls.iter().map(|c| c.id.clone()).collect();
                out.push(Message::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                });
                if ids.is_empty() {
                    continue;
                }
                // Absorb the contiguous run of results that follow, tracking
                // which call ids they answer.
                let mut answered = std::collections::HashSet::new();
                while matches!(iter.peek(), Some(Message::ToolResult { .. })) {
                    if let Some(Message::ToolResult {
                        call_id,
                        content,
                        images,
                    }) = iter.next()
                    {
                        answered.insert(call_id.clone());
                        out.push(Message::ToolResult {
                            call_id,
                            content,
                            images,
                        });
                    }
                }
                for id in ids {
                    if !answered.contains(&id) {
                        out.push(Message::ToolResult {
                            call_id: id,
                            content: "Tool call interrupted; no result was recorded.".into(),
                            images: Vec::new(),
                        });
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn annotate_edit_lines(worktree: &Path, args: &mut serde_json::Value) {
    let str_of = |v: &serde_json::Value, keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
            .map(str::to_string)
    };
    let Some(path) = str_of(
        args,
        &["file_path", "path", "abs_path", "target_file", "filePath"],
    ) else {
        return;
    };
    let full = if Path::new(&path).is_absolute() {
        PathBuf::from(&path)
    } else {
        worktree.join(&path)
    };
    // Only bother when there is at least one old/new snippet to place.
    let has_snippets = args.get("edits").map(|e| e.is_array()).unwrap_or(false)
        || ["old_string", "oldText", "old_text", "old_str"]
            .iter()
            .any(|k| args.get(*k).is_some());
    if !has_snippets {
        return;
    }
    let Ok(mut content) = std::fs::read_to_string(&full) else {
        return;
    };

    // Locate one snippet in `content` (must be unambiguous), then apply the
    // edit so later snippets in a MultiEdit see their predecessors' effect.
    let mut place = |edit: &mut serde_json::Value| {
        let old = str_of(edit, &["old_string", "oldText", "old_text", "old_str"]);
        let new = str_of(edit, &["new_string", "newText", "new_text", "new_str"]);
        let (Some(old), Some(new)) = (old, new) else {
            return;
        };
        if old.is_empty() || content.matches(old.as_str()).nth(1).is_some() {
            return;
        }
        let Some(pos) = content.find(old.as_str()) else {
            return;
        };
        let line = 1 + content[..pos].matches('\n').count();
        edit["_line"] = serde_json::json!(line);
        content = format!("{}{}{}", &content[..pos], new, &content[pos + old.len()..]);
    };
    match args.get_mut("edits").and_then(|v| v.as_array_mut()) {
        Some(edits) => edits.iter_mut().for_each(&mut place),
        None => place(args),
    }
}

/// Tool spec for the engine-served `ask_question` tool (native provider
/// turns and the MCP bridge expose the same schema).
pub fn ask_question_spec() -> ToolSpec {
    ToolSpec {
        name: "ask_question".into(),
        description: "Ask the user one or more multiple-choice questions and wait for their \
                      answers. Use this when you are blocked on a decision only the user can \
                      make. Each question offers your listed options plus an automatic \
                      free-form \"Other\" choice; set allow_multiple for checkbox-style \
                      questions. The user may also skip answering entirely."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Optional short title for the question form."
                },
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Stable id; generated when omitted." },
                            "prompt": { "type": "string", "description": "The question text." },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "label": { "type": "string" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "allow_multiple": {
                                "type": "boolean",
                                "description": "Allow selecting more than one option."
                            }
                        },
                        "required": ["prompt", "options"]
                    }
                }
            },
            "required": ["questions"]
        }),
    }
}

/// Spec for the engine-served `spawn_thread` tool (child agent, same
/// session/worktree). Offered only to threads that aren't themselves
/// spawned children.
pub fn spawn_thread_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_thread".into(),
        description: "Start a child agent on a new thread in this session (same working \
                      tree). Returns the child's thread_id immediately; collect results \
                      with spawn_output. The child inherits your mode, model and \
                      permission level unless overridden. Children in read-only modes \
                      (e.g. plan) run concurrently with your turn — ideal for parallel \
                      exploration and research. Children that can write must wait for \
                      your current turn to finish before starting, so never block on \
                      one with spawn_output's wait_ms; prefer spawn_session for \
                      write-heavy delegation."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task for the child agent. Self-contained: the child does not see your conversation."
                },
                "mode": {
                    "type": "string",
                    "description": "Agent mode id for the child (default: your mode). Use a read-only mode like \"plan\" for concurrent research."
                },
                "model": {
                    "type": "string",
                    "description": "Provider-qualified model for the child (default: your model)."
                }
            },
            "required": ["prompt"]
        }),
    }
}

/// Spec for the engine-served `spawn_session` tool (child agent, isolated
/// worktree).
pub fn spawn_session_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_session".into(),
        description: "Start a child agent in a NEW session with its own git worktree and \
                      branch, based on your latest checkpoint (your work up to the last \
                      completed turn — not the current turn's uncommitted changes). Fully \
                      isolated: it cannot touch your files; its work lands on its own \
                      branch for later review or merge. Returns thread_id, session_id and \
                      branch immediately; collect results with spawn_output. Use for risky \
                      experiments, best-of-N attempts, or parallel feature work."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task for the child agent. Self-contained: the child does not see your conversation."
                },
                "title": {
                    "type": "string",
                    "description": "Session title (also names the branch); derived from the prompt when omitted."
                },
                "mode": {
                    "type": "string",
                    "description": "Agent mode id for the child (default: your mode)."
                },
                "model": {
                    "type": "string",
                    "description": "Provider-qualified model for the child (default: your model)."
                }
            },
            "required": ["prompt"]
        }),
    }
}

/// Spec for the engine-served `spawn_output` tool (child status/result
/// collection).
pub fn spawn_output_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_output".into(),
        description: "Status and latest output of a child agent you spawned with \
                      spawn_thread or spawn_session. Returns status (pending | running \
                      | completed | failed), the child's last assistant message, turns \
                      completed, and token usage. Set wait_ms to block until the child \
                      finishes (or the timeout passes) — but only wait on children that \
                      run concurrently (read-only same-session children, or any \
                      spawn_session child)."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "The child's thread id, as returned by spawn_thread/spawn_session."
                },
                "wait_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 180000,
                    "description": "Milliseconds to wait for the child to finish (default 0: return current status immediately)."
                }
            },
            "required": ["thread_id"]
        }),
    }
}

/// Spec for the engine-served `search_transcript` tool (recovering history
/// lost to compaction or handoff digests, and cross-thread memory).
pub fn search_transcript_spec() -> ToolSpec {
    ToolSpec {
        name: "search_transcript".into(),
        description: "Search the stored conversation history, including turns that were \
                      compacted out of your context or elided from a handoff digest. \
                      Returns turn-stamped snippets around each match (user and \
                      assistant messages plus tool results). scope defaults to this \
                      thread; \"session\" covers all threads in this session, \
                      \"workspace\" every session in this workspace. To read a matched \
                      turn in full, call again with turn (and the match's thread_id) \
                      instead of a query."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Case-insensitive text to find (exact substring match)."
                },
                "scope": {
                    "type": "string",
                    "enum": ["thread", "session", "workspace"],
                    "description": "How far to search (default: thread)."
                },
                "turn": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Read this turn's messages in full instead of searching."
                },
                "thread_id": {
                    "type": "string",
                    "description": "Thread the turn belongs to (default: this thread); from a match."
                }
            }
        }),
    }
}

/// Parse `ask_question` tool arguments into protocol questions, synthesizing
/// ids where the model omitted them.
pub fn parse_question_args(
    args: &serde_json::Value,
) -> std::result::Result<(Option<String>, Vec<trouve_protocol::Question>), String> {
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let raw = args
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or("missing questions array")?;
    let mut questions = Vec::new();
    for (qi, q) in raw.iter().enumerate() {
        let prompt = q
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("question {} has no prompt", qi + 1))?;
        let id = q
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("q{}", qi + 1));
        let mut options = Vec::new();
        for (oi, o) in q
            .get("options")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            // Accept both {id,label} objects and bare strings.
            let label = o
                .get("label")
                .and_then(|v| v.as_str())
                .or_else(|| o.as_str())
                .unwrap_or_default()
                .to_string();
            if label.trim().is_empty() {
                continue;
            }
            let oid = o
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("opt{}", oi + 1));
            options.push(trouve_protocol::QuestionOption { id: oid, label });
        }
        if options.is_empty() {
            return Err(format!("question {} has no options", qi + 1));
        }
        questions.push(trouve_protocol::Question {
            id,
            prompt: prompt.to_string(),
            options,
            allow_multiple: q
                .get("allow_multiple")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }
    if questions.is_empty() {
        return Err("questions array is empty".into());
    }
    Ok((title, questions))
}

/// Fold question answers into the JSON fed back to the model. Selected
/// options are echoed as labels (ids may have been synthesized, so labels
/// are what the model recognizes).
pub fn question_result_json(
    questions: &[trouve_protocol::Question],
    answers: Option<Vec<trouve_protocol::QuestionAnswer>>,
) -> serde_json::Value {
    let Some(answers) = answers else {
        return serde_json::json!({
            "status": "skipped",
            "message": "The user declined to answer the questions.",
        });
    };
    let items: Vec<serde_json::Value> = answers
        .iter()
        .map(|a| {
            let q = questions.iter().find(|q| q.id == a.question_id);
            let selected: Vec<String> = a
                .selected_option_ids
                .iter()
                .map(|id| {
                    q.and_then(|q| q.options.iter().find(|o| &o.id == id))
                        .map(|o| o.label.clone())
                        .unwrap_or_else(|| id.clone())
                })
                .collect();
            serde_json::json!({
                "question": q.map(|q| q.prompt.as_str()).unwrap_or(a.question_id.as_str()),
                "selected": selected,
                "other": a.other_text,
            })
        })
        .collect();
    serde_json::json!({ "status": "answered", "answers": items })
}

/// Build a provider from config. Credential resolution order: inline
/// `api_key` > `api_key_env` > secret store API key > stored OAuth tokens
/// (when `[providers.<id>.oauth]` is configured).
/// Stream one GGUF from HuggingFace to `<data_dir>/models/`, updating
/// `counter` as bytes land. Writes to a `.part` sibling and renames on
/// success so a partial download never looks complete. Returns false when
/// `cancel` was set (the partial file is deleted).
async fn download_gguf(
    data_dir: &Path,
    entry: &crate::local::ModelEntry,
    counter: &std::sync::atomic::AtomicU64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<bool> {
    use futures::TryStreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let target = crate::local::gguf_path(data_dir, entry);
    std::fs::create_dir_all(target.parent().unwrap())?;
    let part = target.with_extension("gguf.part");

    let url = crate::local::download_url(&entry.repo, &entry.file);
    let client = reqwest::Client::builder()
        .user_agent(concat!("trouve/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client.get(&url).send().await?.error_for_status()?;
    let content_length = resp.content_length();
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(&part).await?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.try_next().await? {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Ok(false);
        }
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        counter.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    file.flush().await?;
    drop(file);

    // Integrity checks before promoting the .part file: catch a truncated
    // download (connection dropped mid-stream) or a wrong file served from
    // the mutable `main` ref. Without these a partial/corrupt GGUF would be
    // renamed to final and loaded.
    let verify = |ok: bool, msg: String| -> Result<()> {
        if ok {
            Ok(())
        } else {
            let _ = std::fs::remove_file(&part);
            bail!(msg)
        }
    };
    if let Some(expected) = content_length {
        verify(
            downloaded == expected,
            format!("download truncated: got {downloaded} of {expected} bytes"),
        )?;
    }
    if entry.size_bytes > 0 {
        // Allow a small drift (the curated size can lag a re-quantization),
        // but reject anything clearly wrong.
        let expected = entry.size_bytes;
        let tolerance = expected / 100; // 1%
        let diff = downloaded.abs_diff(expected);
        verify(
            diff <= tolerance,
            format!(
                "downloaded size {downloaded} differs from the expected {expected} by more than 1%"
            ),
        )?;
    }

    std::fs::rename(&part, &target)?;
    Ok(true)
}

fn build_provider(
    id: &str,
    pc: &ProviderConfig,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
) -> Result<Arc<dyn Provider>> {
    use trouve_providers::auth::{StaticToken, StoredOAuthToken, TokenSource};
    use trouve_providers::secrets::oauth_secret;

    // EXPERIMENTAL direct-Codex client: credentials come from the Codex
    // CLI's auth file, not from our credential resolution below.
    if pc.kind == "codex-responses" {
        return Ok(Arc::new(
            trouve_providers::codex_responses::CodexResponsesProvider::new(id),
        ));
    }

    let api_key = resolved_api_key(id, pc, secrets);
    // Local endpoints (e.g. Ollama) don't need a key; send an empty token.
    let local = pc.base_url.as_deref().is_some_and(is_loopback_base_url);
    let mut oauth_bearer = false;
    let token: Arc<dyn TokenSource> = match (api_key, &pc.oauth) {
        (Some(key), _) => Arc::new(StaticToken(key)),
        (None, Some(oauth)) => {
            oauth_bearer = true;
            Arc::new(StoredOAuthToken::new(
                secrets.clone(),
                oauth_secret(id),
                oauth.clone(),
            ))
        }
        (None, None) if local => Arc::new(StaticToken(String::new())),
        (None, None) => anyhow::bail!(
            "no credentials: set api_key/api_key_env, store a key with \
             `trouve auth set-key {id}`, or configure [providers.{id}.oauth]"
        ),
    };
    match pc.kind.as_str() {
        "openai-compat" => Ok(Arc::new(
            trouve_providers::openai_compat::OpenAiCompatProvider::with_token(
                id.to_string(),
                pc.base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into()),
                token,
            ),
        )),
        "anthropic" => {
            let mut provider = trouve_providers::anthropic::AnthropicProvider::new(
                id.to_string(),
                pc.base_url.clone(),
                token,
            );
            if oauth_bearer {
                provider = provider.with_oauth_bearer();
            }
            Ok(Arc::new(provider))
        }
        other => anyhow::bail!("unknown provider kind {other:?}"),
    }
}

fn resolved_api_key(
    id: &str,
    provider: &ProviderConfig,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
) -> Option<String> {
    provider
        .api_key
        .clone()
        .or_else(|| {
            provider
                .api_key_env
                .as_ref()
                .and_then(|variable| std::env::var(variable).ok())
        })
        .or_else(|| {
            secrets
                .get(&trouve_providers::secrets::api_key_secret(id))
                .ok()
                .flatten()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_provider_neutral_pr_evidence() {
        let remote_commit = "9f2c6d8b18c86d48ca2c3f58191f9f5277b9269a";
        let branch_args = serde_json::json!({
            "request": {
                "method": "POST",
                "url": "https://api.github.com/repos/o/r/git/refs",
                "body": {"ref": "refs/heads/fix/manual-pr"}
            }
        });
        let structured_pr = serde_json::json!({
            "data": {"createPullRequest": {"pullRequest": {
                "url": "https://github.com/o/r/pull/75"
            }}},
            "unrelated": "https://github.com/elsewhere/r/pull/99",
        });
        assert_eq!(
            pr_numbers_in_value(&structured_pr, "github.com", "o", "r"),
            HashSet::from([75])
        );

        let events = vec![
            Event::ToolRequested {
                turn: 1,
                call_id: "branch".into(),
                tool: "github_rest".into(),
                args: branch_args.clone(),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "branch".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "ref": "refs/heads/fix/manual-pr",
                    "object": {"sha": remote_commit}
                }),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "failed".into(),
                tool: "shell".into(),
                args: serde_json::json!({"cmd": "gh pr create --head fix/failed"}),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "failed".into(),
                chunk: "https://github.com/o/r/pull/76".into(),
            },
            Event::ToolCompleted {
                call_id: "failed".into(),
                status: ToolStatus::Error,
                result: serde_json::Value::Null,
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "graphql".into(),
                tool: "github_graphql".into(),
                args: serde_json::json!({
                    "query": "mutation { createPullRequest(input: $input) { pullRequest { url } } }"
                }),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "graphql".into(),
                chunk: structured_pr.to_string(),
            },
            Event::ToolCompleted {
                call_id: "graphql".into(),
                status: ToolStatus::Ok,
                result: serde_json::Value::Null,
            },
            // Successful list/view output may mention many PRs, but none of
            // them were created by this session.
            Event::ToolRequested {
                turn: 1,
                call_id: "list".into(),
                tool: "shell".into(),
                args: serde_json::json!({"cmd": "gh pr list --json url"}),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "list".into(),
                chunk: "https://github.com/o/r/pull/74".into(),
            },
            Event::ToolCompleted {
                call_id: "list".into(),
                status: ToolStatus::Ok,
                result: serde_json::Value::Null,
            },
            Event::UserMessage {
                turn: 2,
                content: "Please compare with repos/o/r/pulls/73".into(),
                attachments: vec![],
            },
        ];
        let evidence = pr_evidence_from_events(events, "github.com", "o", "r");
        assert_eq!(evidence.numbers, HashSet::from([75]));
        assert_eq!(evidence.successful_tool_args.len(), 2);
        assert!(
            evidence
                .successful_tool_args
                .iter()
                .any(|args| args.contains("fix/manual-pr"))
        );
        assert!(
            evidence
                .successful_tool_args
                .iter()
                .all(|args| !args.contains("fix/failed"))
        );
        assert_eq!(evidence.commit_ids, HashSet::from([remote_commit.into()]));
    }

    #[test]
    fn recognizes_creation_without_treating_pr_reads_as_associations() {
        assert!(requests_pull_request_creation(
            "functions.exec",
            &serde_json::json!({"cmd": "gh pr create --head fix/other"}),
            "o",
            "r"
        ));
        assert!(requests_pull_request_creation(
            "github_rest",
            &serde_json::json!({
                "request": {
                    "method": "POST",
                    "url": "https://api.github.com/repos/o/r/pulls"
                }
            }),
            "o",
            "r"
        ));
        assert!(requests_pull_request_creation(
            "functions.exec",
            &serde_json::json!({
                "cmd": "gh api repos/o/r/pulls --method POST --field title=test"
            }),
            "o",
            "r"
        ));
        assert!(!requests_pull_request_creation(
            "functions.exec",
            &serde_json::json!({"cmd": "gh pr list --json url"}),
            "o",
            "r"
        ));
        assert!(!requests_pull_request_creation(
            "github_rest",
            &serde_json::json!({
                "request": {
                    "method": "POST",
                    "url": "https://api.github.com/repos/o/r/pulls/75/comments"
                }
            }),
            "o",
            "r"
        ));
        assert!(requests_remote_ref_mutation(
            "functions.exec",
            &serde_json::json!({"cmd": "git push origin HEAD:fix/manual-pr"}),
            "o",
            "r"
        ));
        assert!(requests_remote_ref_mutation(
            "functions.exec",
            &serde_json::json!({
                "cmd": "gh api repos/o/r/git/refs --method POST --field ref=refs/heads/fix/api"
            }),
            "o",
            "r"
        ));
        assert!(!requests_remote_ref_mutation(
            "functions.exec",
            &serde_json::json!({"cmd": "git fetch origin fix/unrelated"}),
            "o",
            "r"
        ));
    }

    #[test]
    fn loopback_base_url_requires_an_exact_loopback_host() {
        for url in [
            "http://localhost:11434",
            "http://LOCALHOST:11434/v1",
            "http://127.0.0.1:8080/v1",
            "https://127.1.2.3", // whole 127/8 block is loopback
            "http://[::1]:8000",
        ] {
            assert!(is_loopback_base_url(url), "should be loopback: {url}");
        }
        for url in [
            // Suffix tricks: remote hosts that merely contain a loopback
            // string must not be treated as local, or offline mode would
            // enable prompts that still need the internet.
            "https://localhost.attacker.example",
            "https://127.0.0.1.attacker.example",
            "https://attacker.example/path?q=://localhost",
            "https://user:pw@attacker.example#://127.0.0.1",
            "https://api.example.com",
            "http://192.168.1.10:11434",
            "http://[::2]:8000",
            "not a url",
        ] {
            assert!(!is_loopback_base_url(url), "should not be loopback: {url}");
        }
    }

    #[test]
    fn cli_command_prefers_explicit_then_managed_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let managed =
            trouve_agents::install::managed_bin(tmp.path(), trouve_agents::install::CliId::Codex);
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"stub").unwrap();

        assert_eq!(
            resolved_cli_command("codex-responses", None, tmp.path()),
            Some(managed.to_string_lossy().into_owned())
        );
        assert_eq!(
            resolved_cli_command(
                "codex-responses",
                Some("/opt/custom/codex".into()),
                tmp.path()
            )
            .as_deref(),
            Some("/opt/custom/codex")
        );
        assert_eq!(
            resolved_cli_command("openai-compat", None, tmp.path()),
            None
        );
    }

    #[tokio::test]
    async fn todo_tool_persists_and_emits_thread_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_todo".into(),
            name: "todo".into(),
            path: tmp.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_todo".into(),
            workspace_id: workspace.id.clone(),
            title: "todo".into(),
            branch: "main".into(),
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_todo".into(),
            session_id: session.id.clone(),
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let config = Config {
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Arc::new(Engine::new(store.clone(), tmp.path().into(), &config));
        let ctx = ToolCtx {
            worktree: tmp.path().into(),
            thread_id: thread.id.clone(),
            ..Default::default()
        };
        let call = trouve_providers::ToolCallRequest {
            id: "call_todo".into(),
            name: "todo_write".into(),
            arguments: serde_json::json!({"todos": [
                {"id": "one", "content": "First", "status": "in_progress"}
            ]}),
        };

        engine
            .handle_tool_call(
                &session,
                &thread,
                1,
                &modes::fallback_mode(),
                &ctx,
                &call,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        let stored = store.thread(&thread.id).unwrap().unwrap();
        assert_eq!(stored.todos.len(), 1);
        assert_eq!(
            stored.todos[0].status,
            trouve_protocol::TodoStatus::InProgress
        );
        let events = store
            .events_after(&Scope::Thread(thread.id.clone()), 0)
            .unwrap();
        assert!(events.iter().any(|env| matches!(
            &env.event,
            Event::TodosUpdated { todos }
                if todos.len() == 1 && todos[0].id == "one"
        )));

        // Vendor-native TodoWrite completions can be an acknowledgement
        // rather than the updated list. Fall back to the paired start args,
        // preserving existing items when the vendor requests a merge.
        let vendor_result = serde_json::json!("Todos updated");
        let vendor_args = serde_json::json!({"merge": true, "todos": [
            {"content": "Second", "activeForm": "Working on second", "status": "pending"}
        ]});
        let vendor_todos = engine
            .persist_todos_from_result(
                &thread.id,
                "TodoWrite",
                ToolStatus::Ok,
                &vendor_result,
                Some(&vendor_args),
            )
            .unwrap()
            .unwrap();
        assert_eq!(vendor_todos.len(), 2);
        assert_eq!(
            vendor_todos[0].status,
            trouve_protocol::TodoStatus::InProgress
        );
        assert_eq!(vendor_todos[1].id, "vendor:Second");
        assert_eq!(
            store.thread(&thread.id).unwrap().unwrap().todos,
            vendor_todos
        );
    }

    #[test]
    fn history_digest_renders_text_skips_tools_and_caps_length() {
        // Empty transcript: nothing to hand off.
        assert_eq!(render_history_digest(&[], false), None);
        assert_eq!(
            render_history_digest(&[Message::System("prompt".into())], false),
            None
        );

        let messages = [
            Message::System("mode prompt".into()),
            Message::User("add a login page".into()),
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![trouve_providers::ToolCallRequest {
                    id: "1".into(),
                    name: "write_file".into(),
                    arguments: "{}".into(),
                }],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "1".into(),
                content: "long tool output that should not appear".into(),
                images: vec![],
            },
            Message::Assistant {
                content: "Done — login page added.".into(),
                tool_calls: vec![],
                reasoning: vec![],
            },
        ];
        let digest = render_history_digest(&messages, false).unwrap();
        assert!(digest.contains("User:\nadd a login page"));
        assert!(digest.contains("[ran tools: write_file]"));
        assert!(digest.contains("Done — login page added."));
        assert!(!digest.contains("should not appear"));
        assert!(!digest.contains("mode prompt"));
        assert!(digest.starts_with("[Handoff: you are continuing"));

        // Resumed sessions get catch-up framing instead.
        let digest = render_history_digest(&messages, true).unwrap();
        assert!(digest.starts_with("[Handoff: since your last turn"));

        // Oversized transcripts lose their middle, keep head and tail, and
        // never split a multi-byte character.
        let long = "é".repeat(HISTORY_DIGEST_MAX);
        let digest = render_history_digest(
            &[
                Message::User(format!("start-marker {long}")),
                Message::User("end-marker".into()),
            ],
            false,
        )
        .unwrap();
        assert!(digest.len() < HISTORY_DIGEST_MAX + 1_000);
        assert!(digest.contains("start-marker"));
        assert!(digest.contains("end-marker"));
        // The cut points at the recovery hatch for the elided middle.
        assert!(digest.contains("truncated — recover specifics with the search_transcript tool"));
    }

    #[test]
    fn cap_chars_truncates_on_char_boundaries() {
        assert_eq!(cap_chars("short", 100), "short");
        let capped = cap_chars(&"é".repeat(100), 21);
        assert!(capped.starts_with("éééééééééé"));
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn annotate_edit_lines_resolves_snippet_positions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "one\ntwo\nthree\nfour\n").unwrap();

        // Single edit: hint points at the snippet's first line.
        let mut args = serde_json::json!({
            "file_path": "a.rs",
            "old_string": "two\nthree",
            "new_string": "TWO",
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert_eq!(args["_line"], 2);

        // MultiEdit: each edit is placed against the file with earlier
        // edits already applied ("four" moves up when lines collapse).
        let mut args = serde_json::json!({
            "file_path": "a.rs",
            "edits": [
                {"old_string": "two\nthree", "new_string": "TWO"},
                {"old_string": "four", "new_string": "FOUR"},
            ],
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert_eq!(args["edits"][0]["_line"], 2);
        assert_eq!(args["edits"][1]["_line"], 3);

        // Ambiguous or missing snippets get no hint; absolute paths and
        // unreadable files are handled without touching the args.
        std::fs::write(tmp.path().join("b.rs"), "dup\ndup\n").unwrap();
        let mut args = serde_json::json!({
            "file_path": tmp.path().join("b.rs").to_str().unwrap(),
            "old_string": "dup",
            "new_string": "d",
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert!(args.get("_line").is_none());
        let mut args = serde_json::json!({
            "file_path": "missing.rs",
            "old_string": "x",
            "new_string": "y",
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert!(args.get("_line").is_none());

        // Write-style args (no snippets) are left alone entirely.
        let mut args = serde_json::json!({"path": "a.rs", "content": "all new"});
        let before = args.clone();
        annotate_edit_lines(tmp.path(), &mut args);
        assert_eq!(args, before);
    }

    #[test]
    fn sanitize_transcript_repairs_dangling_tool_calls() {
        use trouve_providers::{Message, ToolCallRequest};
        let call = |id: &str| ToolCallRequest {
            id: id.to_string(),
            name: "shell".into(),
            arguments: serde_json::json!({}),
        };

        // A crash left two tool calls with only one result, then the next
        // turn's user message.
        let messages = vec![
            Message::User("do it".into()),
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("a"), call("b")],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "a".into(),
                content: "ok".into(),
                images: vec![],
            },
            Message::User("next".into()),
        ];
        let out = sanitize_transcript(messages);
        // The missing result for "b" is synthesized right after "a"'s.
        match &out[2] {
            Message::ToolResult { call_id, .. } => assert_eq!(call_id, "a"),
            other => panic!("expected result a, got {other:?}"),
        }
        match &out[3] {
            Message::ToolResult {
                call_id, content, ..
            } => {
                assert_eq!(call_id, "b");
                assert!(content.contains("interrupted"));
            }
            other => panic!("expected synthesized result b, got {other:?}"),
        }
        assert!(matches!(&out[4], Message::User(u) if u == "next"));

        // An empty assistant message is dropped entirely.
        let out = sanitize_transcript(vec![
            Message::User("hi".into()),
            Message::Assistant {
                content: "   ".into(),
                tool_calls: vec![],
                reasoning: vec![],
            },
        ]);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Message::User(_)));

        // A well-formed transcript is unchanged in length and pairing.
        let clean = vec![
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("x")],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "x".into(),
                content: "done".into(),
                images: vec![],
            },
        ];
        assert_eq!(sanitize_transcript(clean).len(), 2);
    }

    #[test]
    fn inherited_thinking_level_resolves_through_model_schema() {
        let mut inherited = serde_json::Map::new();
        inherit_thinking_option(&mut inherited, Some("low"), Some("high"));
        assert_eq!(inherited["thinking_level"], "low");

        let mut explicit =
            serde_json::Map::from_iter([("reasoning_effort".into(), serde_json::json!("medium"))]);
        inherit_thinking_option(&mut explicit, Some("low"), Some("high"));
        assert_eq!(explicit.len(), 1, "an explicit thread option wins");
        assert_eq!(explicit["reasoning_effort"], "medium");

        let model = trouve_protocol::ModelInfo {
            id: "codex/gpt".into(),
            display_name: "GPT".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "reasoning_effort": {
                        "type": "string",
                        "enum": ["low", "medium", "high"],
                        "default": "medium"
                    }
                }
            }),
        };
        let mut options =
            serde_json::Map::from_iter([("thinking_level".into(), serde_json::json!("high"))]);
        normalize_thinking_option(&mut options, Some(&model));
        assert_eq!(
            options.get("reasoning_effort"),
            Some(&serde_json::json!("high"))
        );
        assert!(!options.contains_key("thinking_level"));

        // A global token the selected model does not offer falls back to
        // that model's advertised default.
        options.remove("reasoning_effort");
        options.insert("thinking_level".into(), serde_json::json!("xhigh"));
        normalize_thinking_option(&mut options, Some(&model));
        assert_eq!(
            options.get("reasoning_effort"),
            Some(&serde_json::json!("medium"))
        );

        // No thinking enum means the inherited option is not sent.
        options.remove("reasoning_effort");
        options.insert("thinking_level".into(), serde_json::json!("high"));
        normalize_thinking_option(&mut options, None);
        assert!(options.is_empty());
    }

    #[tokio::test]
    async fn complete_login_forwards_callback_once() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel(1);
        engine.logins.lock().unwrap().insert(
            "claude-code".into(),
            LoginState::Pending {
                started: trouve_protocol::LoginStarted {
                    verification_url: "https://claude.example.test/oauth".into(),
                    user_code: None,
                },
                callback_sender: Some(callback_tx),
            },
        );

        let callback = "http://localhost:54545/callback?code=test-code&state=test-state";
        let status = engine
            .complete_login(
                "claude-code",
                trouve_protocol::CompleteLoginRequest {
                    callback_url: callback.into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(status.status, "pending");
        assert_eq!(callback_rx.recv().await.as_deref(), Some(callback));

        assert!(matches!(
            engine
                .complete_login(
                    "claude-code",
                    trouve_protocol::CompleteLoginRequest {
                        callback_url: callback.into(),
                    },
                )
                .await,
            Err(EngineError::Conflict(_))
        ));
        assert!(matches!(
            engine
                .complete_login(
                    "claude-code",
                    trouve_protocol::CompleteLoginRequest {
                        callback_url: "http://localhost/callback\ninjected".into(),
                    },
                )
                .await,
            Err(EngineError::BadRequest(_))
        ));
    }
}
