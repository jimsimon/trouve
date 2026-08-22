//! HTTP/SSE server exposing the trouve protocol (ADR 0002).
//!
//! Commands are POST endpoints; server→client state is one append-only
//! event stream per scope, delivered as SSE with cursor resumption via
//! `Last-Event-ID` or `?after=`.

mod mcp;

use std::convert::Infallible;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::Path as FsPath;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    AddLocalModelRequest, AgentPersona, Automation, BranchList, CliInfo, CliInstallStatus, CliList,
    CodeReviewDashboard, CodeReviewJob, CodeReviewJobDetail, CodeReviewJobList,
    CodeReviewRepository, CodeReviewSettings, CodeReviewStats, CodeReviewStatsRange,
    CodeReviewTask, CompleteLoginRequest, ConfigureGithubAppRequest, CreatePrRequest,
    CreateSessionRequest, CreateThreadRequest, DirEntry,
    ERROR_CODE_GITHUB_REAUTHENTICATION_REQUIRED, ERROR_CODE_SESSION_DIFF_TOO_LARGE,
    EVENT_CURSOR_HEADER, ErrorBody, FileContent, ForkCheckpointResponse,
    GenerateSessionTitleRequest, GeneratedSessionTitle, GitWorktreeSettings, GithubAppStatus,
    GithubIntegration, GithubPrList, KnownProvider, LocalSearchResult, LocalStatus, LoginStarted,
    LoginStatus, McpLogs, McpServerInfo, MergePrRequest, ModelInfo, OpenTerminalRequest,
    PROTOCOL_VERSION, PersonaInfo, PrActionRequest, PrDetail, PrDetailSection, PrFileDiff, PrInfo,
    ProviderInfo, ProvidersResponse, QueuedPrompt, RefreshGithubPrsQuery, RegisterWorkspaceRequest,
    ReorderQueueRequest, RequestCodeReviewRequest, ResolveApprovalRequest, ResolveQuestionRequest,
    ReviewerProfile, Scope, SendMessageRequest, ServerInfo, ServerProjection, Session, SessionDiff,
    SessionDiffFileSummary, SessionDiffSummary, SessionFileDiff, SessionSummariesSnapshot,
    SetCodeReviewSettingsRequest, SetDefaultModelRequest, SetDefaultPermissionModeRequest,
    SetGitWorktreeSettingsRequest, SetGlobalDefaultsRequest, SetLocalEnabledRequest,
    SetMcpServerEnabledRequest, SteerAccepted, SteerTurnRequest, SubscriptionHealth, TerminalInfo,
    TerminalInputRequest, TerminalReplayStart, TerminalResizeRequest, Thread, ThreadStatus,
    ThreadToolDetails, ThreadViewQuery, ThreadViewSnapshot, TurnAccepted,
    UpdateCodeReviewRepositoryRequest, UpdateQueuedPromptRequest, UpdateSessionRequest,
    UpdateThreadRequest, UpsertAutomationRequest, UpsertMcpServerRequest, UpsertPersonaRequest,
    UpsertProviderRequest, UsageSummary, Workspace,
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
            EngineError::SessionDiffTooLarge(_) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ERROR_CODE_SESSION_DIFF_TOO_LARGE,
            ),
            EngineError::AuthenticationRequired(_) => (
                StatusCode::UNAUTHORIZED,
                ERROR_CODE_GITHUB_REAUTHENTICATION_REQUIRED,
            ),
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
        server_projection,
        refresh_github_prs,
        generate_session_title,
        create_session,
        list_sessions,
        session_summaries,
        get_session,
        update_session,
        delete_session,
        undo_session,
        redo_session,
        restore_checkpoint,
        fork_checkpoint,
        create_thread,
        list_threads,
        list_thread_statuses,
        get_thread,
        list_thread_subagents,
        get_thread_view,
        get_thread_tool_details,
        update_thread,
        send_message,
        steer_turn,
        get_attachment,
        list_queue,
        reorder_queue,
        dispatch_queue,
        dispatch_queued_prompt,
        update_queued_prompt,
        delete_queued_prompt,
        cancel_turn,
        resolve_approval,
        resolve_question,
        list_models,
        refresh_models,
        list_personas,
        list_persona_infos,
        upsert_persona,
        delete_persona,
        list_providers,
        known_providers,
        upsert_provider,
        delete_provider,
        start_login,
        complete_login,
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
        set_global_defaults,
        set_default_model,
        set_default_permission_mode,
        get_code_review_settings,
        set_code_review_settings,
        get_git_worktree_settings,
        set_git_worktree_settings,
        install_title_model,
        cancel_title_model_install,
        thread_usage,
        session_usage,
        session_mcp_servers,
        session_diff,
        session_diff_summary,
        session_file_diff,
        session_files,
        session_paths,
        session_file,
        open_terminal,
        list_terminals,
        create_terminal,
        kill_terminal,
        terminal_input,
        terminal_resize,
        terminal_output,
        get_session_pr,
        create_session_pr,
        merge_session_pr,
        list_session_prs,
        get_session_pr_detail,
        get_session_pr_file_diff,
        act_on_session_pr,
        get_github_integration,
        add_github_host,
        remove_github_host,
        list_mcp_servers,
        upsert_mcp_server,
        set_mcp_server_enabled,
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
        list_code_review_jobs,
        get_code_review_job,
        get_code_review_task,
        code_review_job_events,
        request_code_review,
        cancel_code_review_job,
        retry_code_review_job,
        retry_code_review_persona,
        retry_code_review_final_editor,
        code_review_stats,
        configure_github_review_app,
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
        ForkCheckpointResponse,
        SessionSummariesSnapshot,
        ServerProjection,
        trouve_protocol::GithubPrHostProjection,
        trouve_protocol::SessionPrProjection,
        trouve_protocol::SessionSummary,
        trouve_protocol::SessionAttention,
        trouve_protocol::SessionOutcome,
        UpdateSessionRequest,
        CreateThreadRequest,
        Thread,
        ThreadStatus,
        ThreadViewQuery,
        ThreadViewSnapshot,
        trouve_protocol::ThreadViewItem,
        ThreadToolDetails,
        trouve_protocol::ThreadToolStatus,
        trouve_protocol::ThreadTurnState,
        UpdateThreadRequest,
        SendMessageRequest,
        SteerTurnRequest,
        SteerAccepted,
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
        CompleteLoginRequest,
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
        SetGlobalDefaultsRequest,
        SetDefaultModelRequest,
        SetDefaultPermissionModeRequest,
        CodeReviewSettings,
        SetCodeReviewSettingsRequest,
        trouve_protocol::TitleModelLoadBehavior,
        trouve_protocol::TitleModelStatus,
        GitWorktreeSettings,
        SetGitWorktreeSettingsRequest,
        GenerateSessionTitleRequest,
        GeneratedSessionTitle,
        UsageSummary,
        SessionDiff,
        SessionDiffFileSummary,
        SessionDiffSummary,
        SessionFileDiff,
        DirEntry,
        FileContent,
        OpenTerminalRequest,
        TerminalInfo,
        TerminalInputRequest,
        TerminalReplayStart,
        TerminalResizeRequest,
        PrInfo,
        PrDetail,
        PrDetailSection,
        PrFileDiff,
        PrActionRequest,
        trouve_protocol::PrActor,
        trouve_protocol::PrAutoMerge,
        trouve_protocol::PrCapabilities,
        trouve_protocol::PrComment,
        trouve_protocol::PrCommentKind,
        trouve_protocol::PrCommit,
        trouve_protocol::PrFile,
        trouve_protocol::PrLabel,
        trouve_protocol::PrMergeQueueEntry,
        trouve_protocol::PrMergeQueueStatus,
        trouve_protocol::PrMilestone,
        trouve_protocol::PrReactionSummary,
        trouve_protocol::PrReview,
        trouve_protocol::PrReviewDetail,
        trouve_protocol::PrReviewThread,
        trouve_protocol::PrStack,
        trouve_protocol::PrStackEntry,
        trouve_protocol::CheckRun,
        trouve_protocol::FirstPartyCodeReview,
        GithubPrList,
        CreatePrRequest,
        MergePrRequest,
        GithubIntegration,
        trouve_protocol::GithubHostIntegration,
        trouve_protocol::AddGithubHostRequest,
        McpServerInfo,
        UpsertMcpServerRequest,
        SetMcpServerEnabledRequest,
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
        trouve_protocol::CodeReviewRoutingMode,
        trouve_protocol::CodeReviewRoutingSource,
        trouve_protocol::CodeReviewRoutingReason,
        trouve_protocol::CodeReviewRoutingDecision,
        CodeReviewRepository,
        trouve_protocol::CodeReviewJob,
        trouve_protocol::CodeReviewJobScope,
        trouve_protocol::CodeReviewProgress,
        trouve_protocol::CodeReviewTaskRole,
        trouve_protocol::CodeReviewOutputStream,
        trouve_protocol::CodeReviewTask,
        trouve_protocol::CodeReviewPersonaResult,
        trouve_protocol::CodeReviewFindingSource,
        trouve_protocol::CodeReviewFindingEvidence,
        trouve_protocol::CodeReviewFindingOrigin,
        trouve_protocol::CodeReviewCandidateRejection,
        trouve_protocol::CodeReviewFinding,
        trouve_protocol::CodeReviewTheme,
        trouve_protocol::CodeReviewThemeObservation,
        trouve_protocol::CodeReviewThemeObservationKind,
        trouve_protocol::CodeReviewJobDetail,
        trouve_protocol::CodeReviewJobList,
        trouve_protocol::RequestCodeReviewRequest,
        trouve_protocol::CodeReviewStatsRange,
        trouve_protocol::CodeReviewStatusCounts,
        trouve_protocol::CodeReviewDurationStats,
        trouve_protocol::CodeReviewStatsBucket,
        trouve_protocol::CodeReviewPersonaModelStats,
        trouve_protocol::CodeReviewRepositoryStats,
        trouve_protocol::CodeReviewChurnStats,
        trouve_protocol::CodeReviewStats,
        trouve_protocol::CodeReviewMode,
        GithubAppStatus,
        ConfigureGithubAppRequest,
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
        trouve_protocol::ModelOptionValue,
        trouve_protocol::AgentPersona,
        PersonaInfo,
        UpsertPersonaRequest,
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
/// runs shell commands and edits files, so the embedded server rejects
/// non-loopback hosts to block browser-based DNS rebinding.
#[derive(Clone, Default)]
pub struct ServerSecurity {
    /// Reject requests whose `Host` header isn't loopback. Blocks DNS
    /// rebinding: an attacker page rebinds its hostname to 127.0.0.1, but
    /// the browser still sends that hostname in `Host`, which won't match.
    pub require_loopback_host: bool,
    /// Ephemeral credential for vendor CLI children calling `/internal/*`.
    /// This is never persisted or user-facing.
    pub internal_token: Option<String>,
}

impl ServerSecurity {
    /// No host or internal-route checks — for in-process tests and embedders
    /// that bind their own trusted listener.
    pub fn open() -> Self {
        Self::default()
    }

    /// Protect an embedded desktop server from browser-based DNS rebinding
    /// and give vendor CLI children a fresh internal bridge credential.
    pub fn loopback() -> Self {
        Self {
            require_loopback_host: true,
            internal_token: Some(fresh_token()),
        }
    }

    /// Configure a standalone server. It accepts remote host names only when
    /// `TROUVE_ALLOW_REMOTE` is set and always protects internal CLI routes
    /// with an ephemeral bridge credential.
    pub fn resolve() -> Self {
        Self {
            require_loopback_host: std::env::var_os("TROUVE_ALLOW_REMOTE").is_none(),
            internal_token: Some(fresh_token()),
        }
    }
}

/// A 256-bit random internal credential (two v4 UUIDs, hex).
fn fresh_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
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

/// Constant-time-ish equality for internal bridge credentials.
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
    if internal && let Some(expected) = security.internal_token.as_deref() {
        let provided = request.uri().query().and_then(|query| {
            query
                .split('&')
                .filter_map(|part| part.split_once('='))
                .find_map(|(key, value)| (key == "bridge_token").then_some(value))
        });
        if !provided.is_some_and(|token| token_matches(expected, token)) {
            return (StatusCode::UNAUTHORIZED, "missing or invalid bridge token").into_response();
        }
    }
    next.run(request).await
}

/// Wrap the router with host and internal bridge-credential enforcement.
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
        .route("/v1/server-projection", get(server_projection))
        .route("/v1/github/prs/refresh", post(refresh_github_prs))
        .route("/v1/session-title", post(generate_session_title))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route("/v1/session-summaries", get(session_summaries))
        .route(
            "/v1/sessions/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/v1/sessions/{id}/undo", post(undo_session))
        .route("/v1/sessions/{id}/redo", post(redo_session))
        .route("/v1/checkpoints/{id}/restore", post(restore_checkpoint))
        .route("/v1/checkpoints/{id}/fork", post(fork_checkpoint))
        .route("/v1/sessions/{id}/events", get(session_events))
        .route("/v1/sessions/{id}/usage", get(session_usage))
        .route("/v1/sessions/{id}/mcp-servers", get(session_mcp_servers))
        .route("/v1/sessions/{id}/diff", get(session_diff))
        .route("/v1/sessions/{id}/diff/summary", get(session_diff_summary))
        .route("/v1/sessions/{id}/diff/file", get(session_file_diff))
        .route("/v1/sessions/{id}/files", get(session_files))
        .route("/v1/sessions/{id}/paths", get(session_paths))
        .route("/v1/sessions/{id}/file", get(session_file))
        .route("/v1/sessions/{id}/terminal", post(open_terminal))
        .route(
            "/v1/sessions/{id}/terminals",
            get(list_terminals).post(create_terminal),
        )
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
        .route("/v1/sessions/{id}/prs/{number}", get(get_session_pr_detail))
        .route(
            "/v1/sessions/{id}/prs/{number}/file",
            get(get_session_pr_file_diff),
        )
        .route(
            "/v1/sessions/{id}/prs/{number}/actions",
            post(act_on_session_pr),
        )
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
        .route(
            "/v1/mcp-servers/{name}/enabled",
            axum::routing::put(set_mcp_server_enabled),
        )
        .route("/v1/mcp-servers/{name}/logs", get(mcp_server_logs))
        .route("/v1/subscriptions", get(subscription_health))
        .route("/v1/models", get(list_models))
        .route("/v1/models/refresh", get(refresh_models))
        .route("/v1/personas", get(list_personas))
        .route("/v1/persona-infos", get(list_persona_infos))
        .route(
            "/v1/personas/{id}",
            axum::routing::put(upsert_persona).delete(delete_persona),
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
        .route("/v1/providers/{id}/login/callback", post(complete_login))
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
        .route("/v1/code-review/jobs", get(list_code_review_jobs))
        .route("/v1/code-review/jobs/{id}", get(get_code_review_job))
        .route(
            "/v1/code-review/jobs/{job_id}/tasks/{task_id}",
            get(get_code_review_task),
        )
        .route(
            "/v1/code-review/jobs/{id}/events",
            get(code_review_job_events),
        )
        .route(
            "/v1/code-review/jobs/{id}/cancel",
            post(cancel_code_review_job),
        )
        .route(
            "/v1/code-review/jobs/{id}/retry",
            post(retry_code_review_job),
        )
        .route(
            "/v1/code-review/jobs/{id}/reviewers/{reviewer_id}/retry",
            post(retry_code_review_persona),
        )
        .route(
            "/v1/code-review/jobs/{id}/final-editor/retry",
            post(retry_code_review_final_editor),
        )
        .route("/v1/code-review/requests", post(request_code_review))
        .route("/v1/code-review/stats", get(code_review_stats))
        .route(
            "/v1/code-review/github-app",
            axum::routing::put(configure_github_review_app),
        )
        .route(
            "/v1/code-review/repository",
            axum::routing::put(update_code_review_repository),
        )
        .route("/v1/code-review/refresh", post(refresh_code_reviews))
        // This public route is authenticated in its handler with the
        // configured HMAC secret.
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
            "/v1/config/defaults",
            axum::routing::put(set_global_defaults),
        )
        .route(
            "/v1/config/default-model",
            axum::routing::put(set_default_model),
        )
        .route(
            "/v1/config/code-review",
            get(get_code_review_settings).put(set_code_review_settings),
        )
        .route(
            "/v1/config/default-permission-mode",
            axum::routing::put(set_default_permission_mode),
        )
        .route(
            "/v1/config/git-worktrees",
            get(get_git_worktree_settings).put(set_git_worktree_settings),
        )
        .route(
            "/v1/config/git-worktrees/title-model/install",
            post(install_title_model).delete(cancel_title_model_install),
        )
        .route("/v1/threads", post(create_thread).get(list_threads))
        .route("/v1/thread-statuses", get(list_thread_statuses))
        .route("/v1/threads/{id}", get(get_thread).patch(update_thread))
        .route("/v1/threads/{id}/subagents", get(list_thread_subagents))
        .route("/v1/threads/{id}/view", get(get_thread_view))
        .route(
            "/v1/threads/{id}/tools/{call_id}",
            get(get_thread_tool_details),
        )
        .route("/v1/threads/{id}/messages", post(send_message))
        .route("/v1/threads/{id}/steer", post(steer_turn))
        .route("/v1/attachments/{id}", get(get_attachment))
        .route("/v1/threads/{id}/queue", get(list_queue).put(reorder_queue))
        .route("/v1/threads/{id}/queue/dispatch", post(dispatch_queue))
        .route("/v1/queue/{id}/dispatch", post(dispatch_queued_prompt))
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
/// `addr` (port 0 for ephemeral). The first process receives the bound address
/// and serve future; later processes receive the elected owner's address and
/// no future, so they attach without opening the database.
///
/// This is the single entry point for embedders (the desktop app, ADR
/// 0008) and the standalone binary alike: an embedder spawns the future
/// and speaks HTTP + SSE to the returned address, keeping the protocol
/// boundary intact without ever touching engine internals.
pub type LocalServerFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

/// Result of claiming the process-wide local data owner. A second product
/// window receives the already-running address and must attach over HTTP/SSE;
/// it never opens SQLite or constructs another Engine.
pub struct LocalServerBinding {
    address: SocketAddr,
    server: Option<LocalServerFuture>,
}

impl LocalServerBinding {
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn into_server(self) -> Option<LocalServerFuture> {
        self.server
    }
}

enum LocalServerOwnership {
    Owner(File),
    Existing(SocketAddr),
}

const LOCAL_SERVER_OWNER_FILE: &str = "local-server.lock";
const LOCAL_SERVER_OWNER_WAIT: Duration = Duration::from_secs(5);
const LOCAL_SERVER_OWNER_RETRY: Duration = Duration::from_millis(50);

fn write_local_server_address(file: &mut File, address: SocketAddr) -> anyhow::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{address}")?;
    file.sync_data()?;
    Ok(())
}

fn read_local_server_address(file: &mut File) -> anyhow::Result<Option<SocketAddr>> {
    file.seek(SeekFrom::Start(0))?;
    let mut registered = String::new();
    file.read_to_string(&mut registered)?;
    let registered = registered.trim();
    if registered.is_empty() {
        return Ok(None);
    }
    let address = registered
        .parse::<SocketAddr>()
        .map_err(|error| anyhow::anyhow!("invalid local server owner address: {error}"))?;
    if !address.ip().is_loopback() {
        anyhow::bail!("local server owner address is not loopback: {address}");
    }
    Ok(Some(address))
}

async fn claim_local_server(
    data: &FsPath,
    address: SocketAddr,
) -> anyhow::Result<LocalServerOwnership> {
    std::fs::create_dir_all(data)?;
    let mut owner = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join(LOCAL_SERVER_OWNER_FILE))?;
    match owner.try_lock() {
        Ok(()) => {
            write_local_server_address(&mut owner, address)?;
            return Ok(LocalServerOwnership::Owner(owner));
        }
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }

    // Give a process that just acquired the lock time to replace a stale
    // registration before reading it. If that process exits during startup,
    // this claimant can take ownership instead of attaching to a dead port.
    let deadline = Instant::now() + LOCAL_SERVER_OWNER_WAIT;
    loop {
        tokio::time::sleep(LOCAL_SERVER_OWNER_RETRY).await;
        match owner.try_lock() {
            Ok(()) => {
                write_local_server_address(&mut owner, address)?;
                return Ok(LocalServerOwnership::Owner(owner));
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        if let Some(existing) = read_local_server_address(&mut owner)? {
            return Ok(LocalServerOwnership::Existing(existing));
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for the local server owner address");
        }
    }
}

pub async fn bind_local(
    addr: SocketAddr,
    security: ServerSecurity,
) -> anyhow::Result<LocalServerBinding> {
    install_crypto_provider();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let data = trouve_core::config::data_dir();
    let owner = match claim_local_server(&data, local).await? {
        LocalServerOwnership::Owner(owner) => owner,
        LocalServerOwnership::Existing(address) => {
            drop(listener);
            tracing::info!(%address, "attaching to the existing local trouve server");
            return Ok(LocalServerBinding {
                address,
                server: None,
            });
        }
    };
    let store = trouve_core::store::Store::open(&data.join("trouve.db"))?;
    let config = trouve_core::config::Config::load();
    let engine = Arc::new(
        Engine::new(store, data, &config)
            .with_config_file(Some(trouve_core::config::config_path()))
            .with_index_hooks()
            .with_connectivity_probe(trouve_core::connectivity::system_probe()),
    );
    let server = Box::pin(async move {
        // The registration lock must outlive Engine and the listener. Dropping
        // this future releases ownership even when startup or serving fails.
        let _owner = owner;
        serve_listener(engine, listener, security).await
    });
    Ok(LocalServerBinding {
        address: local,
        server: Some(server),
    })
}

#[cfg(test)]
mod local_server_ownership_tests {
    use super::*;

    #[tokio::test]
    async fn one_local_server_owns_a_data_directory_at_a_time() {
        let data = tempfile::tempdir().unwrap();
        let first_address: SocketAddr = "127.0.0.1:41001".parse().unwrap();
        let second_address: SocketAddr = "127.0.0.1:41002".parse().unwrap();
        let owner = match claim_local_server(data.path(), first_address)
            .await
            .unwrap()
        {
            LocalServerOwnership::Owner(owner) => owner,
            LocalServerOwnership::Existing(_) => panic!("first claimant must own the server"),
        };
        assert!(matches!(
            claim_local_server(data.path(), second_address).await.unwrap(),
            LocalServerOwnership::Existing(address) if address == first_address
        ));

        drop(owner);
        assert!(matches!(
            claim_local_server(data.path(), second_address)
                .await
                .unwrap(),
            LocalServerOwnership::Owner(_)
        ));
    }
}

/// Serve on an already-bound listener (embedded mode: bind port 0, read the
/// local address, then serve).
pub async fn serve_listener(
    engine: Arc<Engine>,
    listener: tokio::net::TcpListener,
    security: ServerSecurity,
) -> anyhow::Result<()> {
    engine.reconcile_checkpoint_refs().await;
    engine.retry_artifact_cleanup_jobs().await;
    engine.retry_persona_deletions().await;
    engine.start_artifact_cleanup_worker();
    // Backends dialing back in (MCP tool bridge) need our reachable URL;
    // build_secured_router injects their separate ephemeral bridge token.
    engine.set_base_url(&format!("http://{}", listener.local_addr()?));
    // Resolve connectivity before accepting requests so an offline start
    // never serves a model list it immediately retracts (no-op without a
    // configured probe).
    engine.init_connectivity().await;
    engine.start_session_pr_verification_worker();
    engine.warm_title_model();
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
    responses((status = 200, body = CodeReviewDashboard,
        headers(("x-trouve-event-cursor" = u64, description = "Server event cursor for this snapshot"))),
        (status = 500, body = ErrorBody)))]
async fn code_review_dashboard(
    State(engine): State<Arc<Engine>>,
) -> Result<impl IntoResponse, ApiError> {
    let (cursor, dashboard) = engine.code_review_dashboard_snapshot()?;
    Ok(([(EVENT_CURSOR_HEADER, cursor.to_string())], Json(dashboard)))
}

#[derive(Debug, Deserialize)]
struct CodeReviewJobsQuery {
    #[serde(default = "default_review_job_limit")]
    limit: usize,
    status: Option<String>,
    repository: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodeReviewJobQuery {
    #[serde(default = "default_true")]
    include_task_content: bool,
}

fn default_true() -> bool {
    true
}

fn default_review_job_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
struct CodeReviewStatsQuery {
    #[serde(default)]
    range: CodeReviewStatsRange,
    repository: Option<String>,
}

#[utoipa::path(get, path = "/v1/code-review/jobs",
    params(
        ("limit" = Option<usize>, Query, description = "Maximum jobs, 1 through 500"),
        ("status" = Option<String>, Query, description = "Exact job status"),
        ("repository" = Option<String>, Query, description = "Exact owner/name repository")
    ),
    responses((status = 200, body = CodeReviewJobList), (status = 400, body = ErrorBody)))]
async fn list_code_review_jobs(
    State(engine): State<Arc<Engine>>,
    Query(query): Query<CodeReviewJobsQuery>,
) -> Result<Json<CodeReviewJobList>, ApiError> {
    Ok(Json(engine.code_review_jobs(
        query.limit,
        query.status.as_deref(),
        query.repository.as_deref(),
    )?))
}

#[utoipa::path(get, path = "/v1/code-review/jobs/{id}",
    params(
        ("id" = String, Path, description = "Review job id"),
        (
            "include_task_content" = Option<bool>,
            Query,
            description = "Include retained task prompts and output; defaults to true"
        )
    ),
    responses((status = 200, body = CodeReviewJobDetail), (status = 404, body = ErrorBody)))]
async fn get_code_review_job(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(query): Query<CodeReviewJobQuery>,
) -> Result<Json<CodeReviewJobDetail>, ApiError> {
    let detail = if query.include_task_content {
        engine.code_review_job_detail(&id)?
    } else {
        engine.code_review_job_overview(&id)?
    };
    Ok(Json(detail))
}

#[utoipa::path(get, path = "/v1/code-review/jobs/{job_id}/tasks/{task_id}",
    params(
        ("job_id" = String, Path, description = "Review job id"),
        ("task_id" = String, Path, description = "Review task id")
    ),
    responses((status = 200, body = CodeReviewTask), (status = 404, body = ErrorBody)))]
async fn get_code_review_task(
    State(engine): State<Arc<Engine>>,
    Path((job_id, task_id)): Path<(String, String)>,
) -> Result<Json<CodeReviewTask>, ApiError> {
    Ok(Json(engine.code_review_task(&job_id, &task_id)?))
}

#[utoipa::path(post, path = "/v1/code-review/requests",
    request_body = RequestCodeReviewRequest,
    responses((status = 200, body = CodeReviewJob), (status = 400, body = ErrorBody)))]
async fn request_code_review(
    State(engine): State<Arc<Engine>>,
    Json(request): Json<RequestCodeReviewRequest>,
) -> Result<Json<CodeReviewJob>, ApiError> {
    Ok(Json(engine.request_code_review(request).await?))
}

#[utoipa::path(post, path = "/v1/code-review/jobs/{id}/cancel",
    params(("id" = String, Path, description = "Review job id")),
    responses((status = 200, body = CodeReviewJob), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn cancel_code_review_job(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<CodeReviewJob>, ApiError> {
    Ok(Json(engine.cancel_code_review_job(&id).await?))
}

#[utoipa::path(post, path = "/v1/code-review/jobs/{id}/retry",
    params(("id" = String, Path, description = "Review job id")),
    responses((status = 200, body = CodeReviewJob), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn retry_code_review_job(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<CodeReviewJob>, ApiError> {
    Ok(Json(engine.retry_review_job(&id).await?))
}

#[utoipa::path(post, path = "/v1/code-review/jobs/{id}/reviewers/{reviewer_id}/retry",
    params(
        ("id" = String, Path, description = "Review job id"),
        ("reviewer_id" = String, Path, description = "Reviewer persona id")
    ),
    responses((status = 200, body = CodeReviewJob), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn retry_code_review_persona(
    State(engine): State<Arc<Engine>>,
    Path((id, reviewer_id)): Path<(String, String)>,
) -> Result<Json<CodeReviewJob>, ApiError> {
    Ok(Json(engine.retry_review_persona(&id, &reviewer_id).await?))
}

#[utoipa::path(post, path = "/v1/code-review/jobs/{id}/final-editor/retry",
    params(("id" = String, Path, description = "Review job id")),
    responses((status = 200, body = CodeReviewJob), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn retry_code_review_final_editor(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<CodeReviewJob>, ApiError> {
    Ok(Json(engine.retry_review_final_editor(&id).await?))
}

#[utoipa::path(get, path = "/v1/code-review/stats",
    params(
        ("range" = Option<CodeReviewStatsRange>, Query, description = "hour, day, week, month, year, or all"),
        ("repository" = Option<String>, Query, description = "Exact owner/name repository")
    ),
    responses((status = 200, body = CodeReviewStats), (status = 400, body = ErrorBody)))]
async fn code_review_stats(
    State(engine): State<Arc<Engine>>,
    Query(query): Query<CodeReviewStatsQuery>,
) -> Result<Json<CodeReviewStats>, ApiError> {
    Ok(Json(engine.code_review_stats(
        query.range,
        query.repository.as_deref(),
    )?))
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

#[utoipa::path(put, path = "/v1/code-review/repository",
    request_body = UpdateCodeReviewRepositoryRequest,
    responses((status = 200, body = CodeReviewRepository), (status = 400, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
async fn update_code_review_repository(
    State(engine): State<Arc<Engine>>,
    Json(request): Json<UpdateCodeReviewRepositoryRequest>,
) -> Result<Json<CodeReviewRepository>, ApiError> {
    Ok(Json(engine.update_code_review_repository(&request).await?))
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

#[utoipa::path(get, path = "/v1/server-projection",
    responses(
        (status = 200, body = ServerProjection,
            headers(("x-trouve-event-cursor" = u64, description = "Server event cursor for this snapshot"))),
        (status = 500, body = ErrorBody)
    ))]
async fn server_projection(
    State(engine): State<Arc<Engine>>,
) -> Result<impl IntoResponse, ApiError> {
    let (cursor, projection) = engine.server_projection_snapshot()?;
    Ok((
        [(EVENT_CURSOR_HEADER, cursor.to_string())],
        Json(projection),
    ))
}

#[utoipa::path(post, path = "/v1/github/prs/refresh",
    params(("force" = Option<bool>, Query, description = "Bypass the automatic-refresh freshness window for an explicit user refresh")),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn refresh_github_prs(
    State(engine): State<Arc<Engine>>,
    Query(query): Query<RefreshGithubPrsQuery>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.refresh_github_prs(query.force).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/session-title", request_body = GenerateSessionTitleRequest,
    responses((status = 200, body = GeneratedSessionTitle)))]
async fn generate_session_title(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<GenerateSessionTitleRequest>,
) -> Json<GeneratedSessionTitle> {
    Json(engine.generate_session_title(&req.prompt).await)
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

#[utoipa::path(get, path = "/v1/session-summaries",
    responses(
        (status = 200, body = SessionSummariesSnapshot),
        (status = 500, body = ErrorBody)
    ))]
async fn session_summaries(
    State(engine): State<Arc<Engine>>,
) -> Result<Json<SessionSummariesSnapshot>, ApiError> {
    Ok(Json(engine.session_summaries_snapshot()?))
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
    responses(
        (status = 200, body = Session),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)
    ))]
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

#[utoipa::path(post, path = "/v1/checkpoints/{id}/restore", params(("id" = String, Path,)),
    responses((status = 204), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
async fn restore_checkpoint(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.restore_checkpoint_by_id(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/checkpoints/{id}/fork", params(("id" = String, Path,)),
    responses((status = 200, body = ForkCheckpointResponse), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody)))]
async fn fork_checkpoint(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<ForkCheckpointResponse>, ApiError> {
    Ok(Json(engine.fork_checkpoint(&id).await?))
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

#[derive(Default, Deserialize)]
struct ListThreadSubagentsQuery {
    #[serde(default)]
    recursive: bool,
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

#[utoipa::path(get, path = "/v1/thread-statuses",
    params(("session_id" = String, Query,)),
    responses((status = 200, body = [ThreadStatus])))]
async fn list_thread_statuses(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListThreadsQuery>,
) -> Result<Json<Vec<ThreadStatus>>, ApiError> {
    Ok(Json(engine.list_thread_statuses(&q.session_id)?))
}

#[utoipa::path(get, path = "/v1/threads/{id}", params(("id" = String, Path,)),
    responses((status = 200, body = Thread), (status = 404, body = ErrorBody)))]
async fn get_thread(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Thread>, ApiError> {
    Ok(Json(engine.get_thread(&id)?))
}

#[utoipa::path(get, path = "/v1/threads/{id}/subagents",
    params(
        ("id" = String, Path,),
        ("recursive" = Option<bool>, Query, description = "Include nested descendants instead of only direct children")
    ),
    responses((status = 200, body = [Thread]), (status = 404, body = ErrorBody)))]
async fn list_thread_subagents(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(query): Query<ListThreadSubagentsQuery>,
) -> Result<Json<Vec<Thread>>, ApiError> {
    let subagents = if query.recursive {
        engine.list_thread_descendants(&id)?
    } else {
        engine.list_thread_subagents(&id)?
    };
    Ok(Json(subagents))
}

#[utoipa::path(get, path = "/v1/threads/{id}/view",
    params(
        ("id" = String, Path,),
        ("before" = Option<u64>, Query, description = "Exclusive folded-item offset for backward pagination"),
        ("limit" = Option<u32>, Query, description = "Maximum item count, capped by the server"),
        ("turn_aligned" = Option<bool>, Query, description = "Expand backward to a complete turn boundary; the response may exceed limit")
    ),
    responses(
        (status = 200, body = ThreadViewSnapshot,
            headers(("x-trouve-event-cursor" = u64, description = "Thread event cursor for this snapshot"))),
        (status = 404, body = ErrorBody)
    ))]
async fn get_thread_view(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(query): Query<ThreadViewQuery>,
) -> Result<impl IntoResponse, ApiError> {
    const MAX_ITEMS: usize = 512;
    // Omitting `limit` preserves the complete-snapshot behavior for 2.3
    // clients. Pagination-aware clients always send an explicit bound.
    let limit = query
        .limit
        .map(|limit| (limit as usize).clamp(1, MAX_ITEMS))
        .unwrap_or(usize::MAX);
    let turn_aligned = query.turn_aligned.unwrap_or(false);
    let (cursor, snapshot) = tokio::task::spawn_blocking(move || {
        engine.thread_view_snapshot(&id, query.before, limit, turn_aligned)
    })
    .await
    .map_err(|error| {
        ApiError(EngineError::Internal(anyhow::anyhow!(
            "thread view worker failed: {error}"
        )))
    })??;
    Ok(([(EVENT_CURSOR_HEADER, cursor.to_string())], Json(snapshot)))
}

#[utoipa::path(get, path = "/v1/threads/{id}/tools/{call_id}",
    params(
        ("id" = String, Path,),
        ("call_id" = String, Path,)
    ),
    responses(
        (status = 200, body = ThreadToolDetails),
        (status = 404, body = ErrorBody)
    ))]
async fn get_thread_tool_details(
    State(engine): State<Arc<Engine>>,
    Path((id, call_id)): Path<(String, String)>,
) -> Result<Json<ThreadToolDetails>, ApiError> {
    Ok(Json(engine.thread_tool_details(&id, &call_id)?))
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

#[utoipa::path(post, path = "/v1/threads/{id}/steer",
    params(("id" = String, Path,)), request_body = SteerTurnRequest,
    responses((status = 202, body = SteerAccepted),
              (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn steer_turn(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<SteerTurnRequest>,
) -> Result<(StatusCode, Json<SteerAccepted>), ApiError> {
    let accepted = engine.steer_turn(&id, req.content, req.attachments).await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

/// Raw bytes of a stored prompt attachment, with its uploaded MIME type.
#[utoipa::path(get, path = "/v1/attachments/{id}", params(("id" = String, Path,)),
    responses((status = 200, body = String), (status = 404, body = ErrorBody)))]
async fn get_attachment(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let (attachment, bytes) = engine.attachment(&id).await?;
    let preview_mime = safe_attachment_preview_mime(&attachment.mime);
    let disposition = attachment_content_disposition(&attachment.name, preview_mime.is_some());
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(preview_mime.unwrap_or("application/octet-stream")),
    );
    headers.insert(axum::http::header::CONTENT_DISPOSITION, disposition);
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

/// Only browser-safe raster formats may render in the application origin.
/// In particular, SVG and user-supplied text/HTML are downloads even when a
/// legacy database row claims a renderable MIME type.
fn safe_attachment_preview_mime(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn attachment_content_disposition(name: &str, inline: bool) -> axum::http::HeaderValue {
    const MAX_FILENAME_BYTES: usize = 1024;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut fallback = String::new();
    let mut encoded = String::new();
    let mut source_bytes = 0;
    for ch in name.chars() {
        let char_bytes = ch.len_utf8();
        if source_bytes + char_bytes > MAX_FILENAME_BYTES {
            break;
        }
        source_bytes += char_bytes;

        if fallback.len() < 150 {
            fallback.push(
                if ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '-' | '_') {
                    ch
                } else {
                    '_'
                },
            );
        }
        let mut utf8 = [0; 4];
        for &byte in ch.encode_utf8(&mut utf8).as_bytes() {
            if byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'&'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
            {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
    if fallback.trim().is_empty() {
        fallback = "attachment".to_string();
    }
    if encoded.is_empty() {
        encoded = "attachment".to_string();
    }
    let kind = if inline { "inline" } else { "attachment" };
    axum::http::HeaderValue::try_from(format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment"))
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
            queued_prompt: None,
        }),
    ))
}

/// Move one queued prompt to the front and dispatch it immediately. An
/// active turn is interrupted; its terminal event is persisted before the
/// selected prompt starts as the next turn.
#[utoipa::path(post, path = "/v1/queue/{id}/dispatch", params(("id" = String, Path,)),
    responses((status = 202, body = TurnAccepted), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn dispatch_queued_prompt(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<TurnAccepted>), ApiError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(engine.dispatch_queued_prompt(&id)?),
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
    engine.update_queued_prompt(&id, req)?;
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
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn resolve_approval(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<ResolveApprovalRequest>,
) -> Result<StatusCode, ApiError> {
    engine.resolve_approval(&req.thread_id, &req.call_id, req.decision)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/questions", request_body = ResolveQuestionRequest,
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
async fn resolve_question(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<ResolveQuestionRequest>,
) -> Result<StatusCode, ApiError> {
    engine.resolve_question(&req.thread_id, &req.request_id, req.answers)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/models", responses((status = 200, body = [ModelInfo])))]
async fn list_models(State(engine): State<Arc<Engine>>) -> Json<Vec<ModelInfo>> {
    Json(engine.list_models().await)
}

#[utoipa::path(get, path = "/v1/models/refresh", responses((status = 200, body = [ModelInfo])))]
async fn refresh_models(State(engine): State<Arc<Engine>>) -> Json<Vec<ModelInfo>> {
    Json(engine.refresh_models().await)
}

#[utoipa::path(get, path = "/v1/providers", responses((status = 200, body = ProvidersResponse)))]
async fn list_providers(State(engine): State<Arc<Engine>>) -> Json<ProvidersResponse> {
    Json(engine.list_providers())
}

#[utoipa::path(get, path = "/v1/providers/known", responses((status = 200, body = [KnownProvider])))]
async fn known_providers(State(engine): State<Arc<Engine>>) -> Json<Vec<KnownProvider>> {
    Json(engine.known_providers().await)
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

#[utoipa::path(post, path = "/v1/providers/{id}/login/callback",
    params(("id" = String, Path,)), request_body = CompleteLoginRequest,
    responses((status = 200, body = LoginStatus), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn complete_login(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(request): Json<CompleteLoginRequest>,
) -> Result<Json<LoginStatus>, ApiError> {
    Ok(Json(engine.complete_login(&id, request).await?))
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
    Ok(Json(engine.create_automation(req).await?))
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
    Ok(Json(engine.update_automation(&id, req).await?))
}

#[utoipa::path(delete, path = "/v1/automations/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn delete_automation(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.delete_automation(&id).await?;
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

#[utoipa::path(put, path = "/v1/config/defaults",
    request_body = SetGlobalDefaultsRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn set_global_defaults(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<SetGlobalDefaultsRequest>,
) -> Result<StatusCode, ApiError> {
    engine.set_global_defaults(
        &req.model,
        req.default_thinking_level.as_deref(),
        req.permission_mode,
    )?;
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

#[utoipa::path(get, path = "/v1/config/code-review",
    responses((status = 200, body = CodeReviewSettings,
        headers(("x-trouve-event-cursor" = u64, description = "Server event cursor for this snapshot")))))]
async fn get_code_review_settings(
    State(engine): State<Arc<Engine>>,
) -> Result<impl IntoResponse, ApiError> {
    let (cursor, settings) = engine.code_review_settings_snapshot()?;
    Ok(([(EVENT_CURSOR_HEADER, cursor.to_string())], Json(settings)))
}

#[utoipa::path(put, path = "/v1/config/code-review",
    request_body = SetCodeReviewSettingsRequest,
    responses(
        (status = 200, body = CodeReviewSettings,
            headers(("x-trouve-event-cursor" = u64, description = "Server event cursor for this snapshot"))),
        (status = 400, body = ErrorBody)
    ))]
async fn set_code_review_settings(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<SetCodeReviewSettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let (cursor, settings) = engine.set_code_review_settings(req)?;
    Ok(([(EVENT_CURSOR_HEADER, cursor.to_string())], Json(settings)))
}

#[utoipa::path(get, path = "/v1/config/git-worktrees",
    responses((status = 200, body = GitWorktreeSettings,
        headers(("x-trouve-event-cursor" = u64, description = "Server event cursor for this snapshot")))))]
async fn get_git_worktree_settings(
    State(engine): State<Arc<Engine>>,
) -> Result<impl IntoResponse, ApiError> {
    let (cursor, settings) = engine.git_worktree_settings_snapshot()?;
    Ok(([(EVENT_CURSOR_HEADER, cursor.to_string())], Json(settings)))
}

#[utoipa::path(put, path = "/v1/config/git-worktrees",
    request_body = SetGitWorktreeSettingsRequest,
    responses((status = 200, body = GitWorktreeSettings,
        headers(("x-trouve-event-cursor" = u64, description = "Server event cursor for this snapshot")))))]
async fn set_git_worktree_settings(
    State(engine): State<Arc<Engine>>,
    Json(req): Json<SetGitWorktreeSettingsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    engine
        .set_git_worktree_settings(
            req.title_model_load_behavior,
            req.title_model_resource_policy,
            req.derive_branch_name_from_session_title,
        )
        .await?;
    let (cursor, settings) = engine.git_worktree_settings_snapshot()?;
    Ok(([(EVENT_CURSOR_HEADER, cursor.to_string())], Json(settings)))
}

#[utoipa::path(post, path = "/v1/config/git-worktrees/title-model/install",
    responses((status = 202), (status = 409, body = ErrorBody)))]
async fn install_title_model(State(engine): State<Arc<Engine>>) -> Result<StatusCode, ApiError> {
    engine.install_title_model()?;
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(delete, path = "/v1/config/git-worktrees/title-model/install",
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
async fn cancel_title_model_install(
    State(engine): State<Arc<Engine>>,
) -> Result<StatusCode, ApiError> {
    engine.cancel_title_model_install()?;
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
struct ListPersonasQuery {
    workspace_id: Option<String>,
}

#[utoipa::path(get, path = "/v1/personas",
    params(("workspace_id" = Option<String>, Query, description = "Include personas from the workspace's .agents/personas configuration")),
    responses((status = 200, body = [AgentPersona])))]
async fn list_personas(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListPersonasQuery>,
) -> Result<Json<Vec<AgentPersona>>, ApiError> {
    Ok(Json(engine.list_personas(q.workspace_id.as_deref())?))
}

#[utoipa::path(get, path = "/v1/persona-infos",
    params(("workspace_id" = Option<String>, Query, description = "Include personas from the workspace's .agents/personas configuration")),
    responses((status = 200, body = [PersonaInfo])))]
async fn list_persona_infos(
    State(engine): State<Arc<Engine>>,
    Query(q): Query<ListPersonasQuery>,
) -> Result<Json<Vec<PersonaInfo>>, ApiError> {
    Ok(Json(engine.list_persona_infos(q.workspace_id.as_deref())?))
}

#[utoipa::path(put, path = "/v1/personas/{id}", params(("id" = String, Path,)),
    request_body = UpsertPersonaRequest,
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn upsert_persona(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<UpsertPersonaRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.upsert_persona(&id, req).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/personas/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 400, body = ErrorBody)))]
async fn delete_persona(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.delete_persona(&id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/sessions/{id}/diff", params(("id" = String, Path,)),
    responses((status = 200, body = SessionDiff), (status = 404, body = ErrorBody),
              (status = 413, body = ErrorBody)))]
async fn session_diff(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<SessionDiff>, ApiError> {
    Ok(Json(SessionDiff {
        diff: engine.session_diff(&id).await?,
    }))
}

#[utoipa::path(get, path = "/v1/sessions/{id}/diff/summary",
    params(("id" = String, Path,)),
    responses(
        (status = 200, body = SessionDiffSummary),
        (status = 404, body = ErrorBody),
        (status = 413, body = ErrorBody),
    ))]
async fn session_diff_summary(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<SessionDiffSummary>, ApiError> {
    Ok(Json(engine.session_diff_summary(&id).await?))
}

#[utoipa::path(get, path = "/v1/sessions/{id}/diff/file",
    params(
        ("id" = String, Path,),
        ("path" = String, Query, description = "Changed worktree-relative path"),
    ),
    responses(
        (status = 200, body = SessionFileDiff),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 413, body = ErrorBody),
    ))]
async fn session_file_diff(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<SessionFileDiff>, ApiError> {
    Ok(Json(engine.session_file_diff(&id, &q.path).await?))
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

#[utoipa::path(get, path = "/v1/sessions/{id}/terminals", params(("id" = String, Path,)),
    responses((status = 200, body = [TerminalInfo]), (status = 404, body = ErrorBody)))]
async fn list_terminals(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TerminalInfo>>, ApiError> {
    Ok(Json(engine.list_terminals(&id)?))
}

#[utoipa::path(post, path = "/v1/sessions/{id}/terminals", params(("id" = String, Path,)),
    request_body = OpenTerminalRequest,
    responses((status = 200, body = TerminalInfo), (status = 404, body = ErrorBody)))]
async fn create_terminal(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<OpenTerminalRequest>,
) -> Result<Json<TerminalInfo>, ApiError> {
    Ok(Json(engine.create_terminal(&id, req.cols, req.rows)?))
}

#[utoipa::path(delete, path = "/v1/terminals/{id}", params(("id" = String, Path,)),
    responses((status = 204), (status = 404, body = ErrorBody)))]
async fn kill_terminal(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    engine.terminal_kill(&id).await?;
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
    engine.terminal_input(&id, &bytes).await?;
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
    engine.terminal_resize(&id, req.cols, req.rows).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// PTY output as SSE. A named, id-less `replay-start` event first announces
/// the absolute replay offset as JSON. Each following output event's `id` is
/// the byte offset *after* its base64 data. `?after=` resumes from an offset
/// (bytes older than the retained backlog are skipped). A final `exit` event
/// marks shell exit. Ephemeral — not part of the persisted event log.
#[utoipa::path(get, path = "/v1/terminals/{id}/output",
    params(("id" = String, Path,), ("after" = Option<u64>, Query, description = "Resume byte offset")),
    responses((status = 200, description = "SSE replay-start marker and base64 output chunks"),
              (status = 404, body = ErrorBody)))]
async fn terminal_output(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (start, replay, mut live, exited) =
        engine.terminal_subscribe(&id, q.after.unwrap_or(0)).await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<SseEvent>(64);
    tokio::spawn(async move {
        let mut offset = start;
        let replay_start = TerminalReplayStart { offset: start };
        let marker = SseEvent::default()
            .event("replay-start")
            .data(serde_json::to_string(&replay_start).expect("terminal replay marker serializes"));
        if tx.send(marker).await.is_err() {
            return;
        }
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
struct PrDetailQuery {
    section: Option<PrDetailSection>,
}

#[utoipa::path(get, path = "/v1/sessions/{id}/prs/{number}",
    params(
        ("id" = String, Path,),
        ("number" = u64, Path,),
        ("section" = Option<PrDetailSection>, Query, description = "Optional lazy PR-page section; omitted loads every section for older clients")
    ),
    responses(
        (status = 200, body = PrDetail),
        (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody, description = "GitHub OAuth permissions must be renewed"),
        (status = 404, body = ErrorBody)
    ))]
async fn get_session_pr_detail(
    State(engine): State<Arc<Engine>>,
    Path((id, number)): Path<(String, u64)>,
    Query(query): Query<PrDetailQuery>,
) -> Result<Json<PrDetail>, ApiError> {
    Ok(Json(
        engine.session_pr_detail(&id, number, query.section).await?,
    ))
}

#[derive(Deserialize)]
struct PrFileQuery {
    path: String,
}

#[utoipa::path(get, path = "/v1/sessions/{id}/prs/{number}/file",
    params(
        ("id" = String, Path,),
        ("number" = u64, Path,),
        ("path" = String, Query, description = "Exact changed-file path returned by PrDetail")
    ),
    responses(
        (status = 200, body = PrFileDiff),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    ))]
async fn get_session_pr_file_diff(
    State(engine): State<Arc<Engine>>,
    Path((id, number)): Path<(String, u64)>,
    Query(query): Query<PrFileQuery>,
) -> Result<Json<PrFileDiff>, ApiError> {
    Ok(Json(
        engine
            .session_pr_file_diff(&id, number, &query.path)
            .await?,
    ))
}

#[utoipa::path(post, path = "/v1/sessions/{id}/prs/{number}/actions",
    params(("id" = String, Path,), ("number" = u64, Path,)),
    request_body = PrActionRequest,
    responses(
        (status = 200, body = PrDetail),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    ))]
async fn act_on_session_pr(
    State(engine): State<Arc<Engine>>,
    Path((id, number)): Path<(String, u64)>,
    Json(action): Json<PrActionRequest>,
) -> Result<Json<PrDetail>, ApiError> {
    Ok(Json(engine.act_on_session_pr(&id, number, &action).await?))
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
    engine.upsert_mcp_server(&name, &req).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/v1/mcp-servers/{name}/enabled", params(("name" = String, Path,)),
    request_body = SetMcpServerEnabledRequest,
    responses(
        (status = 204),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    ))]
async fn set_mcp_server_enabled(
    State(engine): State<Arc<Engine>>,
    Path(name): Path<String>,
    Json(req): Json<SetMcpServerEnabledRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    engine.set_mcp_server_enabled(&name, &req).await?;
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
    engine
        .delete_mcp_server(&name, &q.scope, q.workspace_id.as_deref())
        .await?;
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
    engine.remove_github_host(&host)?;
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
        let mut live = engine.store().subscribe_scope(&scope);
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

#[utoipa::path(get, path = "/v1/code-review/jobs/{id}/events",
    params(
        ("id" = String, Path, description = "Review job id"),
        ("after" = Option<u64>, Query, description = "Resume after this event cursor")
    ),
    responses((status = 200, description = "Cursor-addressed code-review job SSE stream")))]
async fn code_review_job_events(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    let after = resume_cursor(&headers, &q);
    event_stream(engine, Scope::CodeReviewJob(id), after)
}

#[cfg(test)]
mod attachment_response_tests {
    use super::{attachment_content_disposition, safe_attachment_preview_mime};

    #[test]
    fn only_safe_raster_mime_types_render_inline() {
        assert_eq!(safe_attachment_preview_mime("image/png"), Some("image/png"));
        assert_eq!(
            safe_attachment_preview_mime("IMAGE/JPEG"),
            Some("image/jpeg")
        );
        assert_eq!(safe_attachment_preview_mime("image/svg+xml"), None);
        assert_eq!(safe_attachment_preview_mime("text/html"), None);
        assert_eq!(
            safe_attachment_preview_mime("image/png; charset=utf-8"),
            None
        );
    }

    #[test]
    fn content_disposition_cannot_inject_headers() {
        let value = attachment_content_disposition("bad\"\r\nX-Evil: yes.svg", false);
        let value = value.to_str().expect("ASCII content-disposition");
        assert!(value.starts_with("attachment; filename=\""));
        assert!(!value.contains('\r'));
        assert!(!value.contains('\n'));
        assert!(value.contains("%22%0D%0AX-Evil%3A%20yes.svg"));
    }

    #[test]
    fn content_disposition_preserves_unicode_via_rfc5987() {
        let value = attachment_content_disposition("snowman-☃.png", true);
        let value = value.to_str().expect("ASCII content-disposition");
        assert!(value.starts_with("inline; filename=\"snowman-_.png\""));
        assert!(value.contains("filename*=UTF-8''snowman-%E2%98%83.png"));
    }
}
