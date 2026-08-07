//! Chrome-free, in-process Servo qualification adapter (ADRs 0023–0025).
//!
//! This binary embeds an exact Servo nightly revision directly in a winit
//! window. It intentionally contains no servoshell UI: the Lit application
//! occupies the entire client area and only the operating system's normal
//! window decoration remains.
//!
//! This is still a qualification target, not the default desktop application.
//! It connects to an explicitly selected trouve-server through the hardened
//! desktop gateway and keeps Servo's own storage in a process-owned temporary
//! directory. It never starts an engine or opens Trouve's durable database.

mod system_opener;
mod web_preview_support;

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Read as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use euclid::Scale;
use rfd::AsyncFileDialog;
use servo::{
    Code, CompositionEvent, CompositionState, ConsoleLogLevel, Cursor, DevicePoint,
    EditingActionEvent, EmbedderControl, EventLoopWaker, ImeEvent, InputEvent, Key, KeyState,
    KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, NamedKey, NavigationRequest, Opts, PrefValue,
    Preferences, RenderingContext, Servo, ServoBuilder, Theme, TouchEvent, TouchEventType, TouchId,
    TouchPointerType, WebView, WebViewBuilder, WheelDelta, WheelEvent, WheelMode,
    WindowRenderingContext,
};
use tempfile::TempDir;
use tokio::sync::oneshot;
use trouve_desktop_host::{
    FrontendSource, HostLifecycleHandle, HostNativeActions, LocalFileAction,
    MAX_NATIVE_ATTACHMENT_BYTES, MAX_NATIVE_ATTACHMENT_TOTAL_BYTES, MAX_NATIVE_ATTACHMENTS,
    NativeAttachment, NativeNotification,
};
use url::{Origin, Url};
use web_preview_support::WebPreviewHost;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton as WinitMouseButton, MouseScrollDelta, TouchPhase,
    WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{
    Key as WinitKey, KeyCode, KeyLocation, ModifiersState, NamedKey as WinitNamedKey, PhysicalKey,
};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Fullscreen, Window, WindowId};

const INITIAL_WIDTH: f64 = 1_400.0;
const INITIAL_HEIGHT: f64 = 900.0;
const MINIMUM_WIDTH: f64 = 900.0;
const MINIMUM_HEIGHT: f64 = 560.0;
const WHEEL_LINE_WIDTH: f64 = 38.0;
const WHEEL_LINE_HEIGHT: f64 = 76.0;
const MAX_CLIPBOARD_RGBA_BYTES: usize = 64 * 1024 * 1024;

// This is the pinned Servo nightly's experimental web-platform set. CSS Grid
// is essential to the Trouve shell; keeping the complete set makes
// qualification runs reproducible against the exact engine revision.
const EXPERIMENTAL_PREFERENCES: &[&str] = &[
    "dom_async_clipboard_enabled",
    "dom_exec_command_enabled",
    "dom_fontface_enabled",
    "dom_indexeddb_enabled",
    "dom_intersection_observer_enabled",
    "dom_navigator_protocol_handlers_enabled",
    "dom_notification_enabled",
    "dom_offscreen_canvas_enabled",
    "dom_permissions_enabled",
    "dom_sanitizer_enabled",
    "dom_storage_manager_api_enabled",
    "dom_webgl2_enabled",
    "dom_webgpu_enabled",
    "layout_css_attr_enabled",
    "layout_columns_enabled",
    "layout_container_queries_enabled",
    "layout_grid_enabled",
    "layout_variable_fonts_enabled",
];

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .context("creating the embedded Servo event loop")?;
    let directory_proxy = event_loop.create_proxy();
    let file_proxy = event_loop.create_proxy();
    let quit_proxy = event_loop.create_proxy();
    let attention_proxy = event_loop.create_proxy();
    let lifecycle = HostLifecycleHandle::default();
    let notification_lifecycle = lifecycle.clone();
    let sleep_inhibitor = Arc::new(Mutex::new(NativeSleepInhibitor::default()));
    let sleep_for_action = sleep_inhibitor.clone();
    let native_actions = HostNativeActions::default()
        .with_lifecycle_capabilities(lifecycle.clone(), true, true)
        .with_quit_handler(move || {
            quit_proxy
                .send_event(AppEvent::ExitRequested)
                .map_err(|_| "embedded Servo event loop is unavailable".to_string())
        })
        .with_sleep_inhibitor(move |active| {
            sleep_for_action
                .lock()
                .map_err(|_| "embedded Servo sleep inhibitor is unavailable".to_string())?
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
                .map_err(|_| "embedded Servo event loop is unavailable".to_string())
        })
        .with_local_file_handler(|file, action| {
            let target = match action {
                LocalFileAction::Open => file.as_path(),
                LocalFileAction::Reveal => file
                    .as_path()
                    .parent()
                    .ok_or_else(|| "session file has no parent directory".to_string())?,
            };
            system_opener::open(target);
            Ok(())
        })
        .with_directory_picker(move || {
            let directory_proxy = directory_proxy.clone();
            async move {
                let (reply, result) = oneshot::channel();
                directory_proxy
                    .send_event(AppEvent::PickDirectory(reply))
                    .map_err(|_| "embedded Servo event loop is unavailable".to_string())?;
                result
                    .await
                    .map_err(|_| "embedded Servo directory picker was interrupted".to_string())?
            }
        })
        .with_file_picker(move || {
            let file_proxy = file_proxy.clone();
            async move {
                let (reply, result) = oneshot::channel();
                file_proxy
                    .send_event(AppEvent::PickFiles(reply))
                    .map_err(|_| "embedded Servo event loop is unavailable".to_string())?;
                result
                    .await
                    .map_err(|_| "embedded Servo file picker was interrupted".to_string())?
            }
        })
        .with_clipboard_image_reader(|| async {
            tokio::task::spawn_blocking(read_clipboard_image_attachment)
                .await
                .map_err(|_| "embedded Servo clipboard worker was interrupted".to_string())?
        })
        .with_external_https_opener(|url| {
            system_opener::open(url.as_url().as_str());
            Ok(())
        });
    let frontend = FrontendSource::from_preview_environment(None, true)?;
    let host = WebPreviewHost::start(frontend, native_actions)?;
    let gateway_url = Url::parse(host.gateway_origin())
        .with_context(|| format!("parsing desktop gateway URL {}", host.gateway_origin()))?;
    let mut app = App::new(gateway_url, host.runtime_handle(), lifecycle, &event_loop);

    tracing::warn!(
        gateway_origin = host.gateway_origin(),
        servo_nightly = "2026-08-02",
        servo_revision = "35672cc3d4beb768489f5218e73bee7aff0ddb01",
        text_selection = "keyboard-in-editable-controls",
        temporary_storage = true,
        browser_chrome = false,
        "launching qualification-only in-process Servo adapter; Wry is the product default and Slint remains the rollback"
    );

    let run_result = event_loop
        .run_app(&mut app)
        .context("running the embedded Servo event loop");
    let failure = app.take_failure();

    // Field order in RunningApp drops the WebView before the final Servo
    // handle, then the rendering context and window. Dropping the final Servo
    // handle performs Servo's synchronous clean shutdown.
    drop(app);
    host.shutdown();

    run_result?;
    if let Some(failure) = failure {
        bail!("{failure}");
    }
    Ok(())
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
    // Rich clipboard sources can advertise both representations. Match the
    // shipping Slint behavior and allow ordinary text paste to win.
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
    // Keep a wider raw-pixel ceiling than the encoded attachment limit so a
    // large but highly compressible image can still fit the 10 MiB wire cap.
    // The 64 MiB bound limits worst-case PNG work before NativeAttachment
    // validates the final encoded payload.
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

#[derive(Default)]
struct NativeSleepInhibitor {
    requested: bool,
    guard: Option<keepawake::KeepAwake>,
}

impl NativeSleepInhibitor {
    fn set_active(&mut self, active: bool) {
        if self.requested == active {
            return;
        }
        self.requested = active;
        if active {
            match keepawake::Builder::default()
                .idle(true)
                .reason("Trouve agents are running")
                .app_name("Trouve")
                .app_reverse_domain("io.github.jimsimon.trouve")
                .create()
            {
                Ok(guard) => self.guard = Some(guard),
                Err(error) => tracing::warn!(%error, "could not inhibit automatic system sleep"),
            }
        } else {
            self.guard = None;
        }
    }
}

fn show_native_notification(notification: NativeNotification, lifecycle: HostLifecycleHandle) {
    std::thread::spawn(move || {
        let mut request = notify_rust::Notification::new();
        request
            .appname("Trouve")
            .summary(notification.title())
            .body(notification.body())
            .icon("trouve");
        if notification.sound() {
            #[cfg(all(unix, not(target_os = "macos")))]
            request.sound_name("message-new-instant");
            #[cfg(target_os = "macos")]
            request.sound_name("Ping");
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            request.hint(notify_rust::Hint::DesktopEntry("trouve".into()));
            request.action("default", "Open");
            if let Ok(handle) = request.show() {
                handle.wait_for_action(|action| {
                    if action == "default" {
                        lifecycle.notification_activated(&notification);
                    }
                });
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = lifecycle;
            let _ = request.show();
        }
    });
}

type DirectoryPickerReply = oneshot::Sender<Result<Option<PathBuf>, String>>;
type FilePickerReply = oneshot::Sender<Result<Option<Vec<NativeAttachment>>, String>>;

#[derive(Debug)]
enum AppEvent {
    WakeServo,
    PickDirectory(DirectoryPickerReply),
    PickFiles(FilePickerReply),
    NativePickerClosed,
    RequestAttention,
    ExitRequested,
}

#[derive(Clone)]
struct Waker(EventLoopProxy<AppEvent>);

impl Waker {
    fn new(event_loop: &EventLoop<AppEvent>) -> Self {
        Self(event_loop.create_proxy())
    }
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        if let Err(error) = self.0.send_event(AppEvent::WakeServo) {
            tracing::warn!(?error, "failed to wake the embedded Servo event loop");
        }
    }
}

struct App {
    bootstrap: Option<Bootstrap>,
    running: Option<RunningApp>,
    failure: Rc<RefCell<Option<String>>>,
}

struct Bootstrap {
    gateway_url: Url,
    picker_runtime: tokio::runtime::Handle,
    lifecycle: HostLifecycleHandle,
    waker: Waker,
}

impl App {
    fn new(
        gateway_url: Url,
        picker_runtime: tokio::runtime::Handle,
        lifecycle: HostLifecycleHandle,
        event_loop: &EventLoop<AppEvent>,
    ) -> Self {
        Self {
            bootstrap: Some(Bootstrap {
                gateway_url,
                picker_runtime,
                lifecycle,
                waker: Waker::new(event_loop),
            }),
            running: None,
            failure: Rc::new(RefCell::new(None)),
        }
    }

    fn take_failure(&self) -> Option<String> {
        self.failure.borrow_mut().take()
    }

    fn fail(&self, event_loop: &ActiveEventLoop, error: impl std::fmt::Display) {
        let message = error.to_string();
        tracing::error!(%message, "embedded Servo qualification failed");
        *self.failure.borrow_mut() = Some(message);
        event_loop.exit();
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(running) = &self.running {
            running.webview.focus();
            running.webview.set_throttled(false);
            running.window.request_redraw();
            running.lifecycle.set_visible(true);
            return;
        }

        let Some(bootstrap) = self.bootstrap.take() else {
            self.fail(
                event_loop,
                "embedded Servo cannot recreate its window after a failed initialization",
            );
            return;
        };
        match RunningApp::start(
            event_loop,
            bootstrap.gateway_url,
            bootstrap.picker_runtime,
            bootstrap.lifecycle,
            bootstrap.waker,
            self.failure.clone(),
        ) {
            Ok(running) => self.running = Some(running),
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(running) = &self.running {
            running.webview.blur();
            running.webview.set_throttled(true);
            running.lifecycle.set_visible(false);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::WakeServo => {
                if let Some(running) = &self.running {
                    running.servo.spin_event_loop();
                    if running.animating.get() {
                        running.window.request_redraw();
                    }
                }
            }
            AppEvent::PickDirectory(reply) => {
                if let Some(running) = &self.running {
                    running.pick_directory(reply);
                } else {
                    let _ = reply.send(Err("embedded Servo window is unavailable".into()));
                }
            }
            AppEvent::PickFiles(reply) => {
                if let Some(running) = &self.running {
                    running.pick_files(reply);
                } else {
                    let _ = reply.send(Err("embedded Servo window is unavailable".into()));
                }
            }
            AppEvent::NativePickerClosed => {
                if let Some(running) = &self.running {
                    running.window.focus_window();
                    running.webview.focus();
                    running.webview.set_throttled(false);
                }
            }
            AppEvent::RequestAttention => {
                if let Some(running) = &self.running {
                    use winit::window::UserAttentionType;
                    running.window.set_minimized(false);
                    running.window.focus_window();
                    running
                        .window
                        .request_user_attention(Some(UserAttentionType::Informational));
                    running.lifecycle.set_visible(true);
                }
            }
            AppEvent::ExitRequested => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(running) = self.running.as_mut() else {
            return;
        };
        if running.window.id() != window_id {
            return;
        }

        running.servo.spin_event_loop();
        if let Err(error) = running.handle_window_event(event_loop, event) {
            self.fail(event_loop, error);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(running) = &self.running else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        if running.animating.get() {
            running.servo.spin_event_loop();
            running.window.request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

struct RunningApp {
    // Drop order is load-bearing: WebView -> Servo -> GL context -> Window ->
    // temporary Servo storage.
    webview: WebView,
    servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    window: Rc<Window>,
    _servo_storage: TempDir,
    picker_runtime: tokio::runtime::Handle,
    event_loop: EventLoopProxy<AppEvent>,
    lifecycle: HostLifecycleHandle,
    pointer: DevicePoint,
    modifiers: ModifiersState,
    ime_active: bool,
    animating: Rc<Cell<bool>>,
}

impl RunningApp {
    fn start(
        event_loop: &ActiveEventLoop,
        gateway_url: Url,
        picker_runtime: tokio::runtime::Handle,
        lifecycle: HostLifecycleHandle,
        waker: Waker,
        failure: Rc<RefCell<Option<String>>>,
    ) -> Result<Self> {
        let window = Rc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("trouve")
                        .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT))
                        .with_min_inner_size(LogicalSize::new(MINIMUM_WIDTH, MINIMUM_HEIGHT)),
                )
                .context("creating the chrome-free Servo window")?,
        );
        let display_handle = event_loop
            .display_handle()
            .context("getting the display handle for embedded Servo")?;
        let window_handle = window
            .window_handle()
            .context("getting the window handle for embedded Servo")?;
        let rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window.inner_size())
                .map_err(|error| {
                    anyhow::anyhow!("creating Servo's window rendering context: {error:?}")
                })?,
        );
        rendering_context.make_current().map_err(|error| {
            anyhow::anyhow!("making Servo's window rendering context current: {error:?}")
        })?;

        // Retaining this TempDir gives ClientStorage a process-long lifetime
        // and creates a hard storage boundary between the web engine and
        // Trouve's database.
        let servo_storage =
            tempfile::tempdir().context("creating isolated temporary storage for Servo")?;
        let opts = Opts {
            config_dir: Some(servo_storage.path().to_path_buf()),
            temporary_storage: true,
            ..Opts::default()
        };
        let servo = ServoBuilder::default()
            .opts(opts)
            .preferences(qualification_preferences())
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        let animating = Rc::new(Cell::new(false));
        let app_event_loop = waker.0.clone();
        let delegate = Rc::new(EmbedderDelegate {
            window: window.clone(),
            event_loop: waker.0,
            lifecycle: lifecycle.clone(),
            allowed_origin: gateway_url.origin(),
            animating: animating.clone(),
            failure,
        });
        let webview = WebViewBuilder::new(&servo, rendering_context.clone())
            .url(gateway_url)
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(delegate)
            .build();
        webview.resize(window.inner_size());
        webview.focus();
        if let Some(theme) = window.theme() {
            webview.notify_theme_change(theme_from_winit(theme));
        }
        window.request_redraw();

        Ok(Self {
            webview,
            servo,
            rendering_context,
            window,
            _servo_storage: servo_storage,
            picker_runtime,
            event_loop: app_event_loop,
            lifecycle,
            pointer: DevicePoint::zero(),
            modifiers: ModifiersState::empty(),
            ime_active: false,
            animating,
        })
    }

    fn pick_directory(&self, reply: DirectoryPickerReply) {
        let dialog = AsyncFileDialog::new()
            .set_title("Open workspace (git repository)")
            .set_parent(self.window.as_ref());
        let event_loop = self.event_loop.clone();
        self.picker_runtime.spawn(async move {
            let selected = dialog
                .pick_folder()
                .await
                .map(|folder| folder.path().to_owned());
            let _ = reply.send(Ok(selected));
            let _ = event_loop.send_event(AppEvent::NativePickerClosed);
        });
    }

    fn pick_files(&self, reply: FilePickerReply) {
        let dialog = AsyncFileDialog::new()
            .set_title("Attach files to the prompt")
            .set_parent(self.window.as_ref());
        let event_loop = self.event_loop.clone();
        self.picker_runtime.spawn(async move {
            let result = match dialog.pick_files().await {
                Some(files) => {
                    let paths = files
                        .into_iter()
                        .map(|file| file.path().to_owned())
                        .collect();
                    tokio::task::spawn_blocking(move || read_selected_attachments(paths))
                        .await
                        .map_err(|_| "embedded Servo attachment reader was interrupted".to_string())
                        .and_then(|result| result.map(Some))
                }
                None => Ok(None),
            };
            let _ = reply.send(result);
            let _ = event_loop.send_event(AppEvent::NativePickerClosed);
        });
    }

    fn handle_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) -> Result<()> {
        match event {
            WindowEvent::CloseRequested => {
                self.lifecycle.request_close();
            }
            WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                self.rendering_context.make_current().map_err(|error| {
                    anyhow::anyhow!(
                        "making Servo's rendering context current for redraw: {error:?}"
                    )
                })?;
                self.webview.paint();
                self.rendering_context.present();
            }
            WindowEvent::Resized(size) => {
                self.webview.resize(size);
                self.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.webview
                    .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                self.webview.resize(self.window.inner_size());
                self.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                self.lifecycle.set_focused(focused);
                if focused {
                    self.webview.focus();
                    self.webview.set_throttled(false);
                } else {
                    self.webview.blur();
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.lifecycle.set_occluded(occluded);
                self.webview.set_throttled(occluded);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = point_from_winit(position);
                self.webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        self.pointer.into(),
                    )));
            }
            WindowEvent::CursorLeft { .. } => {
                self.webview
                    .notify_input_event(InputEvent::MouseLeftViewport(
                        MouseLeftViewportEvent::default(),
                    ));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.webview
                    .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        mouse_action_from_winit(state),
                        mouse_button_from_winit(button),
                        self.pointer.into(),
                    )));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (
                        f64::from(x) * WHEEL_LINE_WIDTH,
                        f64::from(y) * WHEEL_LINE_HEIGHT,
                    ),
                    MouseScrollDelta::PixelDelta(delta) => (delta.x, delta.y),
                };
                self.webview
                    .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta {
                            x,
                            y,
                            z: 0.0,
                            mode: WheelMode::DeltaPixel,
                        },
                        self.pointer.into(),
                    )));
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if !self.handle_editing_shortcut(&event) {
                    self.webview.notify_input_event(InputEvent::Keyboard(
                        keyboard_event_from_winit(&event, self.modifiers, self.ime_active),
                    ));
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Ime(event) => self.handle_ime_event(event),
            WindowEvent::Touch(touch) => {
                self.webview
                    .notify_input_event(InputEvent::Touch(TouchEvent::new(
                        touch_type_from_winit(touch.phase),
                        TouchId(touch.id as i32),
                        point_from_winit(touch.location).into(),
                        TouchPointerType::Touch,
                    )));
            }
            WindowEvent::PinchGesture { delta, .. } => {
                self.webview
                    .adjust_pinch_zoom(delta as f32 + 1.0, self.pointer);
            }
            WindowEvent::ThemeChanged(theme) => {
                self.webview.notify_theme_change(theme_from_winit(theme));
            }
            WindowEvent::DroppedFile(path) => {
                tracing::warn!(
                    path = %path.display(),
                    "native file drop ignored; file access must use the typed desktop-host capability"
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_editing_shortcut(&self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed || event.repeat {
            return false;
        }
        #[cfg(target_os = "macos")]
        let command_modifier = self.modifiers.super_key();
        #[cfg(not(target_os = "macos"))]
        let command_modifier = self.modifiers.control_key();
        if !command_modifier {
            return false;
        }
        let WinitKey::Character(character) = &event.logical_key else {
            return false;
        };
        let action = if character.eq_ignore_ascii_case("x") {
            EditingActionEvent::Cut
        } else if character.eq_ignore_ascii_case("c") {
            EditingActionEvent::Copy
        } else if character.eq_ignore_ascii_case("v") {
            EditingActionEvent::Paste
        } else {
            return false;
        };
        // Servo's native clipboard feature is intentionally disabled: the
        // engine may request an editing action, but OS clipboard access must be
        // provided through the typed desktop-host boundary.
        self.webview
            .notify_input_event(InputEvent::EditingAction(action));
        true
    }

    fn handle_ime_event(&mut self, event: Ime) {
        let event = match event {
            Ime::Enabled => {
                self.ime_active = true;
                ImeEvent::Composition(CompositionEvent {
                    state: CompositionState::Start,
                    data: String::new(),
                })
            }
            Ime::Preedit(data, _) => ImeEvent::Composition(CompositionEvent {
                state: CompositionState::Update,
                data,
            }),
            Ime::Commit(data) => {
                self.ime_active = false;
                ImeEvent::Composition(CompositionEvent {
                    state: CompositionState::End,
                    data,
                })
            }
            Ime::Disabled => {
                self.ime_active = false;
                ImeEvent::Dismissed
            }
        };
        self.webview.notify_input_event(InputEvent::Ime(event));
    }
}

struct EmbedderDelegate {
    window: Rc<Window>,
    event_loop: EventLoopProxy<AppEvent>,
    lifecycle: HostLifecycleHandle,
    allowed_origin: Origin,
    animating: Rc<Cell<bool>>,
    failure: Rc<RefCell<Option<String>>>,
}

impl servo::WebViewDelegate for EmbedderDelegate {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.window.request_redraw();
    }

    fn notify_animating_changed(&self, _webview: WebView, animating: bool) {
        self.animating.set(animating);
        let _ = self.event_loop.send_event(AppEvent::WakeServo);
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        self.window.set_title(
            title
                .filter(|title| !title.is_empty())
                .as_deref()
                .unwrap_or("trouve"),
        );
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: Cursor) {
        set_window_cursor(&self.window, cursor);
    }

    fn notify_closed(&self, _webview: WebView) {
        self.lifecycle.request_close();
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, backtrace: Option<String>) {
        let message = backtrace.map_or_else(
            || format!("embedded Servo content crashed: {reason}"),
            |backtrace| format!("embedded Servo content crashed: {reason}\n{backtrace}"),
        );
        tracing::error!(%message);
        *self.failure.borrow_mut() = Some(message);
        let _ = self.event_loop.send_event(AppEvent::ExitRequested);
    }

    fn notify_fullscreen_state_changed(&self, _webview: WebView, fullscreen: bool) {
        let fullscreen = fullscreen.then(|| Fullscreen::Borderless(self.window.current_monitor()));
        self.window.set_fullscreen(fullscreen);
    }

    fn request_navigation(&self, _webview: WebView, request: NavigationRequest) {
        if navigation_allowed(&self.allowed_origin, &request.url) {
            request.allow();
        } else {
            tracing::warn!(url = %request.url, "blocked navigation outside the desktop gateway");
            request.deny();
        }
    }

    fn request_unload(&self, _webview: WebView, request: servo::AllowOrDenyRequest) {
        request.allow();
    }

    fn request_protocol_handler(
        &self,
        _webview: WebView,
        _registration: servo::protocol_handler::ProtocolHandlerRegistration,
        request: servo::AllowOrDenyRequest,
    ) {
        request.deny();
    }

    fn request_permission(&self, _webview: WebView, request: servo::PermissionRequest) {
        request.deny();
    }

    fn request_create_new(
        &self,
        _parent_webview: WebView,
        _request: servo::CreateNewWebViewRequest,
    ) {
        tracing::warn!("blocked page request to create a second embedded webview");
    }

    fn show_embedder_control(&self, _webview: WebView, control: EmbedderControl) {
        if let EmbedderControl::InputMethod(input_method) = control {
            let rect = input_method.position();
            self.window.set_ime_allowed(true);
            self.window.set_ime_cursor_area(
                LogicalPosition::new(rect.min.x, rect.min.y),
                LogicalSize::new(
                    (rect.max.x - rect.min.x).max(1),
                    (rect.max.y - rect.min.y).max(1),
                ),
            );
        } else {
            // Servo request objects dismiss or deny safely when dropped. Native
            // dialogs must be implemented through HostNativeActions, not an
            // engine-owned side channel.
            tracing::warn!("unsupported Servo embedder control was safely dismissed");
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, _control_id: servo::EmbedderControlId) {
        self.window.set_ime_allowed(false);
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        match level {
            ConsoleLogLevel::Error => tracing::error!(target: "servo_console", "{message}"),
            ConsoleLogLevel::Warn => tracing::warn!(target: "servo_console", "{message}"),
            ConsoleLogLevel::Debug | ConsoleLogLevel::Trace => {
                tracing::debug!(target: "servo_console", "{message}")
            }
            _ => tracing::info!(target: "servo_console", "{message}"),
        }
    }
}

fn qualification_preferences() -> Preferences {
    let mut preferences = Preferences::default();
    for name in EXPERIMENTAL_PREFERENCES {
        preferences.set_value(name, PrefValue::Bool(true));
    }
    preferences
}

fn navigation_allowed(allowed_origin: &Origin, candidate: &Url) -> bool {
    candidate.origin() == *allowed_origin
}

fn point_from_winit(position: PhysicalPosition<f64>) -> DevicePoint {
    DevicePoint::new(position.x as f32, position.y as f32)
}

fn mouse_action_from_winit(state: ElementState) -> MouseButtonAction {
    match state {
        ElementState::Pressed => MouseButtonAction::Down,
        ElementState::Released => MouseButtonAction::Up,
    }
}

fn mouse_button_from_winit(button: WinitMouseButton) -> MouseButton {
    match button {
        WinitMouseButton::Left => MouseButton::Left,
        WinitMouseButton::Right => MouseButton::Right,
        WinitMouseButton::Middle => MouseButton::Middle,
        WinitMouseButton::Back => MouseButton::Back,
        WinitMouseButton::Forward => MouseButton::Forward,
        WinitMouseButton::Other(value) => MouseButton::Other(value),
    }
}

fn touch_type_from_winit(phase: TouchPhase) -> TouchEventType {
    match phase {
        TouchPhase::Started => TouchEventType::Down,
        TouchPhase::Moved => TouchEventType::Move,
        TouchPhase::Ended => TouchEventType::Up,
        TouchPhase::Cancelled => TouchEventType::Cancel,
    }
}

fn theme_from_winit(theme: winit::window::Theme) -> Theme {
    match theme {
        winit::window::Theme::Light => Theme::Light,
        winit::window::Theme::Dark => Theme::Dark,
    }
}

fn set_window_cursor(window: &Window, cursor: Cursor) {
    if cursor == Cursor::None {
        window.set_cursor_visible(false);
        return;
    }
    let icon = match cursor {
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move | Cursor::AllScroll => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize | Cursor::ColResize => CursorIcon::EwResize,
        Cursor::NsResize | Cursor::RowResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
        Cursor::None | Cursor::Default => CursorIcon::Default,
    };
    window.set_cursor_visible(true);
    window.set_cursor(icon);
}

fn keyboard_event_from_winit(
    event: &KeyEvent,
    modifiers: ModifiersState,
    is_composing: bool,
) -> KeyboardEvent {
    KeyboardEvent::new_without_event(
        match event.state {
            ElementState::Pressed => KeyState::Down,
            ElementState::Released => KeyState::Up,
        },
        key_from_winit(&event.logical_key),
        code_from_winit(event.physical_key),
        location_from_winit(event.location),
        modifiers_from_winit(modifiers),
        event.repeat,
        is_composing,
    )
}

fn key_from_winit(key: &WinitKey) -> Key {
    match key {
        WinitKey::Character(character) => Key::Character(character.to_string()),
        WinitKey::Named(named) => Key::Named(named_key_from_winit(*named)),
        WinitKey::Unidentified(_) | WinitKey::Dead(_) => Key::Named(NamedKey::Unidentified),
    }
}

fn named_key_from_winit(key: WinitNamedKey) -> NamedKey {
    match key {
        WinitNamedKey::Alt => NamedKey::Alt,
        WinitNamedKey::AltGraph => NamedKey::AltGraph,
        WinitNamedKey::ArrowDown => NamedKey::ArrowDown,
        WinitNamedKey::ArrowLeft => NamedKey::ArrowLeft,
        WinitNamedKey::ArrowRight => NamedKey::ArrowRight,
        WinitNamedKey::ArrowUp => NamedKey::ArrowUp,
        WinitNamedKey::Backspace => NamedKey::Backspace,
        WinitNamedKey::CapsLock => NamedKey::CapsLock,
        WinitNamedKey::ContextMenu => NamedKey::ContextMenu,
        WinitNamedKey::Control => NamedKey::Control,
        WinitNamedKey::Delete => NamedKey::Delete,
        WinitNamedKey::End => NamedKey::End,
        WinitNamedKey::Enter => NamedKey::Enter,
        WinitNamedKey::Escape => NamedKey::Escape,
        WinitNamedKey::F1 => NamedKey::F1,
        WinitNamedKey::F2 => NamedKey::F2,
        WinitNamedKey::F3 => NamedKey::F3,
        WinitNamedKey::F4 => NamedKey::F4,
        WinitNamedKey::F5 => NamedKey::F5,
        WinitNamedKey::F6 => NamedKey::F6,
        WinitNamedKey::F7 => NamedKey::F7,
        WinitNamedKey::F8 => NamedKey::F8,
        WinitNamedKey::F9 => NamedKey::F9,
        WinitNamedKey::F10 => NamedKey::F10,
        WinitNamedKey::F11 => NamedKey::F11,
        WinitNamedKey::F12 => NamedKey::F12,
        WinitNamedKey::Home => NamedKey::Home,
        WinitNamedKey::Insert => NamedKey::Insert,
        WinitNamedKey::Meta => NamedKey::Meta,
        WinitNamedKey::NumLock => NamedKey::NumLock,
        WinitNamedKey::PageDown => NamedKey::PageDown,
        WinitNamedKey::PageUp => NamedKey::PageUp,
        WinitNamedKey::Pause => NamedKey::Pause,
        WinitNamedKey::PrintScreen => NamedKey::PrintScreen,
        WinitNamedKey::ScrollLock => NamedKey::ScrollLock,
        WinitNamedKey::Shift => NamedKey::Shift,
        WinitNamedKey::Tab => NamedKey::Tab,
        _ => NamedKey::Unidentified,
    }
}

fn code_from_winit(key: PhysicalKey) -> Code {
    let PhysicalKey::Code(key) = key else {
        return Code::Unidentified;
    };
    match key {
        KeyCode::Backquote => Code::Backquote,
        KeyCode::Backslash => Code::Backslash,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::BracketLeft => Code::BracketLeft,
        KeyCode::BracketRight => Code::BracketRight,
        KeyCode::Comma => Code::Comma,
        KeyCode::Digit0 => Code::Digit0,
        KeyCode::Digit1 => Code::Digit1,
        KeyCode::Digit2 => Code::Digit2,
        KeyCode::Digit3 => Code::Digit3,
        KeyCode::Digit4 => Code::Digit4,
        KeyCode::Digit5 => Code::Digit5,
        KeyCode::Digit6 => Code::Digit6,
        KeyCode::Digit7 => Code::Digit7,
        KeyCode::Digit8 => Code::Digit8,
        KeyCode::Digit9 => Code::Digit9,
        KeyCode::Equal => Code::Equal,
        KeyCode::IntlBackslash => Code::IntlBackslash,
        KeyCode::IntlRo => Code::IntlRo,
        KeyCode::IntlYen => Code::IntlYen,
        KeyCode::KeyA => Code::KeyA,
        KeyCode::KeyB => Code::KeyB,
        KeyCode::KeyC => Code::KeyC,
        KeyCode::KeyD => Code::KeyD,
        KeyCode::KeyE => Code::KeyE,
        KeyCode::KeyF => Code::KeyF,
        KeyCode::KeyG => Code::KeyG,
        KeyCode::KeyH => Code::KeyH,
        KeyCode::KeyI => Code::KeyI,
        KeyCode::KeyJ => Code::KeyJ,
        KeyCode::KeyK => Code::KeyK,
        KeyCode::KeyL => Code::KeyL,
        KeyCode::KeyM => Code::KeyM,
        KeyCode::KeyN => Code::KeyN,
        KeyCode::KeyO => Code::KeyO,
        KeyCode::KeyP => Code::KeyP,
        KeyCode::KeyQ => Code::KeyQ,
        KeyCode::KeyR => Code::KeyR,
        KeyCode::KeyS => Code::KeyS,
        KeyCode::KeyT => Code::KeyT,
        KeyCode::KeyU => Code::KeyU,
        KeyCode::KeyV => Code::KeyV,
        KeyCode::KeyW => Code::KeyW,
        KeyCode::KeyX => Code::KeyX,
        KeyCode::KeyY => Code::KeyY,
        KeyCode::KeyZ => Code::KeyZ,
        KeyCode::Minus => Code::Minus,
        KeyCode::Period => Code::Period,
        KeyCode::Quote => Code::Quote,
        KeyCode::Semicolon => Code::Semicolon,
        KeyCode::Slash => Code::Slash,
        KeyCode::AltLeft => Code::AltLeft,
        KeyCode::AltRight => Code::AltRight,
        KeyCode::ControlLeft => Code::ControlLeft,
        KeyCode::ControlRight => Code::ControlRight,
        KeyCode::ShiftLeft => Code::ShiftLeft,
        KeyCode::ShiftRight => Code::ShiftRight,
        KeyCode::SuperLeft => Code::MetaLeft,
        KeyCode::SuperRight => Code::MetaRight,
        KeyCode::Enter => Code::Enter,
        KeyCode::Space => Code::Space,
        KeyCode::Tab => Code::Tab,
        KeyCode::Delete => Code::Delete,
        KeyCode::End => Code::End,
        KeyCode::Help => Code::Help,
        KeyCode::Home => Code::Home,
        KeyCode::Insert => Code::Insert,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::ArrowDown => Code::ArrowDown,
        KeyCode::ArrowLeft => Code::ArrowLeft,
        KeyCode::ArrowRight => Code::ArrowRight,
        KeyCode::ArrowUp => Code::ArrowUp,
        KeyCode::Escape => Code::Escape,
        KeyCode::F1 => Code::F1,
        KeyCode::F2 => Code::F2,
        KeyCode::F3 => Code::F3,
        KeyCode::F4 => Code::F4,
        KeyCode::F5 => Code::F5,
        KeyCode::F6 => Code::F6,
        KeyCode::F7 => Code::F7,
        KeyCode::F8 => Code::F8,
        KeyCode::F9 => Code::F9,
        KeyCode::F10 => Code::F10,
        KeyCode::F11 => Code::F11,
        KeyCode::F12 => Code::F12,
        KeyCode::Numpad0 => Code::Numpad0,
        KeyCode::Numpad1 => Code::Numpad1,
        KeyCode::Numpad2 => Code::Numpad2,
        KeyCode::Numpad3 => Code::Numpad3,
        KeyCode::Numpad4 => Code::Numpad4,
        KeyCode::Numpad5 => Code::Numpad5,
        KeyCode::Numpad6 => Code::Numpad6,
        KeyCode::Numpad7 => Code::Numpad7,
        KeyCode::Numpad8 => Code::Numpad8,
        KeyCode::Numpad9 => Code::Numpad9,
        KeyCode::NumpadAdd => Code::NumpadAdd,
        KeyCode::NumpadDecimal => Code::NumpadDecimal,
        KeyCode::NumpadDivide => Code::NumpadDivide,
        KeyCode::NumpadEnter => Code::NumpadEnter,
        KeyCode::NumpadEqual => Code::NumpadEqual,
        KeyCode::NumpadMultiply => Code::NumpadMultiply,
        KeyCode::NumpadSubtract => Code::NumpadSubtract,
        _ => Code::Unidentified,
    }
}

fn location_from_winit(location: KeyLocation) -> Location {
    match location {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    }
}

fn modifiers_from_winit(modifiers: ModifiersState) -> Modifiers {
    let mut result = Modifiers::empty();
    result.set(Modifiers::CONTROL, modifiers.control_key());
    result.set(Modifiers::SHIFT, modifiers.shift_key());
    result.set(Modifiers::ALT, modifiers.alt_key());
    result.set(Modifiers::META, modifiers.super_key());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_confined_to_the_exact_gateway_origin() {
        let gateway = Url::parse("http://127.0.0.1:43127/").unwrap();
        let origin = gateway.origin();

        assert!(navigation_allowed(
            &origin,
            &Url::parse("http://127.0.0.1:43127/thread/one#latest").unwrap()
        ));
        assert!(!navigation_allowed(
            &origin,
            &Url::parse("http://127.0.0.1:43128/").unwrap()
        ));
        assert!(!navigation_allowed(
            &origin,
            &Url::parse("https://127.0.0.1:43127/").unwrap()
        ));
        assert!(!navigation_allowed(
            &origin,
            &Url::parse("https://example.com/").unwrap()
        ));
    }

    #[test]
    fn qualification_enables_the_pinned_experimental_feature_set() {
        let preferences = qualification_preferences();
        assert!(preferences.layout_grid_enabled);
        assert!(preferences.dom_fontface_enabled);
        assert!(preferences.dom_intersection_observer_enabled);
        assert!(preferences.layout_container_queries_enabled);
    }

    #[test]
    fn common_physical_keys_have_dom_codes() {
        assert_eq!(
            code_from_winit(PhysicalKey::Code(KeyCode::KeyA)),
            Code::KeyA
        );
        assert_eq!(
            code_from_winit(PhysicalKey::Code(KeyCode::Enter)),
            Code::Enter
        );
        assert_eq!(
            code_from_winit(PhysicalKey::Code(KeyCode::ArrowLeft)),
            Code::ArrowLeft
        );
    }

    #[test]
    fn keyboard_selection_modifiers_reach_servo() {
        let modifiers = modifiers_from_winit(ModifiersState::SHIFT | ModifiersState::CONTROL);

        assert!(modifiers.contains(Modifiers::SHIFT));
        assert!(modifiers.contains(Modifiers::CONTROL));
        assert!(!modifiers.contains(Modifiers::ALT));
    }
}
