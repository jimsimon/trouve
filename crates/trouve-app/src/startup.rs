//! Native Wry startup updater and runtime update manager.
//!
//! Product releases check and install before the embedded server and main
//! frontend start. Development builds bypass this module entirely.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event as TaoEvent, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::platform::run_return::EventLoopExtRunReturn as _;
use tao::window::WindowBuilder;
use trouve_desktop_host::{DesktopUpdatePhase, DesktopUpdateState, HostPreferences};
use wry::{NewWindowResponse, WebViewBuilder};

use super::AppEvent;

const UPDATE_RESTART_ENV: &str = "TROUVE_UPDATE_RESTARTED_VERSION";
const RUNTIME_POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

const SPLASH_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Updating trouve</title>
<style>
:root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
* { box-sizing: border-box; }
body { margin: 0; min-height: 100vh; display: grid; place-items: center; color: #eef1f8;
  background: radial-gradient(circle at 50% 20%, #26304d 0, #171b29 48%, #11141d 100%); }
main { width: min(430px, calc(100vw - 52px)); text-align: center; }
.logo { width: 72px; height: 72px; margin: 0 auto 22px; display: grid; place-items: center;
  border-radius: 20px; color: #fff; font: 700 42px/1 Georgia, serif;
  background: linear-gradient(145deg, #7f7cff, #4d68dd); box-shadow: 0 18px 45px #06081299; }
h1 { margin: 0; font-size: 21px; font-weight: 650; letter-spacing: -.01em; }
p { min-height: 20px; margin: 9px 0 18px; color: #aeb7cc; font-size: 13px; }
progress { width: 100%; height: 7px; appearance: none; border: 0; border-radius: 99px; overflow: hidden; }
progress::-webkit-progress-bar { background: #2b3142; }
progress::-webkit-progress-value { background: linear-gradient(90deg, #7775ff, #91a4ff); }
progress::-moz-progress-bar { background: linear-gradient(90deg, #7775ff, #91a4ff); }
.actions { display: flex; justify-content: center; gap: 10px; margin-top: 20px; }
.actions[hidden] { display: none; }
button { border: 1px solid #46506a; border-radius: 7px; padding: 8px 14px; color: #e8ecf7;
  background: #252b3a; font: inherit; cursor: pointer; }
button.primary { border-color: #7478ff; background: #6266df; color: white; }
.version { margin-top: 18px; color: #707a91; font-size: 11px; }
</style>
</head>
<body>
<main>
  <div class="logo" aria-hidden="true">t</div>
  <section id="announcement" role="status" aria-live="polite" aria-atomic="true">
    <h1 id="status">Checking for updates…</h1>
    <p id="detail">Contacting the stable release channel</p>
  </section>
  <progress id="progress" max="100" aria-label="Update progress"></progress>
  <div id="actions" class="actions" role="group" aria-label="Update recovery actions" hidden>
    <button id="retry" class="primary" onclick="location.href='https://startup.trouve/retry'">Retry</button>
    <button onclick="location.href='https://startup.trouve/continue'">Open trouve</button>
  </div>
  <div class="version">trouve v__VERSION__</div>
</main>
<script>
window.__trouveStage = (status, detail, progress, failed) => {
  const announcement = document.getElementById("announcement");
  announcement.setAttribute("aria-live", failed ? "assertive" : "polite");
  document.getElementById("status").textContent = status;
  document.getElementById("detail").textContent = detail;
  const bar = document.getElementById("progress");
  if (progress === null) bar.removeAttribute("value");
  else bar.value = progress;
  const actions = document.getElementById("actions");
  const revealActions = failed && actions.hidden;
  actions.hidden = !failed;
  if (revealActions) requestAnimationFrame(() => document.getElementById("retry").focus());
};
</script>
</body>
</html>"#;

pub(crate) enum Event {
    Stage {
        status: String,
        detail: String,
        progress_percent: Option<u8>,
    },
    Failed(String),
    Continue(DesktopUpdateState),
    Retry,
    Open,
    ExitProcess,
}

pub(crate) struct PreflightResult {
    pub exit_process: bool,
    pub update_state: DesktopUpdateState,
}

impl PreflightResult {
    pub fn continue_without_update() -> Self {
        Self {
            exit_process: false,
            update_state: idle_state("Desktop updates are available in the packaged app."),
        }
    }
}

pub(crate) fn run_preflight(event_loop: &mut EventLoop<AppEvent>) -> Result<PreflightResult> {
    if cfg!(debug_assertions) {
        return Ok(PreflightResult {
            exit_process: false,
            update_state: state(
                DesktopUpdatePhase::Disabled,
                None,
                "Self-update is disabled in development builds.",
                None,
            ),
        });
    }

    if let Some(version) = take_restarted_version() {
        return Ok(PreflightResult {
            exit_process: false,
            update_state: idle_state(&format!("Version {version} was installed successfully.")),
        });
    }

    let preferences = super::web_preview_support::preference_path()
        .as_deref()
        .map(|path| trouve_desktop_host::load_host_preferences(path, HostPreferences::default()))
        .transpose()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "loading desktop update preference failed");
            None
        })
        .unwrap_or_default();

    if !trouve_update::auto_update_enabled() {
        return Ok(PreflightResult {
            exit_process: false,
            update_state: idle_state(
                "Automatic updates are disabled by TROUVE_DISABLE_AUTO_UPDATE. Manual checks remain available.",
            ),
        });
    }
    if !preferences.general.automatic_updates {
        return Ok(PreflightResult {
            exit_process: false,
            update_state: idle_state(
                "Automatic updates are off. You can still check manually in Settings.",
            ),
        });
    }

    let window = WindowBuilder::new()
        .with_title("trouve")
        .with_inner_size(LogicalSize::new(520, 330))
        .with_resizable(false)
        .build(event_loop)?;
    let proxy = event_loop.create_proxy();
    let navigation_proxy = proxy.clone();
    let html = SPLASH_HTML.replace("__VERSION__", env!("CARGO_PKG_VERSION"));
    let builder = WebViewBuilder::new()
        .with_html(html)
        .with_navigation_handler(move |url| {
            let event = match url.as_str() {
                "https://startup.trouve/retry" | "https://startup.trouve/retry/" => {
                    Some(Event::Retry)
                }
                "https://startup.trouve/continue" | "https://startup.trouve/continue/" => {
                    Some(Event::Open)
                }
                _ => None,
            };
            if let Some(event) = event {
                let _ = navigation_proxy.send_event(AppEvent::Startup(event));
            }
            false
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_drag_drop_handler(|_| true);

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let webview = builder.build(&window)?;
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        let container = window
            .default_vbox()
            .ok_or_else(|| anyhow::anyhow!("startup window has no GTK container"))?;
        builder.build_gtk(container)?
    };

    let mut preflight_cancel = Arc::new(AtomicBool::new(false));
    spawn_preflight(proxy.clone(), Arc::clone(&preflight_cancel));
    let mut result = None;
    let mut last_failure = String::new();
    let mut preflight_running = true;
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            TaoEvent::UserEvent(AppEvent::Startup(Event::Stage {
                status,
                detail,
                progress_percent,
            })) => {
                render_stage(&webview, &status, &detail, progress_percent, false);
            }
            TaoEvent::UserEvent(AppEvent::Startup(Event::Failed(error))) => {
                preflight_running = false;
                last_failure = concise_error(&error);
                render_stage(
                    &webview,
                    "Couldn't update trouve",
                    &last_failure,
                    Some(0),
                    true,
                );
            }
            TaoEvent::UserEvent(AppEvent::Startup(Event::Retry)) => {
                if preflight_running {
                    return;
                }
                preflight_running = true;
                last_failure.clear();
                preflight_cancel = Arc::new(AtomicBool::new(false));
                render_stage(
                    &webview,
                    "Checking for updates…",
                    "Contacting the stable release channel",
                    None,
                    false,
                );
                spawn_preflight(proxy.clone(), Arc::clone(&preflight_cancel));
            }
            TaoEvent::UserEvent(AppEvent::Startup(Event::Open)) => {
                if preflight_running {
                    return;
                }
                result = Some(PreflightResult {
                    exit_process: false,
                    update_state: state(
                        DesktopUpdatePhase::Error,
                        None,
                        &format!("Startup update failed: {last_failure}"),
                        None,
                    ),
                });
                *control_flow = ControlFlow::Exit;
            }
            TaoEvent::UserEvent(AppEvent::Startup(Event::Continue(update))) => {
                result = Some(PreflightResult {
                    exit_process: false,
                    update_state: update,
                });
                *control_flow = ControlFlow::Exit;
            }
            TaoEvent::UserEvent(AppEvent::Startup(Event::ExitProcess)) => {
                result = Some(PreflightResult {
                    exit_process: true,
                    update_state: state(
                        DesktopUpdatePhase::Restarting,
                        None,
                        "Restarting into the installed update…",
                        Some(100),
                    ),
                });
                *control_flow = ControlFlow::Exit;
            }
            TaoEvent::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if window_id == window.id() => {
                preflight_cancel.store(true, Ordering::Release);
                result = Some(PreflightResult {
                    exit_process: true,
                    update_state: idle_state("Update cancelled."),
                });
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });

    drop(webview);
    drop(window);
    Ok(result.unwrap_or_else(|| PreflightResult {
        exit_process: true,
        update_state: idle_state("Update cancelled."),
    }))
}

fn spawn_preflight(proxy: tao::event_loop::EventLoopProxy<AppEvent>, cancelled: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                send(
                    &proxy,
                    Event::Failed(format!("creating update runtime: {error}")),
                );
                return;
            }
        };
        runtime.block_on(async move {
            let check = match trouve_update::check(
                trouve_update::Component::Desktop,
                env!("CARGO_PKG_VERSION"),
            )
            .await
            {
                Ok(check) => check,
                Err(error) => {
                    send(&proxy, Event::Failed(format!("{error:#}")));
                    return;
                }
            };
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let Some(release) = check.update else {
                send(
                    &proxy,
                    Event::Stage {
                        status: "Starting trouve…".into(),
                        detail: format!("Version {} is up to date", check.current),
                        progress_percent: Some(100),
                    },
                );
                std::thread::sleep(Duration::from_millis(250));
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                send(
                    &proxy,
                    Event::Continue(idle_state(&format!(
                        "Version {} is up to date.",
                        check.current
                    ))),
                );
                return;
            };

            let version = release.version.to_string();
            let artifact = release.artifact_name.clone();
            let progress_proxy = proxy.clone();
            let progress_cancelled = Arc::clone(&cancelled);
            let install_cancelled = Arc::clone(&cancelled);
            if let Err(error) = trouve_update::install_release_with_progress_and_cancel(
                &release,
                move |progress| {
                    if progress_cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    let (status, detail, percent) = install_stage(&version, &artifact, progress);
                    send(
                        &progress_proxy,
                        Event::Stage {
                            status,
                            detail,
                            progress_percent: percent,
                        },
                    );
                },
                move || install_cancelled.load(Ordering::Acquire),
            )
            .await
            {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                send(&proxy, Event::Failed(format!("{error:#}")));
                return;
            }

            if cancelled.load(Ordering::Acquire) {
                return;
            }
            send(
                &proxy,
                Event::Stage {
                    status: "Update installed".into(),
                    detail: format!("Restarting into version {}…", release.version),
                    progress_percent: Some(100),
                },
            );
            std::thread::sleep(Duration::from_millis(250));
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            match restart_updated_app(&release.version.to_string()) {
                Ok(()) => send(&proxy, Event::ExitProcess),
                Err(error) => send(
                    &proxy,
                    Event::Failed(format!(
                        "version {} was installed, but the app could not restart: {error:#}",
                        release.version
                    )),
                ),
            }
        });
    });
}

fn render_stage(
    webview: &wry::WebView,
    status: &str,
    detail: &str,
    progress_percent: Option<u8>,
    failed: bool,
) {
    let status = serde_json::to_string(status).unwrap_or_else(|_| "\"Updating trouve…\"".into());
    let detail = serde_json::to_string(detail).unwrap_or_else(|_| "\"\"".into());
    let progress = progress_percent.map_or_else(|| "null".to_string(), |value| value.to_string());
    let script = format!("window.__trouveStage({status},{detail},{progress},{failed});");
    if let Err(error) = webview.evaluate_script(&script) {
        tracing::warn!(%error, "rendering desktop update progress failed");
    }
}

fn send(proxy: &tao::event_loop::EventLoopProxy<AppEvent>, event: Event) {
    let _ = proxy.send_event(AppEvent::Startup(event));
}

fn take_restarted_version() -> Option<String> {
    let version = std::env::var(UPDATE_RESTART_ENV).ok();
    // Called before the app creates worker threads.
    unsafe {
        std::env::remove_var(UPDATE_RESTART_ENV);
    }
    version.filter(|version| version == env!("CARGO_PKG_VERSION"))
}

pub(crate) fn restart_updated_app(version: &str) -> Result<()> {
    let executable = std::env::current_exe().context("locating the updated executable")?;
    std::process::Command::new(&executable)
        .args(std::env::args_os().skip(1))
        .env(UPDATE_RESTART_ENV, version)
        .spawn()
        .with_context(|| format!("starting {}", executable.display()))?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct UpdateManager {
    inner: Arc<Mutex<UpdateManagerInner>>,
    action: Arc<tokio::sync::Mutex<()>>,
}

struct UpdateManagerInner {
    state: DesktopUpdateState,
    release: Option<trouve_update::Release>,
}

impl UpdateManager {
    pub fn new(initial: DesktopUpdateState) -> Self {
        Self {
            inner: Arc::new(Mutex::new(UpdateManagerInner {
                state: initial,
                release: None,
            })),
            action: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn status(&self) -> DesktopUpdateState {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state
            .clone()
    }

    pub async fn check(&self) -> DesktopUpdateState {
        let _action = self.action.lock().await;
        self.check_inner().await
    }

    async fn check_inner(&self) -> DesktopUpdateState {
        self.set_state(state(
            DesktopUpdatePhase::Checking,
            self.available_version(),
            "Checking the stable release channel…",
            None,
        ));
        match trouve_update::check(trouve_update::Component::Desktop, env!("CARGO_PKG_VERSION"))
            .await
        {
            Ok(check) => {
                let mut inner = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(release) = check.update {
                    let version = release.version.to_string();
                    inner.release = Some(release);
                    inner.state = state(
                        DesktopUpdatePhase::Available,
                        Some(version.clone()),
                        &format!(
                            "Version {version} is available. It will be verified before installation."
                        ),
                        None,
                    );
                } else {
                    inner.release = None;
                    inner.state = idle_state(&format!("Version {} is up to date.", check.current));
                }
                inner.state.clone()
            }
            Err(error) => {
                let update = state(
                    DesktopUpdatePhase::Error,
                    self.available_version(),
                    &format!(
                        "Update check failed: {}",
                        concise_error(&format!("{error:#}"))
                    ),
                    None,
                );
                self.set_state(update.clone());
                update
            }
        }
    }

    pub async fn install_and_restart(&self) -> DesktopUpdateState {
        let _action = self.action.lock().await;
        let release = match self.release() {
            Some(release) => release,
            None => {
                let checked = self.check_inner().await;
                if checked.phase != DesktopUpdatePhase::Available {
                    return checked;
                }
                match self.release() {
                    Some(release) => release,
                    None => return self.status(),
                }
            }
        };
        let version = release.version.to_string();
        let artifact = release.artifact_name.clone();
        let progress_manager = self.clone();
        let install = trouve_update::install_release_with_progress(&release, move |progress| {
            let (phase, message, percent) = runtime_install_stage(&version, &artifact, progress);
            progress_manager.set_state(state(phase, Some(version.clone()), &message, percent));
        })
        .await;

        if let Err(error) = install {
            let update = state(
                DesktopUpdatePhase::Error,
                Some(release.version.to_string()),
                &format!(
                    "Update installation failed: {}",
                    concise_error(&format!("{error:#}"))
                ),
                None,
            );
            self.set_state(update.clone());
            return update;
        }

        let version = release.version.to_string();
        let update = state(
            DesktopUpdatePhase::Restarting,
            Some(version.clone()),
            &format!("Version {version} is installed. Restarting…"),
            Some(100),
        );
        self.set_state(update.clone());
        update
    }

    pub fn restart_failed(&self, version: &str, error: &str) -> DesktopUpdateState {
        let update = state(
            DesktopUpdatePhase::Error,
            Some(version.to_string()),
            &format!(
                "Version {version} is installed, but trouve could not restart: {}. Keep using this window or restart manually.",
                concise_error(error)
            ),
            None,
        );
        self.set_state(update.clone());
        update
    }

    pub fn spawn_runtime_poll(&self, preference_path: Option<PathBuf>) {
        if !trouve_update::auto_update_enabled() {
            return;
        }
        let manager = self.clone();
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                loop {
                    tokio::time::sleep(RUNTIME_POLL_INTERVAL).await;
                    let enabled = preference_path
                        .as_deref()
                        .and_then(|path| {
                            trouve_desktop_host::load_host_preferences(
                                path,
                                HostPreferences::default(),
                            )
                            .ok()
                        })
                        .is_none_or(|preferences| preferences.general.automatic_updates);
                    if enabled && manager.status().phase != DesktopUpdatePhase::Available {
                        let _ = manager.check().await;
                    }
                }
            });
        });
    }

    fn release(&self) -> Option<trouve_update::Release> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release
            .clone()
    }

    fn available_version(&self) -> Option<String> {
        self.status().available_version
    }

    fn set_state(&self, state: DesktopUpdateState) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state = state;
    }
}

fn install_stage(
    version: &str,
    artifact: &str,
    progress: trouve_update::InstallProgress,
) -> (String, String, Option<u8>) {
    match progress {
        trouve_update::InstallProgress::FetchingChecksums => (
            "Preparing update…".into(),
            "Fetching release checksums".into(),
            Some(5),
        ),
        trouve_update::InstallProgress::Downloading {
            received_bytes,
            total_bytes,
        } => {
            let percent = total_bytes
                .filter(|total| *total > 0)
                .map(|total| ((received_bytes.saturating_mul(100) / total).min(100)) as u8);
            (
                format!("Downloading version {version}…"),
                match total_bytes {
                    Some(total) if total > 0 => format!(
                        "{} of {} · {}%",
                        human_bytes(received_bytes),
                        human_bytes(total),
                        percent.unwrap_or_default()
                    ),
                    _ => format!("{} downloaded", human_bytes(received_bytes)),
                },
                percent.map(|value| 8 + (u16::from(value) * 78 / 100) as u8),
            )
        }
        trouve_update::InstallProgress::Verifying => {
            ("Verifying download…".into(), artifact.into(), Some(90))
        }
        trouve_update::InstallProgress::Extracting => (
            "Preparing installation…".into(),
            "Unpacking the verified application".into(),
            Some(95),
        ),
        trouve_update::InstallProgress::Installing => (
            "Installing update…".into(),
            "Replacing the application safely".into(),
            Some(99),
        ),
    }
}

fn runtime_install_stage(
    version: &str,
    artifact: &str,
    progress: trouve_update::InstallProgress,
) -> (DesktopUpdatePhase, String, Option<u8>) {
    let (status, detail, percent) = install_stage(version, artifact, progress);
    let phase = match progress {
        trouve_update::InstallProgress::Downloading { .. } => DesktopUpdatePhase::Downloading,
        trouve_update::InstallProgress::Verifying => DesktopUpdatePhase::Verifying,
        trouve_update::InstallProgress::FetchingChecksums
        | trouve_update::InstallProgress::Extracting
        | trouve_update::InstallProgress::Installing => DesktopUpdatePhase::Installing,
    };
    (phase, format!("{status} {detail}"), percent)
}

fn state(
    phase: DesktopUpdatePhase,
    available_version: Option<String>,
    message: &str,
    progress_percent: Option<u8>,
) -> DesktopUpdateState {
    DesktopUpdateState {
        current_version: env!("CARGO_PKG_VERSION").into(),
        available_version,
        phase,
        message: concise_error(message),
        progress_percent,
    }
}

fn idle_state(message: &str) -> DesktopUpdateState {
    state(DesktopUpdatePhase::Idle, None, message, None)
}

fn concise_error(error: &str) -> String {
    const MAX_CHARS: usize = 240;
    let single_line = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_CHARS {
        return single_line;
    }
    let mut shortened = single_line.chars().take(MAX_CHARS - 1).collect::<String>();
    shortened.push('…');
    shortened
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_messages_are_single_line_and_bounded() {
        assert_eq!(concise_error("one\n two"), "one two");
        assert_eq!(concise_error(&"x".repeat(300)).chars().count(), 240);
    }

    #[test]
    fn download_sizes_use_readable_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn progress_maps_download_into_bounded_startup_range() {
        let (_, _, progress) = install_stage(
            "4.1.0",
            "trouve-v4.1.0-x86_64-unknown-linux-gnu.tar.gz",
            trouve_update::InstallProgress::Downloading {
                received_bytes: 50,
                total_bytes: Some(100),
            },
        );
        assert_eq!(progress, Some(47));
    }

    #[test]
    fn startup_splash_announces_progress_and_focuses_recovery() {
        assert!(SPLASH_HTML.contains("role=\"status\" aria-live=\"polite\""));
        assert!(SPLASH_HTML.contains("aria-label=\"Update progress\""));
        assert!(SPLASH_HTML.contains("aria-label=\"Update recovery actions\" hidden"));
        assert!(SPLASH_HTML.contains("document.getElementById(\"retry\").focus()"));
    }

    #[test]
    fn failed_runtime_restart_keeps_a_recoverable_update_state() {
        let manager = UpdateManager::new(idle_state("ready"));
        let update = manager.restart_failed("4.1.0", "spawn failed");
        assert_eq!(update.phase, DesktopUpdatePhase::Error);
        assert_eq!(update.available_version.as_deref(), Some("4.1.0"));
        assert!(update.message.contains("Keep using this window"));
        assert_eq!(manager.status(), update);
    }
}
