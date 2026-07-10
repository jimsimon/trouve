//! The engine: workspaces, sessions, threads, and the agent loop.
//!
//! One `Engine` backs one server. Turns run as spawned tasks; progress is
//! reported exclusively through the event log. Worktree mutations are
//! serialized per session (threads share the session worktree, ADR 0003).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use trouve_agents::{AgentBackend, BackendEvent, BackendPermission, BackendTurn};
use trouve_protocol::{
    AgentMode, ApprovalDecision, BranchList, CreateSessionRequest, CreateThreadRequest, Event,
    ProviderInfo, ProvidersResponse, RestoreDirection, Scope, Session, Thread, ToolStatus,
    TurnAccepted, UpdateSessionRequest, UpdateThreadRequest, UpsertProviderRequest, Usage,
    Workspace,
};
use trouve_providers::{Message, Provider, ProviderEvent, ToolSpec};

use crate::config::{Config, ProviderConfig};
use crate::permissions::{allow_key, gate, ApprovalHub, Gate, QuestionHub};
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
    /// Latest-version lookups, cached per CLI (network is best-effort).
    cli_latest: Mutex<HashMap<String, (std::time::Instant, Option<String>)>>,
    /// This server's reachable base URL (e.g. "http://127.0.0.1:7433"), set
    /// once the listener binds; the MCP tool bridge dials back through it.
    base_url: RwLock<Option<String>>,
    /// Warm the search index on session creation and GC the shared index
    /// store on archive/delete. Off by default so tests never touch the
    /// embedding model; the server enables it (`with_index_hooks`).
    index_hooks: bool,
}

#[derive(Debug, Clone)]
enum LoginState {
    Pending,
    Success,
    Failed(String),
}

#[derive(Debug, Clone)]
enum CliInstallState {
    Pending(Option<String>),
    Success(String),
    Failed(String),
}

/// Whether a `--version` report refers to the given vendor version. The
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

/// Default `trouve` binary for the MCP bridge: next to the running server
/// (installed layout and cargo target dir), else resolved via `$PATH`.
fn default_bridge_command() -> String {
    let name = format!("trouve{}", std::env::consts::EXE_SUFFIX);
    if let Ok(me) = std::env::current_exe() {
        if let Some(sibling) = me.parent().map(|d| d.join(&name)) {
            if sibling.exists() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    name
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
    if !providers.contains_key("openai") {
        if let Ok(p) = trouve_providers::openai_compat::OpenAiCompatProvider::openai_from_env() {
            providers.insert("openai".into(), Arc::new(p));
        }
    }
    if !providers.contains_key("anthropic") {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            providers.insert(
                "anthropic".into(),
                Arc::new(trouve_providers::anthropic::AnthropicProvider::new(
                    "anthropic",
                    None,
                    Arc::new(trouve_providers::auth::StaticToken(key)),
                )),
            );
        }
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
        let providers = build_all_providers(config, &secrets);
        let backends = build_all_backends(config, &secrets, &data_dir);
        Self {
            store,
            data_dir,
            config_dir: dirs::config_dir().map(|d| d.join("trouve")),
            providers: RwLock::new(providers),
            injected_providers: Mutex::new(HashMap::new()),
            backends: RwLock::new(backends),
            injected_backends: Mutex::new(HashMap::new()),
            executor: Arc::new(LocalToolExecutor::default()),
            approvals: Arc::new(ApprovalHub::default()),
            questions: Arc::new(QuestionHub::default()),
            session_locks: Mutex::new(HashMap::new()),
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
            cli_latest: Mutex::new(HashMap::new()),
            base_url: RwLock::new(None),
            index_hooks: false,
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

        let oauth = {
            let config = self.config.lock().unwrap();
            config
                .providers
                .get(id)
                .and_then(|pc| pc.oauth.clone())
                .ok_or_else(|| {
                    EngineError::BadRequest(format!("provider {id} has no OAuth configuration"))
                })?
        };
        if matches!(
            self.logins.lock().unwrap().get(id),
            Some(LoginState::Pending)
        ) {
            return Err(EngineError::Conflict(format!(
                "a login for {id} is already in progress"
            )));
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
                .insert(id.to_string(), LoginState::Pending);
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
            self.logins
                .lock()
                .unwrap()
                .insert(id.to_string(), LoginState::Pending);
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
            Ok(trouve_protocol::LoginStarted {
                verification_url: url,
                user_code: None,
            })
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
        if matches!(
            self.logins.lock().unwrap().get(id),
            Some(LoginState::Pending)
        ) {
            return Err(EngineError::Conflict(format!(
                "a login for {id} is already in progress"
            )));
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

        self.logins
            .lock()
            .unwrap()
            .insert(id.to_string(), LoginState::Pending);
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
        Ok(trouve_protocol::LoginStarted {
            verification_url: login.verification_url.unwrap_or_default(),
            user_code: login.user_code,
        })
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
            if let Some((at, v)) = cache.get(id.as_str()) {
                if at.elapsed() < TTL {
                    return v.clone();
                }
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
        {
            let mut installs = self.cli_installs.lock().unwrap();
            if matches!(installs.get(id), Some(CliInstallState::Pending(_))) {
                return Err(EngineError::Conflict(format!(
                    "an install for {id} is already in progress"
                )));
            }
            installs.insert(id.to_string(), CliInstallState::Pending(None));
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
                    CliInstallState::Pending(Some(version.clone())),
                );
                trouve_agents::install::install(&engine.data_dir, cli, &version)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<String, String>(version)
            }
            .await;
            let state = match result {
                Ok(version) => {
                    // The managed binary now exists; rebuild backends so it
                    // takes over from any PATH resolution.
                    engine.reload_providers();
                    engine.cli_latest.lock().unwrap().remove(id_owned.as_str());
                    CliInstallState::Success(version)
                }
                Err(e) => CliInstallState::Failed(e),
            };
            engine.cli_installs.lock().unwrap().insert(id_owned, state);
        });
        Ok(())
    }

    /// Report the state of an install started with `start_cli_install`.
    pub fn cli_install_status(&self, id: &str) -> trouve_protocol::CliInstallStatus {
        match self.cli_installs.lock().unwrap().get(id) {
            None => trouve_protocol::CliInstallStatus {
                status: "none".into(),
                version: None,
                error: None,
            },
            Some(CliInstallState::Pending(version)) => trouve_protocol::CliInstallStatus {
                status: "pending".into(),
                version: version.clone(),
                error: None,
            },
            Some(CliInstallState::Success(version)) => trouve_protocol::CliInstallStatus {
                status: "success".into(),
                version: Some(version.clone()),
                error: None,
            },
            Some(CliInstallState::Failed(e)) => trouve_protocol::CliInstallStatus {
                status: "failed".into(),
                version: None,
                error: Some(e.clone()),
            },
        }
    }

    /// Report the state of an OAuth login started with `start_login`.
    pub fn login_status(&self, id: &str) -> trouve_protocol::LoginStatus {
        match self.logins.lock().unwrap().get(id) {
            None => trouve_protocol::LoginStatus {
                status: "none".into(),
                error: None,
            },
            Some(LoginState::Pending) => trouve_protocol::LoginStatus {
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
        if let Some(path) = &self.config_file {
            if let Err(e) = config.save_to(path) {
                tracing::warn!("failed to persist config: {e}");
            }
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

    /// GitHub client for the session's origin remote.
    fn github_for_session(
        &self,
        session: &trouve_protocol::Session,
    ) -> Result<crate::github::GitHub, EngineError> {
        let worktree = PathBuf::from(&session.worktree_path);
        let url = git::remote_url(&worktree, "origin")
            .ok_or_else(|| EngineError::BadRequest("workspace has no 'origin' remote".into()))?;
        let (owner, repo) = crate::github::parse_github_remote(&url).ok_or_else(|| {
            EngineError::BadRequest(format!("origin is not a GitHub remote: {url}"))
        })?;
        let token = crate::github::token_from_env()
            .or_else(|| {
                self.secrets
                    .get(&trouve_providers::secrets::api_key_secret("github"))
                    .ok()
                    .flatten()
            })
            .ok_or_else(|| {
                EngineError::BadRequest(
                    "no GitHub token: set GITHUB_TOKEN or `trouve auth set-key github`".into(),
                )
            })?;
        crate::github::GitHub::new(&token, &owner, &repo).map_err(EngineError::Internal)
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
        Ok(self.store.list_sessions(workspace_id)?)
    }

    pub fn get_session(&self, id: &str) -> Result<Session, EngineError> {
        self.store
            .session(id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {id}")))
    }

    /// Rename and/or (un)archive a session.
    pub fn update_session(
        &self,
        id: &str,
        req: &UpdateSessionRequest,
    ) -> Result<Session, EngineError> {
        let session = self.get_session(id)?;
        if let Some(title) = req.title.as_deref() {
            if title.trim().is_empty() {
                return Err(EngineError::BadRequest("title cannot be empty".into()));
            }
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
        if self.index_hooks && req.archived == Some(true) && !session.archived {
            if let Some(ws) = self.store.workspace(&session.workspace_id)? {
                crate::tools::gc_index_store_in_background(PathBuf::from(&ws.path));
            }
        }
        self.get_session(id)
    }

    pub async fn delete_session(&self, id: &str) -> Result<(), EngineError> {
        let session = self.get_session(id)?;
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
        let model = req
            .model
            .unwrap_or_else(|| self.default_model.read().unwrap().clone());
        let thread = Thread {
            id: new_id("th"),
            session_id: session.id.clone(),
            mode: mode.id.clone(),
            model,
            model_options: req.model_options.clone(),
            permission_mode: req.permission_mode.unwrap_or(mode.default_permission_mode),
            created_at: chrono::Utc::now(),
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
        if let Some(model) = req.model.as_deref() {
            if !model.contains('/') {
                return Err(EngineError::BadRequest(format!(
                    "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
                )));
            }
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

    /// Accept a user message and run the turn in the background. Progress is
    /// visible on the thread's event stream.
    pub fn send_message(
        self: &Arc<Self>,
        thread_id: &str,
        content: String,
    ) -> Result<TurnAccepted, EngineError> {
        let thread = self.get_thread(thread_id)?;
        let turn = self.store.next_turn(thread_id)?;
        let engine = self.clone();
        let thread_id_owned = thread_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = engine.run_turn(&thread, turn, content).await {
                tracing::error!("turn {turn} of {thread_id_owned} failed: {e}");
                let _ = engine.store.append_event(
                    Scope::Thread(thread_id_owned.clone()),
                    Event::TurnFailed {
                        turn,
                        error: e.to_string(),
                    },
                );
            }
        });
        Ok(TurnAccepted {
            thread_id: thread_id.to_string(),
            turn,
        })
    }

    async fn run_turn(&self, thread: &Thread, turn: u64, content: String) -> Result<()> {
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
        };

        // Serialize worktree mutations across the session's threads.
        let lock = self.session_lock(&session.id);
        let _guard = lock.lock().await;

        let all_modes = modes::resolve_modes(self.config_dir.as_deref(), Some(Path::new(&ws.path)));
        let mode = modes::find_mode(&all_modes, &thread.mode)
            .cloned()
            .unwrap_or_else(|| modes::builtin_modes().remove(0));

        // External agent backend? The vendor harness owns the loop; we
        // stream its events and bridge approvals. (Session lock stays held.)
        if let Some((backend, model_name)) = self.backend_for(&thread.model) {
            return self
                .run_backend_turn(&session, thread, turn, &mode, backend, model_name, content)
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

        // Compact the transcript when it nears the model's context window,
        // before this turn's user message joins it.
        if let Err(e) = self
            .maybe_compact(thread, turn, &provider, &model_name)
            .await
        {
            // Compaction is best-effort; the turn proceeds with full history.
            tracing::warn!("compaction failed for {}: {e}", thread.id);
        }

        self.store.append_event(
            scope.clone(),
            Event::UserMessage {
                turn,
                content: content.clone(),
            },
        )?;
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

        let system = context::system_prompt(&mode, self.config_dir.as_deref(), Path::new(&ws.path));
        let mut usage_total = Usage::default();

        for _iteration in 0..MAX_ITERATIONS {
            // Rebuild the transcript each iteration; the store is the truth.
            let mut messages = vec![Message::System(system.clone())];
            for payload in self.store.messages(&thread.id)? {
                messages.push(serde_json::from_value(payload)?);
            }

            let mut stream = provider
                .stream_chat(&model_name, &messages, &specs, &model_options)
                .await
                .map_err(|e| anyhow!("provider error: {e}"))?;

            let mut text = String::new();
            let mut tool_calls = Vec::new();
            while let Some(ev) = stream.next().await {
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
                    ProviderEvent::ToolCall(call) => tool_calls.push(call),
                    ProviderEvent::Completed { usage } => {
                        usage_total.input_tokens += usage.input_tokens;
                        usage_total.output_tokens += usage.output_tokens;
                        usage_total.cached_input_tokens += usage.cached_input_tokens;
                    }
                }
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
            self.store.append_message(
                &thread.id,
                &serde_json::to_value(Message::Assistant {
                    content: text,
                    tool_calls: tool_calls.clone(),
                })?,
            )?;

            if tool_calls.is_empty() {
                break;
            }

            for call in tool_calls {
                let result_content = self
                    .handle_tool_call(&session, thread, turn, &mode, &ctx, &call)
                    .await?;
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::ToolResult {
                        call_id: call.id.clone(),
                        content: result_content,
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
            .record_usage(&session.id, &thread.id, turn, &usage_total)?;

        // Snapshot the worktree when the turn changed it.
        let checkpoint_id = self.maybe_checkpoint(&session, thread, turn).await?;
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
    fn backend_for(&self, model: &str) -> Option<(Arc<dyn AgentBackend>, String)> {
        let (backend_id, model_name) = model.split_once('/')?;
        let backend = self.backends.read().unwrap().get(backend_id).cloned()?;
        Some((backend, model_name.to_string()))
    }

    /// MCP tool-bridge config for a backend turn, when the backend opted in
    /// (`tool_bridge = true`, claude-cli only) and the server URL is known.
    fn mcp_bridge_for(
        &self,
        model: &str,
        thread_id: &str,
    ) -> Option<trouve_agents::McpBridgeConfig> {
        let backend_id = model.split_once('/')?.0;
        let (kind, bridge_tools, bridge_command) = {
            let config = self.config.lock().unwrap();
            let pc = config.providers.get(backend_id)?;
            (
                pc.kind.clone(),
                pc.tool_bridge.unwrap_or(false),
                pc.bridge_command.clone(),
            )
        };
        // Claude Code always gets the bridge: it carries the approval-prompt
        // gate for Ask mode, and optionally (tool_bridge = true) trouve's
        // tools in place of Claude's built-ins.
        if kind != "claude-cli" {
            return None;
        }
        let Some(base_url) = self.base_url.read().unwrap().clone() else {
            tracing::warn!(
                "MCP bridge wanted for {backend_id} but the server base URL is unknown; \
                 running without it (approvals will fail in ask mode)"
            );
            return None;
        };
        let mut env = vec![
            ("TROUVE_SERVER".into(), base_url),
            ("TROUVE_THREAD_ID".into(), thread_id.to_string()),
        ];
        if bridge_tools {
            env.push(("TROUVE_BRIDGE_TOOLS".into(), "1".into()));
        }
        Some(trouve_agents::McpBridgeConfig {
            command: bridge_command.unwrap_or_else(default_bridge_command),
            args: vec!["mcp-bridge".into()],
            env,
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
        Ok(specs)
    }

    /// Execute one tool call on behalf of a bridged vendor agent, through
    /// the same gate/approval/event chokepoint as native tool calls.
    pub async fn bridged_tool_call(
        &self,
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
        self.handle_tool_call(&session, &thread, turn, &mode, &ctx, &call)
            .await
            .map_err(EngineError::Internal)
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
            .unwrap_or_else(|| modes::builtin_modes().remove(0));
        let ctx = ToolCtx {
            worktree: PathBuf::from(&session.worktree_path),
            config_dir: self.config_dir.clone(),
        };
        Ok((session, thread, mode, ctx))
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
        backend: Arc<dyn AgentBackend>,
        model_name: String,
        content: String,
    ) -> Result<()> {
        let scope = Scope::Thread(thread.id.clone());
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
            },
        )?;
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
        let backend_turn = BackendTurn {
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: self.store.backend_session(&thread.id)?,
            model: model_name,
            model_options: self.store.thread_model_options(&thread.id)?,
            prompt: content,
            instructions: (!instructions.is_empty()).then_some(instructions),
            permission,
            mcp_bridge,
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
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| anyhow!("backend stream error: {e}"))? {
                BackendEvent::SessionStarted { session_id } => {
                    self.store.set_backend_session(&thread.id, &session_id)?;
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
                }
            }
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
            })?,
        )?;

        self.store
            .record_usage(&session.id, &thread.id, turn, &usage_total)?;
        let checkpoint_id = self.maybe_checkpoint(session, thread, turn).await?;
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
        let Some(context_window) = provider
            .models()
            .iter()
            .find(|m| m.id == thread.model)
            .map(|m| m.context_window)
        else {
            return Ok(()); // Unknown model: no window to compact against.
        };
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
            "[Context was compacted. Summary of the conversation so far:]\n\n{summary}"
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
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentMode,
        ctx: &ToolCtx,
        call: &trouve_providers::ToolCallRequest,
    ) -> Result<String> {
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
            return Ok(result.to_string());
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

        self.store.append_event(
            scope.clone(),
            Event::ToolRequested {
                turn,
                call_id: call_id.clone(),
                tool: call.name.clone(),
                args: call.arguments.clone(),
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
                return Ok("Tool call denied: not permitted in this mode.".into());
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
                let decision = rx.await.unwrap_or(ApprovalDecision::Deny);
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
            return Ok("Tool call denied by the user.".into());
        }

        self.store.append_event(
            scope.clone(),
            Event::ToolStarted {
                call_id: call_id.clone(),
            },
        )?;
        let outcome = self
            .executor
            .execute(ctx, &call.name, &call.arguments)
            .await;
        self.store.append_event(
            scope,
            Event::ToolCompleted {
                call_id,
                status: outcome.status,
                result: outcome.result.clone(),
            },
        )?;
        Ok(outcome.result.to_string())
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
}
