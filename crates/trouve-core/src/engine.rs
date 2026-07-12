//! The engine: workspaces, sessions, threads, and the agent loop.
//!
//! One `Engine` backs one server. Turns run as spawned tasks; progress is
//! reported exclusively through the event log. Worktree mutations are
//! serialized per session (threads share the session worktree, ADR 0003).

use std::collections::HashMap;
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

pub struct Engine {
    store: Store,
    data_dir: PathBuf,
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
    executor: Arc<dyn ToolExecutor>,
    approvals: Arc<ApprovalHub>,
    questions: Arc<QuestionHub>,
    session_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Threads with a dispatcher currently running turns, mapped to their
    /// session. A thread in this map drains its own prompt queue; sends
    /// while present just enqueue. The session ids feed `Session.active`
    /// and the `session.activity` server event.
    active_threads: Mutex<std::collections::HashMap<String, String>>,
    /// Cancellation tokens for in-flight turns, keyed by thread id. Set while
    /// a turn runs; `cancel_turn` trips one to interrupt the turn's provider
    /// stream, tool calls, and approval waits at the next await point.
    turn_cancels: Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    config: Mutex<Config>,
    /// Where provider configuration changes are persisted. `None` disables
    /// persistence (tests).
    config_file: Option<PathBuf>,
    default_model: RwLock<String>,
    secrets: Arc<dyn trouve_providers::secrets::SecretStore>,
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
    /// Warm the search index on session creation and GC the shared index
    /// store on archive/delete. Off by default so tests never touch the
    /// embedding model; the server enables it (`with_index_hooks`).
    index_hooks: bool,
    /// Per-server MCP logs, shared with the executor's `McpManager` so both
    /// runtime connections and settings health probes land in one place.
    mcp_logs: crate::mcp::McpLogStore,
    /// Interactive shells (one per session) for the client terminal panel.
    terminals: crate::terminal::TerminalManager,
}

#[derive(Debug, Clone)]
enum LoginState {
    /// In flight; carries what the user was told to do so a repeated
    /// start_login can re-present it (e.g. after closing the browser tab)
    /// instead of refusing while the flow is still valid.
    Pending(trouve_protocol::LoginStarted),
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
        && pc
            .base_url
            .as_deref()
            .map(|u| u.contains("://localhost") || u.contains("://127.0.0.1"))
            .unwrap_or(false)
    {
        "none".into()
    } else {
        "api-key".into()
    }
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
        let command = pc.command.clone().or_else(|| {
            cli_for_kind(&pc.kind)
                .map(|cli| trouve_agents::install::managed_bin(data_dir, cli))
                .filter(|bin| bin.exists())
                .map(|bin| bin.to_string_lossy().into_owned())
        });
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
            turn_cancels: Mutex::new(std::collections::HashMap::new()),
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
            secrets,
            logins: Mutex::new(HashMap::new()),
            cli_installs: Mutex::new(HashMap::new()),
            local_manager,
            local_provider,
            local_downloads: Mutex::new(HashMap::new()),
            hardware: std::sync::OnceLock::new(),
            cli_latest: Mutex::new(HashMap::new()),
            base_url: RwLock::new(None),
            index_hooks: false,
            mcp_logs,
            terminals: crate::terminal::TerminalManager::default(),
        }
    }

    /// Enable search-index lifecycle hooks: warm the index when a session is
    /// created (the in-process analogue of the agent plugins' SessionStart
    /// hook) and sweep the shared store when one is archived or deleted.
    pub fn with_index_hooks(mut self) -> Self {
        self.index_hooks = true;
        self
    }

    /// Record the server's reachable base URL (enables the MCP tool bridge
    /// for backends configured with `tool_bridge = true`).
    pub fn set_base_url(&self, url: &str) {
        *self.base_url.write().unwrap() = Some(url.trim_end_matches('/').to_string());
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
    pub async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let providers: Vec<_> = self.providers.read().unwrap().values().cloned().collect();
        let provider_lists =
            futures::future::join_all(providers.iter().map(|p| p.list_models())).await;
        let mut models: Vec<_> = provider_lists.into_iter().flatten().collect();
        let ready: Vec<_> = self
            .backends
            .read()
            .unwrap()
            .values()
            .filter(|b| {
                let status = b.status();
                status.installed && status.has_credentials
            })
            .cloned()
            .collect();
        let listings = futures::future::join_all(ready.iter().map(|b| b.list_models())).await;
        models.extend(listings.into_iter().flatten());
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
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
                    auth,
                    experimental: pc.kind == "codex-responses",
                }
            })
            .collect();
        // Zero-config providers (env keys) that aren't in the config file.
        for id in registry.keys() {
            if !config.providers.contains_key(id) {
                infos.push(ProviderInfo {
                    id: id.clone(),
                    kind: if id == "anthropic" {
                        "anthropic".into()
                    } else {
                        "openai-compat".into()
                    },
                    base_url: None,
                    has_credentials: true,
                    auth: "api-key".into(),
                    experimental: false,
                });
            }
        }
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        ProvidersResponse {
            providers: infos,
            default_model: self.default_model.read().unwrap().clone(),
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
        if let Some(LoginState::Pending(started)) = self.logins.lock().unwrap().get(id) {
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
            self.logins
                .lock()
                .unwrap()
                .insert(id.to_string(), LoginState::Pending(started.clone()));
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
            self.logins
                .lock()
                .unwrap()
                .insert(id.to_string(), LoginState::Pending(started.clone()));
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
        if let Some(LoginState::Pending(started)) = self.logins.lock().unwrap().get(id) {
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
            let cmd = command.unwrap_or_else(|| "codex".into());
            trouve_agents::spawn_login(&cmd, &["login"]).await
        }
        .map_err(|e| EngineError::BadRequest(e.to_string()))?;

        let started = trouve_protocol::LoginStarted {
            verification_url: login.verification_url.clone().unwrap_or_default(),
            user_code: login.user_code.clone(),
        };
        self.logins
            .lock()
            .unwrap()
            .insert(id.to_string(), LoginState::Pending(started.clone()));
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let state = match login.done.await {
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
        if self.store.workspace(&req.workspace_id)?.is_none() {
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
        let (session_id, error) = match self.fire_automation(automation).await {
            Ok(session_id) => (Some(session_id), String::new()),
            Err(e) => (None, e.to_string()),
        };
        let next = crate::automations::next_run(&automation.schedule, chrono::Local::now())
            .filter(|_| automation.enabled)
            .map(|t| t.to_rfc3339());
        let _ = self.store.mark_automation_run(
            &automation.id,
            &chrono::Utc::now().to_rfc3339(),
            session_id.as_deref(),
            &error,
            next.as_deref(),
        );
        if !error.is_empty() {
            tracing::warn!("automation {} failed: {error}", automation.name);
        }
        let _ = self.store.append_event(
            Scope::Server,
            Event::AutomationFired {
                automation_id: automation.id.clone(),
                session_id,
                error,
            },
        );
    }

    async fn fire_automation(
        self: &Arc<Self>,
        automation: &trouve_protocol::Automation,
    ) -> Result<String, EngineError> {
        let session = self
            .create_session(trouve_protocol::CreateSessionRequest {
                workspace_id: automation.workspace_id.clone(),
                title: Some(format!(
                    "{} — {}",
                    automation.name,
                    chrono::Local::now().format("%b %d %H:%M")
                )),
                base_ref: None,
            })
            .await?;
        let thread = self.create_thread(trouve_protocol::CreateThreadRequest {
            session_id: session.id.clone(),
            mode: automation.mode.clone(),
            model: automation.model.clone(),
            model_options: Default::default(),
            permission_mode: None,
        })?;
        self.send_message(&thread.id, automation.prompt.clone(), Vec::new())?;
        Ok(session.id)
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
            Some(LoginState::Pending(_)) => trouve_protocol::LoginStatus {
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
    pub fn set_default_model(&self, model: &str) -> Result<(), EngineError> {
        if !model.contains('/') {
            return Err(EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            )));
        }
        {
            let mut config = self.config.lock().unwrap();
            config.default_model = Some(model.to_string());
            self.persist_config(&config);
        }
        *self.default_model.write().unwrap() = model.to_string();
        Ok(())
    }

    fn persist_config(&self, config: &Config) {
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
        let mode = AgentMode {
            id: id.to_string(),
            display_name: req.display_name,
            system_prompt: req.system_prompt,
            allowed_tools: req.allowed_tools,
            read_only: req.read_only,
            default_permission_mode: req.default_permission_mode,
            default_model: req.default_model,
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

    /// GitHub client for the session's origin remote. Routes to github.com
    /// or a configured GitHub Enterprise host based on the remote URL.
    fn github_for_session(
        &self,
        session: &trouve_protocol::Session,
    ) -> Result<crate::github::GitHub, EngineError> {
        let worktree = PathBuf::from(&session.worktree_path);
        let url = git::remote_url(&worktree, "origin")
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
        let (token, _) = self.github_token(&host).ok_or_else(|| {
            EngineError::BadRequest(format!(
                "no GitHub auth for {host}: sign in or paste a token \
                 (Settings → Integrations), set the token env var, or log in with the gh CLI"
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

    /// The GitHub token for a host and where it came from. Precedence:
    /// environment > OAuth sign-in > pasted token > gh CLI keyring.
    fn github_token(&self, host: &str) -> Option<(String, &'static str)> {
        if let Some(t) = crate::github::token_from_env(host) {
            return Some((t, "environment"));
        }
        let id = Self::github_secret_id(host);
        if let Ok(Some(raw)) = self
            .secrets
            .get(&trouve_providers::secrets::oauth_secret(&id))
        {
            // Device-flow tokens from classic OAuth apps don't expire; apps
            // configured with expiring tokens just need a fresh sign-in.
            if let Ok(tokens) = serde_json::from_str::<trouve_providers::auth::OAuthTokens>(&raw) {
                return Some((tokens.access_token, "oauth"));
            }
        }
        if let Ok(Some(t)) = self
            .secrets
            .get(&trouve_providers::secrets::api_key_secret(&id))
        {
            return Some((t, "settings"));
        }
        crate::github::token_from_gh_cli(host).map(|t| (t, "gh-cli"))
    }

    /// The open PR for the session branch, if one exists.
    pub async fn session_pr(
        &self,
        session_id: &str,
    ) -> Result<Option<trouve_protocol::PrInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let github = self.github_for_session(&session)?;
        github
            .pr_for_branch(&session.branch)
            .await
            .map_err(EngineError::Internal)
    }

    /// Every PR spawned from the session branch (open first, newest first).
    pub async fn session_prs(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::PrInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let github = self.github_for_session(&session)?;
        github
            .prs_for_branch(&session.branch)
            .await
            .map_err(EngineError::Internal)
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

    /// Subscription usage for every configured agent-backend provider.
    /// Codex answers live via its app-server; Cursor and Claude do not
    /// expose subscription data to third parties, so their entries carry
    /// an explanatory note instead.
    pub async fn subscription_health(&self) -> Vec<trouve_protocol::SubscriptionHealth> {
        let backends: Vec<(String, Arc<dyn AgentBackend>)> = {
            let map = self.backends.read().unwrap();
            let mut list: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            list.sort_by(|a, b| a.0.cmp(&b.0));
            list
        };
        let kinds: HashMap<String, String> = {
            let config = self.config.lock().unwrap();
            config
                .providers
                .iter()
                .map(|(id, pc)| (id.clone(), pc.kind.clone()))
                .collect()
        };
        let mut out = Vec::new();
        for (id, backend) in backends {
            match backend.subscription_health().await {
                Some(health) => out.push(health),
                None => {
                    let vendor = match kinds.get(&id).map(String::as_str) {
                        Some("cursor-cli") => "Cursor",
                        Some("claude-cli") => "Anthropic",
                        _ => "This vendor",
                    };
                    out.push(trouve_protocol::SubscriptionHealth {
                        provider_id: id,
                        status: "unsupported".into(),
                        plan: String::new(),
                        windows: Vec::new(),
                        credits: String::new(),
                        note: format!(
                            "{vendor} does not provide subscription usage to third-party apps."
                        ),
                    });
                }
            }
        }
        out
    }

    /// Whether GitHub calls can authenticate, per host. The top-level
    /// fields mirror github.com (`hosts[0]`) for older clients.
    pub fn github_integration(&self) -> trouve_protocol::GithubIntegration {
        let hosts: Vec<trouve_protocol::GithubHostIntegration> = self
            .github_hosts()
            .into_iter()
            .map(|(host, client_id)| {
                let (configured, source) = match self.github_token(&host) {
                    Some((_, source)) => (true, source),
                    None => (false, ""),
                };
                trouve_protocol::GithubHostIntegration {
                    removable: host != crate::github::GITHUB_COM,
                    host,
                    configured,
                    source: source.to_string(),
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

    /// Store a GitHub token in the secret store (empty host = github.com);
    /// an empty token disconnects the host (both the pasted token and any
    /// OAuth sign-in).
    pub fn set_github_token(&self, token: &str, host: &str) -> Result<(), EngineError> {
        let host = if host.trim().is_empty() {
            crate::github::GITHUB_COM.to_string()
        } else {
            host.trim().to_ascii_lowercase()
        };
        if !self.github_hosts().iter().any(|(h, _)| *h == host) {
            return Err(EngineError::NotFound(format!("GitHub host {host}")));
        }
        let id = Self::github_secret_id(&host);
        let key = trouve_providers::secrets::api_key_secret(&id);
        if token.trim().is_empty() {
            self.secrets.delete(&key).map_err(EngineError::Internal)?;
            self.secrets
                .delete(&trouve_providers::secrets::oauth_secret(&id))
                .map_err(EngineError::Internal)?;
        } else {
            self.secrets
                .set(&key, token.trim())
                .map_err(EngineError::Internal)?;
        }
        Ok(())
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
        config
            .github_enterprise
            .push(crate::config::GithubEnterpriseConfig {
                host,
                client_id: Some(client_id.trim().to_string()).filter(|c| !c.is_empty()),
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
        tokio::task::spawn_blocking(move || git::push_branch(&worktree, "origin", &branch))
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)?;
        let base = req.base.clone().unwrap_or_else(|| {
            session
                .base_ref
                .strip_prefix("origin/")
                .unwrap_or(&session.base_ref)
                .to_string()
        });
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
        let pr = github
            .pr_for_branch(&session.branch)
            .await
            .map_err(EngineError::Internal)?
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
    pub async fn session_list_paths(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, EngineError> {
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
            .workspace(&req.workspace_id)?
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
        {
            let repo = repo.clone();
            let worktree_path = worktree_path.clone();
            let branch = branch.clone();
            let base_ref = base_ref.clone();
            tokio::task::spawn_blocking(move || {
                git::create_worktree(&repo, &worktree_path, &branch, &base_ref)
            })
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)?;
        }

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
        self.terminals.remove_session(id);
        let ws = self
            .store
            .workspace(&session.workspace_id)?
            .ok_or_else(|| EngineError::NotFound(format!("workspace {}", session.workspace_id)))?;
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
        self.store.append_event(
            Scope::Session(id.to_string()),
            Event::WorktreeRemoved {
                path: session.worktree_path.clone(),
                branch: session.branch.clone(),
            },
        )?;
        self.store.append_event(
            Scope::Server,
            Event::SessionDeleted {
                session_id: id.to_string(),
                workspace_id: session.workspace_id.clone(),
            },
        )?;
        // Release any MCP server processes spawned for this worktree, so they
        // don't leak for the lifetime of the server.
        self.executor
            .evict_worktree(Path::new(&session.worktree_path))
            .await;
        // Remove attachment files from disk before dropping their DB rows;
        // afterwards their paths are unrecoverable.
        for path in self.store.session_attachment_paths(id)? {
            let _ = std::fs::remove_file(&path);
        }
        // Deleting the session deletes its events (privacy: see event-log doc).
        self.store.delete_session(id)?;
        if self.index_hooks {
            crate::tools::gc_index_store_in_background(PathBuf::from(&ws.path));
        }
        Ok(())
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
        let thread = Thread {
            id: new_id("th"),
            session_id: session.id.clone(),
            mode: mode.id.clone(),
            model,
            model_options: req.model_options.clone(),
            permission_mode: req.permission_mode.unwrap_or(mode.default_permission_mode),
            created_at: chrono::Utc::now(),
            // Spawn parentage is recorded by the spawn tools after insert;
            // reads recompute this flag from the spawned_threads table.
            spawned: false,
        };
        self.store.insert_thread(&thread, &req.model_options)?;
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
            if active.contains_key(thread_id) {
                return Ok(None);
            }
            let Some(p) = self.store.pop_queued_prompt(thread_id)? else {
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
            self.release_thread(thread_id);
            return Err(e.into());
        }
        let turn = match self.store.next_turn(thread_id) {
            Ok(t) => t,
            Err(e) => {
                self.release_thread(thread_id);
                return Err(e.into());
            }
        };
        let engine = self.clone();
        tokio::spawn(async move {
            let thread_id = thread.id.clone();
            // Catch a panic in the turn machinery so the claim (and cancel
            // token) are always released and the UI unsticks — tokio would
            // otherwise swallow the panic and leave the thread wedged as
            // "active" with no TurnFailed event.
            let drained = std::panic::AssertUnwindSafe(engine.drain_queue(
                thread,
                turn,
                prompt.content,
                prompt.attachments,
            ))
            .catch_unwind()
            .await;
            if drained.is_err() {
                tracing::error!("turn dispatcher for {thread_id} panicked");
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
        content: String,
        attachments: Vec<trouve_protocol::Attachment>,
    ) {
        let mut thread = thread;
        let mut turn = turn;
        let mut content = content;
        let mut attachments = attachments;
        loop {
            let cancel = self.register_cancel(&thread.id);
            let result = self
                .run_turn(&thread, turn, content, attachments, cancel.clone())
                .await;
            let cancelled = cancel.is_cancelled();
            self.clear_cancel(&thread.id);
            if let Err(e) = result {
                tracing::error!("turn {turn} of {} failed: {e}", thread.id);
                let _ = self.store.append_event(
                    Scope::Thread(thread.id.clone()),
                    Event::TurnFailed {
                        turn,
                        error: e.to_string(),
                    },
                );
                self.release_thread(&thread.id);
                return;
            }
            if cancelled {
                // A user-cancelled turn pauses the queue (like a failure, but
                // not an error): leave queued prompts for the user to resume.
                let _ = self.store.append_event(
                    Scope::Thread(thread.id.clone()),
                    Event::TurnCancelled { turn },
                );
                self.release_thread(&thread.id);
                return;
            }
            // Pop the next prompt; releasing the claim and inspecting the
            // queue must be atomic against concurrent send_message calls.
            let (next, session_idle) = {
                let mut active = self.active_threads.lock().unwrap();
                match self.store.pop_queued_prompt(&thread.id) {
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
                    self.release_thread(&thread.id);
                    return;
                }
            };
            content = next.content;
            attachments = next.attachments;
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
        content: String,
        attachments: Vec<trouve_protocol::Attachment>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
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
                )
                .await;
        }

        let (provider, model_name) = self
            .resolve_provider(&thread.model)
            .map_err(|e| anyhow!(e.to_string()))?;
        let model_options = self.store.thread_model_options(&thread.id)?;

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
        // follow.
        let content = annotate_attachments(content, &self.resolve_attachments(&attachments));
        self.store
            .append_message(&thread.id, &serde_json::to_value(Message::User(content))?)?;

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
        let spawn_allowed =
            |name: &str| mode.allowed_tools.is_empty() || mode.allowed_tools.iter().any(|t| t == name);
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

        for _iteration in 0..MAX_ITERATIONS {
            if cancel.is_cancelled() {
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

        // Dollar cost from the model catalog, when pricing is known.
        if let Some(model) = provider.models().iter().find(|m| m.id == thread.model) {
            usage_total.cost_usd = trouve_providers::catalog::cost_usd(
                model,
                usage_total.input_tokens,
                usage_total.output_tokens,
            );
        }
        self.store
            .record_usage(&session.id, &thread.id, turn, &usage_total, context_input_tokens)?;

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
        let url = format!(
            "{}/internal/threads/{}/mcp?tools={}&approval={}",
            base_url.trim_end_matches('/'),
            thread_id,
            bridge_tools as u8,
            approval,
        );
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
        let backend_turn = BackendTurn {
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: vendor_session,
            model: model_name,
            model_options: self.store.thread_model_options(&thread.id)?,
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
                    self.store
                        .append_event(scope.clone(), Event::ToolStarted { call_id })?;
                }
                BackendEvent::ToolOutput { call_id, chunk } => {
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
                    self.store.append_event(
                        scope.clone(),
                        Event::ToolCompleted {
                            call_id,
                            status: if ok {
                                ToolStatus::Ok
                            } else {
                                ToolStatus::Error
                            },
                            result,
                        },
                    )?;
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
        self.store
            .record_usage(&session.id, &thread.id, turn, &usage_total, context_input_tokens)?;
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
                let rx = self.approvals.request(call_id);
                self.store.append_event(
                    scope.clone(),
                    Event::ApprovalRequested {
                        turn,
                        call_id: call_id.to_string(),
                    },
                )?;
                let decision = rx.await.unwrap_or(ApprovalDecision::Deny);
                self.store.append_event(
                    scope,
                    Event::ApprovalResolved {
                        call_id: call_id.to_string(),
                        decision,
                    },
                )?;
                if decision == ApprovalDecision::AlwaysApprove {
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
        self.store.append_event(
            scope,
            Event::ToolCompleted {
                call_id,
                status: outcome.status,
                result: outcome.result.clone(),
            },
        )?;
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
                bail!(
                    "already {running} children running; collect some with spawn_output first"
                );
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
                    let snippet: String =
                        prompt.lines().next().unwrap_or("").chars().take(48).collect();
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
        let running = self
            .active_threads
            .lock()
            .unwrap()
            .contains_key(thread_id);
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
            let t = self.get_thread(target).map_err(|e| anyhow!(e.to_string()))?;
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
                    Event::UserMessage { turn: t, content, .. } if t == turn => {
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

fn annotate_attachments(
    content: String,
    files: &[(trouve_protocol::Attachment, PathBuf)],
) -> String {
    if files.is_empty() {
        return content;
    }
    let mut out = content;
    out.push_str("\n\nThe user attached these files (read them from disk as needed):");
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
                      branch, based on this session's branch (committed work only — not \
                      uncommitted changes). Fully isolated: it cannot touch your files; \
                      its work lands on its own branch for later review or merge. \
                      Returns thread_id, session_id and branch immediately; collect \
                      results with spawn_output. Use for risky experiments, best-of-N \
                      attempts, or parallel feature work."
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
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(&part).await?;
    while let Some(chunk) = stream.try_next().await? {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Ok(false);
        }
        file.write_all(&chunk).await?;
        counter.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    file.flush().await?;
    drop(file);
    std::fs::rename(&part, &target)?;
    Ok(true)
}

fn build_provider(
    id: &str,
    pc: &ProviderConfig,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
) -> Result<Arc<dyn Provider>> {
    use trouve_providers::auth::{StaticToken, StoredOAuthToken, TokenSource};
    use trouve_providers::secrets::{api_key_secret, oauth_secret};

    // EXPERIMENTAL direct-Codex client: credentials come from the Codex
    // CLI's auth file, not from our credential resolution below.
    if pc.kind == "codex-responses" {
        return Ok(Arc::new(
            trouve_providers::codex_responses::CodexResponsesProvider::new(id),
        ));
    }

    let api_key = pc
        .api_key
        .clone()
        .or_else(|| pc.api_key_env.as_ref().and_then(|v| std::env::var(v).ok()))
        .or_else(|| secrets.get(&api_key_secret(id)).ok().flatten());
    // Local endpoints (e.g. Ollama) don't need a key; send an empty token.
    let local = pc
        .base_url
        .as_deref()
        .map(|u| u.contains("://localhost") || u.contains("://127.0.0.1"))
        .unwrap_or(false);
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

#[cfg(test)]
mod tests {
    use super::*;

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
            Message::ToolResult { call_id, content, .. } => {
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
}
