//! Feature-gated system-webview qualification host (ADR 0023).
//!
//! This is intentionally a second binary while Slint remains the product
//! default and rollback path. It connects to an explicitly selected protocol
//! server, then loads a packaged, runtime-directory, or loopback Vite Lit
//! frontend exclusively through the hardened loopback gateway.

mod opener;
mod sleep;
mod web_preview_support;

use std::cell::RefCell;
use std::fs::File;
use std::io::Read as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rfd::AsyncFileDialog;
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use tokio::sync::oneshot;
use trouve_desktop_host::{
    FrontendSource, HostLifecycleHandle, HostNativeActions, LocalFileAction,
    MAX_NATIVE_ATTACHMENT_BYTES, MAX_NATIVE_ATTACHMENT_TOTAL_BYTES, MAX_NATIVE_ATTACHMENTS,
    NativeAttachment, NativeNotification, WindowGeometry,
};
use web_preview_support::WebPreviewHost;
use wry::{NewWindowResponse, WebViewBuilder};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

type DirectoryPickerReply = oneshot::Sender<Result<Option<PathBuf>, String>>;
type FilePickerReply = oneshot::Sender<Result<Option<Vec<NativeAttachment>>, String>>;

enum AppEvent {
    PickDirectory(DirectoryPickerReply),
    PickFiles(FilePickerReply),
    NativePickerClosed,
    RequestAttention,
    QuitNow,
}

const MAX_CLIPBOARD_RGBA_BYTES: usize = 64 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let bundled = WEB_ASSETS_BUNDLED.then(bundled_web_assets).transpose()?;
    let frontend = FrontendSource::from_preview_environment(bundled, cfg!(debug_assertions))?;

    let mut event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let directory_proxy = event_loop.create_proxy();
    let file_proxy = event_loop.create_proxy();
    let quit_proxy = event_loop.create_proxy();
    let attention_proxy = event_loop.create_proxy();
    let lifecycle = HostLifecycleHandle::default();
    let notification_lifecycle = lifecycle.clone();
    let sleep_inhibitor = Arc::new(Mutex::new(sleep::SleepInhibitor::default()));
    let sleep_for_action = sleep_inhibitor.clone();
    let native_actions = HostNativeActions::default()
        .with_window_geometry()
        // Tao exposes focus and foreground transitions but no desktop
        // occlusion event, so that capability remains explicitly false.
        .with_lifecycle_capabilities(lifecycle.clone(), true, false)
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
        .with_local_file_handler(|file, action| {
            let target = match action {
                LocalFileAction::Open => file.as_path(),
                LocalFileAction::Reveal => file
                    .as_path()
                    .parent()
                    .ok_or_else(|| "session file has no parent directory".to_string())?,
            };
            opener::open(target);
            Ok(())
        })
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
        .with_external_https_opener(|url| {
            opener::open(url.as_url().as_str());
            Ok(())
        });
    let host = WebPreviewHost::start_with_native_actions(frontend, native_actions)?;
    let picker_runtime = host.runtime_handle();
    let picker_closed_proxy = event_loop.create_proxy();
    let gateway_origin = host.gateway_origin().to_owned();
    let allowed_origin = gateway_origin.clone();
    let allowed_prefix = format!("{allowed_origin}/");
    let restored_geometry = host.initial_preferences().geometry.clone();
    let mut window_builder = WindowBuilder::new()
        .with_title("trouve — web preview")
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
    let mut geometry_save_deadline: Option<Instant> = None;
    let exit_code = event_loop.run_return(move |event, _, control_flow| {
        *control_flow = geometry_save_deadline
            .map(ControlFlow::WaitUntil)
            .unwrap_or(ControlFlow::Wait);
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
                lifecycle.request_close();
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
                let deadline = Instant::now() + Duration::from_millis(350);
                geometry_save_deadline = Some(deadline);
                *control_flow = ControlFlow::WaitUntil(deadline);
            }
            Event::MainEventsCleared
                if geometry_save_deadline.is_some_and(|deadline| Instant::now() >= deadline) =>
            {
                geometry_save_deadline = None;
                let next = geometry_for_events.borrow().clone();
                if let Err(error) =
                    picker_runtime.block_on(preferences.update_window_geometry(next))
                {
                    tracing::warn!(%error, "persisting desktop window geometry failed");
                }
                *control_flow = ControlFlow::Wait;
            }
            Event::WindowEvent {
                event: WindowEvent::Destroyed,
                ..
            } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });

    host.persist_window_geometry(geometry.borrow().clone())?;
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
    // Match the Slint frontend: text wins when a rich clipboard advertises
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
