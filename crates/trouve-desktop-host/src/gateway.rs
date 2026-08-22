use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, LOCATION, ORIGIN,
    TRANSFER_ENCODING,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use utoipa::{OpenApi, ToSchema};

#[cfg(test)]
use crate::AssetManifest;
use crate::{
    Asset, AssetLookup, CloseDecision, ExternalHttpsUrl, FrontendSource, GatewayOrigin,
    HostCapabilities, HostKind, HostLifecycleEnvelope, HostLifecycleState, HostNativeActions,
    HostPreferences, HostValidationError, LocalFileAction, MAX_NATIVE_ATTACHMENT_TOTAL_BYTES,
    MAX_NATIVE_ATTACHMENTS, MAX_SYSTEM_FONT_FAMILIES, NativeAttachment, NativeNotification,
    VideoAttachmentOpenError, system_font_families, valid_session_relative_path,
    validate_native_attachment,
};

pub const HOST_API_PREFIX: &str = "/__trouve/host/v1";
pub const CSRF_HEADER: &str = "x-trouve-host-csrf";

const CAPABILITIES_PATH: &str = "/__trouve/host/v1/capabilities";
const PREFERENCES_PATH: &str = "/__trouve/host/v1/preferences";
const PICK_DIRECTORY_PATH: &str = "/__trouve/host/v1/pick-directory";
const PICK_FILES_PATH: &str = "/__trouve/host/v1/pick-files";
const READ_CLIPBOARD_IMAGE_PATH: &str = "/__trouve/host/v1/read-clipboard-image";
const LIFECYCLE_PATH: &str = "/__trouve/host/v1/lifecycle";
const CLOSE_ACKNOWLEDGEMENT_PATH: &str = "/__trouve/host/v1/close-acknowledgement";
const CLOSE_DECISION_PATH: &str = "/__trouve/host/v1/close-decision";
const SLEEP_INHIBITION_PATH: &str = "/__trouve/host/v1/sleep-inhibition";
const NATIVE_NOTIFICATION_PATH: &str = "/__trouve/host/v1/native-notification";
const USER_ATTENTION_PATH: &str = "/__trouve/host/v1/request-user-attention";
const LOCAL_FILE_ACTION_PATH: &str = "/__trouve/host/v1/local-file-action";
const OPEN_HTTPS_URL_PATH: &str = "/__trouve/host/v1/open-https-url";
const OPEN_VIDEO_ATTACHMENT_PATH: &str = "/__trouve/host/v1/open-video-attachment";
const MAX_NATIVE_PATH_BYTES: usize = 32 * 1024;
const MAX_VIDEO_ATTACHMENT_ACTION_BYTES: usize = 14 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OpenHttpsUrlRequest {
    pub url: String,
}

/// A cancelled picker succeeds with `path: null`; cancellation is not a host
/// error and must not be presented as one by the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PickDirectoryResponse {
    #[schema(required = true, nullable = true)]
    pub path: Option<String>,
}

/// One bounded attachment ready to be staged by a composer. Native paths and
/// file handles are deliberately absent from this schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentPayload {
    /// UTF-8 display name, bounded to 1 KiB by the host and client.
    pub name: String,
    /// ASCII MIME type, bounded to 255 bytes by the host and client.
    pub mime: String,
    /// Standard padded base64, bounded to the encoded 10 MiB payload limit.
    pub data: String,
    #[schema(minimum = 1, maximum = 10485760)]
    pub size_bytes: u64,
}

/// Cancellation succeeds with an empty attachment list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PickFilesResponse {
    #[schema(max_items = 4)]
    pub attachments: Vec<AttachmentPayload>,
}

/// Text, an empty clipboard, and unsupported clipboard content all succeed
/// with `attachment: null`, allowing ordinary browser paste to continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReadClipboardImageResponse {
    #[schema(required = true, nullable = true)]
    pub attachment: Option<AttachmentPayload>,
}

/// Cursor-addressed batch of ephemeral native window events plus the latest
/// recoverable state. The host retains only a bounded event tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostLifecycleBatch {
    pub cursor: u64,
    pub state: HostLifecycleState,
    pub events: Vec<HostLifecycleEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CloseDecisionRequest {
    pub request_id: u64,
    pub decision: CloseDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CloseAcknowledgementRequest {
    pub request_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SleepInhibitionRequest {
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NativeNotificationRequest {
    pub notification_id: String,
    pub title: String,
    pub body: String,
    pub sound: bool,
    pub session_id: String,
    #[schema(required = true, nullable = true)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalFileActionRequest {
    pub session_id: String,
    /// Existing worktree-relative regular file. Absolute paths and traversal
    /// are rejected before the protocol-backed resolver runs.
    pub relative_path: String,
    pub action: LocalFileAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostBootstrap {
    pub capabilities: HostCapabilities,
    /// Installed system UI font families available to the shared frontend.
    /// Older bridge bootstraps omit this field and fall back to the platform
    /// default font.
    #[serde(default)]
    #[schema(max_items = 4096)]
    pub font_families: Vec<String>,
    /// Ephemeral, origin-bound credential required on every gateway mutation.
    /// It is neither persisted nor forwarded to the Trouve protocol server.
    pub csrf_token: String,
}

#[derive(Debug, Default, Deserialize)]
struct LifecycleQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    wait_ms: u64,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_capabilities,
        get_preferences,
        put_preferences,
        pick_directory,
        pick_files,
        read_clipboard_image,
        get_lifecycle,
        close_acknowledgement,
        close_decision,
        set_sleep_inhibition,
        send_native_notification,
        request_user_attention,
        local_file_action,
        open_https_url,
        open_video_attachment
    ),
    components(schemas(
        HostBootstrap,
        HostCapabilities,
        HostPreferences,
        PickDirectoryResponse,
        AttachmentPayload,
        PickFilesResponse,
        ReadClipboardImageResponse,
        HostLifecycleBatch,
        HostLifecycleState,
        HostLifecycleEnvelope,
        crate::HostLifecycleEvent,
        crate::PendingCloseRequest,
        CloseAcknowledgementRequest,
        CloseDecisionRequest,
        crate::CloseDecision,
        SleepInhibitionRequest,
        NativeNotificationRequest,
        LocalFileActionRequest,
        crate::LocalFileAction,
        OpenHttpsUrlRequest,
        crate::AppearancePreferences,
        crate::ChatPreferences,
        crate::GeneralPreferences,
        crate::NotificationPreferences,
        crate::HostKind,
        crate::WindowGeometry
    ))
)]
struct HostApiDoc;

/// Canonical schema for the separately versioned native-host bridge. The
/// proxied `/v1` protocol remains owned by `trouve-protocol` and is excluded.
pub fn host_openapi_json() -> serde_json::Value {
    let mut doc = HostApiDoc::openapi();
    doc.info.title = "trouve desktop host bridge".into();
    doc.info.version = crate::DESKTOP_BRIDGE_VERSION.to_string();
    serde_json::to_value(doc).expect("host OpenAPI document serializes")
}

#[derive(Debug, Error)]
pub enum HostGatewayError {
    #[error(transparent)]
    Validation(#[from] HostValidationError),
    #[error("failed to create gateway HTTP client: {0}")]
    HttpClient(#[from] reqwest::Error),
    #[error("failed to read or write desktop preferences: {0}")]
    PreferencesIo(String),
    #[error("stored desktop preferences are invalid")]
    InvalidStoredPreferences,
}

#[derive(Debug, Error)]
pub enum HostGatewayBindError {
    #[error(transparent)]
    Gateway(#[from] HostGatewayError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
struct GatewayState {
    origin: GatewayOrigin,
    frontend: Arc<FrontendSource>,
    capabilities: HostCapabilities,
    font_families: Arc<[String]>,
    preferences: Arc<tokio::sync::Mutex<HostPreferences>>,
    /// Snapshot last presented to this gateway's web client. Incoming PUTs
    /// are full snapshots, so this baseline identifies which top-level fields
    /// the client actually changed before merging with another process.
    preference_baseline: Arc<tokio::sync::Mutex<HostPreferences>>,
    preference_path: Option<Arc<PathBuf>>,
    csrf_token: Arc<str>,
    protocol_upstream: Option<Url>,
    protocol_upstream_ownership: ProtocolUpstreamOwnership,
    http: reqwest::Client,
    native_actions: HostNativeActions,
    native_picker_permit: Arc<tokio::sync::Semaphore>,
    clipboard_image_permit: Arc<tokio::sync::Semaphore>,
}

/// Whether a configured protocol upstream is owned by this desktop app.
///
/// Only an app-owned embedded/elected server is known to share the desktop
/// host's filesystem namespace. Every URL supplied by a user or launcher is
/// explicit, including loopback URLs that may terminate a tunnel or container
/// port-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolUpstreamOwnership {
    Explicit,
    AppOwned,
}

/// Same-origin loopback gateway for packaged frontend assets, a narrow native
/// capability API, and transparent HTTP/SSE protocol forwarding. It owns no
/// durable agent state and never exposes arbitrary filesystem or process APIs.
#[derive(Clone)]
pub struct HostGateway {
    state: GatewayState,
}

/// Shared, ordered access to presentation preferences for the native window
/// adapter and the loopback web client. Keeping both writers behind the same
/// mutex prevents a resize from reverting a simultaneous theme or splitter
/// update.
#[derive(Clone)]
pub struct HostPreferencesHandle {
    preferences: Arc<tokio::sync::Mutex<HostPreferences>>,
    preference_path: Option<Arc<PathBuf>>,
}

impl HostPreferencesHandle {
    pub async fn snapshot(&self) -> HostPreferences {
        self.preferences.lock().await.clone()
    }

    pub async fn update_window_geometry(
        &self,
        geometry: crate::WindowGeometry,
    ) -> Result<(), HostGatewayError> {
        let mut current = self.preferences.lock().await;
        let baseline = current.clone();
        let geometry = Some(geometry);
        let mut next = baseline.clone();
        next.geometry = geometry.clone();
        validate_preferences(&next)?;
        if let Some(path) = self.preference_path.clone() {
            next = tokio::task::spawn_blocking(move || {
                merge_and_persist_preferences(&path, &baseline, &next, true)
            })
            .await
            .map_err(|error| HostGatewayError::PreferencesIo(error.to_string()))?
            .map_err(|error| HostGatewayError::PreferencesIo(error.to_string()))?;
        }
        *current = next;
        Ok(())
    }
}

impl HostGateway {
    pub fn new(
        origin: GatewayOrigin,
        frontend: impl Into<FrontendSource>,
        capabilities: HostCapabilities,
        preferences: HostPreferences,
    ) -> Result<Self, HostGatewayError> {
        validate_preferences(&preferences)?;
        // A desktop capability is truthful only after an OS adapter is
        // attached. Browser/PWA capabilities describe browser behavior and
        // are not backed by this bridge.
        let mut capabilities = capabilities;
        if capabilities.kind == HostKind::Desktop {
            capabilities.directory_picker = false;
            capabilities.file_picker = false;
            capabilities.clipboard_image = false;
            capabilities.lifecycle_events = false;
            capabilities.close_confirmation = false;
            capabilities.open_local_file = false;
            capabilities.reveal_local_file = false;
            capabilities.open_https_url = false;
            capabilities.open_video_attachment = false;
            capabilities.native_notifications = false;
            capabilities.user_attention = false;
            capabilities.sleep_inhibition = false;
            capabilities.window_geometry = false;
            capabilities.visibility = false;
            capabilities.occlusion = false;
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            // Never set a total timeout: durable and terminal SSE streams are
            // intentionally long lived.
            .build()?;
        Ok(Self {
            state: GatewayState {
                origin,
                frontend: Arc::new(frontend.into()),
                capabilities,
                font_families: system_font_families(),
                preferences: Arc::new(tokio::sync::Mutex::new(preferences.clone())),
                preference_baseline: Arc::new(tokio::sync::Mutex::new(preferences)),
                preference_path: None,
                csrf_token: fresh_token().into(),
                protocol_upstream: None,
                protocol_upstream_ownership: ProtocolUpstreamOwnership::AppOwned,
                http,
                native_actions: HostNativeActions::default(),
                native_picker_permit: Arc::new(tokio::sync::Semaphore::new(1)),
                clipboard_image_permit: Arc::new(tokio::sync::Semaphore::new(1)),
            },
        })
    }

    /// Attach typed, app-owned native actions and advertise only those that
    /// are actually callable. Synchronous callbacks must return promptly;
    /// asynchronous callbacks may remain pending without blocking the gateway.
    pub fn with_native_actions(mut self, native_actions: HostNativeActions) -> Self {
        if self.state.capabilities.kind == HostKind::Desktop {
            self.state.native_actions = native_actions;
            self.refresh_native_capabilities();
        }
        self
    }

    fn refresh_native_capabilities(&mut self) {
        if self.state.capabilities.kind != HostKind::Desktop {
            return;
        }
        self.state.capabilities.open_https_url = self.state.native_actions.can_open_https_url();
        self.state.capabilities.open_video_attachment =
            self.state.native_actions.can_open_video_attachment();
        self.state.capabilities.window_geometry =
            self.state.native_actions.can_manage_window_geometry();
        self.state.capabilities.file_picker = self.state.native_actions.can_pick_files();
        self.state.capabilities.clipboard_image =
            self.state.native_actions.can_read_clipboard_image();
        self.state.capabilities.lifecycle_events = self.state.native_actions.can_stream_lifecycle();
        self.state.capabilities.close_confirmation = self.state.native_actions.can_confirm_close();
        self.state.capabilities.native_notifications =
            self.state.native_actions.can_send_native_notifications();
        self.state.capabilities.user_attention =
            self.state.native_actions.can_request_user_attention();
        self.state.capabilities.sleep_inhibition = self.state.native_actions.can_inhibit_sleep();
        self.state.capabilities.visibility = self.state.native_actions.can_report_visibility();
        self.state.capabilities.occlusion = self.state.native_actions.can_report_occlusion();
        // Only the app-owned embedded/elected server is known to share this
        // process's filesystem namespace. An explicit upstream can be a
        // loopback tunnel or container port-forward, so fail closed for every
        // configured URL and retain the manual server-host path form.
        let picker_targets_local_server = self.state.protocol_upstream.is_none()
            || self.state.protocol_upstream_ownership == ProtocolUpstreamOwnership::AppOwned;
        self.state.capabilities.directory_picker =
            picker_targets_local_server && self.state.native_actions.can_pick_directory();
        let local_file_actions =
            picker_targets_local_server && self.state.native_actions.can_open_session_files();
        self.state.capabilities.open_local_file = local_file_actions;
        self.state.capabilities.reveal_local_file = local_file_actions;
    }

    pub fn with_protocol_upstream(self, upstream: &str) -> Result<Self, HostGatewayError> {
        self.with_protocol_upstream_ownership(upstream, ProtocolUpstreamOwnership::Explicit)
    }

    fn with_protocol_upstream_ownership(
        mut self,
        upstream: &str,
        ownership: ProtocolUpstreamOwnership,
    ) -> Result<Self, HostGatewayError> {
        let url = Url::parse(upstream)
            .map_err(|error| HostValidationError::InvalidUrl(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(HostValidationError::InvalidUrl(upstream.into()).into());
        }
        let loopback = match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if url.scheme() == "http" && !loopback {
            return Err(HostValidationError::InvalidUrl(
                "non-loopback protocol upstreams require HTTPS".into(),
            )
            .into());
        }
        self.state.protocol_upstream = Some(url);
        self.state.protocol_upstream_ownership = ownership;
        self.refresh_native_capabilities();
        Ok(self)
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(CAPABILITIES_PATH, get(get_capabilities))
            .route(PREFERENCES_PATH, get(get_preferences).put(put_preferences))
            .route(PICK_DIRECTORY_PATH, post(pick_directory))
            .route(PICK_FILES_PATH, post(pick_files))
            .route(READ_CLIPBOARD_IMAGE_PATH, post(read_clipboard_image))
            .route(LIFECYCLE_PATH, get(get_lifecycle))
            .route(CLOSE_ACKNOWLEDGEMENT_PATH, post(close_acknowledgement))
            .route(CLOSE_DECISION_PATH, post(close_decision))
            .route(SLEEP_INHIBITION_PATH, post(set_sleep_inhibition))
            .route(NATIVE_NOTIFICATION_PATH, post(send_native_notification))
            .route(USER_ATTENTION_PATH, post(request_user_attention))
            .route(LOCAL_FILE_ACTION_PATH, post(local_file_action))
            .route(OPEN_HTTPS_URL_PATH, post(open_https_url))
            .route(OPEN_VIDEO_ATTACHMENT_PATH, post(open_video_attachment))
            .route("/v1/{*path}", any(proxy_protocol))
            .fallback(any(serve_frontend))
            .with_state(self.state.clone())
    }

    pub fn origin(&self) -> &GatewayOrigin {
        &self.state.origin
    }

    /// Bind a same-origin gateway to loopback. Port zero is supported; the
    /// exact origin is constructed only after the OS selects the port.
    pub async fn bind_loopback(
        address: std::net::SocketAddr,
        frontend: impl Into<FrontendSource>,
        capabilities: HostCapabilities,
        preferences: HostPreferences,
        protocol_upstream: Option<&str>,
        preference_path: Option<PathBuf>,
    ) -> Result<
        (
            std::net::SocketAddr,
            impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
        ),
        HostGatewayBindError,
    > {
        Self::bind_loopback_with_actions(
            address,
            frontend,
            capabilities,
            preferences,
            protocol_upstream,
            preference_path,
            HostNativeActions::default(),
        )
        .await
    }

    /// Bind a loopback gateway with concrete native action adapters. This is
    /// separate from [`Self::bind_loopback`] so callers cannot accidentally
    /// advertise a desktop action merely by setting a capability flag.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_loopback_with_actions(
        address: std::net::SocketAddr,
        frontend: impl Into<FrontendSource>,
        capabilities: HostCapabilities,
        preferences: HostPreferences,
        protocol_upstream: Option<&str>,
        preference_path: Option<PathBuf>,
        native_actions: HostNativeActions,
    ) -> Result<
        (
            std::net::SocketAddr,
            impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
        ),
        HostGatewayBindError,
    > {
        let (address, gateway, _preferences) = Self::bind_loopback_with_actions_and_preferences(
            address,
            frontend,
            capabilities,
            preferences,
            protocol_upstream,
            preference_path,
            native_actions,
        )
        .await?;
        Ok((address, gateway))
    }

    /// Variant used by a native window adapter that must restore and persist
    /// geometry without racing the web client's preference writes.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_loopback_with_actions_and_preferences(
        address: std::net::SocketAddr,
        frontend: impl Into<FrontendSource>,
        capabilities: HostCapabilities,
        preferences: HostPreferences,
        protocol_upstream: Option<&str>,
        preference_path: Option<PathBuf>,
        native_actions: HostNativeActions,
    ) -> Result<
        (
            std::net::SocketAddr,
            impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
            HostPreferencesHandle,
        ),
        HostGatewayBindError,
    > {
        Self::bind_loopback_with_protocol_ownership_and_preferences(
            address,
            frontend,
            capabilities,
            preferences,
            protocol_upstream,
            ProtocolUpstreamOwnership::Explicit,
            preference_path,
            native_actions,
        )
        .await
    }

    /// Variant for hosts that must distinguish an app-owned embedded/elected
    /// server from an explicitly configured URL. `AppOwned` is valid only when
    /// the desktop app itself bound or elected the server and therefore knows
    /// that session worktree paths refer to the same filesystem namespace.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_loopback_with_protocol_ownership_and_preferences(
        address: std::net::SocketAddr,
        frontend: impl Into<FrontendSource>,
        capabilities: HostCapabilities,
        preferences: HostPreferences,
        protocol_upstream: Option<&str>,
        protocol_upstream_ownership: ProtocolUpstreamOwnership,
        preference_path: Option<PathBuf>,
        native_actions: HostNativeActions,
    ) -> Result<
        (
            std::net::SocketAddr,
            impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
            HostPreferencesHandle,
        ),
        HostGatewayBindError,
    > {
        if !address.ip().is_loopback() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "desktop gateway must bind loopback",
            )
            .into());
        }
        let listener = tokio::net::TcpListener::bind(address).await?;
        let local = listener.local_addr()?;
        let origin =
            GatewayOrigin::parse(&format!("http://{local}")).map_err(HostGatewayError::from)?;
        let mut capabilities = capabilities;
        capabilities.persistent_preferences = preference_path.is_some();
        let preferences = match preference_path.as_deref() {
            Some(path) => load_preferences(path, preferences)?,
            None => preferences,
        };
        let mut gateway = Self::new(origin, frontend, capabilities, preferences)?
            .with_native_actions(native_actions);
        gateway.state.preference_path = preference_path.map(Arc::new);
        if let Some(upstream) = protocol_upstream {
            gateway =
                gateway.with_protocol_upstream_ownership(upstream, protocol_upstream_ownership)?;
        }
        let preference_handle = HostPreferencesHandle {
            preferences: gateway.state.preferences.clone(),
            preference_path: gateway.state.preference_path.clone(),
        };
        Ok((
            local,
            async move { axum::serve(listener, gateway.router()).await },
            preference_handle,
        ))
    }
}

#[derive(Debug)]
enum GatewayRejection {
    Forbidden,
    InvalidPreferences,
    InvalidAction,
    Busy,
    Missing,
    BadGateway,
    VideoPlaybackCapacity,
    Internal,
}

impl IntoResponse for GatewayRejection {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::InvalidPreferences => (StatusCode::BAD_REQUEST, "invalid host preferences"),
            Self::InvalidAction => (StatusCode::BAD_REQUEST, "invalid native action request"),
            Self::Busy => (StatusCode::CONFLICT, "native action already in progress"),
            Self::Missing => (StatusCode::NOT_FOUND, "not found"),
            Self::BadGateway => (StatusCode::BAD_GATEWAY, "protocol server unavailable"),
            Self::VideoPlaybackCapacity => (
                StatusCode::INSUFFICIENT_STORAGE,
                "temporary video playback capacity is full",
            ),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "host gateway failure"),
        };
        let mut response = (status, message).into_response();
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        apply_security_headers(response.headers_mut());
        response
    }
}

#[utoipa::path(
    get,
    path = "/__trouve/host/v1/capabilities",
    responses((status = 200, body = HostBootstrap))
)]
async fn get_capabilities(
    State(state): State<GatewayState>,
    headers: HeaderMap,
) -> Result<Response, GatewayRejection> {
    validate_read(&state, &headers)?;
    let mut response = Json(HostBootstrap {
        capabilities: state.capabilities,
        font_families: state
            .font_families
            .iter()
            .take(MAX_SYSTEM_FONT_FAMILIES)
            .cloned()
            .collect(),
        csrf_token: state.csrf_token.to_string(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(response.headers_mut());
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/__trouve/host/v1/preferences",
    responses((status = 200, body = HostPreferences))
)]
async fn get_preferences(
    State(state): State<GatewayState>,
    headers: HeaderMap,
) -> Result<Response, GatewayRejection> {
    validate_read(&state, &headers)?;
    let preferences = state.preferences.lock().await.clone();
    *state.preference_baseline.lock().await = preferences.clone();
    let mut response = Json(preferences).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(response.headers_mut());
    Ok(response)
}

#[utoipa::path(
    put,
    path = "/__trouve/host/v1/preferences",
    request_body = HostPreferences,
    responses((status = 200, body = HostPreferences), (status = 400))
)]
async fn put_preferences(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .map_err(|_| GatewayRejection::InvalidPreferences)?;
    let mut preferences: HostPreferences =
        serde_json::from_slice(&bytes).map_err(|_| GatewayRejection::InvalidPreferences)?;
    // Serialize persistence and the in-memory replacement as one ordered
    // operation. Concurrent resize/theme writes can no longer complete out
    // of order or leave disk and memory on different versions.
    let mut current = state.preferences.lock().await;
    let mut baseline = state.preference_baseline.lock().await;
    // Geometry is owned by the native window adapter. A web-client PUT is a
    // snapshot replacement for web-owned fields, so preserve a resize that
    // happened after the client read that snapshot.
    preferences.geometry = current.geometry.clone();
    validate_preferences(&preferences).map_err(|_| GatewayRejection::InvalidPreferences)?;
    if let Some(path) = state.preference_path.clone() {
        let client_baseline = baseline.clone();
        let incoming = preferences.clone();
        preferences = tokio::task::spawn_blocking(move || {
            merge_and_persist_preferences(&path, &client_baseline, &incoming, false)
        })
        .await
        .map_err(|_| GatewayRejection::Internal)?
        .map_err(|_| GatewayRejection::Internal)?;
    }
    // Advance this client's baseline to what it submitted, while serving the
    // merged snapshot so the frontend immediately learns concurrent changes.
    *baseline = preferences.clone();
    *current = preferences.clone();
    let mut response = Json(preferences).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(response.headers_mut());
    Ok(response)
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/pick-directory",
    responses(
        (status = 200, body = PickDirectoryResponse),
        (status = 403),
        (status = 404),
        (status = 409),
        (status = 500)
    )
)]
async fn pick_directory(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.directory_picker
        || !state.native_actions.can_pick_directory()
    {
        return Err(GatewayRejection::Missing);
    }
    require_empty_action_body(request).await?;
    let _permit = state
        .native_picker_permit
        .clone()
        .try_acquire_owned()
        .map_err(|_| GatewayRejection::Busy)?;
    let picked = state
        .native_actions
        .pick_directory()
        .await
        .map_err(|_| GatewayRejection::Internal)?;
    let path = picked
        .as_deref()
        .map(validate_picked_directory)
        .transpose()
        .map_err(|_| GatewayRejection::Internal)?;

    let mut response = Json(PickDirectoryResponse { path }).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(response.headers_mut());
    Ok(response)
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/pick-files",
    responses(
        (status = 200, body = PickFilesResponse),
        (status = 400),
        (status = 403),
        (status = 404),
        (status = 409),
        (status = 500)
    )
)]
async fn pick_files(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.file_picker
        || !state.native_actions.can_pick_files()
    {
        return Err(GatewayRejection::Missing);
    }
    require_empty_action_body(request).await?;
    // Directory and file pickers share one permit: platforms generally allow
    // only one parented modal picker per application window.
    let _permit = state
        .native_picker_permit
        .clone()
        .try_acquire_owned()
        .map_err(|_| GatewayRejection::Busy)?;
    let picked = state
        .native_actions
        .pick_files()
        .await
        .map_err(|_| GatewayRejection::Internal)?;
    let attachments = native_attachment_payloads(picked.unwrap_or_default())
        .map_err(|_| GatewayRejection::Internal)?;
    json_no_store(PickFilesResponse { attachments })
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/read-clipboard-image",
    responses(
        (status = 200, body = ReadClipboardImageResponse),
        (status = 400),
        (status = 403),
        (status = 404),
        (status = 409),
        (status = 500)
    )
)]
async fn read_clipboard_image(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.clipboard_image
        || !state.native_actions.can_read_clipboard_image()
    {
        return Err(GatewayRejection::Missing);
    }
    require_empty_action_body(request).await?;
    let _permit = state
        .clipboard_image_permit
        .clone()
        .try_acquire_owned()
        .map_err(|_| GatewayRejection::Busy)?;
    let attachment = state
        .native_actions
        .read_clipboard_image()
        .await
        .map_err(|_| GatewayRejection::Internal)?
        .map(native_attachment_payload)
        .transpose()
        .map_err(|_| GatewayRejection::Internal)?;
    if attachment
        .as_ref()
        .is_some_and(|attachment| !attachment.mime.starts_with("image/"))
    {
        return Err(GatewayRejection::Internal);
    }
    json_no_store(ReadClipboardImageResponse { attachment })
}

#[utoipa::path(
    get,
    path = "/__trouve/host/v1/lifecycle",
    params(
        ("after" = Option<u64>, Query, description = "Last consumed ephemeral host cursor"),
        ("wait_ms" = Option<u64>, Query, description = "Bounded long-poll wait, at most 25000 ms")
    ),
    responses((status = 200, body = HostLifecycleBatch), (status = 400), (status = 404))
)]
async fn get_lifecycle(
    State(state): State<GatewayState>,
    Query(query): Query<LifecycleQuery>,
    headers: HeaderMap,
) -> Result<Response, GatewayRejection> {
    const MAX_WAIT_MS: u64 = 25_000;
    validate_read(&state, &headers)?;
    if query.wait_ms > MAX_WAIT_MS
        || state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.lifecycle_events
    {
        return if query.wait_ms > MAX_WAIT_MS {
            Err(GatewayRejection::InvalidAction)
        } else {
            Err(GatewayRejection::Missing)
        };
    }
    let lifecycle = state
        .native_actions
        .lifecycle()
        .ok_or(GatewayRejection::Missing)?;
    let mut changed = lifecycle.subscribe();
    let (cursor, _, _) = lifecycle.batch_after(query.after);
    if cursor <= query.after && query.wait_ms > 0 {
        let _ = tokio::time::timeout(Duration::from_millis(query.wait_ms), changed.changed()).await;
    }
    let (cursor, lifecycle_state, events) = lifecycle.batch_after(query.after);
    json_no_store(HostLifecycleBatch {
        cursor,
        state: lifecycle_state,
        events,
    })
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/close-acknowledgement",
    request_body = CloseAcknowledgementRequest,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn close_acknowledgement(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.close_confirmation
        || !state.native_actions.can_confirm_close()
    {
        return Err(GatewayRejection::Missing);
    }
    let request: CloseAcknowledgementRequest = read_json_action(request, 1024).await?;
    if request.request_id == 0 {
        return Err(GatewayRejection::InvalidAction);
    }
    state
        .native_actions
        .lifecycle()
        .ok_or(GatewayRejection::Missing)?
        .acknowledge_close_request(request.request_id)
        .map_err(|_| GatewayRejection::InvalidAction)?;
    state
        .native_actions
        .close_request_acknowledged(request.request_id)
        .map_err(|_| GatewayRejection::Internal)?;
    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/close-decision",
    request_body = CloseDecisionRequest,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn close_decision(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.close_confirmation
        || !state.native_actions.can_confirm_close()
    {
        return Err(GatewayRejection::Missing);
    }
    let request: CloseDecisionRequest = read_json_action(request, 1024).await?;
    if request.request_id == 0 {
        return Err(GatewayRejection::InvalidAction);
    }
    let lifecycle = state
        .native_actions
        .lifecycle()
        .ok_or(GatewayRejection::Missing)?;
    let quit = lifecycle
        .apply_close_decision(request.request_id, request.decision)
        .map_err(|_| GatewayRejection::InvalidAction)?;
    state
        .native_actions
        .close_decision_applied(request.request_id, request.decision)
        .map_err(|_| GatewayRejection::Internal)?;
    if quit {
        state
            .native_actions
            .quit()
            .map_err(|_| GatewayRejection::Internal)?;
    }
    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/sleep-inhibition",
    request_body = SleepInhibitionRequest,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn set_sleep_inhibition(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.sleep_inhibition
        || !state.native_actions.can_inhibit_sleep()
    {
        return Err(GatewayRejection::Missing);
    }
    let request: SleepInhibitionRequest = read_json_action(request, 1024).await?;
    state
        .native_actions
        .set_sleep_inhibition(request.active)
        .map_err(|_| GatewayRejection::Internal)?;
    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/native-notification",
    request_body = NativeNotificationRequest,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn send_native_notification(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.native_notifications
        || !state.native_actions.can_send_native_notifications()
    {
        return Err(GatewayRejection::Missing);
    }
    let request: NativeNotificationRequest = read_json_action(request, 8 * 1024).await?;
    let notification = NativeNotification::new(
        request.notification_id,
        request.title,
        request.body,
        request.sound,
        request.session_id,
        request.thread_id,
    )
    .map_err(|_| GatewayRejection::InvalidAction)?;
    state
        .native_actions
        .send_native_notification(notification)
        .map_err(|_| GatewayRejection::Internal)?;
    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/request-user-attention",
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn request_user_attention(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.user_attention
        || !state.native_actions.can_request_user_attention()
    {
        return Err(GatewayRejection::Missing);
    }
    require_empty_action_body(request).await?;
    state
        .native_actions
        .request_user_attention()
        .map_err(|_| GatewayRejection::Internal)?;
    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/local-file-action",
    request_body = LocalFileActionRequest,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn local_file_action(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.native_actions.can_open_session_files()
        || (!state.capabilities.open_local_file && !state.capabilities.reveal_local_file)
    {
        return Err(GatewayRejection::Missing);
    }
    let request: LocalFileActionRequest = read_json_action(request, 36 * 1024).await?;
    let action_available = match request.action {
        LocalFileAction::Open => state.capabilities.open_local_file,
        LocalFileAction::Reveal => state.capabilities.reveal_local_file,
    };
    if !action_available {
        return Err(GatewayRejection::Missing);
    }
    if !valid_bridge_id(&request.session_id) || !valid_session_relative_path(&request.relative_path)
    {
        return Err(GatewayRejection::InvalidAction);
    }
    let file = state
        .native_actions
        .resolve_session_file(request.session_id, request.relative_path)
        .await
        .map_err(|_| GatewayRejection::Internal)?;
    state
        .native_actions
        .handle_local_file(&file, request.action)
        .map_err(|_| GatewayRejection::Internal)?;
    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/open-https-url",
    request_body = OpenHttpsUrlRequest,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500))
)]
async fn open_https_url(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.open_https_url
        || !state.native_actions.can_open_https_url()
    {
        return Err(GatewayRejection::Missing);
    }
    let request: OpenHttpsUrlRequest = read_json_action(request, 8 * 1024).await?;
    let url = ExternalHttpsUrl::parse(&request.url).map_err(|_| GatewayRejection::InvalidAction)?;
    state
        .native_actions
        .open_https_url(&url)
        .map_err(|_| GatewayRejection::Internal)?;

    no_content()
}

#[utoipa::path(
    post,
    path = "/__trouve/host/v1/open-video-attachment",
    request_body = AttachmentPayload,
    responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 500), (status = 507))
)]
async fn open_video_attachment(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    validate_mutation(&state, request.headers())?;
    if state.capabilities.kind != HostKind::Desktop
        || !state.capabilities.open_video_attachment
        || !state.native_actions.can_open_video_attachment()
    {
        return Err(GatewayRejection::Missing);
    }
    let payload: AttachmentPayload =
        read_json_action(request, MAX_VIDEO_ATTACHMENT_ACTION_BYTES).await?;
    let attachment =
        native_attachment_from_payload(payload).map_err(|_| GatewayRejection::InvalidAction)?;
    if attachment.video_extension().is_none() {
        return Err(GatewayRejection::InvalidAction);
    }
    state
        .native_actions
        .open_video_attachment(attachment)
        .await
        .map_err(|error| match error {
            VideoAttachmentOpenError::Capacity => GatewayRejection::VideoPlaybackCapacity,
            VideoAttachmentOpenError::Failed(_) => GatewayRejection::Internal,
        })?;

    no_content()
}

fn validate_picked_directory(path: &Path) -> Result<String, ()> {
    let Some(path) = path.to_str() else {
        return Err(());
    };
    if !Path::new(path).is_absolute()
        || path.is_empty()
        || path.len() > MAX_NATIVE_PATH_BYTES
        || path.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(path.to_owned())
}

async fn require_empty_action_body(request: Request<Body>) -> Result<(), GatewayRejection> {
    let bytes = axum::body::to_bytes(request.into_body(), 1024)
        .await
        .map_err(|_| GatewayRejection::InvalidAction)?;
    if bytes.is_empty() {
        Ok(())
    } else {
        Err(GatewayRejection::InvalidAction)
    }
}

async fn read_json_action<T: DeserializeOwned>(
    request: Request<Body>,
    limit: usize,
) -> Result<T, GatewayRejection> {
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| GatewayRejection::InvalidAction)?;
    serde_json::from_slice(&bytes).map_err(|_| GatewayRejection::InvalidAction)
}

fn valid_bridge_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_chat_item_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn no_content() -> Result<Response, GatewayRejection> {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(response.headers_mut());
    Ok(response)
}

fn native_attachment_payloads(
    attachments: Vec<NativeAttachment>,
) -> Result<Vec<AttachmentPayload>, ()> {
    if attachments.len() > MAX_NATIVE_ATTACHMENTS {
        return Err(());
    }
    let mut total = 0usize;
    for attachment in &attachments {
        validate_native_attachment(attachment).map_err(|_| ())?;
        total = total.checked_add(attachment.bytes.len()).ok_or(())?;
        if total > MAX_NATIVE_ATTACHMENT_TOTAL_BYTES {
            return Err(());
        }
    }
    attachments
        .into_iter()
        .map(native_attachment_payload)
        .collect()
}

fn native_attachment_payload(attachment: NativeAttachment) -> Result<AttachmentPayload, ()> {
    validate_native_attachment(&attachment).map_err(|_| ())?;
    let size_bytes = u64::try_from(attachment.bytes.len()).map_err(|_| ())?;
    Ok(AttachmentPayload {
        name: attachment.name,
        mime: attachment.mime,
        data: base64::engine::general_purpose::STANDARD.encode(attachment.bytes),
        size_bytes,
    })
}

fn native_attachment_from_payload(payload: AttachmentPayload) -> Result<NativeAttachment, ()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.data)
        .map_err(|_| ())?;
    if u64::try_from(bytes.len()).map_err(|_| ())? != payload.size_bytes {
        return Err(());
    }
    NativeAttachment::new(payload.name, payload.mime, bytes).map_err(|_| ())
}

fn json_no_store<T: Serialize>(value: T) -> Result<Response, GatewayRejection> {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers(response.headers_mut());
    Ok(response)
}

fn load_preferences(
    path: &Path,
    fallback: HostPreferences,
) -> Result<HostPreferences, HostGatewayError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(fallback),
        Err(error) => return Err(HostGatewayError::PreferencesIo(error.to_string())),
    };
    if bytes.len() > 64 * 1024 {
        return Err(HostGatewayError::InvalidStoredPreferences);
    }
    let preferences: HostPreferences =
        serde_json::from_slice(&bytes).map_err(|_| HostGatewayError::InvalidStoredPreferences)?;
    validate_preferences(&preferences).map_err(|_| HostGatewayError::InvalidStoredPreferences)?;
    Ok(preferences)
}

fn merge_and_persist_preferences(
    path: &Path,
    baseline: &HostPreferences,
    incoming: &HostPreferences,
    include_geometry: bool,
) -> std::io::Result<HostPreferences> {
    use fs4::fs_std::FileExt as _;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "preference path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("preferences")
    ));
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    let result = (|| {
        let latest = load_preferences(path, baseline.clone()).map_err(std::io::Error::other)?;
        let merged = merge_preference_changes(&latest, baseline, incoming, include_geometry);
        validate_preferences(&merged).map_err(std::io::Error::other)?;
        persist_preferences(path, &merged)?;
        Ok(merged)
    })();
    let unlock = lock.unlock();
    match (result, unlock) {
        (Ok(preferences), Ok(())) => Ok(preferences),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn merge_preference_changes(
    latest: &HostPreferences,
    baseline: &HostPreferences,
    incoming: &HostPreferences,
    include_geometry: bool,
) -> HostPreferences {
    let mut merged = latest.clone();
    if include_geometry && incoming.geometry != baseline.geometry {
        merged.geometry = incoming.geometry.clone();
    }
    macro_rules! merge_leaf {
        ($($field:ident).+) => {
            if incoming.$($field).+ != baseline.$($field).+ {
                merged.$($field).+ = incoming.$($field).+.clone();
            }
        };
    }
    merge_leaf!(appearance.theme);
    merge_leaf!(appearance.font_family);
    merge_leaf!(appearance.font_size);
    merge_leaf!(appearance.reduce_motion);
    merge_leaf!(general.prevent_sleep_while_running);
    merge_leaf!(chat.collapse_sequential_tool_calls);
    merge_leaf!(chat.collapse_thinking_with_tools);
    merge_leaf!(chat.collapse_compaction_with_tools);
    merge_leaf!(chat.collapse_todo_updates_with_tools);
    merge_leaf!(notifications.enabled);
    merge_leaf!(notifications.on_finish);
    merge_leaf!(notifications.on_fail);
    merge_leaf!(notifications.on_attention);
    merge_leaf!(notifications.sound);
    merge_leaf!(workspace_order);
    merge_leaf!(pull_request_group_order);
    merge_leaf!(resume.selected_session_id);
    merged.resume.session_threads = merge_preference_map_changes(
        &latest.resume.session_threads,
        &baseline.resume.session_threads,
        &incoming.resume.session_threads,
    );
    merged.resume.thread_scroll = merge_preference_map_changes(
        &latest.resume.thread_scroll,
        &baseline.resume.thread_scroll,
        &incoming.resume.thread_scroll,
    );
    merge_leaf!(resume.closed_thread_tabs);
    merge_leaf!(resume.pinned_thread_tabs);
    merge_leaf!(navigation_width);
    merge_leaf!(inspection_width);
    merged
}

fn merge_preference_map_changes<K, V>(
    latest: &std::collections::BTreeMap<K, V>,
    baseline: &std::collections::BTreeMap<K, V>,
    incoming: &std::collections::BTreeMap<K, V>,
) -> std::collections::BTreeMap<K, V>
where
    K: Ord + Clone,
    V: PartialEq + Clone,
{
    let mut merged = latest.clone();
    for key in baseline.keys().chain(incoming.keys()) {
        if incoming.get(key) == baseline.get(key) {
            continue;
        }
        match incoming.get(key) {
            Some(value) => {
                merged.insert(key.clone(), value.clone());
            }
            None => {
                merged.remove(key);
            }
        }
    }
    merged
}

fn persist_preferences(path: &Path, preferences: &HostPreferences) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "preference path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("preferences"),
        uuid::Uuid::new_v4().simple()
    ));
    let bytes = serde_json::to_vec_pretty(preferences).map_err(std::io::Error::other)?;
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        replace_file(&temporary, path)?;
        sync_preference_parent(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_preference_parent(parent: &Path) -> std::io::Result<()> {
    // The file contents are durable before rename; syncing the directory
    // makes the replacement itself durable across a crash or power loss.
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_preference_parent(_parent: &Path) -> std::io::Result<()> {
    // Windows replacement requests MOVEFILE_WRITE_THROUGH below.
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temporary, path)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            path.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

async fn serve_frontend(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return Err(GatewayRejection::Missing);
    }
    validate_read(&state, request.headers())?;
    if request.uri().path().starts_with(HOST_API_PREFIX) {
        return Err(GatewayRejection::Missing);
    }
    if let Some(server) = state.frontend.dev_server() {
        let upstream = server.url().clone();
        let websocket_origin = server.websocket_origin();
        return proxy_upstream(&state, request, upstream, Some(&websocket_origin)).await;
    }
    let uri = request.uri();
    if uri.query().is_some() {
        return Err(GatewayRejection::Missing);
    }
    let assets = state
        .frontend
        .static_assets()
        .expect("non-development frontend source has a static manifest");
    let asset = match assets.resolve(uri.path()) {
        Ok(AssetLookup::Exact(asset) | AssetLookup::SpaFallback(asset)) => asset,
        Ok(AssetLookup::Missing) => return Err(GatewayRejection::Missing),
        Err(_) => return Err(GatewayRejection::Missing),
    };
    asset_response(asset)
}

async fn proxy_protocol(
    State(state): State<GatewayState>,
    request: Request<Body>,
) -> Result<Response, GatewayRejection> {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        validate_read(&state, request.headers())?;
    } else {
        validate_mutation(&state, request.headers())?;
    }
    let upstream = state
        .protocol_upstream
        .as_ref()
        .ok_or(GatewayRejection::BadGateway)?;
    proxy_upstream(&state, request, upstream.clone(), None).await
}

async fn proxy_upstream(
    state: &GatewayState,
    request: Request<Body>,
    mut url: Url,
    development_websocket_origin: Option<&str>,
) -> Result<Response, GatewayRejection> {
    url.set_path(request.uri().path());
    url.set_query(request.uri().query());

    let (parts, body) = request.into_parts();
    let request_connection_headers = connection_nominated_headers(&parts.headers);
    let mut outbound = state.http.request(parts.method, url);
    for (name, value) in &parts.headers {
        if !is_hop_by_hop(name)
            && !request_connection_headers.contains(name)
            && !is_forwarding_header(name)
            && name != HOST
            && name.as_str() != CSRF_HEADER
        {
            outbound = outbound.header(name, value);
        }
    }
    let upstream_response = outbound
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await
        .map_err(|_| GatewayRejection::BadGateway)?;

    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    let response_connection_headers = connection_nominated_headers(&response_headers);
    let mut response = Response::builder().status(status);
    for (name, value) in &response_headers {
        let safe_location = name != LOCATION
            || value
                .to_str()
                .is_ok_and(|location| location.starts_with('/') && !location.starts_with("//"));
        if !is_hop_by_hop(name)
            && !response_connection_headers.contains(name)
            && !is_forwarding_header(name)
            && safe_location
        {
            response = response.header(name, value);
        }
    }
    let mut response = response
        .body(Body::from_stream(upstream_response.bytes_stream()))
        .map_err(|_| GatewayRejection::Internal)?;
    // Protocol payloads can contain prompts, source, diffs, and terminal
    // content. Never let a webview cache survive the ephemeral host origin.
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_security_headers_with_development_websocket(
        response.headers_mut(),
        development_websocket_origin,
    )?;
    Ok(response)
}

fn validate_read(state: &GatewayState, headers: &HeaderMap) -> Result<(), GatewayRejection> {
    let host = header_value(headers, HOST).ok_or(GatewayRejection::Forbidden)?;
    if host != state.origin.authority() {
        return Err(GatewayRejection::Forbidden);
    }
    if let Some(origin) = header_value(headers, ORIGIN)
        && origin != state.origin.as_str()
    {
        return Err(GatewayRejection::Forbidden);
    }
    Ok(())
}

fn validate_mutation(state: &GatewayState, headers: &HeaderMap) -> Result<(), GatewayRejection> {
    let host = header_value(headers, HOST).ok_or(GatewayRejection::Forbidden)?;
    let origin = header_value(headers, ORIGIN).ok_or(GatewayRejection::Forbidden)?;
    state
        .origin
        .validate(host, origin)
        .map_err(|_| GatewayRejection::Forbidden)?;
    let csrf_name = HeaderName::from_static(CSRF_HEADER);
    let csrf = header_value(headers, csrf_name).ok_or(GatewayRejection::Forbidden)?;
    if csrf != state.csrf_token.as_ref() {
        return Err(GatewayRejection::Forbidden);
    }
    Ok(())
}

fn header_value(headers: &HeaderMap, name: impl axum::http::header::AsHeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

fn validate_preferences(preferences: &HostPreferences) -> Result<(), HostValidationError> {
    const THEMES: &[&str] = &[
        "system",
        "dark",
        "light",
        "high-contrast-dark",
        "colorblind-dark",
        "colorblind-light",
    ];
    let appearance = &preferences.appearance;
    let resume = &preferences.resume;
    let mut workspace_ids = std::collections::HashSet::new();
    let mut pull_request_group_ids = std::collections::HashSet::new();
    let mut closed_thread_ids = std::collections::HashSet::new();
    let mut pinned_thread_ids = std::collections::HashSet::new();
    let valid = THEMES.contains(&appearance.theme.as_str())
        && (10..=32).contains(&appearance.font_size)
        && appearance.font_family.len() <= 256
        && !appearance.font_family.chars().any(char::is_control)
        && preferences.navigation_width.is_finite()
        && (180.0..=600.0).contains(&preferences.navigation_width)
        && preferences.inspection_width.is_finite()
        && (240.0..=1_000.0).contains(&preferences.inspection_width)
        && preferences.geometry.as_ref().is_none_or(|geometry| {
            (-16_384..=16_384).contains(&geometry.x)
                && (-16_384..=16_384).contains(&geometry.y)
                && (320..=16_384).contains(&geometry.width)
                && (240..=16_384).contains(&geometry.height)
        })
        && preferences.workspace_order.len() <= 1_000
        && preferences.workspace_order.iter().all(|workspace_id| {
            valid_bridge_id(workspace_id) && workspace_ids.insert(workspace_id)
        })
        && preferences.pull_request_group_order.len() <= 32
        && preferences.pull_request_group_order.iter().all(|group_id| {
            valid_pull_request_group_id(group_id) && pull_request_group_ids.insert(group_id)
        })
        && (resume.selected_session_id.is_empty() || valid_bridge_id(&resume.selected_session_id))
        && resume.session_threads.len() <= 1_000
        && resume
            .session_threads
            .iter()
            .all(|(session_id, thread_id)| {
                valid_bridge_id(session_id) && valid_bridge_id(thread_id)
            })
        && resume.thread_scroll.len() <= 1_000
        && resume.thread_scroll.iter().all(|(thread_id, bookmark)| {
            valid_bridge_id(thread_id)
                && valid_chat_item_id(&bookmark.item_id)
                && bookmark.offset.is_finite()
                && (0.0..=1_000_000.0).contains(&bookmark.offset)
        })
        && resume.closed_thread_tabs.len() <= 1_000
        && resume
            .closed_thread_tabs
            .iter()
            .all(|thread_id| valid_bridge_id(thread_id) && closed_thread_ids.insert(thread_id))
        && resume.pinned_thread_tabs.len() <= 1_000
        && resume.pinned_thread_tabs.iter().all(|thread_id| {
            valid_bridge_id(thread_id)
                && !closed_thread_ids.contains(thread_id)
                && pinned_thread_ids.insert(thread_id)
        });
    if valid {
        Ok(())
    } else {
        Err(HostValidationError::InvalidPreferences(
            "value is outside the native host bounds".into(),
        ))
    }
}

fn valid_pull_request_group_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
}

fn asset_response(asset: &Asset) -> Result<Response, GatewayRejection> {
    let content_type =
        HeaderValue::from_str(&asset.content_type).map_err(|_| GatewayRejection::Internal)?;
    let is_html = asset
        .content_type
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/html"));
    let cache_control = if asset.immutable && !is_html {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else {
        HeaderValue::from_static("no-store")
    };
    let mut response = Response::new(Body::from(asset.bytes.to_vec()));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response.headers_mut().insert(CACHE_CONTROL, cache_control);
    apply_security_headers(response.headers_mut());
    Ok(response)
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    name == CONNECTION
        || name == TRANSFER_ENCODING
        || name == CONTENT_LENGTH
        || matches!(
            name.as_str(),
            "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "upgrade"
        )
}

fn connection_nominated_headers(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect()
}

fn is_forwarding_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "forwarded"
            | "via"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-port"
            | "x-forwarded-proto"
            | "x-real-ip"
    )
}

fn apply_security_headers(headers: &mut HeaderMap) {
    apply_security_headers_with_development_websocket(headers, None)
        .expect("the static desktop CSP is a valid header value");
}

fn apply_security_headers_with_development_websocket(
    headers: &mut HeaderMap,
    development_websocket_origin: Option<&str>,
) -> Result<(), GatewayRejection> {
    let content_security_policy = development_websocket_origin.map_or_else(
        || {
            "default-src 'self'; base-uri 'none'; connect-src 'self'; form-action 'none'; frame-ancestors 'none'; img-src 'self' data:; media-src 'self' blob: data:; object-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'".to_owned()
        },
        |websocket_origin| {
            format!(
                "default-src 'self'; base-uri 'none'; connect-src 'self' {websocket_origin}; form-action 'none'; frame-ancestors 'none'; img-src 'self' data:; media-src 'self' blob: data:; object-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'"
            )
        },
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_str(&content_security_policy).map_err(|_| GatewayRejection::Internal)?,
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), geolocation=(), microphone=(), payment=(), usb=()"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    Ok(())
}

fn fresh_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn asset(content_type: &str, bytes: &'static [u8], immutable: bool) -> Asset {
        Asset {
            content_type: content_type.into(),
            bytes: Arc::from(bytes),
            immutable,
        }
    }

    fn gateway() -> HostGateway {
        HostGateway::new(
            GatewayOrigin::parse("http://127.0.0.1:43127").unwrap(),
            AssetManifest::new([
                (
                    "/index.html".into(),
                    asset("text/html", b"<html>shell</html>", false),
                ),
                (
                    "/assets/app-12345678.js".into(),
                    asset("text/javascript", b"app", true),
                ),
            ])
            .unwrap(),
            HostCapabilities::desktop(),
            HostPreferences::default(),
        )
        .unwrap()
    }

    fn temporary_preference_path(label: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "trouve-desktop-host-test-{}-{}",
                label,
                uuid::Uuid::new_v4().simple()
            ))
            .join("preferences.json")
    }

    async fn response_json<T: serde::de::DeserializeOwned>(response: Response) -> T {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn picker_request(csrf_token: &str) -> Request<Body> {
        action_request(PICK_DIRECTORY_PATH, csrf_token)
    }

    fn action_request(path: &str, csrf_token: &str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(HOST, "127.0.0.1:43127")
            .header(ORIGIN, "http://127.0.0.1:43127")
            .header(CSRF_HEADER, csrf_token)
            .body(Body::empty())
            .unwrap()
    }

    fn json_action_request(path: &str, csrf_token: &str, body: impl Serialize) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(HOST, "127.0.0.1:43127")
            .header(ORIGIN, "http://127.0.0.1:43127")
            .header(CSRF_HEADER, csrf_token)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn native_attachment(name: &str, mime: &str, bytes: &[u8]) -> NativeAttachment {
        NativeAttachment::new(name, mime, bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn capabilities_require_exact_host_and_return_fresh_csrf() {
        let app = gateway().router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            HeaderValue::from_static("no-store")
        );
        let bootstrap: HostBootstrap = response_json(response).await;
        assert_eq!(bootstrap.capabilities.kind, crate::HostKind::Desktop);
        assert_eq!(bootstrap.csrf_token.len(), 64);

        let rejected = app
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "attacker.invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn preference_mutation_requires_origin_and_csrf() {
        let app = gateway().router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        let body = serde_json::to_vec(&HostPreferences::default()).unwrap();

        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(PREFERENCES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

        let accepted = app
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri(PREFERENCES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn external_https_action_is_capability_gated_validated_and_csrf_protected() {
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let opened_for_action = opened.clone();
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_external_https_opener(
                move |url| {
                    opened_for_action
                        .lock()
                        .unwrap()
                        .push(url.as_url().as_str().to_string());
                    Ok(())
                },
            ))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.open_https_url);
        assert_eq!(
            bootstrap.font_families.as_slice(),
            system_font_families().as_ref()
        );

        let body = serde_json::to_vec(&OpenHttpsUrlRequest {
            url: "https://example.com/docs?q=1#start".into(),
        })
        .unwrap();
        let missing_proof = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_HTTPS_URL_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_proof.status(), StatusCode::FORBIDDEN);
        assert!(opened.lock().unwrap().is_empty());

        let accepted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_HTTPS_URL_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, &bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
        assert_eq!(accepted.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(
            opened.lock().unwrap().as_slice(),
            ["https://example.com/docs?q=1#start"]
        );

        for unsafe_url in [
            "http://example.com",
            "https://user:secret@example.com",
            "file:///tmp/secret",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(OPEN_HTTPS_URL_PATH)
                        .header(HOST, "127.0.0.1:43127")
                        .header(ORIGIN, "http://127.0.0.1:43127")
                        .header(CSRF_HEADER, &bootstrap.csrf_token)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&OpenHttpsUrlRequest {
                                url: unsafe_url.into(),
                            })
                            .unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(opened.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn external_https_action_is_not_advertised_without_an_adapter() {
        let app = gateway().router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(!bootstrap.capabilities.open_https_url);
        assert!(!bootstrap.capabilities.open_video_attachment);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_HTTPS_URL_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"url":"https://example.com"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn video_attachment_action_is_bounded_typed_and_csrf_protected() {
        let opened = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
        let opened_for_action = Arc::clone(&opened);
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_video_attachment_opener(
                move |attachment| {
                    let opened_for_action = Arc::clone(&opened_for_action);
                    async move {
                        opened_for_action.lock().unwrap().push((
                            attachment.video_extension().unwrap().to_string(),
                            attachment.bytes().to_vec(),
                        ));
                        Ok(())
                    }
                },
            ))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.open_video_attachment);

        let payload = AttachmentPayload {
            name: "clip.exe".into(),
            mime: "video/mp4".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"video"),
            size_bytes: 5,
        };
        let body = serde_json::to_vec(&payload).unwrap();
        let missing_proof = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_VIDEO_ATTACHMENT_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_proof.status(), StatusCode::FORBIDDEN);
        assert!(opened.lock().unwrap().is_empty());

        let accepted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_VIDEO_ATTACHMENT_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, &bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            opened.lock().unwrap().as_slice(),
            [("mp4".into(), b"video".to_vec())]
        );

        let rejected = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_VIDEO_ATTACHMENT_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, &bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AttachmentPayload {
                            name: "payload.exe".into(),
                            mime: "application/x-executable".into(),
                            data: base64::engine::general_purpose::STANDARD.encode(b"video"),
                            size_bytes: 5,
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        assert_eq!(opened.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn video_attachment_launcher_does_not_block_gateway_reads() {
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_action = Arc::clone(&started);
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let release_rx_for_action = Arc::clone(&release_rx);
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_video_attachment_opener(
                move |_| {
                    let started = Arc::clone(&started_for_action);
                    let release_rx = release_rx_for_action.lock().unwrap().take().unwrap();
                    async move {
                        started.notify_one();
                        release_rx.await.map_err(|_| {
                            VideoAttachmentOpenError::Failed(
                                "test launcher release was dropped".to_string(),
                            )
                        })?;
                        Ok(())
                    }
                },
            ))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        let body = serde_json::to_vec(&AttachmentPayload {
            name: "clip.mp4".into(),
            mime: "video/mp4".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"video"),
            size_bytes: 5,
        })
        .unwrap();

        let launch_app = app.clone();
        let csrf_token = bootstrap.csrf_token.clone();
        let launch = tokio::spawn(async move {
            launch_app
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(OPEN_VIDEO_ATTACHMENT_PATH)
                        .header(HOST, "127.0.0.1:43127")
                        .header(ORIGIN, "http://127.0.0.1:43127")
                        .header(CSRF_HEADER, csrf_token)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        started.notified().await;

        let capabilities = app
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(capabilities.status(), StatusCode::OK);

        release.send(()).unwrap();
        assert_eq!(launch.await.unwrap().status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn video_attachment_capacity_has_a_distinct_actionable_response() {
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_video_attachment_opener(
                |_| async { Err(VideoAttachmentOpenError::Capacity) },
            ))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        let body = serde_json::to_vec(&AttachmentPayload {
            name: "clip.mp4".into(),
            mime: "video/mp4".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"video"),
            size_bytes: 5,
        })
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_VIDEO_ATTACHMENT_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "temporary video playback capacity is full"
        );
    }

    #[tokio::test]
    async fn window_geometry_capability_requires_a_native_window_adapter() {
        let mut requested = HostCapabilities::desktop();
        requested.window_geometry = true;
        let without_adapter = HostGateway::new(
            GatewayOrigin::parse("http://127.0.0.1:43127").unwrap(),
            AssetManifest::new([(
                "/index.html".into(),
                asset("text/html", b"<html></html>", false),
            )])
            .unwrap(),
            requested,
            HostPreferences::default(),
        )
        .unwrap();
        assert!(!without_adapter.state.capabilities.window_geometry);

        let with_adapter = without_adapter
            .with_native_actions(HostNativeActions::default().with_window_geometry());
        assert!(with_adapter.state.capabilities.window_geometry);
    }

    #[tokio::test]
    async fn directory_picker_is_async_capability_gated_and_cancellation_aware() {
        let results = Arc::new(Mutex::new(VecDeque::from([
            Ok(Some(PathBuf::from("/srv/repos/trouve"))),
            Ok(None),
        ])));
        let results_for_action = results.clone();
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_directory_picker(move || {
                let result = results_for_action.lock().unwrap().pop_front().unwrap();
                async move { result }
            }))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.directory_picker);

        let missing_proof = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(PICK_DIRECTORY_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_proof.status(), StatusCode::FORBIDDEN);
        assert_eq!(results.lock().unwrap().len(), 2);

        let unexpected_body = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(PICK_DIRECTORY_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, &bootstrap.csrf_token)
                    .body(Body::from(r#"{"path":"/attacker/chosen"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unexpected_body.status(), StatusCode::BAD_REQUEST);
        assert_eq!(results.lock().unwrap().len(), 2);

        let selected = app
            .clone()
            .oneshot(picker_request(&bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(selected.status(), StatusCode::OK);
        assert_eq!(selected.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(
            response_json::<PickDirectoryResponse>(selected).await,
            PickDirectoryResponse {
                path: Some("/srv/repos/trouve".into())
            }
        );

        let cancelled = app
            .oneshot(picker_request(&bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(
            response_json::<PickDirectoryResponse>(cancelled).await,
            PickDirectoryResponse { path: None }
        );
    }

    #[tokio::test]
    async fn directory_picker_allows_only_one_in_flight_dialog_without_blocking_reads() {
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_action = started.clone();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let release_rx_for_action = release_rx.clone();
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_directory_picker(move || {
                let started = started_for_action.clone();
                let release_rx = release_rx_for_action.lock().unwrap().take().unwrap();
                async move {
                    started.notify_one();
                    release_rx
                        .await
                        .map_err(|_| "test picker release was dropped".to_string())?;
                    Ok(Some(PathBuf::from("/srv/repos/slow")))
                }
            }))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;

        let first_app = app.clone();
        let first_csrf = bootstrap.csrf_token.clone();
        let first = tokio::spawn(async move {
            first_app
                .oneshot(picker_request(&first_csrf))
                .await
                .unwrap()
        });
        started.notified().await;

        let busy = app
            .clone()
            .oneshot(picker_request(&bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(busy.status(), StatusCode::CONFLICT);

        let capabilities = app
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(capabilities.status(), StatusCode::OK);

        release.send(()).unwrap();
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn directory_picker_fails_closed_for_invalid_results_and_adapter_errors() {
        for result in [
            Ok(Some(PathBuf::from("relative/repository"))),
            Ok(Some(PathBuf::from("/srv/repos/bad\npath"))),
            Err("secret native picker detail".to_string()),
        ] {
            let result = Arc::new(Mutex::new(Some(result)));
            let result_for_action = result.clone();
            let app = gateway()
                .with_native_actions(HostNativeActions::default().with_directory_picker(
                    move || {
                        let result = result_for_action.lock().unwrap().take().unwrap();
                        async move { result }
                    },
                ))
                .router();
            let bootstrap_response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(CAPABILITIES_PATH)
                        .header(HOST, "127.0.0.1:43127")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
            let response = app
                .oneshot(picker_request(&bootstrap.csrf_token))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert_eq!(body, "host gateway failure");
            assert!(!body.contains("secret"));
        }
    }

    #[tokio::test]
    async fn file_picker_returns_only_bounded_attachment_data_and_treats_cancel_normally() {
        let results = Arc::new(Mutex::new(VecDeque::from([
            Ok(Some(vec![native_attachment(
                "notes.txt",
                "text/plain",
                &[0, 1, 2, 255],
            )])),
            Ok(None),
        ])));
        let results_for_action = results.clone();
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_file_picker(move || {
                let result = results_for_action.lock().unwrap().pop_front().unwrap();
                async move { result }
            }))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.file_picker);

        let missing_proof = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(PICK_FILES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_proof.status(), StatusCode::FORBIDDEN);
        assert_eq!(results.lock().unwrap().len(), 2);

        let unexpected_body = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(PICK_FILES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, &bootstrap.csrf_token)
                    .body(Body::from(r#"{"path":"/tmp/secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unexpected_body.status(), StatusCode::BAD_REQUEST);
        assert_eq!(results.lock().unwrap().len(), 2);

        let selected = app
            .clone()
            .oneshot(action_request(PICK_FILES_PATH, &bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(selected.status(), StatusCode::OK);
        assert_eq!(selected.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(
            response_json::<PickFilesResponse>(selected).await,
            PickFilesResponse {
                attachments: vec![AttachmentPayload {
                    name: "notes.txt".into(),
                    mime: "text/plain".into(),
                    data: "AAEC/w==".into(),
                    size_bytes: 4,
                }],
            }
        );

        let cancelled = app
            .oneshot(action_request(PICK_FILES_PATH, &bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(
            response_json::<PickFilesResponse>(cancelled).await,
            PickFilesResponse {
                attachments: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn file_and_directory_pickers_share_one_nonblocking_single_flight_permit() {
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_action = started.clone();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let release_rx_for_action = release_rx.clone();
        let actions = HostNativeActions::default()
            .with_file_picker(move || {
                let started = started_for_action.clone();
                let release_rx = release_rx_for_action.lock().unwrap().take().unwrap();
                async move {
                    started.notify_one();
                    release_rx
                        .await
                        .map_err(|_| "test picker release was dropped".to_string())?;
                    Ok(Some(vec![native_attachment(
                        "slow.txt",
                        "text/plain",
                        b"slow",
                    )]))
                }
            })
            .with_directory_picker(|| async { Ok(Some(PathBuf::from("/srv/repos/other"))) });
        let app = gateway().with_native_actions(actions).router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;

        let first_app = app.clone();
        let first_csrf = bootstrap.csrf_token.clone();
        let first = tokio::spawn(async move {
            first_app
                .oneshot(action_request(PICK_FILES_PATH, &first_csrf))
                .await
                .unwrap()
        });
        started.notified().await;

        let second_file = app
            .clone()
            .oneshot(action_request(PICK_FILES_PATH, &bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(second_file.status(), StatusCode::CONFLICT);
        let directory = app
            .clone()
            .oneshot(picker_request(&bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(directory.status(), StatusCode::CONFLICT);
        let capabilities = app
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(capabilities.status(), StatusCode::OK);

        release.send(()).unwrap();
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn clipboard_image_is_bounded_text_aware_and_single_flight() {
        let results = Arc::new(Mutex::new(VecDeque::from([
            Ok(Some(native_attachment("pasted-1.png", "image/png", b"png"))),
            Ok(None),
        ])));
        let results_for_action = results.clone();
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_clipboard_image_reader(
                move || {
                    let result = results_for_action.lock().unwrap().pop_front().unwrap();
                    async move { result }
                },
            ))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.clipboard_image);

        let image = app
            .clone()
            .oneshot(action_request(
                READ_CLIPBOARD_IMAGE_PATH,
                &bootstrap.csrf_token,
            ))
            .await
            .unwrap();
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(
            response_json::<ReadClipboardImageResponse>(image).await,
            ReadClipboardImageResponse {
                attachment: Some(AttachmentPayload {
                    name: "pasted-1.png".into(),
                    mime: "image/png".into(),
                    data: "cG5n".into(),
                    size_bytes: 3,
                }),
            }
        );

        let no_image = app
            .oneshot(action_request(
                READ_CLIPBOARD_IMAGE_PATH,
                &bootstrap.csrf_token,
            ))
            .await
            .unwrap();
        assert_eq!(
            response_json::<ReadClipboardImageResponse>(no_image).await,
            ReadClipboardImageResponse { attachment: None }
        );
    }

    #[tokio::test]
    async fn native_attachment_actions_reject_invalid_adapter_output_without_leaking_it() {
        let invalid = NativeAttachment {
            name: "secret/path.txt".into(),
            mime: "text/plain".into(),
            bytes: b"secret bytes".to_vec(),
        };
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_file_picker(move || {
                let invalid = invalid.clone();
                async move { Ok(Some(vec![invalid])) }
            }))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        let response = app
            .oneshot(action_request(PICK_FILES_PATH, &bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(body, "host gateway failure");
        assert!(!body.contains("secret"));

        let too_many = (0..=MAX_NATIVE_ATTACHMENTS)
            .map(|index| native_attachment(&format!("{index}.txt"), "text/plain", b"x"))
            .collect();
        assert!(native_attachment_payloads(too_many).is_err());
        let too_large_total = vec![
            native_attachment(
                "one.bin",
                "application/octet-stream",
                &vec![0; 8 * 1024 * 1024],
            ),
            native_attachment(
                "two.bin",
                "application/octet-stream",
                &vec![0; 8 * 1024 * 1024],
            ),
            native_attachment(
                "three.bin",
                "application/octet-stream",
                &vec![0; 8 * 1024 * 1024],
            ),
        ];
        assert!(native_attachment_payloads(too_large_total).is_err());
    }

    #[tokio::test]
    async fn clipboard_reader_rejects_non_image_output_and_concurrent_reads() {
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_action = started.clone();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let release_rx_for_action = release_rx.clone();
        let app = gateway()
            .with_native_actions(HostNativeActions::default().with_clipboard_image_reader(
                move || {
                    let started = started_for_action.clone();
                    let release_rx = release_rx_for_action.lock().unwrap().take().unwrap();
                    async move {
                        started.notify_one();
                        release_rx
                            .await
                            .map_err(|_| "secret release error".to_string())?;
                        Ok(Some(native_attachment(
                            "not-image.txt",
                            "text/plain",
                            b"secret",
                        )))
                    }
                },
            ))
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        let first_app = app.clone();
        let first_csrf = bootstrap.csrf_token.clone();
        let first = tokio::spawn(async move {
            first_app
                .oneshot(action_request(READ_CLIPBOARD_IMAGE_PATH, &first_csrf))
                .await
                .unwrap()
        });
        started.notified().await;
        let busy = app
            .oneshot(action_request(
                READ_CLIPBOARD_IMAGE_PATH,
                &bootstrap.csrf_token,
            ))
            .await
            .unwrap();
        assert_eq!(busy.status(), StatusCode::CONFLICT);
        release.send(()).unwrap();
        let invalid = first.await.unwrap();
        assert_eq!(invalid.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = invalid.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"host gateway failure");
    }

    #[cfg(unix)]
    #[test]
    fn directory_picker_rejects_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));
        assert!(validate_picked_directory(&path).is_err());
    }

    #[tokio::test]
    async fn directory_picker_is_unavailable_without_an_adapter_or_for_remote_servers() {
        let no_adapter = gateway().router();
        let bootstrap_response = no_adapter
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(!bootstrap.capabilities.directory_picker);
        assert_eq!(
            no_adapter
                .oneshot(picker_request(&bootstrap.csrf_token))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        let remote = gateway()
            .with_protocol_upstream("https://server.example:7433")
            .unwrap()
            .with_native_actions(
                HostNativeActions::default()
                    .with_directory_picker(|| async {
                        Ok(Some(PathBuf::from("/srv/repos/not-on-server")))
                    })
                    .with_file_picker(|| async {
                        Ok(Some(vec![native_attachment(
                            "local.txt",
                            "text/plain",
                            b"upload",
                        )]))
                    })
                    .with_clipboard_image_reader(|| async {
                        Ok(Some(native_attachment("pasted.png", "image/png", b"png")))
                    }),
            )
            .router();
        let bootstrap_response = remote
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(!bootstrap.capabilities.directory_picker);
        // Unlike a workspace path, bounded attachment bytes can be uploaded
        // through the protocol to a remote server.
        assert!(bootstrap.capabilities.file_picker);
        assert!(bootstrap.capabilities.clipboard_image);

        let remote_after_actions = gateway()
            .with_native_actions(
                HostNativeActions::default().with_directory_picker(|| async {
                    Ok(Some(PathBuf::from("/srv/repos/not-on-server")))
                }),
            )
            .with_protocol_upstream("https://server.example:7433")
            .unwrap()
            .router();
        let bootstrap_response = remote_after_actions
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(!bootstrap.capabilities.directory_picker);

        let explicit_loopback = gateway()
            .with_native_actions(
                HostNativeActions::default().with_directory_picker(|| async {
                    Ok(Some(PathBuf::from("/srv/repos/local")))
                }),
            )
            .with_protocol_upstream("http://localhost:7433")
            .unwrap()
            .router();
        let bootstrap_response = explicit_loopback
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(!bootstrap.capabilities.directory_picker);
    }

    #[tokio::test]
    async fn pwa_kind_never_attaches_native_actions() {
        let opened = Arc::new(Mutex::new(false));
        let opened_for_action = opened.clone();
        let app = HostGateway::new(
            GatewayOrigin::parse("http://127.0.0.1:43127").unwrap(),
            AssetManifest::new([(
                "/index.html".into(),
                asset("text/html", b"<html>shell</html>", false),
            )])
            .unwrap(),
            HostCapabilities::pwa(),
            HostPreferences::default(),
        )
        .unwrap()
        .with_native_actions(
            HostNativeActions::default()
                .with_directory_picker(|| async { Ok(Some(PathBuf::from("/tmp/secret"))) })
                .with_file_picker(|| async {
                    Ok(Some(vec![native_attachment(
                        "secret.txt",
                        "text/plain",
                        b"secret",
                    )]))
                })
                .with_clipboard_image_reader(|| async {
                    Ok(Some(native_attachment(
                        "secret.png",
                        "image/png",
                        b"secret",
                    )))
                })
                .with_external_https_opener(move |_| {
                    *opened_for_action.lock().unwrap() = true;
                    Ok(())
                }),
        )
        .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        // PWA/browser opening is a browser feature, not permission to call a
        // desktop native action.
        assert!(bootstrap.capabilities.open_https_url);
        assert!(!bootstrap.capabilities.directory_picker);
        assert!(!bootstrap.capabilities.clipboard_image);
        assert!(!bootstrap.capabilities.lifecycle_events);
        assert!(!bootstrap.capabilities.close_confirmation);
        assert!(!bootstrap.capabilities.open_local_file);
        assert!(!bootstrap.capabilities.reveal_local_file);
        assert!(!bootstrap.capabilities.native_notifications);
        assert!(!bootstrap.capabilities.sleep_inhibition);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(OPEN_HTTPS_URL_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .header(ORIGIN, "http://127.0.0.1:43127")
                    .header(CSRF_HEADER, &bootstrap.csrf_token)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"url":"https://example.com"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!*opened.lock().unwrap());

        let native_file_response = app
            .oneshot(action_request(PICK_FILES_PATH, &bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(native_file_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn lifecycle_is_cursor_addressed_and_close_requires_frontend_confirmation() {
        let lifecycle = crate::HostLifecycleHandle::default();
        let quit_count = Arc::new(AtomicUsize::new(0));
        let quit_for_action = quit_count.clone();
        let close_decisions = Arc::new(Mutex::new(Vec::new()));
        let decisions_for_action = close_decisions.clone();
        let close_acknowledgements = Arc::new(Mutex::new(Vec::new()));
        let acknowledgements_for_action = close_acknowledgements.clone();
        let app = gateway()
            .with_native_actions(
                HostNativeActions::default()
                    .with_lifecycle_capabilities(lifecycle.clone(), true, true)
                    .with_close_acknowledgement_observer(move |request_id| {
                        acknowledgements_for_action.lock().unwrap().push(request_id);
                        Ok(())
                    })
                    .with_close_decision_observer(move |request_id, decision| {
                        decisions_for_action
                            .lock()
                            .unwrap()
                            .push((request_id, decision));
                        Ok(())
                    })
                    .with_quit_handler(move || {
                        quit_for_action.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
            )
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.lifecycle_events);
        assert!(bootstrap.capabilities.close_confirmation);
        assert!(bootstrap.capabilities.visibility);
        assert!(bootstrap.capabilities.occlusion);

        lifecycle.set_focused(true);
        lifecycle.set_occluded(true);
        let request_id = lifecycle.request_close();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{LIFECYCLE_PATH}?after=0&wait_ms=0"))
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let batch: HostLifecycleBatch = response_json(response).await;
        assert_eq!(batch.events.len(), 3);
        assert!(batch.state.focused);
        assert!(batch.state.occluded);
        assert_eq!(
            batch.state.pending_close,
            Some(crate::PendingCloseRequest {
                request_id,
                waiting_for_idle: false,
            })
        );

        let acknowledged = app
            .clone()
            .oneshot(json_action_request(
                CLOSE_ACKNOWLEDGEMENT_PATH,
                &bootstrap.csrf_token,
                CloseAcknowledgementRequest { request_id },
            ))
            .await
            .unwrap();
        assert_eq!(acknowledged.status(), StatusCode::NO_CONTENT);
        assert_eq!(quit_count.load(Ordering::SeqCst), 0);
        assert!(close_decisions.lock().unwrap().is_empty());
        assert_eq!(
            close_acknowledgements.lock().unwrap().as_slice(),
            &[request_id]
        );

        let deferred = app
            .clone()
            .oneshot(json_action_request(
                CLOSE_DECISION_PATH,
                &bootstrap.csrf_token,
                CloseDecisionRequest {
                    request_id,
                    decision: CloseDecision::QuitWhenIdle,
                },
            ))
            .await
            .unwrap();
        assert_eq!(deferred.status(), StatusCode::NO_CONTENT);
        assert_eq!(quit_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            close_decisions.lock().unwrap().as_slice(),
            &[(request_id, CloseDecision::QuitWhenIdle)]
        );

        let state_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{LIFECYCLE_PATH}?after={}&wait_ms=0", batch.cursor))
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let state: HostLifecycleBatch = response_json(state_response).await;
        assert!(state.events.is_empty());
        assert!(state.state.pending_close.unwrap().waiting_for_idle);

        let quit = app
            .clone()
            .oneshot(json_action_request(
                CLOSE_DECISION_PATH,
                &bootstrap.csrf_token,
                CloseDecisionRequest {
                    request_id,
                    decision: CloseDecision::QuitNow,
                },
            ))
            .await
            .unwrap();
        assert_eq!(quit.status(), StatusCode::NO_CONTENT);
        assert_eq!(quit_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            close_decisions.lock().unwrap().as_slice(),
            &[
                (request_id, CloseDecision::QuitWhenIdle),
                (request_id, CloseDecision::QuitNow),
            ]
        );

        let stale = app
            .clone()
            .oneshot(json_action_request(
                CLOSE_DECISION_PATH,
                &bootstrap.csrf_token,
                CloseDecisionRequest {
                    request_id,
                    decision: CloseDecision::Cancel,
                },
            ))
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::BAD_REQUEST);
        let stale_acknowledgement = app
            .clone()
            .oneshot(json_action_request(
                CLOSE_ACKNOWLEDGEMENT_PATH,
                &bootstrap.csrf_token,
                CloseAcknowledgementRequest { request_id },
            ))
            .await
            .unwrap();
        assert_eq!(stale_acknowledgement.status(), StatusCode::BAD_REQUEST);

        let cancelled_request_id = lifecycle.request_close();
        let cancelled = app
            .oneshot(json_action_request(
                CLOSE_DECISION_PATH,
                &bootstrap.csrf_token,
                CloseDecisionRequest {
                    request_id: cancelled_request_id,
                    decision: CloseDecision::Cancel,
                },
            ))
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::NO_CONTENT);
        assert_eq!(quit_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            close_decisions.lock().unwrap().as_slice(),
            &[
                (request_id, CloseDecision::QuitWhenIdle),
                (request_id, CloseDecision::QuitNow),
                (cancelled_request_id, CloseDecision::Cancel),
            ]
        );
    }

    #[tokio::test]
    async fn native_delivery_and_sleep_are_typed_capability_gated_actions() {
        let sleep = Arc::new(Mutex::new(Vec::new()));
        let sleep_for_action = sleep.clone();
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let notifications_for_action = notifications.clone();
        let attention = Arc::new(AtomicUsize::new(0));
        let attention_for_action = attention.clone();
        let app = gateway()
            .with_native_actions(
                HostNativeActions::default()
                    .with_sleep_inhibitor(move |active| {
                        sleep_for_action.lock().unwrap().push(active);
                        Ok(())
                    })
                    .with_native_notification_sender(move |notification| {
                        notifications_for_action.lock().unwrap().push(notification);
                        Ok(())
                    })
                    .with_user_attention_requester(move || {
                        attention_for_action.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
            )
            .router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.sleep_inhibition);
        assert!(bootstrap.capabilities.native_notifications);
        assert!(bootstrap.capabilities.user_attention);

        for active in [true, false] {
            let response = app
                .clone()
                .oneshot(json_action_request(
                    SLEEP_INHIBITION_PATH,
                    &bootstrap.csrf_token,
                    SleepInhibitionRequest { active },
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
        assert_eq!(*sleep.lock().unwrap(), [true, false]);

        let response = app
            .clone()
            .oneshot(json_action_request(
                NATIVE_NOTIFICATION_PATH,
                &bootstrap.csrf_token,
                NativeNotificationRequest {
                    notification_id: "notice-1".into(),
                    title: "Approval needed".into(),
                    body: "Visual parity".into(),
                    sound: true,
                    session_id: "se-1".into(),
                    thread_id: Some("th-1".into()),
                },
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        {
            let sent = notifications.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].session_id(), "se-1");
        }

        let invalid = app
            .clone()
            .oneshot(json_action_request(
                NATIVE_NOTIFICATION_PATH,
                &bootstrap.csrf_token,
                NativeNotificationRequest {
                    notification_id: "bad".into(),
                    title: "bad\0title".into(),
                    body: String::new(),
                    sound: false,
                    session_id: "se-1".into(),
                    thread_id: None,
                },
            ))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let response = app
            .oneshot(action_request(USER_ATTENTION_PATH, &bootstrap.csrf_token))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(attention.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn local_file_actions_resolve_existing_files_inside_a_local_session_worktree() {
        let root = temporary_preference_path("local-file")
            .parent()
            .unwrap()
            .join("worktree");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}\n").unwrap();
        crate::VerifiedSessionFile::resolve(&root, "src/main.rs").unwrap();
        let handled = Arc::new(Mutex::new(Vec::new()));
        let handled_for_action = handled.clone();
        let root_for_resolver = root.clone();
        let actions = HostNativeActions::default()
            .with_session_file_resolver(move |session_id, relative_path| {
                let root = root_for_resolver.clone();
                async move {
                    if session_id != "se-active" {
                        return Err("session unavailable".into());
                    }
                    crate::VerifiedSessionFile::resolve(root, relative_path)
                        .map_err(|_| "file unavailable".into())
                }
            })
            .with_local_file_handler(move |file, action| {
                handled_for_action
                    .lock()
                    .unwrap()
                    .push((file.as_path().to_owned(), action));
                Ok(())
            });
        let app = gateway().with_native_actions(actions.clone()).router();
        let bootstrap_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bootstrap: HostBootstrap = response_json(bootstrap_response).await;
        assert!(bootstrap.capabilities.open_local_file);
        assert!(bootstrap.capabilities.reveal_local_file);

        let response = app
            .clone()
            .oneshot(json_action_request(
                LOCAL_FILE_ACTION_PATH,
                &bootstrap.csrf_token,
                LocalFileActionRequest {
                    session_id: "se-active".into(),
                    relative_path: "src/main.rs".into(),
                    action: LocalFileAction::Reveal,
                },
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(handled.lock().unwrap().len(), 1);

        let traversal = app
            .oneshot(json_action_request(
                LOCAL_FILE_ACTION_PATH,
                &bootstrap.csrf_token,
                LocalFileActionRequest {
                    session_id: "se-active".into(),
                    relative_path: "../outside".into(),
                    action: LocalFileAction::Open,
                },
            ))
            .await
            .unwrap();
        assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);
        assert_eq!(handled.lock().unwrap().len(), 1);

        let remote = gateway()
            .with_native_actions(actions.clone())
            .with_protocol_upstream("https://server.example:7433")
            .unwrap()
            .router();
        let remote_bootstrap_response = remote
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let remote_bootstrap: HostBootstrap = response_json(remote_bootstrap_response).await;
        assert!(!remote_bootstrap.capabilities.open_local_file);
        assert!(!remote_bootstrap.capabilities.reveal_local_file);
        let denied = remote
            .oneshot(json_action_request(
                LOCAL_FILE_ACTION_PATH,
                &remote_bootstrap.csrf_token,
                LocalFileActionRequest {
                    session_id: "se-active".into(),
                    relative_path: "src/main.rs".into(),
                    action: LocalFileAction::Open,
                },
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);

        let explicit_loopback = gateway()
            .with_native_actions(actions)
            .with_protocol_upstream("http://localhost:7433")
            .unwrap()
            .router();
        let loopback_bootstrap_response = explicit_loopback
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let loopback_bootstrap: HostBootstrap = response_json(loopback_bootstrap_response).await;
        assert!(!loopback_bootstrap.capabilities.open_local_file);
        assert!(!loopback_bootstrap.capabilities.reveal_local_file);
        let denied = explicit_loopback
            .oneshot(json_action_request(
                LOCAL_FILE_ACTION_PATH,
                &loopback_bootstrap.csrf_token,
                LocalFileActionRequest {
                    session_id: "se-active".into(),
                    relative_path: "src/main.rs".into(),
                    action: LocalFileAction::Open,
                },
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);

        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn assets_are_fixed_csp_protected_and_spa_aware() {
        let app = gateway().router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/app-12345678.js")
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            HeaderValue::from_static("public, max-age=31536000, immutable")
        );
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("media-src 'self' blob: data:")
        );

        let shell = app
            .oneshot(
                Request::builder()
                    .uri("/workspaces/ws/sessions/se")
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(shell.status(), StatusCode::OK);
        assert_eq!(
            shell.headers()[CACHE_CONTROL],
            HeaderValue::from_static("no-store")
        );
    }

    #[tokio::test]
    async fn vite_development_assets_proxy_without_crossing_host_or_protocol_routes() {
        let upstream =
            Router::new().fallback(get(|headers: HeaderMap, uri: axum::http::Uri| async move {
                let mut response = Json(serde_json::json!({
                    "path": uri.path(),
                    "query": uri.query(),
                    "host": header_value(&headers, HOST),
                }))
                .into_response();
                response.headers_mut().insert(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=3600"),
                );
                response
                    .headers_mut()
                    .insert(CONNECTION, HeaderValue::from_static("x-upstream-secret"));
                response.headers_mut().insert(
                    "x-upstream-secret",
                    HeaderValue::from_static("must-not-cross"),
                );
                response
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let gateway = HostGateway::new(
            GatewayOrigin::parse("http://127.0.0.1:43127").unwrap(),
            FrontendSource::ViteDevServer(
                crate::FrontendDevServer::parse(&format!("http://{address}")).unwrap(),
            ),
            HostCapabilities::desktop(),
            HostPreferences::default(),
        )
        .unwrap();
        let app = gateway.router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/@vite/client?t=123")
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            HeaderValue::from_static("no-store")
        );
        assert!(!response.headers().contains_key("x-upstream-secret"));
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains(&format!("ws://{address}"))
        );
        let value: serde_json::Value = response_json(response).await;
        assert_eq!(value["path"], "/@vite/client");
        assert_eq!(value["query"], "t=123");
        assert_eq!(value["host"], address.to_string());

        let host_api = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(CAPABILITIES_PATH)
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(host_api.status(), StatusCode::OK);
        let protocol = app
            .oneshot(
                Request::builder()
                    .uri("/v1/info")
                    .header(HOST, "127.0.0.1:43127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(protocol.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn protocol_proxy_rewrites_authority_and_streams_response() {
        let upstream = Router::new().route(
            "/v1/info",
            get(|headers: HeaderMap| async move {
                let mut response = Json(serde_json::json!({
                    "host": header_value(&headers, HOST),
                    "forwarded": header_value(&headers, "forwarded"),
                    "x_forwarded_for": header_value(&headers, "x-forwarded-for"),
                    "connection_secret": header_value(&headers, "x-connection-secret"),
                }))
                .into_response();
                response.headers_mut().insert(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=3600"),
                );
                response
                    .headers_mut()
                    .insert(CONNECTION, HeaderValue::from_static("x-upstream-secret"));
                response.headers_mut().insert(
                    "x-upstream-secret",
                    HeaderValue::from_static("must-not-cross"),
                );
                response
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let gateway = gateway()
            .with_protocol_upstream(&format!("http://{address}"))
            .unwrap();
        let response = gateway
            .router()
            .oneshot(
                Request::builder()
                    .uri("/v1/info")
                    .header(HOST, "127.0.0.1:43127")
                    .header("forwarded", "for=203.0.113.10;proto=https")
                    .header("x-forwarded-for", "203.0.113.10")
                    .header(CONNECTION, "x-connection-secret")
                    .header("x-connection-secret", "must-not-cross")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            HeaderValue::from_static("no-store")
        );
        assert!(!response.headers().contains_key("x-upstream-secret"));
        let value: serde_json::Value = response_json(response).await;
        assert_eq!(value["host"], address.to_string());
        assert!(value["forwarded"].is_null());
        assert!(value["x_forwarded_for"].is_null());
        assert!(value["connection_secret"].is_null());
    }

    #[test]
    fn remote_protocol_upstreams_require_https() {
        assert!(
            gateway()
                .with_protocol_upstream("http://server.example:7433")
                .is_err()
        );
        assert!(
            gateway()
                .with_protocol_upstream("https://server.example:7433")
                .is_ok()
        );
        assert!(
            gateway()
                .with_protocol_upstream("http://localhost:7433")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn bound_gateway_loads_and_atomically_persists_preferences() {
        let path = temporary_preference_path("persistence");
        let mut stored = HostPreferences::default();
        stored.appearance.theme = "light".into();
        stored.general.prevent_sleep_while_running = false;
        stored.chat.collapse_sequential_tool_calls = true;
        stored.chat.collapse_thinking_with_tools = true;
        stored.notifications.on_finish = false;
        stored.notifications.sound = true;
        stored.workspace_order = vec!["ws-2".into(), "ws-1".into()];
        stored.pull_request_group_order = vec!["ready-to-merge".into(), "drafts".into()];
        persist_preferences(&path, &stored).unwrap();
        let assets = AssetManifest::new([(
            "/index.html".into(),
            asset("text/html", b"<html></html>", false),
        )])
        .unwrap();
        let (address, server) = HostGateway::bind_loopback(
            "127.0.0.1:0".parse().unwrap(),
            assets,
            HostCapabilities::desktop(),
            HostPreferences::default(),
            None,
            Some(path.clone()),
        )
        .await
        .unwrap();
        let task = tokio::spawn(server);
        let client = reqwest::Client::new();
        let origin = format!("http://{address}");
        let bootstrap: HostBootstrap = client
            .get(format!("{origin}{CAPABILITIES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(bootstrap.capabilities.persistent_preferences);
        let loaded: HostPreferences = client
            .get(format!("{origin}{PREFERENCES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(loaded.appearance.theme, "light");
        assert!(!loaded.general.prevent_sleep_while_running);
        assert!(loaded.chat.collapse_sequential_tool_calls);
        assert!(loaded.chat.collapse_thinking_with_tools);
        assert!(!loaded.chat.collapse_compaction_with_tools);
        assert!(!loaded.chat.collapse_todo_updates_with_tools);
        assert!(!loaded.notifications.on_finish);
        assert!(loaded.notifications.sound);
        assert_eq!(loaded.workspace_order, ["ws-2", "ws-1"]);
        assert_eq!(
            loaded.pull_request_group_order,
            ["ready-to-merge", "drafts"]
        );

        let mut updated = loaded;
        updated.appearance.theme = "colorblind-dark".into();
        updated.chat.collapse_sequential_tool_calls = false;
        updated.chat.collapse_thinking_with_tools = false;
        let response = client
            .put(format!("{origin}{PREFERENCES_PATH}"))
            .header(ORIGIN, &origin)
            .header(CSRF_HEADER, bootstrap.csrf_token)
            .json(&updated)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            load_preferences(&path, HostPreferences::default())
                .unwrap()
                .appearance
                .theme,
            "colorblind-dark"
        );
        assert!(
            !load_preferences(&path, HostPreferences::default())
                .unwrap()
                .chat
                .collapse_thinking_with_tools
        );
        assert!(
            !load_preferences(&path, HostPreferences::default())
                .unwrap()
                .chat
                .collapse_sequential_tool_calls
        );
        let parent = path.parent().unwrap();
        let leftovers = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        task.abort();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn preference_persistence_replaces_an_existing_file() {
        let path = temporary_preference_path("replace-existing");
        let mut first = HostPreferences::default();
        first.appearance.theme = "light".into();
        persist_preferences(&path, &first).unwrap();

        let mut second = HostPreferences::default();
        second.appearance.theme = "system".into();
        second.navigation_width = 312.0;
        persist_preferences(&path, &second).unwrap();

        assert_eq!(
            load_preferences(&path, HostPreferences::default()).unwrap(),
            second
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stale_gateway_snapshots_merge_only_changed_fields() {
        let path = temporary_preference_path("cross-process-merge");
        let baseline = HostPreferences::default();
        persist_preferences(&path, &baseline).unwrap();

        let mut first = baseline.clone();
        first.appearance.theme = "light".into();
        first
            .resume
            .session_threads
            .insert("se-first".into(), "th-first".into());
        let first = merge_and_persist_preferences(&path, &baseline, &first, false).unwrap();
        assert_eq!(first.appearance.theme, "light");

        // A second gateway still has the original full snapshot, but changed
        // only navigation width. Its write must retain the first process's
        // theme rather than reverting it to the stale baseline value.
        let mut second = baseline.clone();
        second.navigation_width = 318.0;
        second.appearance.font_size = 15;
        second
            .resume
            .session_threads
            .insert("se-second".into(), "th-second".into());
        let second = merge_and_persist_preferences(&path, &baseline, &second, false).unwrap();
        assert_eq!(second.appearance.theme, "light");
        assert_eq!(second.appearance.font_size, 15);
        assert_eq!(second.navigation_width, 318.0);
        assert_eq!(
            second.resume.session_threads,
            [
                ("se-first".into(), "th-first".into()),
                ("se-second".into(), "th-second".into()),
            ]
            .into()
        );
        assert_eq!(
            load_preferences(&path, HostPreferences::default()).unwrap(),
            second
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn separate_gateways_merge_writes_from_stale_client_snapshots() {
        let path = temporary_preference_path("two-gateway-merge");
        let assets = || {
            AssetManifest::new([(
                "/index.html".into(),
                asset("text/html", b"<html></html>", false),
            )])
            .unwrap()
        };
        let (first_address, first_server) = HostGateway::bind_loopback(
            "127.0.0.1:0".parse().unwrap(),
            assets(),
            HostCapabilities::desktop(),
            HostPreferences::default(),
            None,
            Some(path.clone()),
        )
        .await
        .unwrap();
        let (second_address, second_server) = HostGateway::bind_loopback(
            "127.0.0.1:0".parse().unwrap(),
            assets(),
            HostCapabilities::desktop(),
            HostPreferences::default(),
            None,
            Some(path.clone()),
        )
        .await
        .unwrap();
        let first_task = tokio::spawn(first_server);
        let second_task = tokio::spawn(second_server);
        let client = reqwest::Client::new();
        let first_origin = format!("http://{first_address}");
        let second_origin = format!("http://{second_address}");

        let first_bootstrap: HostBootstrap = client
            .get(format!("{first_origin}{CAPABILITIES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let second_bootstrap: HostBootstrap = client
            .get(format!("{second_origin}{CAPABILITIES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut first_snapshot: HostPreferences = client
            .get(format!("{first_origin}{PREFERENCES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut second_snapshot: HostPreferences = client
            .get(format!("{second_origin}{PREFERENCES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        first_snapshot.appearance.theme = "light".into();
        let first_response = client
            .put(format!("{first_origin}{PREFERENCES_PATH}"))
            .header(ORIGIN, &first_origin)
            .header(CSRF_HEADER, first_bootstrap.csrf_token)
            .json(&first_snapshot)
            .send()
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);

        second_snapshot.navigation_width = 318.0;
        // This edit is queued locally before the first response arrives, so
        // it still lacks the other process's theme while including the first
        // optimistic edit.
        let mut queued_second_snapshot = second_snapshot.clone();
        queued_second_snapshot.inspection_width = 512.0;
        let second_response = client
            .put(format!("{second_origin}{PREFERENCES_PATH}"))
            .header(ORIGIN, &second_origin)
            .header(CSRF_HEADER, &second_bootstrap.csrf_token)
            .json(&second_snapshot)
            .send()
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::OK);
        let merged: HostPreferences = second_response.json().await.unwrap();
        assert_eq!(merged.appearance.theme, "light");
        assert_eq!(merged.navigation_width, 318.0);

        // HostClient rebases queued intent onto this merged response before
        // sending it. Mirror that wire contract here; raw stale snapshots are
        // indistinguishable from intentional reversions at the gateway.
        queued_second_snapshot.appearance = merged.appearance.clone();
        queued_second_snapshot.resume = merged.resume.clone();
        let queued_response = client
            .put(format!("{second_origin}{PREFERENCES_PATH}"))
            .header(ORIGIN, &second_origin)
            .header(CSRF_HEADER, &second_bootstrap.csrf_token)
            .json(&queued_second_snapshot)
            .send()
            .await
            .unwrap();
        assert_eq!(queued_response.status(), StatusCode::OK);
        let merged: HostPreferences = queued_response.json().await.unwrap();
        assert_eq!(merged.appearance.theme, "light");
        assert_eq!(merged.navigation_width, 318.0);
        assert_eq!(merged.inspection_width, 512.0);
        assert_eq!(
            load_preferences(&path, HostPreferences::default()).unwrap(),
            merged
        );

        first_task.abort();
        second_task.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn native_geometry_patch_preserves_external_web_preferences() {
        let path = temporary_preference_path("cross-process-geometry");
        let baseline = HostPreferences::default();
        persist_preferences(&path, &baseline).unwrap();
        let mut web = baseline.clone();
        web.appearance.theme = "light".into();
        merge_and_persist_preferences(&path, &baseline, &web, false).unwrap();

        let mut native = baseline.clone();
        native.geometry = Some(crate::WindowGeometry {
            x: 40,
            y: 60,
            width: 1200,
            height: 800,
            maximized: false,
        });
        let merged = merge_and_persist_preferences(&path, &baseline, &native, true).unwrap();
        assert_eq!(merged.appearance.theme, "light");
        assert_eq!(merged.geometry, native.geometry);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn native_geometry_updates_preserve_concurrent_frontend_preferences() {
        let path = temporary_preference_path("geometry-handle");
        let assets = AssetManifest::new([(
            "/index.html".into(),
            asset("text/html", b"<html></html>", false),
        )])
        .unwrap();
        let (address, server, preferences) =
            HostGateway::bind_loopback_with_actions_and_preferences(
                "127.0.0.1:0".parse().unwrap(),
                assets,
                HostCapabilities::desktop(),
                HostPreferences::default(),
                None,
                Some(path.clone()),
                HostNativeActions::default().with_window_geometry(),
            )
            .await
            .unwrap();
        let task = tokio::spawn(server);
        let client = reqwest::Client::new();
        let origin = format!("http://{address}");
        let bootstrap: HostBootstrap = client
            .get(format!("{origin}{CAPABILITIES_PATH}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(bootstrap.capabilities.window_geometry);

        let mut frontend_update = preferences.snapshot().await;
        frontend_update.appearance.theme = "light".into();
        frontend_update.navigation_width = 318.0;
        let geometry = crate::WindowGeometry {
            x: -120,
            y: 80,
            width: 1_420,
            height: 880,
            maximized: true,
        };
        preferences
            .update_window_geometry(geometry.clone())
            .await
            .unwrap();

        // The web request was constructed before the native resize completed.
        // Its stale `geometry: null` must not overwrite the native-owned value.
        let response = client
            .put(format!("{origin}{PREFERENCES_PATH}"))
            .header(ORIGIN, &origin)
            .header(CSRF_HEADER, bootstrap.csrf_token)
            .json(&frontend_update)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let returned: HostPreferences = response.json().await.unwrap();
        assert_eq!(returned.geometry, Some(geometry.clone()));
        let stored = load_preferences(&path, HostPreferences::default()).unwrap();
        assert_eq!(stored.geometry, Some(geometry));
        assert_eq!(stored.appearance.theme, "light");
        assert_eq!(stored.navigation_width, 318.0);

        task.abort();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn window_geometry_bounds_include_position_and_size() {
        let mut preferences = HostPreferences {
            geometry: Some(crate::WindowGeometry {
                x: -16_384,
                y: 16_384,
                width: 320,
                height: 240,
                maximized: false,
            }),
            ..HostPreferences::default()
        };
        assert!(validate_preferences(&preferences).is_ok());

        preferences.geometry.as_mut().unwrap().x = -16_385;
        assert!(validate_preferences(&preferences).is_err());
        preferences.geometry.as_mut().unwrap().x = 0;
        preferences.geometry.as_mut().unwrap().width = 16_385;
        assert!(validate_preferences(&preferences).is_err());
    }

    #[test]
    fn old_host_preferences_receive_new_field_defaults() {
        let preferences: HostPreferences = serde_json::from_value(serde_json::json!({
            "geometry": null,
            "appearance": {
                "theme": "dark",
                "font_family": "",
                "font_size": 13,
                "reduce_motion": false
            },
            "chat": {
                "collapse_thinking_with_tools": true,
                "collapse_compaction_with_tools": true
            },
            "navigation_width": 260.0,
            "inspection_width": 460.0
        }))
        .unwrap();
        assert_eq!(preferences.general, crate::GeneralPreferences::default());
        assert!(preferences.chat.collapse_sequential_tool_calls);
        assert!(preferences.chat.collapse_thinking_with_tools);
        assert!(preferences.chat.collapse_compaction_with_tools);
        assert!(!preferences.chat.collapse_todo_updates_with_tools);
        assert_eq!(
            preferences.notifications,
            crate::NotificationPreferences::default()
        );
        assert!(preferences.workspace_order.is_empty());
        assert!(preferences.pull_request_group_order.is_empty());
        assert_eq!(preferences.resume, crate::ResumePreferences::default());
    }

    #[test]
    fn pull_request_group_order_is_bounded_and_unique() {
        let mut preferences = HostPreferences {
            pull_request_group_order: vec![
                "ready-to-merge".into(),
                "drafts".into(),
                "needs-attention".into(),
            ],
            ..HostPreferences::default()
        };
        assert!(validate_preferences(&preferences).is_ok());

        preferences.pull_request_group_order.push("drafts".into());
        assert!(validate_preferences(&preferences).is_err());
        preferences.pull_request_group_order.pop();
        preferences.pull_request_group_order[0] = "Invalid Group".into();
        assert!(validate_preferences(&preferences).is_err());
    }

    #[test]
    fn resume_preferences_are_bounded_and_validate_stable_anchors() {
        let mut preferences = HostPreferences::default();
        preferences.resume.selected_session_id = "se-1".into();
        preferences
            .resume
            .session_threads
            .insert("se-1".into(), "th-1".into());
        preferences.resume.thread_scroll.insert(
            "th-1".into(),
            crate::ChatScrollBookmark {
                item_id: "assistant:42".into(),
                offset: 18.5,
            },
        );
        preferences.resume.closed_thread_tabs.push("th-2".into());
        preferences.resume.pinned_thread_tabs.push("th-3".into());
        assert!(validate_preferences(&preferences).is_ok());

        preferences
            .resume
            .thread_scroll
            .get_mut("th-1")
            .unwrap()
            .offset = f32::INFINITY;
        assert!(validate_preferences(&preferences).is_err());
        preferences
            .resume
            .thread_scroll
            .get_mut("th-1")
            .unwrap()
            .offset = 0.0;
        preferences
            .resume
            .thread_scroll
            .get_mut("th-1")
            .unwrap()
            .item_id = "bad\nitem".into();
        assert!(validate_preferences(&preferences).is_err());
        preferences
            .resume
            .thread_scroll
            .get_mut("th-1")
            .unwrap()
            .item_id = "assistant:42".into();
        preferences.resume.closed_thread_tabs.push("th-2".into());
        assert!(validate_preferences(&preferences).is_err());
        preferences.resume.closed_thread_tabs.pop();
        preferences.resume.pinned_thread_tabs.push("th-3".into());
        assert!(validate_preferences(&preferences).is_err());
    }

    #[tokio::test]
    async fn invalid_stored_preferences_fail_closed() {
        let path = temporary_preference_path("invalid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"appearance":{"theme":"secret"}}"#).unwrap();
        let assets = AssetManifest::new([(
            "/index.html".into(),
            asset("text/html", b"<html></html>", false),
        )])
        .unwrap();
        let result = HostGateway::bind_loopback(
            "127.0.0.1:0".parse().unwrap(),
            assets,
            HostCapabilities::desktop(),
            HostPreferences::default(),
            None,
            Some(path.clone()),
        )
        .await;
        assert!(matches!(
            result,
            Err(HostGatewayBindError::Gateway(
                HostGatewayError::InvalidStoredPreferences
            ))
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
