//! System-webview desktop host (ADRs 0023 and 0028).
//!
//! The default `trouve` binary embeds the protocol server when no explicit
//! server URL is configured. The `trouve-web-preview` comparison binary keeps
//! requiring an explicit server so it remains safe to run beside another
//! frontend. Both load Lit exclusively through the hardened loopback gateway.

#[path = "native_notification.rs"]
mod native_notification;
#[path = "opener.rs"]
mod opener;
#[path = "sleep.rs"]
mod sleep;
#[path = "web_preview_support.rs"]
mod web_preview_support;

use std::cell::RefCell;
use std::fs::File;
use std::future::Future;
use std::io::Read as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use self::web_preview_support::WebPreviewHost;
use rfd::AsyncFileDialog;
use sha2::{Digest as _, Sha256};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use tokio::sync::{oneshot, watch};
use trouve_desktop_host::{
    CloseDecision, FrontendSource, HostLifecycleHandle, HostNativeActions,
    MAX_NATIVE_ATTACHMENT_BYTES, MAX_NATIVE_ATTACHMENT_TOTAL_BYTES, MAX_NATIVE_ATTACHMENTS,
    NativeAttachment, NativeNotification, VideoAttachmentOpenError, WindowGeometry,
};
use wry::{NewWindowResponse, WebViewBuilder};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

type DirectoryPickerReply = oneshot::Sender<Result<Option<PathBuf>, String>>;
type FilePickerReply = oneshot::Sender<Result<Option<Vec<NativeAttachment>>, String>>;

enum AppEvent {
    PickDirectory(DirectoryPickerReply),
    PickFiles(FilePickerReply),
    NativePickerClosed,
    RequestAttention,
    CloseRequestAcknowledged {
        request_id: u64,
    },
    CloseDecisionApplied {
        request_id: u64,
        decision: CloseDecision,
    },
    QuitNow,
}

const MAX_CLIPBOARD_RGBA_BYTES: usize = 64 * 1024 * 1024;
const CLOSE_CONFIRMATION_GRACE: Duration = Duration::from_secs(5);
const MAX_VIDEO_PLAYBACK_CACHE_FILES: usize = 8;
const MAX_VIDEO_PLAYBACK_CACHE_BYTES: usize = 4 * MAX_NATIVE_ATTACHMENT_BYTES;

#[derive(Debug)]
struct VideoPlaybackCacheEntry {
    path: PathBuf,
    identity: [u8; 32],
    size: usize,
    modified: SystemTime,
    active_leases: usize,
    launched: bool,
}

#[derive(Debug)]
struct VideoPlaybackLease {
    path: PathBuf,
    identity: [u8; 32],
}

#[derive(Debug)]
struct VideoPlaybackCache {
    directory: tempfile::TempDir,
    entries: Vec<VideoPlaybackCacheEntry>,
    next_sequence: u64,
    max_files: usize,
    max_bytes: usize,
}

impl VideoPlaybackCache {
    fn new() -> std::io::Result<Self> {
        Self::with_limits(
            MAX_VIDEO_PLAYBACK_CACHE_FILES,
            MAX_VIDEO_PLAYBACK_CACHE_BYTES,
        )
    }

    /// Successful launches stay retained until the host exits so an external
    /// player can keep reading for as long as playback lasts.
    fn with_limits(max_files: usize, max_bytes: usize) -> std::io::Result<Self> {
        Ok(Self {
            directory: tempfile::Builder::new().prefix("trouve-video-").tempdir()?,
            entries: Vec::new(),
            next_sequence: 0,
            max_files,
            max_bytes,
        })
    }

    #[cfg(test)]
    fn open_with<F>(
        &mut self,
        attachment: &NativeAttachment,
        mut open: F,
    ) -> Result<(), VideoAttachmentOpenError>
    where
        F: FnMut(&std::path::Path) -> Result<(), String>,
    {
        let lease = self.prepare(attachment)?;
        let result = open(&lease.path).map_err(VideoAttachmentOpenError::Failed);
        self.complete(lease, result)
    }

    #[cfg(test)]
    fn open_with_cleanup<F, R>(
        &mut self,
        attachment: &NativeAttachment,
        mut open: F,
        mut remove: R,
    ) -> Result<(), VideoAttachmentOpenError>
    where
        F: FnMut(&std::path::Path) -> Result<(), String>,
        R: FnMut(&std::path::Path) -> bool,
    {
        let lease = self.prepare_with_cleanup(attachment, &mut remove)?;
        let result = open(&lease.path).map_err(VideoAttachmentOpenError::Failed);
        self.complete_with_cleanup(lease, result, &mut remove)
    }

    fn prepare(
        &mut self,
        attachment: &NativeAttachment,
    ) -> Result<VideoPlaybackLease, VideoAttachmentOpenError> {
        self.prepare_with_cleanup(attachment, remove_temporary_video)
    }

    fn prepare_with_cleanup<R>(
        &mut self,
        attachment: &NativeAttachment,
        mut remove: R,
    ) -> Result<VideoPlaybackLease, VideoAttachmentOpenError>
    where
        R: FnMut(&std::path::Path) -> bool,
    {
        let extension = attachment.video_extension().ok_or_else(|| {
            VideoAttachmentOpenError::Failed("unsupported video attachment type".to_string())
        })?;
        let identity = video_attachment_identity(extension, attachment.bytes());
        self.entries.retain(|entry| {
            entry.active_leases > 0 || retained_video_is_usable(entry) || !remove(&entry.path)
        });
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity == identity)
        {
            if !retained_video_is_usable(entry) {
                return Err(VideoAttachmentOpenError::Failed(
                    "stale temporary video could not be replaced".to_string(),
                ));
            }
            entry.active_leases = entry.active_leases.checked_add(1).ok_or_else(|| {
                VideoAttachmentOpenError::Failed(
                    "video playback lease count overflowed".to_string(),
                )
            })?;
            return Ok(VideoPlaybackLease {
                path: entry.path.clone(),
                identity,
            });
        }

        let retained_bytes = self
            .entries
            .iter()
            .try_fold(0usize, |total, entry| total.checked_add(entry.size))
            .ok_or_else(|| {
                VideoAttachmentOpenError::Failed("video playback cache size overflowed".to_string())
            })?;
        let next_bytes = retained_bytes
            .checked_add(attachment.bytes().len())
            .ok_or_else(|| {
                VideoAttachmentOpenError::Failed("video playback cache size overflowed".to_string())
            })?;
        if self.entries.len() >= self.max_files || next_bytes > self.max_bytes {
            return Err(VideoAttachmentOpenError::Capacity);
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let path = self
            .directory
            .path()
            .join(format!("{sequence}.{extension}"));
        std::fs::write(&path, attachment.bytes())
            .map_err(|error| VideoAttachmentOpenError::Failed(error.to_string()))?;
        let modified = match std::fs::metadata(&path).and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(error) => {
                let _ = remove_temporary_video(&path);
                return Err(VideoAttachmentOpenError::Failed(error.to_string()));
            }
        };
        self.entries.push(VideoPlaybackCacheEntry {
            path: path.clone(),
            identity,
            size: attachment.bytes().len(),
            modified,
            active_leases: 1,
            launched: false,
        });
        Ok(VideoPlaybackLease { path, identity })
    }

    fn complete(
        &mut self,
        lease: VideoPlaybackLease,
        result: Result<(), VideoAttachmentOpenError>,
    ) -> Result<(), VideoAttachmentOpenError> {
        self.complete_with_cleanup(lease, result, remove_temporary_video)
    }

    fn complete_with_cleanup<R>(
        &mut self,
        lease: VideoPlaybackLease,
        result: Result<(), VideoAttachmentOpenError>,
        mut remove: R,
    ) -> Result<(), VideoAttachmentOpenError>
    where
        R: FnMut(&std::path::Path) -> bool,
    {
        let mut remove_failed_entry = false;
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity == lease.identity && entry.path == lease.path)
        {
            entry.active_leases = entry.active_leases.checked_sub(1).ok_or_else(|| {
                VideoAttachmentOpenError::Failed(
                    "video playback lease was already reconciled".to_string(),
                )
            })?;
            if result.is_ok() {
                entry.launched = true;
            } else {
                remove_failed_entry = entry.active_leases == 0 && !entry.launched;
            }
        }
        if remove_failed_entry && remove(&lease.path) {
            self.entries.retain(|entry| entry.path != lease.path);
        }
        result
    }
}

async fn open_video_attachment(
    cache: Arc<Mutex<VideoPlaybackCache>>,
    attachment: NativeAttachment,
) -> Result<(), VideoAttachmentOpenError> {
    let lease = {
        let mut cache = cache.lock().map_err(|_| {
            VideoAttachmentOpenError::Failed("video playback cache is unavailable".to_string())
        })?;
        cache.prepare(&attachment)?
    };

    let result = opener::open_confirmed(&lease.path)
        .await
        .map_err(VideoAttachmentOpenError::Failed);
    let mut cache = cache.lock().map_err(|_| {
        VideoAttachmentOpenError::Failed("video playback cache is unavailable".to_string())
    })?;
    cache.complete(lease, result)
}

fn retained_video_is_usable(entry: &VideoPlaybackCacheEntry) -> bool {
    File::open(&entry.path)
        .and_then(|file| file.metadata())
        .is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.len() == entry.size as u64
                && metadata
                    .modified()
                    .is_ok_and(|modified| modified == entry.modified)
        })
}

fn video_attachment_identity(extension: &str, bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((extension.len() as u64).to_le_bytes());
    hasher.update(extension.as_bytes());
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

fn remove_temporary_video(path: &std::path::Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

#[derive(Debug, Default)]
struct CloseConfirmationWatchdog {
    request_id: Option<u64>,
    deadline: Option<Instant>,
}

impl CloseConfirmationWatchdog {
    /// Arm the first close request. A request while one is still pending is
    /// the native escape hatch and must close immediately.
    fn begin(&mut self, request_id: u64, now: Instant) -> bool {
        if self.request_id.is_some() {
            return true;
        }
        self.request_id = Some(request_id);
        self.deadline = Some(now + CLOSE_CONFIRMATION_GRACE);
        false
    }

    /// Apply only the decision for the currently pending native request.
    /// `quit_when_idle` keeps the request active for a later `quit_now` while
    /// disarming automatic timeout; cancel makes the next native close fresh.
    fn resolve(&mut self, request_id: u64, decision: CloseDecision) -> bool {
        if self.request_id != Some(request_id) {
            return false;
        }
        match decision {
            CloseDecision::Cancel => {
                self.request_id = None;
                self.deadline = None;
                false
            }
            CloseDecision::QuitWhenIdle => {
                self.deadline = None;
                false
            }
            CloseDecision::QuitNow => true,
        }
    }

    /// Acknowledgement means the typed frontend owns the confirmation UI. It
    /// disarms only the matching watchdog without choosing a close decision.
    fn acknowledge(&mut self, request_id: u64) -> bool {
        if self.request_id != Some(request_id) {
            return false;
        }
        self.deadline = None;
        true
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn expired(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

/// Serialize and coalesce native geometry writes. The worker is drained at
/// shutdown so no earlier detached write can overwrite the final rectangle.
async fn run_geometry_persistence_worker<F, Fut, E>(
    mut receiver: watch::Receiver<Option<WindowGeometry>>,
    mut persist: F,
) -> Result<(), E>
where
    F: FnMut(WindowGeometry) -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut latest_error = None;
    while receiver.changed().await.is_ok() {
        let Some(next) = receiver.borrow_and_update().clone() else {
            continue;
        };
        match persist(next).await {
            Ok(()) => latest_error = None,
            Err(error) => {
                tracing::warn!(%error, "persisting desktop window geometry failed");
                latest_error = Some(error);
            }
        }
    }
    match latest_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// One event loop can have both a geometry debounce and a close watchdog.
/// Always wait for the earliest pending deadline: assigning either deadline
/// directly can postpone the watchdog, while switching to `Wait` after a
/// geometry flush can lose it entirely.
fn pending_event_deadline(
    geometry_save_deadline: Option<Instant>,
    close_confirmation_deadline: Option<Instant>,
) -> Option<Instant> {
    geometry_save_deadline
        .into_iter()
        .chain(close_confirmation_deadline)
        .min()
}

fn control_flow_for_pending_deadlines(
    geometry_save_deadline: Option<Instant>,
    close_confirmation_deadline: Option<Instant>,
) -> ControlFlow {
    pending_event_deadline(geometry_save_deadline, close_confirmation_deadline)
        .map(ControlFlow::WaitUntil)
        .unwrap_or(ControlFlow::Wait)
}

fn allow_unbundled_frontend(product_host: bool, debug_build: bool) -> bool {
    !product_host || debug_build
}

#[allow(dead_code)] // Used only by the explicit `trouve-web-preview` target.
fn main() -> anyhow::Result<()> {
    run(false)
}

pub(crate) fn run(product_host: bool) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let bundled = WEB_ASSETS_BUNDLED.then(bundled_web_assets).transpose()?;
    let frontend = FrontendSource::from_preview_environment(
        bundled,
        allow_unbundled_frontend(product_host, cfg!(debug_assertions)),
    )?;

    let mut event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let directory_proxy = event_loop.create_proxy();
    let file_proxy = event_loop.create_proxy();
    let quit_proxy = event_loop.create_proxy();
    let close_acknowledgement_proxy = event_loop.create_proxy();
    let close_decision_proxy = event_loop.create_proxy();
    let attention_proxy = event_loop.create_proxy();
    let lifecycle = HostLifecycleHandle::default();
    let notification_lifecycle = lifecycle.clone();
    let sleep_inhibitor = Arc::new(Mutex::new(sleep::SleepInhibitor::default()));
    let sleep_for_action = sleep_inhibitor.clone();
    let video_playback_cache = Arc::new(Mutex::new(VideoPlaybackCache::new()?));
    let video_playback_cache_for_action = Arc::clone(&video_playback_cache);
    let native_actions = HostNativeActions::default()
        .with_window_geometry()
        // Tao exposes focus and foreground transitions but no desktop
        // occlusion event, so that capability remains explicitly false.
        .with_lifecycle_capabilities(lifecycle.clone(), true, false)
        .with_close_acknowledgement_observer(move |request_id| {
            close_acknowledgement_proxy
                .send_event(AppEvent::CloseRequestAcknowledged { request_id })
                .map_err(|_| "desktop event loop is unavailable".to_string())
        })
        .with_close_decision_observer(move |request_id, decision| {
            close_decision_proxy
                .send_event(AppEvent::CloseDecisionApplied {
                    request_id,
                    decision,
                })
                .map_err(|_| "desktop event loop is unavailable".to_string())
        })
        .with_quit_handler(move || {
            quit_proxy
                .send_event(AppEvent::QuitNow)
                .map_err(|_| "desktop event loop is unavailable".to_string())
        })
        .with_sleep_inhibitor(move |active| {
            sleep_for_action
                .lock()
                .map_err(|_| "desktop sleep inhibitor is unavailable".to_string())?
                .set_active(active);
            Ok(())
        })
        .with_native_notification_sender(move |notification| {
            show_native_notification(notification, notification_lifecycle.clone());
            Ok(())
        })
        .with_user_attention_requester(move || {
            attention_proxy
                .send_event(AppEvent::RequestAttention)
                .map_err(|_| "desktop event loop is unavailable".to_string())
        })
        // Session file open/reveal stays unadvertised until the native opener
        // can consume a confined file handle instead of a racy pathname.
        .with_directory_picker(move || {
            let directory_proxy = directory_proxy.clone();
            async move {
                let (reply, result) = oneshot::channel();
                directory_proxy
                    .send_event(AppEvent::PickDirectory(reply))
                    .map_err(|_| "desktop event loop is unavailable".to_string())?;
                result
                    .await
                    .map_err(|_| "desktop directory picker was interrupted".to_string())?
            }
        })
        .with_file_picker(move || {
            let file_proxy = file_proxy.clone();
            async move {
                let (reply, result) = oneshot::channel();
                file_proxy
                    .send_event(AppEvent::PickFiles(reply))
                    .map_err(|_| "desktop event loop is unavailable".to_string())?;
                result
                    .await
                    .map_err(|_| "desktop file picker was interrupted".to_string())?
            }
        })
        .with_clipboard_image_reader(|| async {
            tokio::task::spawn_blocking(read_clipboard_image_attachment)
                .await
                .map_err(|_| "desktop clipboard worker was interrupted".to_string())?
        })
        .with_video_attachment_opener(move |attachment| {
            open_video_attachment(Arc::clone(&video_playback_cache_for_action), attachment)
        })
        .with_external_https_opener(|url| opener::open(url.as_url().as_str()));
    let host = if product_host {
        WebPreviewHost::start_product_with_native_actions(frontend, native_actions)?
    } else {
        WebPreviewHost::start_with_native_actions(frontend, native_actions)?
    };
    let picker_runtime = host.runtime_handle();
    let picker_closed_proxy = event_loop.create_proxy();
    let gateway_origin = host.gateway_origin().to_owned();
    let allowed_origin = gateway_origin.clone();
    let allowed_prefix = format!("{allowed_origin}/");
    let restored_geometry = host
        .initial_preferences()
        .geometry
        .clone()
        .and_then(|geometry| {
            let monitors = event_loop
                .available_monitors()
                .map(|monitor| {
                    let position = monitor.position();
                    let size = monitor.size();
                    MonitorBounds {
                        x: position.x,
                        y: position.y,
                        width: size.width,
                        height: size.height,
                    }
                })
                .collect::<Vec<_>>();
            restore_geometry_on_monitors(geometry, &monitors)
        });
    let mut window_builder = WindowBuilder::new()
        .with_title(if product_host {
            "trouve"
        } else {
            "trouve — web preview"
        })
        .with_min_inner_size(LogicalSize::new(900, 560));
    if let Some(geometry) = restored_geometry.as_ref() {
        window_builder = window_builder
            .with_inner_size(PhysicalSize::new(geometry.width, geometry.height))
            .with_position(PhysicalPosition::new(geometry.x, geometry.y))
            .with_maximized(geometry.maximized);
    } else {
        window_builder = window_builder.with_inner_size(LogicalSize::new(1_400, 900));
    }
    let window = Rc::new(window_builder.build(&event_loop)?);
    let geometry = Rc::new(RefCell::new(capture_window_geometry(
        window.as_ref(),
        restored_geometry.unwrap_or(WindowGeometry {
            x: 0,
            y: 0,
            width: 1_400,
            height: 900,
            maximized: false,
        }),
    )));
    let builder = WebViewBuilder::new()
        .with_url(&gateway_origin)
        .with_navigation_handler(move |url| {
            url == allowed_origin || url.starts_with(&allowed_prefix)
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        // The web frontend handles file selection through explicit host
        // capabilities. Native webview drag/drop must not bypass that API.
        .with_drag_drop_handler(|_| true);

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let webview = builder.build(window.as_ref())?;
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let container = window
            .default_vbox()
            .ok_or_else(|| anyhow::anyhow!("system webview window has no GTK container"))?;
        builder.build_gtk(container)?
    };

    let window_for_events = window.clone();
    let geometry_for_events = geometry.clone();
    let preferences = host.preferences_handle();
    let geometry_runtime = picker_runtime.clone();
    let (geometry_updates, geometry_receiver) = watch::channel(None);
    let geometry_preferences = preferences.clone();
    let geometry_worker = geometry_runtime.spawn(run_geometry_persistence_worker(
        geometry_receiver,
        move |next| {
            let preferences = geometry_preferences.clone();
            async move { preferences.update_window_geometry(next).await }
        },
    ));
    let geometry_updates_for_events = geometry_updates.clone();
    let mut geometry_save_deadline: Option<Instant> = None;
    let mut close_confirmation = CloseConfirmationWatchdog::default();
    let exit_code = event_loop.run_return(move |event, _, control_flow| {
        *control_flow = control_flow_for_pending_deadlines(
            geometry_save_deadline,
            close_confirmation.deadline(),
        );
        match event {
            Event::UserEvent(AppEvent::PickDirectory(reply)) => {
                let dialog = AsyncFileDialog::new()
                    .set_title("Open workspace (git repository)")
                    .set_parent(window_for_events.as_ref());
                let picker_closed_proxy = picker_closed_proxy.clone();
                picker_runtime.spawn(async move {
                    let selected = dialog
                        .pick_folder()
                        .await
                        .map(|folder| folder.path().to_owned());
                    let _ = reply.send(Ok(selected));
                    let _ = picker_closed_proxy.send_event(AppEvent::NativePickerClosed);
                });
            }
            Event::UserEvent(AppEvent::PickFiles(reply)) => {
                let dialog = AsyncFileDialog::new()
                    .set_title("Attach files to the prompt")
                    .set_parent(window_for_events.as_ref());
                let picker_closed_proxy = picker_closed_proxy.clone();
                picker_runtime.spawn(async move {
                    let result = match dialog.pick_files().await {
                        Some(files) => {
                            let paths = files
                                .into_iter()
                                .map(|file| file.path().to_owned())
                                .collect();
                            tokio::task::spawn_blocking(move || read_selected_attachments(paths))
                                .await
                                .map_err(|_| {
                                    "desktop attachment reader was interrupted".to_string()
                                })
                                .and_then(|result| result.map(Some))
                        }
                        None => Ok(None),
                    };
                    let _ = reply.send(result);
                    let _ = picker_closed_proxy.send_event(AppEvent::NativePickerClosed);
                });
            }
            Event::UserEvent(AppEvent::NativePickerClosed) => {
                window_for_events.set_focus();
            }
            Event::UserEvent(AppEvent::RequestAttention) => {
                use tao::window::UserAttentionType;
                window_for_events.set_minimized(false);
                window_for_events.set_visible(true);
                window_for_events.set_focus();
                window_for_events.request_user_attention(Some(UserAttentionType::Informational));
            }
            Event::UserEvent(AppEvent::CloseRequestAcknowledged { request_id }) => {
                close_confirmation.acknowledge(request_id);
                *control_flow = control_flow_for_pending_deadlines(
                    geometry_save_deadline,
                    close_confirmation.deadline(),
                );
            }
            Event::UserEvent(AppEvent::CloseDecisionApplied {
                request_id,
                decision,
            }) => {
                if close_confirmation.resolve(request_id, decision) {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(AppEvent::QuitNow) => *control_flow = ControlFlow::Exit,
            Event::WindowEvent {
                event: WindowEvent::Focused(focused),
                ..
            } => lifecycle.set_focused(focused),
            Event::WindowEvent {
                event: WindowEvent::Started | WindowEvent::Resumed,
                ..
            } => lifecycle.set_visible(true),
            Event::WindowEvent {
                event: WindowEvent::Suspended | WindowEvent::Stopped,
                ..
            } => lifecycle.set_visible(false),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if close_confirmation.begin(lifecycle.request_close(), Instant::now()) {
                    // A second close request is an explicit native escape
                    // hatch when the frontend cannot acknowledge the first.
                    *control_flow = ControlFlow::Exit;
                } else {
                    *control_flow = control_flow_for_pending_deadlines(
                        geometry_save_deadline,
                        close_confirmation.deadline(),
                    );
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(_) | WindowEvent::Resized(_),
                ..
            } => {
                let next = capture_window_geometry(
                    window_for_events.as_ref(),
                    geometry_for_events.borrow().clone(),
                );
                *geometry_for_events.borrow_mut() = next;
                geometry_save_deadline = Some(Instant::now() + Duration::from_millis(350));
                *control_flow = control_flow_for_pending_deadlines(
                    geometry_save_deadline,
                    close_confirmation.deadline(),
                );
            }
            Event::MainEventsCleared if close_confirmation.expired(Instant::now()) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::MainEventsCleared
                if geometry_save_deadline.is_some_and(|deadline| Instant::now() >= deadline) =>
            {
                geometry_save_deadline = None;
                let next = geometry_for_events.borrow().clone();
                if geometry_updates_for_events.send(Some(next)).is_err() {
                    tracing::warn!("desktop geometry persistence worker is unavailable");
                }
                *control_flow = control_flow_for_pending_deadlines(
                    geometry_save_deadline,
                    close_confirmation.deadline(),
                );
            }
            Event::WindowEvent {
                event: WindowEvent::Destroyed,
                ..
            } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });

    geometry_updates
        .send(Some(geometry.borrow().clone()))
        .map_err(|_| anyhow::anyhow!("desktop geometry persistence worker stopped early"))?;
    drop(geometry_updates);
    geometry_runtime.block_on(geometry_worker)??;
    drop(webview);
    drop(window);
    host.shutdown();
    if exit_code != 0 {
        anyhow::bail!("desktop webview event loop exited with status {exit_code}");
    }
    Ok(())
}

/// Keep the last valid floating rectangle while maximized. Several window
/// systems report the maximized frame (or a transient zero-sized frame) from
/// resize callbacks; persisting that frame would make the next unmaximized
/// launch surprising and can fail host preference validation.
fn capture_window_geometry(
    window: &tao::window::Window,
    mut previous: WindowGeometry,
) -> WindowGeometry {
    previous.maximized = window.is_maximized();
    if previous.maximized {
        return previous;
    }

    let size = window.inner_size();
    if (320..=16_384).contains(&size.width) && (240..=16_384).contains(&size.height) {
        previous.width = size.width;
        previous.height = size.height;
    }
    if let Ok(position) = window.outer_position()
        && (-16_384..=16_384).contains(&position.x)
        && (-16_384..=16_384).contains(&position.y)
    {
        previous.x = position.x;
        previous.y = position.y;
    }
    previous
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonitorBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn restore_geometry_on_monitors(
    mut geometry: WindowGeometry,
    monitors: &[MonitorBounds],
) -> Option<WindowGeometry> {
    if monitors.is_empty() {
        return None;
    }
    const MIN_TITLE_WIDTH: i64 = 128;
    const TITLE_HEIGHT: i64 = 32;
    let intersects = |geometry: &WindowGeometry, monitor: &MonitorBounds| {
        let left = i64::from(geometry.x).max(i64::from(monitor.x));
        let right = (i64::from(geometry.x) + i64::from(geometry.width))
            .min(i64::from(monitor.x) + i64::from(monitor.width));
        let title_top = i64::from(geometry.y).max(i64::from(monitor.y));
        let title_bottom = (i64::from(geometry.y) + TITLE_HEIGHT)
            .min(i64::from(monitor.y) + i64::from(monitor.height));
        right - left >= MIN_TITLE_WIDTH && title_bottom - title_top >= TITLE_HEIGHT
    };
    if monitors
        .iter()
        .any(|monitor| intersects(&geometry, monitor))
    {
        return Some(geometry);
    }

    // Monitor topology changed. Preserve the saved size as far as practical,
    // then place the window inside the first current monitor with a small
    // inset so the title bar and resize affordances remain reachable.
    let monitor = monitors[0];
    geometry.width = geometry.width.min(monitor.width.max(320));
    geometry.height = geometry.height.min(monitor.height.max(240));
    let inset_x = i64::from(monitor.width.saturating_sub(geometry.width).min(32));
    let inset_y = i64::from(monitor.height.saturating_sub(geometry.height).min(32));
    geometry.x =
        (i64::from(monitor.x) + inset_x).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    geometry.y =
        (i64::from(monitor.y) + inset_y).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    Some(geometry)
}

fn show_native_notification(notification: NativeNotification, lifecycle: HostLifecycleHandle) {
    native_notification::show(
        notification.title().to_owned(),
        notification.body().to_owned(),
        notification.sound(),
        move || lifecycle.notification_activated(&notification),
    );
}

fn read_selected_attachments(paths: Vec<PathBuf>) -> Result<Vec<NativeAttachment>, String> {
    if paths.len() > MAX_NATIVE_ATTACHMENTS {
        return Err("too many files were selected".into());
    }
    let mut total = 0usize;
    let mut attachments = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| "selected file has no UTF-8 display name".to_string())?
            .to_owned();
        let metadata = std::fs::metadata(&path).map_err(|_| format!("cannot inspect {name}"))?;
        let reported_size = usize::try_from(metadata.len())
            .map_err(|_| format!("selected file {name} is too large"))?;
        if !metadata.is_file() || reported_size == 0 || reported_size > MAX_NATIVE_ATTACHMENT_BYTES
        {
            return Err(format!("selected file {name} is outside attachment bounds"));
        }
        let mut bytes = Vec::with_capacity(reported_size);
        File::open(&path)
            .map_err(|_| format!("cannot open {name}"))?
            .take((MAX_NATIVE_ATTACHMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| format!("cannot read {name}"))?;
        if bytes.is_empty() || bytes.len() > MAX_NATIVE_ATTACHMENT_BYTES {
            return Err(format!("selected file {name} is outside attachment bounds"));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| "selected attachment batch is too large".to_string())?;
        if total > MAX_NATIVE_ATTACHMENT_TOTAL_BYTES {
            return Err("selected attachment batch is too large".into());
        }
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        attachments
            .push(NativeAttachment::new(name, mime, bytes).map_err(|error| error.to_string())?);
    }
    Ok(attachments)
}

fn read_clipboard_image_attachment() -> Result<Option<NativeAttachment>, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|_| "desktop clipboard is unavailable".to_string())?;
    // Text wins when a rich clipboard advertises
    // both text and image representations.
    if clipboard.get_text().is_ok() {
        return Ok(None);
    }
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(_) => return Ok(None),
    };
    let rgba_bytes = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "clipboard image dimensions overflow".to_string())?;
    if image.width == 0
        || image.height == 0
        || rgba_bytes != image.bytes.len()
        || rgba_bytes > MAX_CLIPBOARD_RGBA_BYTES
    {
        return Err("clipboard image is outside native bounds".into());
    }
    let width = u32::try_from(image.width)
        .map_err(|_| "clipboard image width is outside native bounds".to_string())?;
    let height = u32::try_from(image.height)
        .map_err(|_| "clipboard image height is outside native bounds".to_string())?;
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|_| "clipboard PNG header could not be encoded".to_string())?;
        writer
            .write_image_data(&image.bytes)
            .map_err(|_| "clipboard PNG pixels could not be encoded".to_string())?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    NativeAttachment::new(format!("pasted-{stamp}.png"), "image/png", bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod close_confirmation_tests {
    use super::*;

    fn video_attachment(name: &str, bytes: &[u8]) -> NativeAttachment {
        NativeAttachment::new(name, "video/mp4", bytes.to_vec()).unwrap()
    }

    #[test]
    fn video_playback_cache_reuses_identical_attachments() {
        let mut cache = VideoPlaybackCache::with_limits(2, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        let attachment = video_attachment("demo.mp4", b"video");
        let mut opened = Vec::new();

        cache
            .open_with(&attachment, |path| {
                opened.push(path.to_owned());
                Ok(())
            })
            .unwrap();
        cache
            .open_with(&attachment, |path| {
                opened.push(path.to_owned());
                Ok(())
            })
            .unwrap();

        assert_eq!(cache.entries.len(), 1);
        assert_eq!(opened.len(), 2);
        assert_eq!(opened[0], opened[1]);
    }

    #[test]
    fn video_playback_cache_removes_failed_new_launches() {
        let mut cache = VideoPlaybackCache::with_limits(2, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        let attachment = video_attachment("demo.mp4", b"video");

        assert_eq!(
            cache.open_with(&attachment, |_| Err("no system player".into())),
            Err(VideoAttachmentOpenError::Failed("no system player".into()))
        );
        assert!(cache.entries.is_empty());
        assert_eq!(
            std::fs::read_dir(cache.directory.path()).unwrap().count(),
            0
        );
    }

    #[test]
    fn video_playback_leases_keep_shared_launches_alive_until_reconciled() {
        let mut cache = VideoPlaybackCache::with_limits(2, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        let attachment = video_attachment("demo.mp4", b"video");
        let failed_lease = cache.prepare(&attachment).unwrap();
        let successful_lease = cache.prepare(&attachment).unwrap();

        assert_eq!(failed_lease.path, successful_lease.path);
        assert_eq!(cache.entries[0].active_leases, 2);
        assert_eq!(
            cache.complete(
                failed_lease,
                Err(VideoAttachmentOpenError::Failed("no player".into())),
            ),
            Err(VideoAttachmentOpenError::Failed("no player".into()))
        );
        assert_eq!(cache.entries[0].active_leases, 1);
        assert!(cache.entries[0].path.exists());

        cache.complete(successful_lease, Ok(())).unwrap();
        assert_eq!(cache.entries[0].active_leases, 0);
        assert!(cache.entries[0].launched);
        assert!(cache.entries[0].path.exists());
    }

    #[test]
    fn video_playback_cache_bounds_distinct_retained_files() {
        let mut cache = VideoPlaybackCache::with_limits(2, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        for bytes in [b"first".as_slice(), b"second".as_slice()] {
            cache
                .open_with(&video_attachment("demo.mp4", bytes), |_| Ok(()))
                .unwrap();
        }

        assert!(
            cache
                .open_with(&video_attachment("demo.mp4", b"third"), |_| Ok(()))
                .is_err()
        );
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(
            std::fs::read_dir(cache.directory.path()).unwrap().count(),
            2
        );
    }

    #[test]
    fn video_playback_cache_retains_successful_files_for_the_host_lifetime() {
        let mut cache = VideoPlaybackCache::with_limits(2, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        cache
            .open_with(&video_attachment("first.mp4", b"first"), |_| Ok(()))
            .unwrap();
        let first_path = cache.entries[0].path.clone();
        cache
            .open_with(&video_attachment("second.mp4", b"second"), |_| Ok(()))
            .unwrap();

        assert_eq!(cache.entries.len(), 2);
        assert_eq!(std::fs::read(first_path).unwrap(), b"first");
    }

    #[test]
    fn video_playback_cache_rematerializes_a_deleted_retained_file() {
        let mut cache = VideoPlaybackCache::with_limits(2, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        let attachment = video_attachment("demo.mp4", b"video");
        cache.open_with(&attachment, |_| Ok(())).unwrap();
        let stale_path = cache.entries[0].path.clone();
        std::fs::remove_file(&stale_path).unwrap();

        let mut reopened_path = None;
        cache
            .open_with(&attachment, |path| {
                reopened_path = Some(path.to_owned());
                Ok(())
            })
            .unwrap();

        let reopened_path = reopened_path.unwrap();
        assert_ne!(reopened_path, stale_path);
        assert_eq!(std::fs::read(reopened_path).unwrap(), b"video");
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn video_playback_cache_retries_stale_cleanup_before_capacity_checks() {
        let mut cache = VideoPlaybackCache::with_limits(1, MAX_NATIVE_ATTACHMENT_BYTES).unwrap();
        cache
            .open_with(&video_attachment("first.mp4", b"first"), |_| Ok(()))
            .unwrap();
        let stale_path = cache.entries[0].path.clone();
        std::fs::write(&stale_path, b"changed length").unwrap();
        let second = video_attachment("second.mp4", b"second");

        assert_eq!(
            cache.open_with_cleanup(&second, |_| Ok(()), |_| false),
            Err(VideoAttachmentOpenError::Capacity)
        );
        cache.open_with(&second, |_| Ok(())).unwrap();

        assert_eq!(cache.entries.len(), 1);
        assert_eq!(std::fs::read(&cache.entries[0].path).unwrap(), b"second");
        assert_ne!(cache.entries[0].path, stale_path);
    }

    #[test]
    fn optimized_qualification_hosts_allow_runtime_frontend_sources() {
        assert!(allow_unbundled_frontend(false, false));
        assert!(allow_unbundled_frontend(false, true));
        assert!(allow_unbundled_frontend(true, true));
        assert!(!allow_unbundled_frontend(true, false));
    }

    #[test]
    fn restored_geometry_stays_when_visible_on_any_monitor() {
        let geometry = WindowGeometry {
            x: 2_000,
            y: 100,
            width: 1_200,
            height: 800,
            maximized: false,
        };
        let monitors = [
            MonitorBounds {
                x: 0,
                y: 0,
                width: 1_920,
                height: 1_080,
            },
            MonitorBounds {
                x: 1_920,
                y: 0,
                width: 1_920,
                height: 1_080,
            },
        ];
        assert_eq!(
            restore_geometry_on_monitors(geometry.clone(), &monitors),
            Some(geometry)
        );
    }

    #[test]
    fn restored_geometry_moves_onto_remaining_monitor() {
        let geometry = WindowGeometry {
            x: 4_000,
            y: 500,
            width: 2_400,
            height: 1_400,
            maximized: true,
        };
        let monitors = [MonitorBounds {
            x: -1_920,
            y: 0,
            width: 1_920,
            height: 1_080,
        }];
        let restored = restore_geometry_on_monitors(geometry, &monitors).unwrap();
        assert_eq!(restored.x, -1_920);
        assert_eq!(restored.y, 0);
        assert_eq!(restored.width, 1_920);
        assert_eq!(restored.height, 1_080);
        assert!(restored.maximized);
    }

    #[test]
    fn restored_geometry_with_only_a_bottom_corner_visible_is_repositioned() {
        let monitor = MonitorBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let geometry = WindowGeometry {
            x: 1800,
            y: 1016,
            width: 1200,
            height: 800,
            maximized: false,
        };

        let restored = restore_geometry_on_monitors(geometry, &[monitor]).unwrap();
        assert!(restored.x >= monitor.x);
        assert!(restored.y >= monitor.y);
        assert!(restored.x + 128 <= monitor.x + monitor.width as i32);
        assert!(restored.y + 32 <= monitor.y + monitor.height as i32);
    }

    #[test]
    fn restored_geometry_with_only_its_bottom_edge_visible_is_repositioned() {
        let monitor = MonitorBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let geometry = WindowGeometry {
            x: 200,
            y: -736,
            width: 1200,
            height: 800,
            maximized: false,
        };

        let restored = restore_geometry_on_monitors(geometry, &[monitor]).unwrap();
        assert!(restored.y >= monitor.y);
        assert!(restored.y + 32 <= monitor.y + monitor.height as i32);
    }

    #[test]
    fn restored_geometry_falls_back_when_no_monitor_is_reported() {
        let geometry = WindowGeometry {
            x: 10,
            y: 10,
            width: 1_200,
            height: 800,
            maximized: false,
        };
        assert_eq!(restore_geometry_on_monitors(geometry, &[]), None);
    }

    #[test]
    fn cancel_disarms_the_deadline_and_makes_the_next_close_fresh() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(1, now));
        assert!(watchdog.deadline().is_some());

        assert!(!watchdog.resolve(1, CloseDecision::Cancel));
        assert!(watchdog.deadline().is_none());
        assert!(!watchdog.begin(2, now));
    }

    #[test]
    fn quit_when_idle_disarms_timeout_but_keeps_the_native_escape_hatch() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(7, now));

        assert!(!watchdog.resolve(7, CloseDecision::QuitWhenIdle));
        assert!(watchdog.deadline().is_none());
        assert!(watchdog.begin(7, now + CLOSE_CONFIRMATION_GRACE));
    }

    #[test]
    fn unresolved_close_exits_only_after_the_grace_period() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(3, now));
        assert!(!watchdog.expired(now + CLOSE_CONFIRMATION_GRACE - Duration::from_millis(1)));
        assert!(watchdog.expired(now + CLOSE_CONFIRMATION_GRACE));
    }

    #[test]
    fn exact_acknowledgement_keeps_a_healthy_prompt_open_without_deciding() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(3, now));

        assert!(watchdog.acknowledge(3));
        assert!(!watchdog.expired(now + CLOSE_CONFIRMATION_GRACE * 10));
        // A second native close remains the explicit escape hatch even after
        // the frontend has acknowledged the first request.
        assert!(watchdog.begin(3, now + CLOSE_CONFIRMATION_GRACE * 10));
    }

    #[test]
    fn stale_acknowledgement_cannot_disarm_the_current_request() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(4, now));
        assert!(!watchdog.acknowledge(3));
        assert!(watchdog.expired(now + CLOSE_CONFIRMATION_GRACE));
    }

    #[test]
    fn stale_decision_cannot_disarm_a_newer_close_request() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(4, now));
        assert!(!watchdog.resolve(3, CloseDecision::Cancel));
        assert!(watchdog.deadline().is_some());
        assert!(watchdog.resolve(4, CloseDecision::QuitNow));
    }

    #[test]
    fn geometry_and_close_deadlines_always_select_the_earliest_wakeup() {
        let now = Instant::now();
        let close = now + Duration::from_secs(2);
        let earlier_geometry = now + Duration::from_millis(350);
        let later_geometry = now + Duration::from_secs(3);

        assert_eq!(
            pending_event_deadline(Some(earlier_geometry), Some(close)),
            Some(earlier_geometry)
        );
        assert_eq!(
            pending_event_deadline(Some(later_geometry), Some(close)),
            Some(close)
        );
        assert_eq!(pending_event_deadline(None, Some(close)), Some(close));
    }

    #[test]
    fn geometry_expiry_does_not_clear_the_close_watchdog() {
        let now = Instant::now();
        let mut watchdog = CloseConfirmationWatchdog::default();
        assert!(!watchdog.begin(11, now));
        let close = watchdog.deadline().unwrap();
        let geometry = now + Duration::from_millis(350);

        assert_eq!(
            pending_event_deadline(Some(geometry), watchdog.deadline()),
            Some(geometry)
        );
        assert!(!watchdog.expired(geometry));
        assert_eq!(
            pending_event_deadline(None, watchdog.deadline()),
            Some(close)
        );
        assert!(watchdog.expired(close));
    }

    #[tokio::test]
    async fn geometry_worker_coalesces_bursts_and_persists_final_after_inflight_write() {
        let stale = WindowGeometry {
            x: 1,
            y: 2,
            width: 900,
            height: 600,
            maximized: false,
        };
        let final_geometry = WindowGeometry {
            x: 30,
            y: 40,
            width: 1_400,
            height: 900,
            maximized: true,
        };
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_for_worker = observed.clone();
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let mut started = Some(started);
        let mut release_rx = Some(release_rx);
        let (updates, receiver) = watch::channel(None);
        updates.send(Some(stale.clone())).unwrap();

        let worker = tokio::spawn(run_geometry_persistence_worker(receiver, move |geometry| {
            let observed = observed_for_worker.clone();
            let started = started.take();
            let release_rx = release_rx.take();
            async move {
                observed.lock().unwrap().push(geometry);
                if let Some(started) = started {
                    let _ = started.send(());
                }
                if let Some(release_rx) = release_rx {
                    let _ = release_rx.await;
                }
                Ok::<(), &'static str>(())
            }
        }));

        started_rx.await.unwrap();
        for offset in 0..1_000 {
            updates
                .send(Some(WindowGeometry {
                    x: offset,
                    ..stale.clone()
                }))
                .unwrap();
        }
        updates.send(Some(final_geometry.clone())).unwrap();
        drop(updates);
        assert!(!worker.is_finished());
        release.send(()).unwrap();
        worker.await.unwrap().unwrap();

        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &[stale, final_geometry]
        );
    }
}
