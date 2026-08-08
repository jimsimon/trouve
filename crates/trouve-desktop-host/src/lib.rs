//! Engine-neutral boundary for Trouve's desktop web frontend.
//!
//! This crate deliberately contains no agent, protocol, or durable session
//! state. It defines the native capabilities a webview may request and the
//! validation primitives used before a future Servo or system-webview adapter
//! reaches the operating system. See ADR 0023.

mod gateway;

pub use gateway::{
    AttachmentPayload, CSRF_HEADER, CloseDecisionRequest, HOST_API_PREFIX, HostBootstrap,
    HostGateway, HostGatewayBindError, HostGatewayError, HostLifecycleBatch, HostPreferencesHandle,
    LocalFileActionRequest, NativeNotificationRequest, OpenHttpsUrlRequest, PickDirectoryResponse,
    PickFilesResponse, ReadClipboardImageResponse, SleepInhibitionRequest, host_openapi_json,
};

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

/// Version of the separately schema-tested native bridge.
///
/// This is not the Trouve HTTP protocol version. Increment it only when the
/// native capability request/response schema changes.
pub const DESKTOP_BRIDGE_VERSION: u16 = 10;

/// Runtime desktop build selected by development and qualification hosts.
pub const APP_UI_DIST_ENV: &str = "TROUVE_APP_UI_DIST";
/// Loopback Vite server selected by development and qualification hosts.
pub const APP_UI_DEV_URL_ENV: &str = "TROUVE_APP_UI_DEV_URL";

/// Maximum number of files a native picker may return for one composer action.
pub const MAX_NATIVE_ATTACHMENTS: usize = 4;
/// Maximum decoded bytes in one native attachment.
pub const MAX_NATIVE_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
/// Maximum decoded bytes returned by one native file-picker action.
pub const MAX_NATIVE_ATTACHMENT_TOTAL_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const MAX_NATIVE_ATTACHMENT_NAME_BYTES: usize = 1_024;
pub(crate) const MAX_NATIVE_ATTACHMENT_MIME_BYTES: usize = 255;
pub(crate) const MAX_SYSTEM_FONT_FAMILIES: usize = 4_096;
const MAX_SYSTEM_FONT_FAMILY_LENGTH: usize = 256;

/// Installed UI font families exposed through the read-only host bootstrap.
///
/// Font discovery is process-wide and lazy: parsing every installed font is
/// useful once at host startup, but repeating it for each gateway or test
/// would add avoidable latency. `fontdb` keeps this portable across Linux,
/// macOS, and Windows without exposing a process-spawning capability to the
/// web frontend.
pub(crate) fn system_font_families() -> Arc<[String]> {
    static SYSTEM_FONT_FAMILIES: OnceLock<Arc<[String]>> = OnceLock::new();
    Arc::clone(SYSTEM_FONT_FAMILIES.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let names = database.faces().filter_map(|face| {
            face.families
                .first()
                .map(|(family, _language)| family.clone())
        });
        Arc::from(normalize_system_font_families(names))
    }))
}

fn normalize_system_font_families(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names = names
        .into_iter()
        .filter_map(|name| {
            let name = name.trim();
            let safe = !name.is_empty()
                && !name.starts_with('.')
                && name.len() <= MAX_SYSTEM_FONT_FAMILY_LENGTH
                && name.chars().all(|character| {
                    !character.is_control() && !matches!(character, ';' | '{' | '}')
                });
            safe.then(|| (name.to_lowercase(), name.to_owned()))
        })
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup_by(|left, right| left.0 == right.0);
    names.truncate(MAX_SYSTEM_FONT_FAMILIES);
    names.into_iter().map(|(_key, name)| name).collect()
}

/// Where the shared Lit application is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Desktop,
    Pwa,
    Browser,
}

/// Capabilities are explicit so the PWA never pretends to have native access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostCapabilities {
    pub kind: HostKind,
    pub bridge_version: Option<u16>,
    pub directory_picker: bool,
    pub file_picker: bool,
    pub clipboard_image: bool,
    pub lifecycle_events: bool,
    pub close_confirmation: bool,
    pub open_local_file: bool,
    pub reveal_local_file: bool,
    pub open_https_url: bool,
    pub native_notifications: bool,
    pub web_notifications: bool,
    pub user_attention: bool,
    pub sleep_inhibition: bool,
    pub window_geometry: bool,
    pub visibility: bool,
    pub occlusion: bool,
    pub persistent_preferences: bool,
    pub installable: bool,
}

impl HostCapabilities {
    /// Conservative PWA capabilities before browser permission checks.
    pub fn pwa() -> Self {
        Self {
            kind: HostKind::Pwa,
            bridge_version: None,
            directory_picker: false,
            file_picker: true,
            clipboard_image: false,
            lifecycle_events: false,
            close_confirmation: false,
            open_local_file: false,
            reveal_local_file: false,
            open_https_url: true,
            native_notifications: false,
            web_notifications: false,
            user_attention: false,
            sleep_inhibition: false,
            window_geometry: false,
            visibility: true,
            occlusion: false,
            persistent_preferences: false,
            installable: true,
        }
    }

    /// Conservative desktop preview capabilities before an OS adapter has
    /// been qualified and attached. Never advertise a native action merely
    /// because the eventual bridge schema intends to support it.
    pub fn desktop() -> Self {
        Self {
            kind: HostKind::Desktop,
            bridge_version: Some(DESKTOP_BRIDGE_VERSION),
            directory_picker: false,
            file_picker: false,
            clipboard_image: false,
            lifecycle_events: false,
            close_confirmation: false,
            open_local_file: false,
            reveal_local_file: false,
            open_https_url: false,
            native_notifications: false,
            web_notifications: false,
            user_attention: false,
            sleep_inhibition: false,
            window_geometry: false,
            visibility: false,
            occlusion: false,
            persistent_preferences: false,
            installable: false,
        }
    }
}

/// Nonsecret settings that the native host may persist across ephemeral ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostPreferences {
    pub geometry: Option<WindowGeometry>,
    pub appearance: AppearancePreferences,
    #[serde(default)]
    pub general: GeneralPreferences,
    #[serde(default)]
    pub chat: ChatPreferences,
    #[serde(default)]
    pub notifications: NotificationPreferences,
    #[serde(default)]
    pub workspace_order: Vec<String>,
    #[serde(default)]
    pub pull_request_group_order: Vec<String>,
    #[serde(default)]
    pub resume: ResumePreferences,
    pub navigation_width: f32,
    pub inspection_width: f32,
}

impl Default for HostPreferences {
    fn default() -> Self {
        Self {
            geometry: None,
            appearance: AppearancePreferences::default(),
            general: GeneralPreferences::default(),
            chat: ChatPreferences::default(),
            notifications: NotificationPreferences::default(),
            workspace_order: Vec::new(),
            pull_request_group_order: Vec::new(),
            resume: ResumePreferences::default(),
            navigation_width: 260.0,
            inspection_width: 460.0,
        }
    }
}

/// Client-owned navigation and row-anchored chat position restored when the
/// shared frontend starts again. This is presentation state only: it never
/// carries durable harness state around the HTTP protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, Default)]
pub struct ResumePreferences {
    #[serde(default)]
    pub selected_session_id: String,
    #[serde(default)]
    pub session_threads: BTreeMap<String, String>,
    #[serde(default)]
    pub thread_scroll: BTreeMap<String, ChatScrollBookmark>,
}

/// Stable first-visible chat item plus its non-negative offset into the item.
/// Unlike raw scroll pixels, this survives font and window-size changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChatScrollBookmark {
    pub item_id: String,
    pub offset: f32,
}

/// General desktop-only behavior persisted by the stable native host rather
/// than by an ephemeral loopback browser origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GeneralPreferences {
    #[serde(default = "default_true")]
    pub prevent_sleep_while_running: bool,
}

impl Default for GeneralPreferences {
    fn default() -> Self {
        Self {
            prevent_sleep_while_running: true,
        }
    }
}

/// Chat transcript presentation remains client-owned and does not affect the
/// durable thread view shared through the harness protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChatPreferences {
    /// Summarize consecutive tool calls inside collapsible activity groups.
    #[serde(default = "default_true")]
    pub collapse_sequential_tool_calls: bool,
    /// Include thinking output in collapsible tool-activity groups.
    #[serde(default)]
    pub collapse_thinking_with_tools: bool,
    /// Include context-compaction boundaries in collapsible tool-activity groups.
    #[serde(default)]
    pub collapse_compaction_with_tools: bool,
}

impl Default for ChatPreferences {
    fn default() -> Self {
        Self {
            collapse_sequential_tool_calls: true,
            collapse_thinking_with_tools: false,
            collapse_compaction_with_tools: false,
        }
    }
}

/// Notification policy remains frontend-owned. The native host persists the
/// choices and delivers only already-gated notification requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotificationPreferences {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub on_finish: bool,
    #[serde(default = "default_true")]
    pub on_fail: bool,
    #[serde(default = "default_true")]
    pub on_attention: bool,
    #[serde(default)]
    pub sound: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            on_finish: true,
            on_fail: true,
            on_attention: true,
            sound: false,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppearancePreferences {
    pub theme: String,
    pub font_family: String,
    pub font_size: u16,
    pub reduce_motion: bool,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            font_family: String::new(),
            font_size: 13,
            reduce_motion: false,
        }
    }
}

/// Current frontend-visible native window state. This is ephemeral host state,
/// not agent or session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostLifecycleState {
    pub focused: bool,
    pub visible: bool,
    pub occluded: bool,
    pub pending_close: Option<PendingCloseRequest>,
}

impl Default for HostLifecycleState {
    fn default() -> Self {
        Self {
            focused: false,
            visible: true,
            occluded: false,
            pending_close: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingCloseRequest {
    pub request_id: u64,
    /// True only after the frontend chose “quit when idle”. The host never
    /// decides whether the application is idle; the protocol-backed frontend
    /// must later confirm `quit_now` for this same request.
    pub waiting_for_idle: bool,
}

/// Ephemeral events emitted by the desktop host to its webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostLifecycleEvent {
    FocusChanged {
        focused: bool,
    },
    VisibilityChanged {
        visible: bool,
    },
    OcclusionChanged {
        occluded: bool,
    },
    CloseRequested {
        request_id: u64,
    },
    NotificationActivated {
        notification_id: String,
        session_id: String,
        thread_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HostLifecycleEnvelope {
    pub cursor: u64,
    pub event: HostLifecycleEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloseDecision {
    Cancel,
    QuitNow,
    QuitWhenIdle,
}

#[derive(Debug)]
struct LifecycleFeed {
    cursor: u64,
    next_close_request: u64,
    state: HostLifecycleState,
    events: VecDeque<HostLifecycleEnvelope>,
}

impl Default for LifecycleFeed {
    fn default() -> Self {
        Self {
            cursor: 0,
            next_close_request: 1,
            state: HostLifecycleState::default(),
            events: VecDeque::new(),
        }
    }
}

#[derive(Debug)]
struct LifecycleInner {
    feed: Mutex<LifecycleFeed>,
    cursor: tokio::sync::watch::Sender<u64>,
}

/// Thread-safe publisher retained by the native event loop and read through
/// the loopback gateway. Only bounded, ephemeral window events are retained.
#[derive(Debug, Clone)]
pub struct HostLifecycleHandle(Arc<LifecycleInner>);

impl Default for HostLifecycleHandle {
    fn default() -> Self {
        let (cursor, _) = tokio::sync::watch::channel(0);
        Self(Arc::new(LifecycleInner {
            feed: Mutex::new(LifecycleFeed::default()),
            cursor,
        }))
    }
}

impl HostLifecycleHandle {
    pub fn set_focused(&self, focused: bool) {
        self.update_state(
            |state| {
                if state.focused == focused {
                    return false;
                }
                state.focused = focused;
                true
            },
            HostLifecycleEvent::FocusChanged { focused },
        );
    }

    pub fn set_visible(&self, visible: bool) {
        self.update_state(
            |state| {
                if state.visible == visible {
                    return false;
                }
                state.visible = visible;
                true
            },
            HostLifecycleEvent::VisibilityChanged { visible },
        );
    }

    pub fn set_occluded(&self, occluded: bool) {
        self.update_state(
            |state| {
                if state.occluded == occluded {
                    return false;
                }
                state.occluded = occluded;
                true
            },
            HostLifecycleEvent::OcclusionChanged { occluded },
        );
    }

    /// Intercept one native close request. Repeated requests while the
    /// frontend dialog is open reuse the same id but republish the edge.
    pub fn request_close(&self) -> u64 {
        let mut feed = self.0.feed.lock().unwrap();
        let request_id = match feed.state.pending_close.as_ref() {
            Some(pending) => pending.request_id,
            None => {
                let request_id = feed.next_close_request;
                feed.next_close_request = feed.next_close_request.saturating_add(1);
                feed.state.pending_close = Some(PendingCloseRequest {
                    request_id,
                    waiting_for_idle: false,
                });
                request_id
            }
        };
        Self::push_event(
            &self.0,
            &mut feed,
            HostLifecycleEvent::CloseRequested { request_id },
        );
        request_id
    }

    pub fn notification_activated(&self, notification: &NativeNotification) {
        let mut feed = self.0.feed.lock().unwrap();
        Self::push_event(
            &self.0,
            &mut feed,
            HostLifecycleEvent::NotificationActivated {
                notification_id: notification.id.clone(),
                session_id: notification.session_id.clone(),
                thread_id: notification.thread_id.clone(),
            },
        );
    }

    pub(crate) fn apply_close_decision(
        &self,
        request_id: u64,
        decision: CloseDecision,
    ) -> Result<bool, HostValidationError> {
        let mut feed = self.0.feed.lock().unwrap();
        let Some(pending) = feed.state.pending_close.as_mut() else {
            return Err(HostValidationError::InvalidLifecycle(
                "there is no pending close request".into(),
            ));
        };
        if pending.request_id != request_id {
            return Err(HostValidationError::InvalidLifecycle(
                "close request id is stale".into(),
            ));
        }
        match decision {
            CloseDecision::Cancel => feed.state.pending_close = None,
            CloseDecision::QuitWhenIdle => pending.waiting_for_idle = true,
            CloseDecision::QuitNow => feed.state.pending_close = None,
        }
        Ok(decision == CloseDecision::QuitNow)
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0.cursor.subscribe()
    }

    pub(crate) fn batch_after(
        &self,
        after: u64,
    ) -> (u64, HostLifecycleState, Vec<HostLifecycleEnvelope>) {
        let feed = self.0.feed.lock().unwrap();
        (
            feed.cursor,
            feed.state.clone(),
            feed.events
                .iter()
                .filter(|envelope| envelope.cursor > after)
                .cloned()
                .collect(),
        )
    }

    fn update_state(
        &self,
        update: impl FnOnce(&mut HostLifecycleState) -> bool,
        event: HostLifecycleEvent,
    ) {
        let mut feed = self.0.feed.lock().unwrap();
        if update(&mut feed.state) {
            Self::push_event(&self.0, &mut feed, event);
        }
    }

    fn push_event(inner: &LifecycleInner, feed: &mut LifecycleFeed, event: HostLifecycleEvent) {
        const MAX_RETAINED_EVENTS: usize = 128;
        feed.cursor = feed.cursor.saturating_add(1);
        let cursor = feed.cursor;
        feed.events
            .push_back(HostLifecycleEnvelope { cursor, event });
        while feed.events.len() > MAX_RETAINED_EVENTS {
            feed.events.pop_front();
        }
        inner.cursor.send_replace(cursor);
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HostValidationError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("only HTTPS external URLs are allowed")]
    NonHttpsUrl,
    #[error("external URLs may not contain credentials")]
    UrlCredentials,
    #[error("URL has no host")]
    MissingHost,
    #[error("request origin does not match the desktop gateway")]
    OriginMismatch,
    #[error("request Host does not match the desktop gateway")]
    HostMismatch,
    #[error("desktop gateway must use a loopback host")]
    NonLoopbackGateway,
    #[error("unsafe asset path: {0}")]
    UnsafeAssetPath(String),
    #[error("invalid desktop frontend asset directory: {0}")]
    InvalidAssetDirectory(String),
    #[error("asset manifest has no /index.html fallback")]
    MissingIndex,
    #[error("invalid host preferences: {0}")]
    InvalidPreferences(String),
    #[error("invalid native attachment: {0}")]
    InvalidAttachment(String),
    #[error("invalid native lifecycle action: {0}")]
    InvalidLifecycle(String),
    #[error("invalid native notification: {0}")]
    InvalidNotification(String),
    #[error("invalid session-local file: {0}")]
    InvalidSessionFile(String),
}

/// A validated external navigation target. Only HTTPS is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalHttpsUrl(Url);

impl ExternalHttpsUrl {
    pub fn parse(value: &str) -> Result<Self, HostValidationError> {
        if value.chars().any(char::is_control) {
            return Err(HostValidationError::InvalidUrl(
                "external URL contains control characters".into(),
            ));
        }
        let url = Url::parse(value)
            .map_err(|error| HostValidationError::InvalidUrl(error.to_string()))?;
        if url.scheme() != "https" {
            return Err(HostValidationError::NonHttpsUrl);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(HostValidationError::UrlCredentials);
        }
        if url.host_str().is_none() {
            return Err(HostValidationError::MissingHost);
        }
        Ok(Self(url))
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }
}

type ExternalHttpsOpener = dyn Fn(&ExternalHttpsUrl) -> Result<(), String> + Send + Sync + 'static;
type DirectoryPickerFuture =
    Pin<Box<dyn Future<Output = Result<Option<PathBuf>, String>> + Send + 'static>>;
type DirectoryPicker = dyn Fn() -> DirectoryPickerFuture + Send + Sync + 'static;
type FilePickerFuture =
    Pin<Box<dyn Future<Output = Result<Option<Vec<NativeAttachment>>, String>> + Send + 'static>>;
type FilePicker = dyn Fn() -> FilePickerFuture + Send + Sync + 'static;
type ClipboardImageFuture =
    Pin<Box<dyn Future<Output = Result<Option<NativeAttachment>, String>> + Send + 'static>>;
type ClipboardImageReader = dyn Fn() -> ClipboardImageFuture + Send + Sync + 'static;
type QuitHandler = dyn Fn() -> Result<(), String> + Send + Sync + 'static;
type SleepInhibitor = dyn Fn(bool) -> Result<(), String> + Send + Sync + 'static;
type NativeNotificationSender =
    dyn Fn(NativeNotification) -> Result<(), String> + Send + Sync + 'static;
type UserAttentionRequester = dyn Fn() -> Result<(), String> + Send + Sync + 'static;
type SessionFileResolverFuture =
    Pin<Box<dyn Future<Output = Result<VerifiedSessionFile, String>> + Send + 'static>>;
type SessionFileResolver =
    dyn Fn(String, String) -> SessionFileResolverFuture + Send + Sync + 'static;
type LocalFileHandler =
    dyn Fn(&VerifiedSessionFile, LocalFileAction) -> Result<(), String> + Send + Sync + 'static;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LocalFileAction {
    Open,
    Reveal,
}

/// Existing regular file resolved beneath the canonical worktree root. The
/// absolute path is available only to a trusted native adapter and is never
/// serialized into a bridge response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSessionFile {
    canonical_path: PathBuf,
    relative_path: String,
}

/// Whether a client-supplied path is a bounded, platform-neutral relative
/// file path. The gateway and native resolver share this predicate so their
/// validation policies cannot drift.
pub fn valid_session_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 32 * 1024
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

impl VerifiedSessionFile {
    pub fn resolve(
        worktree: impl AsRef<Path>,
        relative_path: impl Into<String>,
    ) -> Result<Self, HostValidationError> {
        let relative_path = relative_path.into();
        if !valid_session_relative_path(&relative_path) {
            return Err(HostValidationError::InvalidSessionFile(
                "path must be a bounded worktree-relative file".into(),
            ));
        }
        let relative = Path::new(&relative_path);
        let root = worktree.as_ref().canonicalize().map_err(|_| {
            HostValidationError::InvalidSessionFile("session worktree is unavailable".into())
        })?;
        if !root.is_dir() {
            return Err(HostValidationError::InvalidSessionFile(
                "session worktree is unavailable".into(),
            ));
        }
        let canonical_path = root.join(relative).canonicalize().map_err(|_| {
            HostValidationError::InvalidSessionFile("session file is unavailable".into())
        })?;
        if !canonical_path.starts_with(&root)
            || !canonical_path
                .metadata()
                .is_ok_and(|metadata| metadata.is_file())
        {
            return Err(HostValidationError::InvalidSessionFile(
                "session file escapes the active worktree or is not a regular file".into(),
            ));
        }
        Ok(Self {
            canonical_path,
            relative_path,
        })
    }

    pub fn as_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }
}

/// Already-gated notification request produced by the Lit notification
/// coordinator. No durable agent payload or arbitrary action is carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeNotification {
    id: String,
    title: String,
    body: String,
    sound: bool,
    session_id: String,
    thread_id: Option<String>,
}

impl NativeNotification {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        sound: bool,
        session_id: impl Into<String>,
        thread_id: Option<String>,
    ) -> Result<Self, HostValidationError> {
        let notification = Self {
            id: id.into(),
            title: title.into(),
            body: body.into(),
            sound,
            session_id: session_id.into(),
            thread_id,
        };
        validate_native_notification(&notification)?;
        Ok(notification)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn sound(&self) -> bool {
        self.sound
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }
}

fn validate_native_notification(
    notification: &NativeNotification,
) -> Result<(), HostValidationError> {
    let valid_id = |value: &str, allow_empty: bool| {
        (allow_empty || !value.is_empty())
            && value.len() <= 256
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let valid_text = |value: &str, max: usize, allow_empty: bool| {
        (allow_empty || !value.trim().is_empty())
            && value.len() <= max
            && value
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
    };
    if !valid_id(&notification.id, false)
        || !valid_text(&notification.title, 256, false)
        || !valid_text(&notification.body, 4 * 1024, true)
        || !valid_id(&notification.session_id, false)
        || notification
            .thread_id
            .as_deref()
            .is_some_and(|thread_id| !valid_id(thread_id, false))
    {
        return Err(HostValidationError::InvalidNotification(
            "notification is outside the native host bounds".into(),
        ));
    }
    Ok(())
}

/// Attachment data produced by an app-owned native adapter.
///
/// Paths and file handles never cross the desktop-host boundary. The adapter
/// reads only the files explicitly returned by the operating-system picker,
/// then hands this bounded data to the gateway for a second validation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAttachment {
    pub(crate) name: String,
    pub(crate) mime: String,
    pub(crate) bytes: Vec<u8>,
}

impl NativeAttachment {
    pub fn new(
        name: impl Into<String>,
        mime: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Self, HostValidationError> {
        let attachment = Self {
            name: name.into(),
            mime: mime.into(),
            bytes,
        };
        validate_native_attachment(&attachment)?;
        Ok(attachment)
    }
}

pub(crate) fn validate_native_attachment(
    attachment: &NativeAttachment,
) -> Result<(), HostValidationError> {
    let name = attachment.name.as_str();
    if name.trim().is_empty()
        || matches!(name, "." | "..")
        || name.len() > MAX_NATIVE_ATTACHMENT_NAME_BYTES
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\'])
    {
        return Err(HostValidationError::InvalidAttachment(
            "display name is outside the native host bounds".into(),
        ));
    }
    let mime = attachment.mime.as_str();
    let valid_mime = mime.len() <= MAX_NATIVE_ATTACHMENT_MIME_BYTES
        && mime.split_once('/').is_some_and(|(kind, subtype)| {
            !kind.is_empty()
                && !subtype.is_empty()
                && !subtype.contains('/')
                && mime.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(
                            byte,
                            b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-' | b'/'
                        )
                })
        });
    if !valid_mime {
        return Err(HostValidationError::InvalidAttachment(
            "MIME type is outside the native host bounds".into(),
        ));
    }
    if attachment.bytes.is_empty() || attachment.bytes.len() > MAX_NATIVE_ATTACHMENT_BYTES {
        return Err(HostValidationError::InvalidAttachment(
            "byte length is outside the native host bounds".into(),
        ));
    }
    Ok(())
}

/// App-owned adapters for the narrow native capability boundary.
///
/// Empty by default so constructing a gateway never grants OS access. Each
/// builder method attaches one typed operation; the gateway derives its
/// advertised desktop capabilities from the attached adapters.
#[derive(Clone, Default)]
pub struct HostNativeActions {
    lifecycle: Option<HostLifecycleHandle>,
    lifecycle_visibility: bool,
    lifecycle_occlusion: bool,
    directory_picker: Option<Arc<DirectoryPicker>>,
    file_picker: Option<Arc<FilePicker>>,
    clipboard_image_reader: Option<Arc<ClipboardImageReader>>,
    external_https_opener: Option<Arc<ExternalHttpsOpener>>,
    quit_handler: Option<Arc<QuitHandler>>,
    sleep_inhibitor: Option<Arc<SleepInhibitor>>,
    native_notification_sender: Option<Arc<NativeNotificationSender>>,
    user_attention_requester: Option<Arc<UserAttentionRequester>>,
    session_file_resolver: Option<Arc<SessionFileResolver>>,
    local_file_handler: Option<Arc<LocalFileHandler>>,
    window_geometry: bool,
}

impl HostNativeActions {
    /// Declare that the embedding window restores and persists geometry
    /// through the shared host-preferences handle. This grants no callable
    /// web operation; it makes the existing read-only capability truthful.
    pub fn with_window_geometry(mut self) -> Self {
        self.window_geometry = true;
        self
    }

    pub fn with_lifecycle(mut self, lifecycle: HostLifecycleHandle) -> Self {
        self.lifecycle_visibility = true;
        self.lifecycle_occlusion = true;
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Attach lifecycle publishing with explicit engine/platform support.
    pub fn with_lifecycle_capabilities(
        mut self,
        lifecycle: HostLifecycleHandle,
        visibility: bool,
        occlusion: bool,
    ) -> Self {
        self.lifecycle = Some(lifecycle);
        self.lifecycle_visibility = visibility;
        self.lifecycle_occlusion = occlusion;
        self
    }

    /// Attach a native directory picker.
    ///
    /// The returned future may remain pending for as long as the operating
    /// system dialog is open. Callers must not emulate this contract with a
    /// blocking callback on the gateway runtime.
    pub fn with_directory_picker<F, Fut>(mut self, picker: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<PathBuf>, String>> + Send + 'static,
    {
        self.directory_picker = Some(Arc::new(move || Box::pin(picker())));
        self
    }

    pub fn with_external_https_opener<F>(mut self, opener: F) -> Self
    where
        F: Fn(&ExternalHttpsUrl) -> Result<(), String> + Send + Sync + 'static,
    {
        self.external_https_opener = Some(Arc::new(opener));
        self
    }

    pub fn with_quit_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn() -> Result<(), String> + Send + Sync + 'static,
    {
        self.quit_handler = Some(Arc::new(handler));
        self
    }

    pub fn with_sleep_inhibitor<F>(mut self, inhibitor: F) -> Self
    where
        F: Fn(bool) -> Result<(), String> + Send + Sync + 'static,
    {
        self.sleep_inhibitor = Some(Arc::new(inhibitor));
        self
    }

    pub fn with_native_notification_sender<F>(mut self, sender: F) -> Self
    where
        F: Fn(NativeNotification) -> Result<(), String> + Send + Sync + 'static,
    {
        self.native_notification_sender = Some(Arc::new(sender));
        self
    }

    pub fn with_user_attention_requester<F>(mut self, requester: F) -> Self
    where
        F: Fn() -> Result<(), String> + Send + Sync + 'static,
    {
        self.user_attention_requester = Some(Arc::new(requester));
        self
    }

    pub fn with_session_file_resolver<F, Fut>(mut self, resolver: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<VerifiedSessionFile, String>> + Send + 'static,
    {
        self.session_file_resolver = Some(Arc::new(move |session_id, relative_path| {
            Box::pin(resolver(session_id, relative_path))
        }));
        self
    }

    pub fn with_local_file_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&VerifiedSessionFile, LocalFileAction) -> Result<(), String> + Send + Sync + 'static,
    {
        self.local_file_handler = Some(Arc::new(handler));
        self
    }

    /// Attach a native multi-file picker. Cancellation is `Ok(None)`.
    pub fn with_file_picker<F, Fut>(mut self, picker: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Vec<NativeAttachment>>, String>> + Send + 'static,
    {
        self.file_picker = Some(Arc::new(move || Box::pin(picker())));
        self
    }

    /// Attach a text-aware native clipboard image reader.
    ///
    /// Adapters return `Ok(None)` when text is available or the clipboard has
    /// no image, preserving the browser's ordinary text-paste behavior.
    pub fn with_clipboard_image_reader<F, Fut>(mut self, reader: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<NativeAttachment>, String>> + Send + 'static,
    {
        self.clipboard_image_reader = Some(Arc::new(move || Box::pin(reader())));
        self
    }

    pub fn can_open_https_url(&self) -> bool {
        self.external_https_opener.is_some()
    }

    pub fn can_stream_lifecycle(&self) -> bool {
        self.lifecycle.is_some()
    }

    pub fn can_manage_window_geometry(&self) -> bool {
        self.window_geometry
    }

    pub fn can_report_visibility(&self) -> bool {
        self.lifecycle.is_some() && self.lifecycle_visibility
    }

    pub fn can_report_occlusion(&self) -> bool {
        self.lifecycle.is_some() && self.lifecycle_occlusion
    }

    pub fn can_confirm_close(&self) -> bool {
        self.lifecycle.is_some() && self.quit_handler.is_some()
    }

    pub fn can_inhibit_sleep(&self) -> bool {
        self.sleep_inhibitor.is_some()
    }

    pub fn can_send_native_notifications(&self) -> bool {
        self.native_notification_sender.is_some()
    }

    pub fn can_request_user_attention(&self) -> bool {
        self.user_attention_requester.is_some()
    }

    pub fn can_open_session_files(&self) -> bool {
        self.session_file_resolver.is_some() && self.local_file_handler.is_some()
    }

    pub fn can_pick_directory(&self) -> bool {
        self.directory_picker.is_some()
    }

    pub fn can_pick_files(&self) -> bool {
        self.file_picker.is_some()
    }

    pub fn can_read_clipboard_image(&self) -> bool {
        self.clipboard_image_reader.is_some()
    }

    pub(crate) async fn pick_directory(&self) -> Result<Option<PathBuf>, String> {
        self.directory_picker
            .as_ref()
            .ok_or_else(|| "directory picker is unavailable".to_string())?()
        .await
    }

    pub(crate) async fn pick_files(&self) -> Result<Option<Vec<NativeAttachment>>, String> {
        self.file_picker
            .as_ref()
            .ok_or_else(|| "file picker is unavailable".to_string())?()
        .await
    }

    pub(crate) async fn read_clipboard_image(&self) -> Result<Option<NativeAttachment>, String> {
        self.clipboard_image_reader
            .as_ref()
            .ok_or_else(|| "clipboard image reader is unavailable".to_string())?()
        .await
    }

    pub(crate) fn lifecycle(&self) -> Option<&HostLifecycleHandle> {
        self.lifecycle.as_ref()
    }

    pub(crate) fn quit(&self) -> Result<(), String> {
        self.quit_handler
            .as_ref()
            .ok_or_else(|| "quit handler is unavailable".to_string())?()
    }

    pub(crate) fn set_sleep_inhibition(&self, active: bool) -> Result<(), String> {
        self.sleep_inhibitor
            .as_ref()
            .ok_or_else(|| "sleep inhibition is unavailable".to_string())?(active)
    }

    pub(crate) fn send_native_notification(
        &self,
        notification: NativeNotification,
    ) -> Result<(), String> {
        self.native_notification_sender
            .as_ref()
            .ok_or_else(|| "native notifications are unavailable".to_string())?(notification)
    }

    pub(crate) fn request_user_attention(&self) -> Result<(), String> {
        self.user_attention_requester
            .as_ref()
            .ok_or_else(|| "user attention is unavailable".to_string())?()
    }

    pub(crate) async fn resolve_session_file(
        &self,
        session_id: String,
        relative_path: String,
    ) -> Result<VerifiedSessionFile, String> {
        self.session_file_resolver
            .as_ref()
            .ok_or_else(|| "session file resolver is unavailable".to_string())?(
            session_id,
            relative_path,
        )
        .await
    }

    pub(crate) fn handle_local_file(
        &self,
        file: &VerifiedSessionFile,
        action: LocalFileAction,
    ) -> Result<(), String> {
        self.local_file_handler
            .as_ref()
            .ok_or_else(|| "local file handler is unavailable".to_string())?(file, action)
    }

    pub(crate) fn open_https_url(&self, url: &ExternalHttpsUrl) -> Result<(), String> {
        self.external_https_opener
            .as_ref()
            .ok_or_else(|| "external URL opener is unavailable".to_string())?(url)
    }
}

/// Exact origin and authority expected by the loopback gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayOrigin {
    serialized: String,
    authority: String,
}

impl GatewayOrigin {
    pub fn parse(value: &str) -> Result<Self, HostValidationError> {
        let url = Url::parse(value)
            .map_err(|error| HostValidationError::InvalidUrl(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(HostValidationError::InvalidUrl(value.into()));
        }
        let is_loopback = match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !is_loopback {
            return Err(HostValidationError::NonLoopbackGateway);
        }
        let host = url.host_str().ok_or(HostValidationError::MissingHost)?;
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        Ok(Self {
            serialized: url.origin().ascii_serialization(),
            authority,
        })
    }

    pub fn validate(&self, host: &str, origin: &str) -> Result<(), HostValidationError> {
        if host != self.authority {
            return Err(HostValidationError::HostMismatch);
        }
        if origin != self.serialized {
            return Err(HostValidationError::OriginMismatch);
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.serialized
    }

    pub fn authority(&self) -> &str {
        &self.authority
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub content_type: String,
    pub bytes: Arc<[u8]>,
    pub immutable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetLookup<'a> {
    Exact(&'a Asset),
    SpaFallback(&'a Asset),
    Missing,
}

/// Fixed packaged assets. Request paths never become filesystem paths.
#[derive(Debug, Clone)]
pub struct AssetManifest {
    assets: BTreeMap<String, Asset>,
}

impl AssetManifest {
    pub fn new(
        assets: impl IntoIterator<Item = (String, Asset)>,
    ) -> Result<Self, HostValidationError> {
        let mut manifest = BTreeMap::new();
        for (path, asset) in assets {
            validate_asset_path(&path)?;
            manifest.insert(path, asset);
        }
        if !manifest.contains_key("/index.html") {
            return Err(HostValidationError::MissingIndex);
        }
        Ok(Self { assets: manifest })
    }

    /// Snapshot one Vite desktop output at process startup.
    ///
    /// This is deliberately stricter than a general static-file server: the
    /// directory must contain the desktop shell, must not contain PWA output,
    /// and nested symlinks are rejected before request paths are constructed.
    pub fn load_desktop_dist(directory: impl AsRef<Path>) -> Result<Self, HostValidationError> {
        let configured = directory.as_ref();
        let root = configured.canonicalize().map_err(|error| {
            invalid_asset_directory(format!("resolving {}: {error}", configured.display()))
        })?;
        if !root.is_dir() {
            return Err(invalid_asset_directory(format!(
                "{} is not a directory",
                root.display()
            )));
        }

        let mut files = Vec::new();
        collect_desktop_asset_files(&root, &mut files)?;
        files.sort();
        if !files.iter().any(|file| file == &root.join("index.html")) {
            return Err(HostValidationError::MissingIndex);
        }
        for forbidden in ["service-worker.js", "pwa-meta.json"] {
            if files.iter().any(|file| file == &root.join(forbidden)) {
                return Err(invalid_asset_directory(format!(
                    "{} is PWA output ({forbidden} is present)",
                    root.display()
                )));
            }
        }

        let mut assets = Vec::with_capacity(files.len());
        for file in files {
            let relative = file
                .strip_prefix(&root)
                .expect("collected desktop asset remains below its canonical root");
            let mut segments = Vec::new();
            for component in relative.components() {
                let Component::Normal(segment) = component else {
                    return Err(invalid_asset_directory(format!(
                        "unsafe path below {}: {}",
                        root.display(),
                        relative.display()
                    )));
                };
                segments.push(segment.to_str().ok_or_else(|| {
                    invalid_asset_directory(format!(
                        "asset path is not UTF-8: {}",
                        relative.display()
                    ))
                })?);
            }
            let request_path = format!("/{}", segments.join("/"));
            if request_path.starts_with("/assets/") && !has_vite_content_hash(&file) {
                return Err(invalid_asset_directory(format!(
                    "desktop asset lacks a Vite content hash: {request_path}"
                )));
            }
            let bytes = std::fs::read(&file).map_err(|error| {
                invalid_asset_directory(format!("reading {}: {error}", file.display()))
            })?;
            assets.push((
                request_path.clone(),
                Asset {
                    content_type: desktop_asset_content_type(&file).to_owned(),
                    bytes: Arc::from(bytes.into_boxed_slice()),
                    immutable: request_path.starts_with("/assets/"),
                },
            ));
        }
        Self::new(assets)
    }

    pub fn resolve(&self, path: &str) -> Result<AssetLookup<'_>, HostValidationError> {
        validate_asset_path(path)?;
        if let Some(asset) = self.assets.get(path) {
            return Ok(AssetLookup::Exact(asset));
        }
        let final_segment = path.rsplit('/').next().unwrap_or_default();
        if !final_segment.contains('.') {
            return Ok(AssetLookup::SpaFallback(
                self.assets
                    .get("/index.html")
                    .expect("constructor requires index"),
            ));
        }
        Ok(AssetLookup::Missing)
    }
}

/// Validated Vite development server. Only an explicit, credential-free
/// loopback HTTP origin is accepted; shipping hosts never select this source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendDevServer(Url);

impl FrontendDevServer {
    pub fn parse(value: &str) -> Result<Self, HostValidationError> {
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(HostValidationError::InvalidUrl(
                "frontend development URL is empty or contains control characters".into(),
            ));
        }
        let url = Url::parse(value)
            .map_err(|error| HostValidationError::InvalidUrl(error.to_string()))?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if url.scheme() != "http"
            || !loopback
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(HostValidationError::InvalidUrl(
                "frontend development server must be a credential-free loopback HTTP origin".into(),
            ));
        }
        Ok(Self(url))
    }

    pub(crate) fn url(&self) -> &Url {
        &self.0
    }

    pub(crate) fn websocket_origin(&self) -> String {
        let mut url = self.0.clone();
        url.set_scheme("ws")
            .expect("validated HTTP development URL accepts the ws scheme");
        url.origin().ascii_serialization()
    }
}

/// Frontend bytes selected for one desktop-host process.
#[derive(Debug, Clone)]
pub enum FrontendSource {
    Static(AssetManifest),
    ViteDevServer(FrontendDevServer),
}

impl From<AssetManifest> for FrontendSource {
    fn from(value: AssetManifest) -> Self {
        Self::Static(value)
    }
}

impl FrontendSource {
    /// Apply the common preview environment policy used by Wry and Servo.
    ///
    /// `allow_unbundled` is true for debug product previews and disposable
    /// qualification harnesses. Shipping product builds pass false and can
    /// therefore select only their compile-time packaged manifest.
    pub fn from_preview_environment(
        bundled: Option<AssetManifest>,
        allow_unbundled: bool,
    ) -> Result<Self, FrontendSourceError> {
        select_preview_frontend_source(
            std::env::var_os(APP_UI_DEV_URL_ENV),
            std::env::var_os(APP_UI_DIST_ENV),
            bundled,
            allow_unbundled,
        )
    }

    pub(crate) fn static_assets(&self) -> Option<&AssetManifest> {
        match self {
            Self::Static(assets) => Some(assets),
            Self::ViteDevServer(_) => None,
        }
    }

    pub(crate) fn dev_server(&self) -> Option<&FrontendDevServer> {
        match self {
            Self::Static(_) => None,
            Self::ViteDevServer(server) => Some(server),
        }
    }
}

#[derive(Debug, Error)]
pub enum FrontendSourceError {
    #[error("{APP_UI_DEV_URL_ENV} and {APP_UI_DIST_ENV} are mutually exclusive")]
    ConflictingSources,
    #[error("{APP_UI_DEV_URL_ENV} is available only to development and qualification hosts")]
    DevelopmentServerDisabled,
    #[error("{0} cannot be empty")]
    EmptyEnvironment(&'static str),
    #[error("{APP_UI_DEV_URL_ENV} must be valid UTF-8")]
    NonUtf8DevelopmentUrl,
    #[error(
        "no desktop frontend source is available; set {APP_UI_DEV_URL_ENV}, set {APP_UI_DIST_ENV}, or build a packaged frontend"
    )]
    MissingSource,
    #[error(transparent)]
    Validation(#[from] HostValidationError),
}

fn select_preview_frontend_source(
    development_url: Option<std::ffi::OsString>,
    dist: Option<std::ffi::OsString>,
    bundled: Option<AssetManifest>,
    allow_unbundled: bool,
) -> Result<FrontendSource, FrontendSourceError> {
    if development_url.is_some() && dist.is_some() {
        return Err(FrontendSourceError::ConflictingSources);
    }
    if let Some(development_url) = development_url {
        if !allow_unbundled {
            return Err(FrontendSourceError::DevelopmentServerDisabled);
        }
        if development_url.is_empty() {
            return Err(FrontendSourceError::EmptyEnvironment(APP_UI_DEV_URL_ENV));
        }
        let development_url = development_url
            .to_str()
            .ok_or(FrontendSourceError::NonUtf8DevelopmentUrl)?;
        return Ok(FrontendSource::ViteDevServer(FrontendDevServer::parse(
            development_url,
        )?));
    }
    if allow_unbundled && let Some(dist) = dist {
        if dist.is_empty() {
            return Err(FrontendSourceError::EmptyEnvironment(APP_UI_DIST_ENV));
        }
        return Ok(FrontendSource::Static(AssetManifest::load_desktop_dist(
            PathBuf::from(dist),
        )?));
    }
    bundled
        .map(FrontendSource::Static)
        .ok_or(FrontendSourceError::MissingSource)
}

fn invalid_asset_directory(detail: String) -> HostValidationError {
    HostValidationError::InvalidAssetDirectory(detail)
}

fn collect_desktop_asset_files(
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), HostValidationError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| {
            invalid_asset_directory(format!("reading {}: {error}", directory.display()))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            invalid_asset_directory(format!("enumerating {}: {error}", directory.display()))
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| {
            invalid_asset_directory(format!("reading {}: {error}", path.display()))
        })?;
        if kind.is_symlink() {
            return Err(invalid_asset_directory(format!(
                "frontend assets may not contain symlinks: {}",
                path.display()
            )));
        }
        if kind.is_dir() {
            collect_desktop_asset_files(&path, output)?;
        } else if kind.is_file() {
            output.push(path);
        }
    }
    Ok(())
}

fn desktop_asset_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map" | "webmanifest") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn has_vite_content_hash(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    let bytes = stem.as_bytes();
    bytes.len() >= 9
        && bytes[bytes.len() - 9] == b'-'
        && bytes[bytes.len() - 8..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_asset_path(path: &str) -> Result<(), HostValidationError> {
    let unsafe_path = !path.starts_with('/')
        || path.contains('\0')
        || path.contains('\\')
        || path.contains('%')
        || path.contains('?')
        || path.contains('#')
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..");
    if unsafe_path {
        return Err(HostValidationError::UnsafeAssetPath(path.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(content_type: &str) -> Asset {
        Asset {
            content_type: content_type.into(),
            bytes: Arc::from(&b"asset"[..]),
            immutable: true,
        }
    }

    #[test]
    fn capabilities_do_not_grant_native_access_to_the_pwa() {
        let pwa = HostCapabilities::pwa();
        assert_eq!(pwa.kind, HostKind::Pwa);
        assert_eq!(pwa.bridge_version, None);
        assert!(!pwa.directory_picker);
        assert!(!pwa.open_local_file);
        assert!(!pwa.sleep_inhibition);
        assert!(pwa.installable);
    }

    #[test]
    fn desktop_preview_does_not_advertise_unattached_native_actions() {
        let desktop = HostCapabilities::desktop();
        assert_eq!(desktop.kind, HostKind::Desktop);
        assert_eq!(desktop.bridge_version, Some(DESKTOP_BRIDGE_VERSION));
        assert!(!desktop.persistent_preferences);
        assert!(!desktop.visibility);
        assert!(!desktop.directory_picker);
        assert!(!desktop.file_picker);
        assert!(!desktop.clipboard_image);
        assert!(!desktop.open_local_file);
        assert!(!desktop.native_notifications);
        assert!(!desktop.sleep_inhibition);
        assert!(!desktop.window_geometry);
    }

    #[test]
    fn system_font_families_are_safe_sorted_and_deduplicated() {
        let names = normalize_system_font_families([
            "Zed Sans".to_owned(),
            " Noto Sans ".to_owned(),
            "Alpha Sans".to_owned(),
            "M+ 1m".to_owned(),
            "Font (Body)".to_owned(),
            "Noto Sans".to_owned(),
            ".Hidden Font".to_owned(),
            "Unsafe; Font".to_owned(),
            "Line\nBreak".to_owned(),
            "x".repeat(MAX_SYSTEM_FONT_FAMILY_LENGTH + 1),
        ]);
        assert_eq!(
            names,
            [
                "Alpha Sans",
                "Font (Body)",
                "M+ 1m",
                "Noto Sans",
                "Zed Sans"
            ]
        );
    }

    #[test]
    fn external_navigation_is_https_without_credentials() {
        assert!(ExternalHttpsUrl::parse("https://example.com/path?q=1").is_ok());
        assert_eq!(
            ExternalHttpsUrl::parse("http://example.com"),
            Err(HostValidationError::NonHttpsUrl)
        );
        assert_eq!(
            ExternalHttpsUrl::parse("file:///tmp/secret"),
            Err(HostValidationError::NonHttpsUrl)
        );
        assert_eq!(
            ExternalHttpsUrl::parse("https://user:secret@example.com"),
            Err(HostValidationError::UrlCredentials)
        );
        assert!(matches!(
            ExternalHttpsUrl::parse("https://example.com/\nsecret"),
            Err(HostValidationError::InvalidUrl(_))
        ));
    }

    #[test]
    fn frontend_development_servers_are_explicit_loopback_http_origins() {
        let server = FrontendDevServer::parse("http://127.0.0.1:5173").unwrap();
        assert_eq!(server.websocket_origin(), "ws://127.0.0.1:5173");
        assert!(FrontendDevServer::parse("http://localhost:5173").is_ok());
        for rejected in [
            "https://127.0.0.1:5173",
            "http://example.com:5173",
            "http://user:secret@127.0.0.1:5173",
            "http://127.0.0.1:5173/app",
            "http://127.0.0.1:5173/?token=secret",
        ] {
            assert!(
                FrontendDevServer::parse(rejected).is_err(),
                "accepted {rejected}"
            );
        }
    }

    #[test]
    fn desktop_dist_loader_snapshots_only_hashed_non_pwa_assets() {
        let root = std::env::temp_dir().join(format!(
            "trouve-desktop-assets-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join("icons")).unwrap();
        std::fs::write(root.join("index.html"), b"<html>desktop</html>").unwrap();
        std::fs::write(root.join("assets/app-12345678.js"), b"app").unwrap();
        std::fs::write(root.join("icons/trouve.svg"), b"<svg/>").unwrap();

        let manifest = AssetManifest::load_desktop_dist(&root).unwrap();
        let AssetLookup::Exact(script) = manifest.resolve("/assets/app-12345678.js").unwrap()
        else {
            panic!("hashed script was not loaded");
        };
        assert_eq!(&*script.bytes, b"app");
        assert!(script.immutable);
        let AssetLookup::SpaFallback(shell) = manifest.resolve("/sessions/se_1").unwrap() else {
            panic!("desktop shell did not provide the SPA fallback");
        };
        assert_eq!(&*shell.bytes, b"<html>desktop</html>");
        assert!(!shell.immutable);

        std::fs::write(root.join("service-worker.js"), b"pwa").unwrap();
        assert!(matches!(
            AssetManifest::load_desktop_dist(&root),
            Err(HostValidationError::InvalidAssetDirectory(_))
        ));
        std::fs::remove_file(root.join("service-worker.js")).unwrap();
        std::fs::write(root.join("assets/unhashed.js"), b"bad").unwrap();
        assert!(matches!(
            AssetManifest::load_desktop_dist(&root),
            Err(HostValidationError::InvalidAssetDirectory(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_source_policy_separates_unbundled_and_packaged_hosts() {
        let packaged = AssetManifest::new([("/index.html".into(), asset("text/html"))]).unwrap();
        assert!(matches!(
            select_preview_frontend_source(None, None, Some(packaged.clone()), false).unwrap(),
            FrontendSource::Static(_)
        ));
        assert!(matches!(
            select_preview_frontend_source(
                Some("http://127.0.0.1:5173".into()),
                None,
                Some(packaged.clone()),
                false,
            ),
            Err(FrontendSourceError::DevelopmentServerDisabled)
        ));
        assert!(matches!(
            select_preview_frontend_source(
                Some("http://127.0.0.1:5173".into()),
                Some("/tmp/dist".into()),
                Some(packaged),
                true,
            ),
            Err(FrontendSourceError::ConflictingSources)
        ));
    }

    #[test]
    fn lifecycle_close_state_is_ephemeral_and_frontend_resolved() {
        let lifecycle = HostLifecycleHandle::default();
        lifecycle.set_focused(true);
        let request_id = lifecycle.request_close();
        assert!(
            !lifecycle
                .apply_close_decision(request_id, CloseDecision::QuitWhenIdle)
                .unwrap()
        );
        let (_, state, events) = lifecycle.batch_after(0);
        assert!(state.focused);
        assert_eq!(events.len(), 2);
        assert_eq!(
            state.pending_close,
            Some(PendingCloseRequest {
                request_id,
                waiting_for_idle: true,
            })
        );
        assert!(
            lifecycle
                .apply_close_decision(request_id, CloseDecision::QuitNow)
                .unwrap()
        );
        assert!(lifecycle.batch_after(0).1.pending_close.is_none());
    }

    #[test]
    fn verified_session_files_reject_traversal_and_symlink_escape() {
        let container = std::env::temp_dir().join(format!(
            "trouve-desktop-file-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let worktree = container.join("worktree");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/main.rs"), b"fn main() {}\n").unwrap();
        let verified = VerifiedSessionFile::resolve(&worktree, "src/main.rs").unwrap();
        assert_eq!(verified.relative_path(), "src/main.rs");
        assert!(
            verified
                .as_path()
                .starts_with(worktree.canonicalize().unwrap())
        );
        assert!(VerifiedSessionFile::resolve(&worktree, "../outside").is_err());
        assert!(VerifiedSessionFile::resolve(&worktree, "/etc/passwd").is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = container.join("outside.txt");
            std::fs::write(&outside, b"secret").unwrap();
            symlink(&outside, worktree.join("src/link.txt")).unwrap();
            assert!(VerifiedSessionFile::resolve(&worktree, "src/link.txt").is_err());
        }
        std::fs::remove_dir_all(container).unwrap();
    }

    #[test]
    fn gateway_requires_exact_host_and_origin() {
        assert_eq!(
            GatewayOrigin::parse("http://example.com:43127"),
            Err(HostValidationError::NonLoopbackGateway)
        );
        let expected = GatewayOrigin::parse("http://127.0.0.1:43127").unwrap();
        assert_eq!(expected.as_str(), "http://127.0.0.1:43127");
        assert!(
            expected
                .validate("127.0.0.1:43127", "http://127.0.0.1:43127")
                .is_ok()
        );
        assert_eq!(
            expected.validate("localhost:43127", "http://127.0.0.1:43127"),
            Err(HostValidationError::HostMismatch)
        );
        assert_eq!(
            expected.validate("127.0.0.1:43127", "http://localhost:43127"),
            Err(HostValidationError::OriginMismatch)
        );
        let ipv6 = GatewayOrigin::parse("http://[::1]:43127").unwrap();
        assert_eq!(ipv6.authority(), "[::1]:43127");
    }

    #[test]
    fn asset_manifest_never_resolves_request_paths_on_disk() {
        let manifest = AssetManifest::new([
            ("/index.html".into(), asset("text/html")),
            ("/assets/app-123.js".into(), asset("text/javascript")),
        ])
        .unwrap();
        assert!(matches!(
            manifest.resolve("/assets/app-123.js").unwrap(),
            AssetLookup::Exact(_)
        ));
        assert!(matches!(
            manifest.resolve("/sessions/se_1").unwrap(),
            AssetLookup::SpaFallback(_)
        ));
        assert!(matches!(
            manifest.resolve("/assets/missing.js").unwrap(),
            AssetLookup::Missing
        ));
        for path in ["../secret", "/../secret", "/%2e%2e/secret", "/x\\y"] {
            assert!(matches!(
                manifest.resolve(path),
                Err(HostValidationError::UnsafeAssetPath(_))
            ));
        }
    }
}
