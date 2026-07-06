//! HTTP/SSE server exposing the trouve protocol (ADR 0002).
//!
//! Commands are POST endpoints; server→client state is one append-only
//! event stream per scope, delivered as SSE with cursor resumption via
//! `Last-Event-ID` or `?after=`.

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
use trouve_core::engine::EngineError;
use trouve_core::Engine;
use trouve_protocol::{
    AgentMode, BranchList, CreatePrRequest, CreateSessionRequest, CreateThreadRequest, DirEntry,
    ErrorBody, FileContent, KnownProvider, LoginStarted, LoginStatus, MergePrRequest, ModelInfo,
    PrInfo, ProviderInfo, ProvidersResponse, RegisterWorkspaceRequest, ResolveApprovalRequest,
    Scope, SendMessageRequest, ServerInfo, Session, SessionDiff, SetDefaultModelRequest, Thread,
    TurnAccepted, UpdateSessionRequest, UpdateThreadRequest, UpsertProviderRequest, UsageSummary,
    Workspace, PROTOCOL_VERSION,
};
use utoipa::OpenApi;

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
        workspace_branches,
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
        resolve_approval,
        list_models,
        list_modes,
        list_providers,
        known_providers,
        upsert_provider,
        delete_provider,
        start_login,
        login_status,
        set_default_model,
        thread_usage,
        session_usage,
        session_diff,
        session_files,
        session_file,
        get_session_pr,
        create_session_pr,
        merge_session_pr,
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
        ResolveApprovalRequest,
        ModelInfo,
        ProviderInfo,
        ProvidersResponse,
        KnownProvider,
        LoginStarted,
        LoginStatus,
        UpsertProviderRequest,
        SetDefaultModelRequest,
        UsageSummary,
        SessionDiff,
        DirEntry,
        FileContent,
        PrInfo,
        CreatePrRequest,
        MergePrRequest,
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

pub fn build_router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/openapi.json", get(openapi))
        .route(
            "/v1/workspaces",
            post(register_workspace).get(list_workspaces),
        )
        .route("/v1/workspaces/{id}/branches", get(workspace_branches))
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
        .route("/v1/sessions/{id}/diff", get(session_diff))
        .route("/v1/sessions/{id}/files", get(session_files))
        .route("/v1/sessions/{id}/file", get(session_file))
        .route(
            "/v1/sessions/{id}/pr",
            get(get_session_pr).post(create_session_pr),
        )
        .route("/v1/sessions/{id}/pr/merge", post(merge_session_pr))
        .route("/v1/models", get(list_models))
        .route("/v1/modes", get(list_modes))
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
        .route(
            "/v1/config/default-model",
            axum::routing::put(set_default_model),
        )
        .route("/v1/threads", post(create_thread).get(list_threads))
        .route("/v1/threads/{id}", get(get_thread).patch(update_thread))
        .route("/v1/threads/{id}/messages", post(send_message))
        .route("/v1/threads/{id}/events", get(thread_events))
        .route("/v1/threads/{id}/usage", get(thread_usage))
        .route("/v1/approvals", post(resolve_approval))
        .route("/v1/events", get(server_events))
        // Internal (undocumented, same-host trust domain): tool bridge for
        // external agent backends running with trouve's ToolExecutor.
        .route("/internal/threads/{id}/tools", get(bridged_tools))
        .route("/internal/threads/{id}/tools/call", post(bridged_tool_call))
        .route(
            "/internal/threads/{id}/approval",
            post(bridged_approval_prompt),
        )
        .with_state(engine)
}

pub async fn serve(engine: Arc<Engine>, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_listener(engine, listener).await
}

/// Serve on an already-bound listener (embedded mode: bind port 0, read the
/// local address, then serve).
pub async fn serve_listener(
    engine: Arc<Engine>,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    // Backends dialing back in (MCP tool bridge) need our reachable URL.
    engine.set_base_url(&format!("http://{}", listener.local_addr()?));
    let router = build_router(engine);
    tracing::info!(
        "trouve-server listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, router).await?;
    Ok(())
}

// --- handlers --------------------------------------------------------------

#[utoipa::path(get, path = "/v1/info", responses((status = 200, body = ServerInfo)))]
async fn info() -> Json<ServerInfo> {
    Json(ServerInfo {
        name: "trouve-server".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION.into(),
    })
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

#[utoipa::path(get, path = "/v1/workspaces/{id}/branches", params(("id" = String, Path,)),
    responses((status = 200, body = BranchList), (status = 404, body = ErrorBody)))]
async fn workspace_branches(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<BranchList>, ApiError> {
    Ok(Json(engine.workspace_branches(&id).await?))
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
    let accepted = engine.send_message(&id, req.content)?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
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

// --- internal tool bridge (undocumented; not part of the public protocol) ---

/// Tool specs for a thread, consumed by the `trouve mcp-bridge` process that
/// external agent backends (Claude Code) launch to reach trouve's tools.
async fn bridged_tools(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let specs = engine.bridged_tool_specs(&id).await?;
    Ok(Json(serde_json::to_value(specs).unwrap_or_default()))
}

#[derive(serde::Deserialize)]
struct BridgedCallRequest {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct BridgedApprovalRequest {
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
}

/// Permission-prompt gate for vendor-executed tools (Claude Code's
/// `--permission-prompt-tool` hook, relayed by `trouve mcp-bridge`).
async fn bridged_approval_prompt(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<BridgedApprovalRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let approved = engine.bridged_approval(&id, &req.tool, &req.args).await?;
    Ok(Json(serde_json::json!({ "approved": approved })))
}

/// Execute one bridged tool call through the engine's gate/approval/event
/// chokepoint; returns the content string fed back to the vendor agent.
async fn bridged_tool_call(
    State(engine): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(req): Json<BridgedCallRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let content = engine
        .bridged_tool_call(&id, &req.name, &req.arguments)
        .await?;
    Ok(Json(serde_json::json!({ "content": content })))
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
    engine.set_default_model(&req.model)?;
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
        let replayed = match engine.store().events_after(&scope, after) {
            Ok(events) => events,
            Err(e) => {
                tracing::error!("event replay failed: {e}");
                return;
            }
        };
        let mut last = after;
        for env in replayed {
            last = env.cursor;
            if send_envelope(&tx, &env).await.is_err() {
                return;
            }
        }
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
                    match engine.store().events_after(&scope, last) {
                        Ok(events) => {
                            for env in events {
                                last = env.cursor;
                                if send_envelope(&tx, &env).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("event catch-up failed: {e}");
                            return;
                        }
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
