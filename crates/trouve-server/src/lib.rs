//! HTTP/SSE server exposing the trouve protocol (ADR 0002).
//!
//! Commands are POST endpoints; server→client state is one append-only
//! event stream per scope, delivered as SSE with cursor resumption via
//! `Last-Event-ID` or `?after=`.

mod mcp;

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;
use trouve_core::Engine;
use trouve_core::engine::EngineError;
use trouve_protocol::{
    AddLocalModelRequest, AgentMode, Automation, BranchList, CliInfo, CliInstallStatus, CliList,
    CodeReviewDashboard, CodeReviewRepository, ConfigureGithubAppRequest, CreatePrRequest,
    CreateSessionRequest, CreateThreadRequest, DirEntry, ErrorBody, FileContent, GithubAppStatus,
    GithubIntegration, GithubPrList, KnownProvider, LocalSearchResult, LocalStatus, LoginStarted,
    LoginStatus, McpLogs, McpServerInfo, MergePrRequest, ModeInfo, ModelInfo, OpenTerminalRequest,
    PROTOCOL_VERSION, PrInfo, ProviderInfo, ProvidersResponse, QueuedPrompt,
    RegisterWorkspaceRequest, ReorderQueueRequest, ResolveApprovalRequest, ResolveQuestionRequest,
    ReviewerProfile, Scope, SendMessageRequest, ServerInfo, Session, SessionDiff,
    SetDefaultModelRequest, SetDefaultPermissionModeRequest, SetLocalEnabledRequest,
    SubscriptionHealth, TerminalInfo, TerminalInputRequest, TerminalResizeRequest, Thread,
    TurnAccepted, UpdateCodeReviewRepositoryRequest, UpdateQueuedPromptRequest,
    UpdateSessionRequest, UpdateThreadRequest, UpsertAutomationRequest, UpsertMcpServerRequest,
    UpsertModeRequest, UpsertProviderRequest, UpsertReviewerProfileRequest, UsageSummary,
    Workspace,
};
use utoipa::OpenApi;

/// Select the process-wide Rustls backend before any HTTP client constructs
/// a TLS configuration. The desktop binary links both Ring (via Octocrab)
/// and AWS-LC (via Reqwest), so Rustls cannot infer a provider from features.
/// Ring is already required by the GitHub client and works on every target
/// supported by the app.
pub fn install_crypto_provider() {
    // Another embedder may have selected a provider before calling us. In
    // that case the process-wide choice is already valid and immutable.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub struct ApiError(EngineError);

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self.0 {
            EngineError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            EngineError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            EngineError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            EngineError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let body = ErrorBody {
            code: code.into(),
            message: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "trouve harness protocol",
        description = "Commands are POST endpoints; state flows back on cursor-addressed SSE event streams.",
    ),
    paths(
        info,
        register_workspace,
        list_workspaces,
        close_workspace,
        workspace_branches,
        refresh_github_prs,
        create_session,
        list_sessions,
        get_session,
        update_session,
        delete_session,
        undo_session,
        redo_session,
        create_thread,
        list_threads,
        get_thread,
        update_thread,
        send_message,
        get_attachment,
        list_queue,
        reorder_queue,
        dispatch_queue,
        update_queued_prompt,
        delete_queued_prompt,
        cancel_turn,
        resolve_approval,
        resolve_question,
        list_models,
        list_modes,
        list_mode_infos,
        upsert_mode,
        delete_mode,
        list_providers,
        known_providers,
        upsert_provider,
        delete_provider,
        start_login,
        login_status,
        list_clis,
        start_cli_install,
        cli_install_status,
        cancel_cli_install,
        uninstall_cli,
        local_status,
        set_local_enabled,
        add_local_model,
        search_local_models,
        delete_local_model,
        start_local_model_download,
        cancel_local_model_download,
        stop_local_server,
        restart_local_server,
        set_default_model,
        set_default_permission_mode,
        thread_usage,
        session_usage,
        session_mcp_servers,
        session_diff,
        session_files,
        session_paths,
        session_file,
        open_terminal,
        kill_terminal,
        terminal_input,
        terminal_resize,
        terminal_output,
        get_session_pr,
        create_session_pr,
        merge_session_pr,
        list_session_prs,
        get_github_integration,
        add_github_host,
        remove_github_host,
        list_mcp_servers,
        upsert_mcp_server,
        delete_mcp_server,
        mcp_server_logs,
        subscription_health,
        list_automations,
        automation_templates,
        create_automation,
        update_automation,
        delete_automation,
        run_automation,
        code_review_dashboard,
        configure_github_review_app,
        upsert_reviewer_profile,
        delete_reviewer_profile,
        update_code_review_repository,
        refresh_code_reviews,
    ),
    components(schemas(
        ServerInfo,
        RegisterWorkspaceRequest,
        Workspace,
        BranchList,
        CreateSessionRequest,
        Session,
        UpdateSessionRequest,
        CreateThreadRequest,
        Thread,
        UpdateThreadRequest,
        SendMessageRequest,
        TurnAccepted,
        QueuedPrompt,
        UpdateQueuedPromptRequest,
        ReorderQueueRequest,
        ResolveApprovalRequest,
        ResolveQuestionRequest,
        trouve_protocol::Question,
        trouve_protocol::QuestionOption,
        trouve_protocol::QuestionAnswer,
        trouve_protocol::CommandInfo,
        ModelInfo,
        ProviderInfo,
        ProvidersResponse,
        KnownProvider,
        LoginStarted,
        LoginStatus,
        CliInfo,
        CliList,
        CliInstallStatus,
        LocalStatus,
        trouve_protocol::LocalGpu,
        trouve_protocol::LocalModelInfo,
        AddLocalModelRequest,
        SetLocalEnabledRequest,
        UpsertProviderRequest,
        SetDefaultModelRequest,
        SetDefaultPermissionModeRequest,
        UsageSummary,
        SessionDiff,
        DirEntry,
        FileContent,
        OpenTerminalRequest,
        TerminalInfo,
        TerminalInputRequest,
        TerminalResizeRequest,
        PrInfo,
        GithubPrList,
        CreatePrRequest,
        MergePrRequest,
        GithubIntegration,
        trouve_protocol::GithubHostIntegration,
        trouve_protocol::AddGithubHostRequest,
        McpServerInfo,
        UpsertMcpServerRequest,
        McpLogs,
        SubscriptionHealth,
        trouve_protocol::SubscriptionWindow,
        Automation,
        trouve_protocol::AutomationSchedule,
        trouve_protocol::AutomationTemplate,
        UpsertAutomationRequest,
        CodeReviewDashboard,
        ReviewerProfile,
        trouve_protocol::ReviewerOverride,
        trouve_protocol::ReviewerPromptMode,
        CodeReviewRepository,
        trouve_protocol::CodeReviewJob,
        trouve_protocol::CodeReviewMode,
        GithubAppStatus,
        ConfigureGithubAppRequest,
        UpsertReviewerProfileRequest,
        UpdateCodeReviewRepositoryRequest,
        ErrorBody,
        trouve_protocol::EventEnvelope,
        trouve_protocol::Event,
        trouve_protocol::Scope,
        trouve_protocol::Usage,
        trouve_protocol::ToolStatus,
        trouve_protocol::ApprovalDecision,
        trouve_protocol::RestoreDirection,
        trouve_protocol::PermissionMode,
        trouve_protocol::AgentMode,
        ModeInfo,
        UpsertModeRequest,
    ))
)]
struct ApiDoc;

/// The OpenAPI document, stamped with the protocol version. A snapshot test
/// pins this: schema changes must be deliberate.
pub fn openapi_json() -> serde_json::Value {
    let mut doc = ApiDoc::openapi();
    doc.info.version = PROTOCOL_VERSION.to_string();
    serde_json::to_value(doc).expect("openapi doc serializes")
}

/// Access controls for the HTTP surface. The server drives an agent that
/// runs shell commands and edits files, so an open local port is a
/// privilege boundary: any other local process, or a web page via DNS
/// rebinding, could otherwise drive it.
#[derive(Clone, Default)]
pub struct ServerSecurity {
    /// Bearer token required on every `/v1` request. `None` disables the
    /// token check (tests, and explicit opt-out).
    pub token: Option<String>,
    /// Reject requests whose `Host` header isn't loopback. Blocks DNS
    /// rebinding: an attacker page rebinds its hostname to 127.0.0.1, but
    /// the browser still sends that hostname in `Host`, which won't match.
    pub require_loopback_host: bool,
    /// Ephemeral credential for vendor CLI children calling `/internal/*`.
    /// Unlike the API token this is never persisted or user-facing.
    pub internal_token: Option<String>,
}

impl ServerSecurity {
    /// No auth and no host check — for in-process tests and embedders that
    /// bind their own trusted listener.
    pub fn open() -> Self {
        Self::default()
    }

    /// A per-launch token supplied by a trusted embedder (the desktop app):
    /// loopback-only, with a fresh internal bridge token. Nothing is read
    /// from the environment or persisted.
    pub fn with_token(token: String) -> Self {
        Self {
            token: Some(token),
            require_loopback_host: true,
            internal_token: Some(fresh_token()),
        }
    }

    /// Resolve from the environment and data dir:
    /// - token: `TROUVE_AUTH_TOKEN`, else `<data_dir>/auth-token`, else a
    ///   freshly generated token persisted there with 0600 perms.
    ///   `TROUVE_NO_AUTH=1` disables the token (host check stays on).
    /// - host: loopback-only unless `TROUVE_ALLOW_REMOTE` is set.
    pub fn resolve(data_dir: &std::path::Path) -> Self {
        let require_loopback_host = std::env::var_os("TROUVE_ALLOW_REMOTE").is_none();
        let internal_token = Some(fresh_token());
        if std::env::var("TROUVE_NO_AUTH").is_ok_and(|v| v == "1" || v == "true") {
            tracing::warn!("TROUVE_NO_AUTH set: the API is unauthenticated");
            return Self {
                token: None,
                require_loopback_host,
                internal_token,
            };
        }
        let token = match std::env::var("TROUVE_AUTH_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => Self::load_or_create_token(data_dir),
        };
        Self {
            token: Some(token),
            require_loopback_host,
            internal_token,
        }
    }

    fn load_or_create_token(data_dir: &std::path::Path) -> String {
        let path = data_dir.join("auth-token");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let existing = existing.trim().to_string();
            if !existing.is_empty() {
                return existing;
            }
        }
        let token = fresh_token();
        let _ = std::fs::create_dir_all(data_dir);
        if write_private(&path, token.as_bytes()).is_err() {
            tracing::warn!("could not persist auth token to {}", path.display());
        } else {
            tracing::info!("generated API auth token at {}", path.display());
        }
        token
    }
}

/// A 256-bit random bearer token (two v4 UUIDs, hex).
fn fresh_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Write a file readable only by the owner (0600 on unix).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// True when the `Host` header names a loopback address (or `localhost`).
fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    // Strip a trailing :port (but keep IPv6 brackets intact for parsing).
    let hostname = if let Some(stripped) = host.strip_prefix('[') {
        // [::1]:port or [::1]
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    if hostname.eq_ignore_ascii_case("localhost") {
        return true;
    }
    hostname
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Constant-time-ish equality for the bearer token.
fn token_matches(expected: &str, provided: &str) -> bool {
    let (a, b) = (expected.as_bytes(), provided.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn enforce_security(
    security: Arc<ServerSecurity>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let webhook = request.uri().path() == "/github/webhooks";
    if !webhook && security.require_loopback_host && !host_is_loopback(request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            "host not allowed (set TROUVE_ALLOW_REMOTE to serve non-loopback hosts)",
        )
            .into_response();
    }
    let internal = request.uri().path().starts_with("/internal/");
    if internal {
        if let Some(expected) = security.internal_token.as_deref() {
            let provided = request.uri().query().and_then(|query| {
                query
                    .split('&')
                    .filter_map(|part| part.split_once('='))
                    .find_map(|(key, value)| (key == "bridge_token").then_some(value))
            });
            if !provided.is_some_and(|token| token_matches(expected, token)) {
                return (StatusCode::UNAUTHORIZED, "missing or invalid bridge token")
                    .into_response();
            }
        }
    } else if !webhook && let Some(expected) = security.token.as_deref() {
        let provided = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !provided.is_some_and(|p| token_matches(expected, p)) {
            return (StatusCode::UNAUTHORIZED, "missing or invalid auth token").into_response();
        }
    }
    next.run(request).await
}

/// Wrap the router with host + token enforcement.
pub fn build_secured_router(engine: Arc<Engine>, security: ServerSecurity) -> Router {
    engine.set_bridge_token(security.internal_token.clone());
    let security = Arc::new(security);
    build_router(engine).layer(axum::middleware::from_fn(move |req, next| {
        let security = security.clone();
        async move { enforce_security(security, req, next).await }
    }))
}

pub fn build_router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/openapi.json", get(openapi))
        .route(
            "/v1/workspaces",
            post(register_workspace).get(list_workspaces),
        )
        .route(
            "/v1/workspaces/{id}",
            axum::routing::delete(close_workspace),
        )
        .route("/v1/workspaces/{id}/branches", get(workspace_branches))
        .route("/v1/github/prs/refresh", post(refresh_github_prs))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route(
            "/v1/sessions/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/v1/sessions/{id}/undo", post(undo_session))
        .route("/v1/sessions/{id}/redo", post(redo_session))
        .route("/v1/sessions/{id}/events", get(session_events))
        .route("/v1/sessions/{id}/usage", get(session_usage))
        .route("/v1/sessions/{id}/mcp-servers", get(session_mcp_servers))
        .route("/v1/sessions/{id}/diff", get(session_diff))
        .route("/v1/sessions/{id}/files", get(session_files))
        .route("/v1/sessions/{id}/paths", get(session_paths))
        .route("/v1/sessions/{id}/file", get(session_file))
        .route("/v1/sessions/{id}/terminal", post(open_terminal))
        .route("/v1/terminals/{id}", axum::routing::delete(kill_terminal))
        .route("/v1/terminals/{id}/input", post(terminal_input))
        .route("/v1/terminals/{id}/resize", post(terminal_resize))
        .route("/v1/terminals/{id}/output", get(terminal_output))
        .route(
            "/v1/sessions/{id}/pr",
            get(get_session_pr).post(create_session_pr),
        )
        .route("/v1/sessions/{id}/pr/merge", post(merge_session_pr))
        .route("/v1/sessions/{id}/prs", get(list_session_prs))
        .route("/v1/integrations/github", get(get_github_integration))
        .route("/v1/integrations/github/hosts", post(add_github_host))
        .route(
            "/v1/integrations/github/hosts/{host}",
            axum::routing::delete(remove_github_host),
        )
        .route("/v1/mcp-servers", get(list_mcp_servers))
        .route(
            "/v1/mcp-servers/{name}",
            axum::routing::put(upsert_mcp_server).delete(delete_mcp_server),
        )
        .route("/v1/mcp-servers/{name}/logs", get(mcp_server_logs))
        .route("/v1/subscriptions", get(subscription_health))
        .route("/v1/models", get(list_models))
        .route("/v1/modes", get(list_modes))
        .route("/v1/mode-infos", get(list_mode_infos))
        .route(
            "/v1/modes/{id}",
            axum::routing::put(upsert_mode).delete(delete_mode),
        )
        .route("/v1/providers", get(list_providers))
        .route("/v1/providers/known", get(known_providers))
        .route(
            "/v1/providers/{id}",
            axum::routing::put(upsert_provider).delete(delete_provider),
        )
        .route(
            "/v1/providers/{id}/login",
            post(start_login).get(login_status),
        )
        .route("/v1/clis", get(list_clis))
        .route(
            "/v1/clis/{id}/install",
            post(start_cli_install)
                .get(cli_install_status)
                .delete(cancel_cli_install),
        )
        .route("/v1/clis/{id}", axum::routing::delete(uninstall_cli))
        .route(
            "/v1/automations",
            get(list_automations).post(create_automation),
        )
        // Static segment must not collide with `{id}`: axum's router gives
        // literal segments precedence, so /automations/templates wins.
        .route("/v1/automations/templates", get(automation_templates))
        .route(
            "/v1/automations/{id}",
            axum::routing::put(update_automation).delete(delete_automation),
        )
        .route("/v1/automations/{id}/run", post(run_automation))
        .route("/v1/code-review", get(code_review_dashboard))
        .route(
            "/v1/code-review/github-app",
            axum::routing::put(configure_github_review_app),
        )
        .route(
            "/v1/code-review/reviewer",
            axum::routing::put(upsert_reviewer_profile),
        )
        .route(
            "/v1/code-review/reviewer/{id}",
            axum::routing::delete(delete_reviewer_profile),
        )
        .route(
            "/v1/code-review/repository",
            axum::routing::put(update_code_review_repository),
        )
        .route("/v1/code-review/refresh", post(refresh_code_reviews))
        // GitHub cannot attach trouve's bearer token. This one public route
        // is authenticated in its handler with the configured HMAC secret.
        .route("/github/webhooks", post(github_review_webhook))
        .route("/v1/local", get(local_status))
        .route("/v1/local/enabled", axum::routing::put(set_local_enabled))
        .route("/v1/local/search", get(search_local_models))
        .route("/v1/local/models", post(add_local_model))
        .route(
            "/v1/local/models/{id}",
            axum::routing::delete(delete_local_model),
        )
        .route(
            "/v1/local/models/{id}/download",
            post(start_local_model_download).delete(cancel_local_model_download),
        )
        .route("/v1/local/server/stop", post(stop_local_server))
        .route("/v1/local/server/restart", post(restart_local_server))
        .route(
            "/v1/config/default-model",
            axum::routing::put(set_default_model),
        )
        .route(
            "/v1/config/default-permission-mode",
            axum::routing::put(set_default_permission_mode),
        )
        .route("/v1/threads", post(create_thread).get(list_threads))
        .route("/v1/threads/{id}", get(get_thread).patch(update_thread))
        .route("/v1/threads/{id}/messages", post(send_message))
        .route("/v1/attachments/{id}", get(get_attachment))
        .route("/v1/threads/{id}/queue", get(list_queue).put(reorder_queue))
        .route("/v1/threads/{id}/queue/dispatch", post(dispatch_queue))
        .route("/v1/threads/{id}/cancel", post(cancel_turn))
        .route(
            "/v1/queue/{id}",
            axum::routing::patch(update_queued_prompt).delete(delete_queued_prompt),
        )
        .route("/v1/threads/{id}/events", get(thread_events))
        .route("/v1/threads/{id}/usage", get(thread_usage))
        .route("/v1/approvals", post(resolve_approval))
        .route("/v1/questions", post(resolve_question))
        .route("/v1/events", get(server_events))
        // Internal (undocumented, same-host trust domain): streamable-HTTP
        // MCP endpoint bridging external agent backends into trouve's
        // tools and approval gate.
        .route("/internal/threads/{id}/mcp", post(mcp::mcp_endpoint))
        // Attachment uploads ride base64 inside the JSON body; axum's 2 MB
        // default would cap a prompt at roughly one screenshot.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(engine)
}

pub async fn serve(
    engine: Arc<Engine>,
    addr: std::net::SocketAddr,
    security: ServerSecurity,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_listener(engine, listener, security).await
}

/// Bootstrap the full local stack — store, real config file (provider
/// changes write back), index hooks, system connectivity probe — and bind
/// `addr` (port 0 for ephemeral). Returns the bound address and the serve
/// future.
///
/// This is the single entry point for embedders (the desktop app, ADR
/// 0008) and the standalone binary alike: an embedder spawns the future
/// and speaks HTTP + SSE to the returned address, keeping the protocol
/// boundary intact without ever touching engine internals.
pub async fn bind_local(
    addr: std::net::SocketAddr,
    security: ServerSecurity,
) -> anyhow::Result<(
    std::net::SocketAddr,
    impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
)> {
    install_crypto_provider();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let data = trouve_core::config::data_dir();
    let store = trouve_core::store::Store::open(&data.join("trouve.db"))?;
    let config = trouve_core::config::Config::load();
    let engine = Arc::new(
        Engine::new(store, data, &config)
            .with_config_file(Some(trouve_core::config::config_path()))
            .with_index_hooks()
            .with_connectivity_probe(trouve_core::connectivity::system_probe()),
    );
    Ok((local, serve_listener(engine, listener, security)))
}

/// Serve on an already-bound listener (embedded mode: bind port 0, read the
/// local address, then serve).
pub async fn serve_listener(
    engine: Arc<Engine>,
    listener: tokio::net::TcpListener,
    security: ServerSecurity,
) -> anyhow::Result<()> {
    // Backends dialing back in (MCP tool bridge) need our reachable URL;
    // build_secured_router injects their separate ephemeral bridge token.
    engine.set_base_url(&format!("http://{}", listener.local_addr()?));
    // Resolve connectivity before accepting requests so an offline start
    // never serves a model list it immediately retracts (no-op without a
    // configured probe).
    engine.init_connectivity().await;
    engine.start_connectivity_monitor();
    engine.start_automation_scheduler();
    engine.start_code_review_service();
    let router = build_secured_router(engine, security);
    tracing::info!(
        "trouve-server listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, router).await?;
    Ok(())
}

// --- handlers --------------------------------------------------------------

#[utoipa::path(get, path = "/v1/info", responses((status = 200, body = ServerInfo)))]
async fn info(State(engine): State<Arc<Engine>>) -> Json<ServerInfo> {
    Json(ServerInfo {
        name: "trouve-server".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION.into(),
        online: engine.is_online(),
    })
}

#[utoipa::path(get, path = "/v1/code-review",
    responses((status = 200, body = CodeReviewDashboard), (status = 500, body = ErrorBody)))]
async fn code_review_dashboard(
    State(engine): State<Arc<Engine>>,
) -> Result<Json<CodeReviewDashboard>, ApiError> {
    Ok(Json(engine.code_review_dashboard()?))
}

#[utoipa::path(put, path = "/v1/code-review/github-app",
    request_body = ConfigureGithubAppRequest,
    responses((status = 200, body = GithubAppStatus), (status = 400, body = ErrorBody)))]
async fn configure_github_review_app(
    State(engine): State<Arc<Engine>>,
    Json(request): Json<ConfigureGithubAppRequest>,
) -> Result<Json<GithubAppStatus>, ApiError> {
    Ok(Json(engine.configure_github_review_app(request).await?))
}

#[utoipa::path(put, path = "/v1/code-review/reviewer",
    request_body = UpsertReviewerProfileRequest,
    responses((status = 200, body = ReviewerProfile), (status = 400, body = ErrorBody)))]
async fn upsert_reviewer_profile(
    State(engine): State<Arc<Engine>>,
    Json(request): Json<UpsertReviewerProfileRequest>,
) -> Result<Json<ReviewerProfile>, ApiError> {
    Ok(Json(engine.upsert_reviewer_profile(request)?))
}

#[utoipa::path(delete, path = "/v1/code-review/reviewer/{id}",
    params(("id" = String, Path, description = "Custom reviewer profile id")),
    responses((status = 204), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn delete_reviewer_profile(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.delete_reviewer_profile(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/v1/code-review/repository",
    request_body = UpdateCodeReviewRepositoryRequest,
    responses((status = 200, body = CodeReviewRepository), (status = 400, body = ErrorBody)))]
async fn update_code_review_repository(
    State(engine): State<Arc<Engine>>,
    Json(request): Json<UpdateCodeReviewRepositoryRequest>,
) -> Result<Json<CodeReviewRepository>, ApiError> {
    Ok(Json(engine.update_code_review_repository(&request)?))
}

#[utoipa::path(post, path = "/v1/code-review/refresh",
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn refresh_code_reviews(State(engine): State<Arc<Engine>>) -> Result<StatusCode, ApiError> {
    engine.refresh_code_reviews().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn github_review_webhook(
    State(engine): State<Arc<Engine>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, ApiError> {
    let header = |name: &'static str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| EngineError::BadRequest(format!("missing {name}")))
    };
    engine.accept_github_review_webhook(
        header("x-github-event")?,
        header("x-github-delivery")?,
        header("x-hub-signature-256")?,
        &body,
    )?;
    Ok(StatusCode::ACCEPTED)
}

async fn openapi() -> Json<serde_json::Value> {
    Json(openapi_json())
}

#[utoipa::path(post, path = "/v1/workspaces", request_body = RegisterWorkspaceRequest,
    responses((status = 200, body = Workspace), (status = 400, body = ErrorBody)))]
async fn register_workspace(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<RegisterWorkspaceRequest>,
) -> Result<Json<Workspace>, ApiError> {
    Ok(Json(engine.register_workspace(&req.path, req.name)?))
}

#[utoipa::path(get, path = "/v1/workspaces", responses((status = 200, body = [Workspace])))]
async fn list_workspaces(
    State(engine): State<Arc<Engine>>,
) -> Result<Json<Vec<Workspace>>, ApiError> {
    Ok(Json(engine.list_workspaces()?))
}

#[utoipa::path(delete, path = "/v1/workspaces/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn close_workspace(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.close_workspace(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/workspaces/{id}/branches", params(("id" = String, Path,)),
    responses((status = 200, body = BranchList), (status = 404, body = ErrorBody)))]
async fn workspace_branches(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<BranchList>, ApiError> {
    Ok(Json(engine.workspace_branches(&id).await?))
}

#[utoipa::path(post, path = "/v1/github/prs/refresh",
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn refresh_github_prs(
    State(engine): State<Arc<Engine>>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.refresh_github_prs().await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/sessions", request_body = CreateSessionRequest,
    responses((status = 200, body = Session), (status = 404, body = ErrorBody)))]
async fn create_session(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<Session>, ApiError> {
    Ok(Json(engine.create_session(req).await?))
}

#[derive(Deserialize)]
struct ListSessionsQuery {
    workspace_id: Option<String>,
}

#[utoipa::path(get, path = "/v1/sessions",
    params(("workspace_id" = Option<String>, Query, description = "Filter by workspace")),
    responses((status = 200, body = [Session])))]
async fn list_sessions(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<Vec<Session>>, ApiError> {
    Ok(Json(engine.list_sessions(q.workspace_id.as_deref())?))
}

#[utoipa::path(get, path = "/v1/sessions/{id}", params(("id" = String, Path,)),
    responses((status = 200, body = Session), (status = 404, body = ErrorBody)))]
async fn get_session(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Session>, ApiError> {
    Ok(Json(engine.get_session(&id)?))
}

#[utoipa::path(patch, path = "/v1/sessions/{id}", params(("id" = String, Path,)),
    request_body = UpdateSessionRequest,
    responses((status = 200, body = Session), (status = 404, body = ErrorBody)))]
async fn update_session(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionRequest>,
) -> Result<Json<Session>, ApiError> {
    Ok(Json(engine.update_session(&id, &req)?))
}

#[utoipa::path(delete, path = "/v1/sessions/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_session(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.delete_session(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/sessions/{id}/undo", params(("id" = String, Path,)),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn undo_session(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.undo(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/sessions/{id}/redo", params(("id" = String, Path,)),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn redo_session(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.redo(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/threads", request_body = CreateThreadRequest,
    responses((status = 200, body = Thread), (status = 400, body = ErrorBody)))]
async fn create_thread(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<CreateThreadRequest>,
) -> Result<Json<Thread>, ApiError> {
    Ok(Json(engine.create_thread(req)?))
}

#[derive(Deserialize)]
struct ListThreadsQuery {
    session_id: String,
}

#[utoipa::path(get, path = "/v1/threads",
    params(("session_id" = String, Query,)),
    responses((status = 200, body = [Thread])))]
async fn list_threads(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListThreadsQuery>,
) -> Result<Json<Vec<Thread>>, ApiError> {
    Ok(Json(engine.list_threads(&q.session_id)?))
}

#[utoipa::path(get, path = "/v1/threads/{id}", params(("id" = String, Path,)),
    responses((status = 200, body = Thread), (status = 404, body = ErrorBody)))]
async fn get_thread(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Thread>, ApiError> {
    Ok(Json(engine.get_thread(&id)?))
}

#[utoipa::path(patch, path = "/v1/threads/{id}", params(("id" = String, Path,)),
    request_body = UpdateThreadRequest,
    responses((status = 200, body = Thread), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn update_thread(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateThreadRequest>,
) -> Result<Json<Thread>, ApiError> {
    Ok(Json(engine.update_thread(&id, &req)?))
}

#[utoipa::path(post, path = "/v1/threads/{id}/messages",
    params(("id" = String, Path,)), request_body = SendMessageRequest,
    responses((status = 202, body = TurnAccepted), (status = 404, body = ErrorBody)))]
async fn send_message(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<TurnAccepted>), ApiError> {
    let accepted = engine.send_message(&id, req.content, req.attachments)?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

/// Raw bytes of a stored prompt attachment, with its uploaded MIME type.
#[utoipa::path(get, path = "/v1/attachments/{id}", params(("id" = String, Path,)),
    responses((status = 200, body = String), (status = 404, body = ErrorBody)))]
async fn get_attachment(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let (attachment, path) = engine.attachment(&id)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| ApiError::from(EngineError::NotFound(format!("attachment {id}: {e}"))))?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, attachment.mime),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{}\"", attachment.name.replace('"', "")),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[utoipa::path(get, path = "/v1/threads/{id}/queue", params(("id" = String, Path,)),
    responses((status = 200, body = [QueuedPrompt]), (status = 404, body = ErrorBody)))]
async fn list_queue(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<QueuedPrompt>>, ApiError> {
    Ok(Json(engine.list_queued_prompts(&id)?))
}

#[utoipa::path(put, path = "/v1/threads/{id}/queue", params(("id" = String, Path,)),
    request_body = ReorderQueueRequest,
    responses((status = 200, body = [QueuedPrompt]), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn reorder_queue(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<ReorderQueueRequest>,
) -> Result<Json<Vec<QueuedPrompt>>, ApiError> {
    engine.reorder_queue(&id, &req.ids)?;
    Ok(Json(engine.list_queued_prompts(&id)?))
}

/// Kick an idle thread into draining its queue. Queued prompts never
/// auto-run at startup (a crash may have cut the previous turn short) and a
/// failed turn pauses its queue — both wait for this explicit resume.
#[utoipa::path(post, path = "/v1/threads/{id}/queue/dispatch", params(("id" = String, Path,)),
    responses((status = 202, body = TurnAccepted), (status = 404, body = ErrorBody)))]
async fn dispatch_queue(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<TurnAccepted>), ApiError> {
    let turn = engine.dispatch_queue(&id)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(TurnAccepted {
            thread_id: id,
            turn: turn.unwrap_or(0),
            queued: turn.is_none(),
        }),
    ))
}

#[utoipa::path(post, path = "/v1/threads/{id}/cancel", params(("id" = String, Path,)),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn cancel_turn(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.cancel_turn(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(patch, path = "/v1/queue/{id}", params(("id" = String, Path,)),
    request_body = UpdateQueuedPromptRequest,
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn update_queued_prompt(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateQueuedPromptRequest>,
) -> Result<StatusCode, ApiError> {
    engine.update_queued_prompt(&id, &req.content)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/queue/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_queued_prompt(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.delete_queued_prompt(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/approvals", request_body = ResolveApprovalRequest,
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn resolve_approval(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<ResolveApprovalRequest>,
) -> Result<StatusCode, ApiError> {
    engine.resolve_approval(&req.call_id, req.decision)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/questions", request_body = ResolveQuestionRequest,
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn resolve_question(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<ResolveQuestionRequest>,
) -> Result<StatusCode, ApiError> {
    engine.resolve_question(&req.request_id, req.answers)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/models", responses((status = 200, body = [ModelInfo])))]
async fn list_models(State(engine): State<Arc<Engine>>) -> Json<Vec<ModelInfo>> {
    Json(engine.list_models().await)
}

#[utoipa::path(get, path = "/v1/providers", responses((status = 200, body = ProvidersResponse)))]
async fn list_providers(State(engine): State<Arc<Engine>>) -> Json<ProvidersResponse> {
    Json(engine.list_providers())
}

#[utoipa::path(get, path = "/v1/providers/known", responses((status = 200, body = [KnownProvider])))]
async fn known_providers(State(engine): State<Arc<Engine>>) -> Json<Vec<KnownProvider>> {
    Json(engine.known_providers())
}

#[utoipa::path(put, path = "/v1/providers/{id}", params(("id" = String, Path,)),
    request_body = UpsertProviderRequest,
    responses((status = 200, body = ProviderInfo), (status = 400, body = ErrorBody)))]
async fn upsert_provider(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpsertProviderRequest>,
) -> Result<Json<ProviderInfo>, ApiError> {
    Ok(Json(engine.upsert_provider(&id, &req)?))
}

#[utoipa::path(post, path = "/v1/providers/{id}/login", params(("id" = String, Path,)),
    responses((status = 200, body = LoginStarted), (status = 400, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn start_login(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<LoginStarted>, ApiError> {
    Ok(Json(engine.start_login(&id).await?))
}

#[utoipa::path(get, path = "/v1/providers/{id}/login", params(("id" = String, Path,)),
    responses((status = 200, body = LoginStatus)))]
async fn login_status(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Json<LoginStatus> {
    Json(engine.login_status(&id))
}

#[utoipa::path(get, path = "/v1/clis", responses((status = 200, body = CliList)))]
async fn list_clis(State(engine): State<Arc<Engine>>) -> Json<CliList> {
    Json(engine.list_clis().await)
}

#[utoipa::path(post, path = "/v1/clis/{id}/install", params(("id" = String, Path,)),
    responses((status = 202), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn start_cli_install(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.start_cli_install(&id)?;
    Ok(axum::http::StatusCode::ACCEPTED)
}

#[utoipa::path(get, path = "/v1/clis/{id}/install", params(("id" = String, Path,)),
    responses((status = 200, body = CliInstallStatus)))]
async fn cli_install_status(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Json<CliInstallStatus> {
    Json(engine.cli_install_status(&id))
}

/// Cancel an in-flight install; the CLI returns to its previous state.
#[utoipa::path(delete, path = "/v1/clis/{id}/install", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn cancel_cli_install(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.cancel_cli_install(&id)?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Remove the managed install of a CLI (a system install found on PATH is
/// untouched and will be used again if present).
#[utoipa::path(delete, path = "/v1/clis/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn uninstall_cli(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.uninstall_cli(&id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/local", responses((status = 200, body = LocalStatus)))]
async fn local_status(State(engine): State<Arc<Engine>>) -> Json<LocalStatus> {
    Json(engine.local_status().await)
}

/// Enable or disable local models. Disabling stops the llama-server
/// sidecar and unregisters the "local" provider.
#[utoipa::path(put, path = "/v1/local/enabled", request_body = SetLocalEnabledRequest,
    responses((status = 204)))]
async fn set_local_enabled(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<SetLocalEnabledRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.set_local_enabled(req.enabled).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/local/models", request_body = AddLocalModelRequest,
    responses((status = 204), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn add_local_model(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<AddLocalModelRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.add_local_model(req).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/local/models/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_local_model(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.delete_local_model(&id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/local/models/{id}/download", params(("id" = String, Path,)),
    responses((status = 202), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn start_local_model_download(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.start_local_model_download(&id)?;
    Ok(axum::http::StatusCode::ACCEPTED)
}

#[utoipa::path(post, path = "/v1/local/server/stop", responses((status = 204)))]
async fn stop_local_server(State(engine): State<Arc<Engine>>) -> axum::http::StatusCode {
    engine.stop_local_server().await;
    axum::http::StatusCode::NO_CONTENT
}

/// Cancel an in-flight model download; the partial file is deleted.
#[utoipa::path(delete, path = "/v1/local/models/{id}/download", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn cancel_local_model_download(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.cancel_local_model_download(&id)?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/automations", responses((status = 200, body = [Automation])))]
async fn list_automations(
    State(engine): State<Arc<Engine>>,
) -> Result<Json<Vec<Automation>>, ApiError> {
    Ok(Json(engine.list_automations()?))
}

/// Static catalog of pre-canned automations for common development tasks.
#[utoipa::path(get, path = "/v1/automations/templates",
    responses((status = 200, body = [trouve_protocol::AutomationTemplate])))]
async fn automation_templates() -> Json<Vec<trouve_protocol::AutomationTemplate>> {
    Json(trouve_core::automations::templates())
}

#[utoipa::path(post, path = "/v1/automations", request_body = UpsertAutomationRequest,
    responses((status = 200, body = Automation), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody)))]
async fn create_automation(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<UpsertAutomationRequest>,
) -> Result<Json<Automation>, ApiError> {
    Ok(Json(engine.create_automation(req)?))
}

#[utoipa::path(put, path = "/v1/automations/{id}", params(("id" = String, Path,)),
    request_body = UpsertAutomationRequest,
    responses((status = 200, body = Automation), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody)))]
async fn update_automation(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpsertAutomationRequest>,
) -> Result<Json<Automation>, ApiError> {
    Ok(Json(engine.update_automation(&id, req)?))
}

#[utoipa::path(delete, path = "/v1/automations/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_automation(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.delete_automation(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fire the automation immediately (in the background); the outcome shows
/// up on the automation's last_* fields and an `automation.fired` event.
#[utoipa::path(post, path = "/v1/automations/{id}/run", params(("id" = String, Path,)),
    responses((status = 202), (status = 404, body = ErrorBody)))]
async fn run_automation(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.run_automation_now(&id)?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct LocalSearchQuery {
    q: String,
}

/// Search HuggingFace for GGUF repos, with per-file hardware-fit guidance
/// for this machine.
#[utoipa::path(get, path = "/v1/local/search",
    params(("q" = String, Query, description = "Search text")),
    responses((status = 200, body = [LocalSearchResult]), (status = 400, body = ErrorBody)))]
async fn search_local_models(
    State(engine): State<Arc<Engine>>,
    axum::extract::Query(query): axum::extract::Query<LocalSearchQuery>,
) -> Result<Json<Vec<LocalSearchResult>>, ApiError> {
    Ok(Json(engine.search_local_models(&query.q).await?))
}

/// Restart llama-server with the model it is serving (reload happens in
/// the background; poll `GET /v1/local` for server_status).
#[utoipa::path(post, path = "/v1/local/server/restart",
    responses((status = 202), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn restart_local_server(
    State(engine): State<Arc<Engine>>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.restart_local_server().await?;
    Ok(axum::http::StatusCode::ACCEPTED)
}

#[utoipa::path(delete, path = "/v1/providers/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_provider(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.delete_provider(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/v1/config/default-model",
    request_body = SetDefaultModelRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn set_default_model(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<SetDefaultModelRequest>,
) -> Result<StatusCode, ApiError> {
    engine.set_default_model(&req.model, req.default_thinking_level.as_deref())?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/v1/config/default-permission-mode",
    request_body = SetDefaultPermissionModeRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn set_default_permission_mode(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<SetDefaultPermissionModeRequest>,
) -> Result<StatusCode, ApiError> {
    engine.set_default_permission_mode(req.permission_mode)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/threads/{id}/usage", params(("id" = String, Path,)),
    responses((status = 200, body = UsageSummary), (status = 404, body = ErrorBody)))]
async fn thread_usage(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<UsageSummary>, ApiError> {
    Ok(Json(engine.thread_usage(&id)?))
}

#[utoipa::path(get, path = "/v1/sessions/{id}/mcp-servers", params(("id" = String, Path,)),
    responses((status = 200, body = [McpServerInfo]), (status = 404, body = ErrorBody)))]
async fn session_mcp_servers(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<McpServerInfo>>, ApiError> {
    Ok(Json(engine.session_mcp_servers(&id)?))
}

#[utoipa::path(get, path = "/v1/sessions/{id}/usage", params(("id" = String, Path,)),
    responses((status = 200, body = UsageSummary), (status = 404, body = ErrorBody)))]
async fn session_usage(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<UsageSummary>, ApiError> {
    Ok(Json(engine.session_usage(&id)?))
}

#[derive(Deserialize)]
struct ListModesQuery {
    workspace_id: Option<String>,
}

#[utoipa::path(get, path = "/v1/modes",
    params(("workspace_id" = Option<String>, Query, description = "Include the workspace's .agents modes")),
    responses((status = 200, body = [AgentMode])))]
async fn list_modes(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListModesQuery>,
) -> Result<Json<Vec<AgentMode>>, ApiError> {
    Ok(Json(engine.list_modes(q.workspace_id.as_deref())?))
}

#[utoipa::path(get, path = "/v1/mode-infos",
    params(("workspace_id" = Option<String>, Query, description = "Include the workspace's .agents modes")),
    responses((status = 200, body = [ModeInfo])))]
async fn list_mode_infos(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListModesQuery>,
) -> Result<Json<Vec<ModeInfo>>, ApiError> {
    Ok(Json(engine.list_mode_infos(q.workspace_id.as_deref())?))
}

#[utoipa::path(put, path = "/v1/modes/{id}", params(("id" = String, Path,)),
    request_body = UpsertModeRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn upsert_mode(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpsertModeRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.upsert_mode(&id, req)?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/modes/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn delete_mode(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.delete_mode(&id)?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/sessions/{id}/diff", params(("id" = String, Path,)),
    responses((status = 200, body = SessionDiff), (status = 404, body = ErrorBody)))]
async fn session_diff(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<SessionDiff>, ApiError> {
    Ok(Json(SessionDiff {
        diff: engine.session_diff(&id).await?,
    }))
}

#[derive(Deserialize)]
struct PathQuery {
    #[serde(default = "default_dot")]
    path: String,
}

fn default_dot() -> String {
    ".".into()
}

#[utoipa::path(get, path = "/v1/sessions/{id}/files",
    params(("id" = String, Path,), ("path" = Option<String>, Query, description = "Worktree-relative directory")),
    responses((status = 200, body = [DirEntry]), (status = 404, body = ErrorBody)))]
async fn session_files(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<Vec<DirEntry>>, ApiError> {
    Ok(Json(engine.session_list_dir(&id, &q.path).await?))
}

#[utoipa::path(get, path = "/v1/sessions/{id}/paths", params(("id" = String, Path,)),
    responses((status = 200, body = [String]), (status = 404, body = ErrorBody)))]
async fn session_paths(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(engine.session_list_paths(&id).await?))
}

#[utoipa::path(get, path = "/v1/sessions/{id}/file",
    params(("id" = String, Path,), ("path" = String, Query, description = "Worktree-relative file")),
    responses((status = 200, body = FileContent), (status = 404, body = ErrorBody)))]
async fn session_file(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<FileContent>, ApiError> {
    let content = engine.session_read_file(&id, &q.path).await?;
    Ok(Json(FileContent {
        path: q.path,
        content,
    }))
}

#[utoipa::path(post, path = "/v1/sessions/{id}/terminal", params(("id" = String, Path,)),
    request_body = OpenTerminalRequest,
    responses((status = 200, body = TerminalInfo), (status = 404, body = ErrorBody)))]
async fn open_terminal(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<OpenTerminalRequest>,
) -> Result<Json<TerminalInfo>, ApiError> {
    Ok(Json(engine.open_terminal(&id, req.cols, req.rows)?))
}

#[utoipa::path(delete, path = "/v1/terminals/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn kill_terminal(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.terminal_kill(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/terminals/{id}/input", params(("id" = String, Path,)),
    request_body = TerminalInputRequest,
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn terminal_input(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<TerminalInputRequest>,
) -> Result<StatusCode, ApiError> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&req.data)
        .map_err(|e| EngineError::BadRequest(format!("bad base64 input: {e}")))?;
    engine.terminal_input(&id, &bytes)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/terminals/{id}/resize", params(("id" = String, Path,)),
    request_body = TerminalResizeRequest,
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn terminal_resize(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<TerminalResizeRequest>,
) -> Result<StatusCode, ApiError> {
    engine.terminal_resize(&id, req.cols, req.rows)?;
    Ok(StatusCode::NO_CONTENT)
}

/// PTY output as SSE: each event's `id` is the byte offset *after* the
/// chunk, data is base64. `?after=` resumes from an offset (bytes older
/// than the retained backlog are silently skipped). A final `exit` event
/// marks shell exit. Ephemeral — not part of the persisted event log.
#[utoipa::path(get, path = "/v1/terminals/{id}/output",
    params(("id" = String, Path,), ("after" = Option<u64>, Query, description = "Resume byte offset")),
    responses((status = 200, description = "SSE stream of base64 output chunks"),
              (status = 404, body = ErrorBody)))]
async fn terminal_output(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (start, replay, mut live, exited) = engine.terminal_subscribe(&id, q.after.unwrap_or(0))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    tokio::spawn(async move {
        let mut offset = start;
        if !replay.is_empty() {
            offset += replay.len() as u64;
            let ev = SseEvent::default()
                .id(offset.to_string())
                .data(b64.encode(&replay));
            if tx.send(ev).await.is_err() {
                return;
            }
        }
        if exited {
            let _ = tx.send(SseEvent::default().event("exit").data("")).await;
            return;
        }
        loop {
            match live.recv().await {
                // Empty chunk = the reader thread's end-of-stream sentinel.
                Ok(chunk) if chunk.is_empty() => {
                    let _ = tx.send(SseEvent::default().event("exit").data("")).await;
                    return;
                }
                Ok(chunk) => {
                    offset += chunk.len() as u64;
                    let ev = SseEvent::default()
                        .id(offset.to_string())
                        .data(b64.encode(&chunk));
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Dropped chunks would corrupt the escape-code stream;
                    // tell the client to reconnect (it replays the backlog).
                    let _ = tx.send(SseEvent::default().event("lagged").data("")).await;
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    Ok(
        Sse::new(futures::StreamExt::map(ReceiverStream::new(rx), Ok))
            .keep_alive(KeepAlive::default()),
    )
}

#[utoipa::path(get, path = "/v1/sessions/{id}/pr", params(("id" = String, Path,)),
    responses((status = 200, body = Option<PrInfo>), (status = 404, body = ErrorBody)))]
async fn get_session_pr(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Option<PrInfo>>, ApiError> {
    Ok(Json(engine.session_pr(&id).await?))
}

#[utoipa::path(post, path = "/v1/sessions/{id}/pr", params(("id" = String, Path,)),
    request_body = CreatePrRequest,
    responses((status = 200, body = PrInfo), (status = 400, body = ErrorBody)))]
async fn create_session_pr(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<CreatePrRequest>,
) -> Result<Json<PrInfo>, ApiError> {
    Ok(Json(engine.create_session_pr(&id, &req).await?))
}

#[utoipa::path(post, path = "/v1/sessions/{id}/pr/merge", params(("id" = String, Path,)),
    request_body = MergePrRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn merge_session_pr(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<MergePrRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.merge_session_pr(&id, req.method.as_deref()).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/sessions/{id}/prs", params(("id" = String, Path,)),
    responses((status = 200, body = [PrInfo]), (status = 400, body = ErrorBody)))]
async fn list_session_prs(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<PrInfo>>, ApiError> {
    Ok(Json(engine.session_prs(&id).await?))
}

#[derive(Deserialize)]
struct McpListQuery {
    workspace_id: Option<String>,
    /// Spawn each server and run the MCP handshake to report health.
    #[serde(default)]
    probe: bool,
}

#[utoipa::path(get, path = "/v1/mcp-servers",
    params(
        ("workspace_id" = Option<String>, Query, description = "Include the workspace's .agents servers"),
        ("probe" = Option<bool>, Query, description = "Health-check each server"),
    ),
    responses((status = 200, body = [McpServerInfo]), (status = 404, body = ErrorBody)))]
async fn list_mcp_servers(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<McpListQuery>,
) -> Result<Json<Vec<McpServerInfo>>, ApiError> {
    Ok(Json(
        engine
            .list_mcp_servers(q.workspace_id.as_deref(), q.probe)
            .await?,
    ))
}

#[utoipa::path(put, path = "/v1/mcp-servers/{name}", params(("name" = String, Path,)),
    request_body = UpsertMcpServerRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn upsert_mcp_server(
    State(engine): State<Arc<Engine>>,
    Path(name): Path<String>,
    Json(req): Json<UpsertMcpServerRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.upsert_mcp_server(&name, &req)?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct McpDeleteQuery {
    scope: String,
    workspace_id: Option<String>,
}

#[utoipa::path(delete, path = "/v1/mcp-servers/{name}",
    params(
        ("name" = String, Path,),
        ("scope" = String, Query, description = "user or workspace"),
        ("workspace_id" = Option<String>, Query,),
    ),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn delete_mcp_server(
    State(engine): State<Arc<Engine>>,
    Path(name): Path<String>,
    Query(q): Query<McpDeleteQuery>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.delete_mcp_server(&name, &q.scope, q.workspace_id.as_deref())?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/mcp-servers/{name}/logs", params(("name" = String, Path,)),
    responses((status = 200, body = McpLogs)))]
async fn mcp_server_logs(
    State(engine): State<Arc<Engine>>,
    Path(name): Path<String>,
) -> Json<McpLogs> {
    Json(engine.mcp_server_logs(&name))
}

#[utoipa::path(get, path = "/v1/subscriptions",
    responses((status = 200, body = [SubscriptionHealth])))]
async fn subscription_health(State(engine): State<Arc<Engine>>) -> Json<Vec<SubscriptionHealth>> {
    Json(engine.subscription_health().await)
}

#[utoipa::path(get, path = "/v1/integrations/github",
    responses((status = 200, body = GithubIntegration)))]
async fn get_github_integration(State(engine): State<Arc<Engine>>) -> Json<GithubIntegration> {
    Json(engine.github_integration())
}

/// Register a self-hosted GitHub Enterprise instance.
#[utoipa::path(post, path = "/v1/integrations/github/hosts",
    request_body = trouve_protocol::AddGithubHostRequest,
    responses((status = 200, body = GithubIntegration), (status = 400, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn add_github_host(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<trouve_protocol::AddGithubHostRequest>,
) -> Result<Json<GithubIntegration>, ApiError> {
    engine.add_github_host(&req.host, &req.client_id)?;
    Ok(Json(engine.github_integration()))
}

/// Remove an enterprise host (and forget its stored secrets).
#[utoipa::path(delete, path = "/v1/integrations/github/hosts/{host}",
    params(("host" = String, Path,)),
    responses((status = 200, body = GithubIntegration), (status = 404, body = ErrorBody)))]
async fn remove_github_host(
    State(engine): State<Arc<Engine>>,
    Path(host): Path<String>,
) -> Result<Json<GithubIntegration>, ApiError> {
    engine.remove_github_host(&host).await?;
    Ok(Json(engine.github_integration()))
}

// --- SSE -------------------------------------------------------------------

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<u64>,
}

fn resume_cursor(headers: &HeaderMap, q: &EventsQuery) -> u64 {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .or(q.after)
        .unwrap_or(0)
}

const EVENT_REPLAY_PAGE_SIZE: usize = 64;

enum EventReplayError {
    Store(anyhow::Error),
    Disconnected,
}

/// Send a fixed snapshot of persisted history in bounded pages. The caller
/// subscribes to live events first, so appends after the snapshot ceiling are
/// waiting in the broadcast receiver and duplicates can be filtered by the
/// returned cursor.
async fn replay_persisted_events(
    engine: &Engine,
    scope: &Scope,
    tx: &tokio::sync::mpsc::Sender<SseEvent>,
    after: u64,
) -> Result<u64, EventReplayError> {
    let through = engine
        .store()
        .latest_event_cursor(scope)
        .map_err(EventReplayError::Store)?;
    let mut cursor = after;
    while cursor < through {
        let page = engine
            .store()
            .event_replay_page(scope, cursor, through, EVENT_REPLAY_PAGE_SIZE)
            .map_err(EventReplayError::Store)?;
        let next = page.next_after;
        for env in page.events {
            send_envelope(tx, &env)
                .await
                .map_err(|()| EventReplayError::Disconnected)?;
        }
        if page.exhausted || next <= cursor {
            cursor = next;
            break;
        }
        cursor = next;
    }
    Ok(cursor)
}

/// Replay persisted events after the cursor, then continue live. The live
/// subscription is opened *before* the replay query so no event can fall in
/// the gap; duplicates at the boundary are filtered by cursor.
fn event_stream(
    engine: Arc<Engine>,
    scope: Scope,
    after: u64,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(256);
    tokio::spawn(async move {
        let mut live = engine.store().subscribe();
        let mut last = match replay_persisted_events(&engine, &scope, &tx, after).await {
            Ok(last) => last,
            Err(EventReplayError::Store(e)) => {
                tracing::error!("event replay failed: {e}");
                return;
            }
            Err(EventReplayError::Disconnected) => return,
        };
        loop {
            match live.recv().await {
                Ok(env) => {
                    if env.scope != scope || env.cursor <= last {
                        continue;
                    }
                    last = env.cursor;
                    if send_envelope(&tx, &env).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Fall back to replay to fill the hole.
                    match replay_persisted_events(&engine, &scope, &tx, last).await {
                        Ok(replayed_through) => last = replayed_through,
                        Err(EventReplayError::Store(e)) => {
                            tracing::error!("event catch-up failed: {e}");
                            return;
                        }
                        Err(EventReplayError::Disconnected) => return,
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    Sse::new(futures::StreamExt::map(ReceiverStream::new(rx), Ok)).keep_alive(KeepAlive::default())
}

async fn send_envelope(
    tx: &tokio::sync::mpsc::Sender<SseEvent>,
    env: &trouve_protocol::EventEnvelope,
) -> Result<(), ()> {
    let data = serde_json::to_string(env).map_err(|_| ())?;
    let ev = SseEvent::default().id(env.cursor.to_string()).data(data);
    tx.send(ev).await.map_err(|_| ())
}

async fn server_events(
    State(engine): State<Arc<Engine>>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    let after = resume_cursor(&headers, &q);
    event_stream(engine, Scope::Server, after)
}

async fn session_events(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    let after = resume_cursor(&headers, &q);
    event_stream(engine, Scope::Session(id), after)
}

async fn thread_events(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    let after = resume_cursor(&headers, &q);
    event_stream(engine, Scope::Thread(id), after)
}
