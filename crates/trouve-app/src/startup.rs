//! Pre-main-window desktop update preflight.

use std::time::Duration;

use anyhow::{Context as _, Result};
use slint::ComponentHandle as _;

use crate::StartupWindow;

const UPDATE_RESTART_ENV: &str = "TROUVE_UPDATE_RESTARTED_VERSION";

/// Consume the one-shot marker passed by the old executable after a
/// successful replacement. It prevents the replacement process from showing
/// a second update preflight before opening the main window.
pub fn take_restarted_version() -> Option<String> {
    let version = std::env::var(UPDATE_RESTART_ENV).ok();
    // Called before any app threads exist.
    unsafe {
        std::env::remove_var(UPDATE_RESTART_ENV);
    }
    version.filter(|version| version == env!("CARGO_PKG_VERSION"))
}

pub fn configure(window: &StartupWindow) {
    let weak = window.as_weak();
    window.on_retry(move || begin(weak.clone()));
}

pub fn begin(window: slint::Weak<StartupWindow>) {
    if crate::DEVELOPMENT_BUILD {
        continue_to_app(
            &window,
            "Self-update is disabled in development builds.".into(),
            "",
        );
        return;
    }

    set_stage(
        &window,
        "Checking for updates…",
        "Contacting the stable release channel",
        -1.0,
    );

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                show_failure(&window, format!("creating the update runtime: {error}"));
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
                    show_failure(&window, format!("{error:#}"));
                    return;
                }
            };

            let Some(release) = check.update else {
                set_stage(
                    &window,
                    "Starting trouve…",
                    &format!("Version {} is up to date", check.current),
                    1.0,
                );
                std::thread::sleep(Duration::from_millis(250));
                continue_to_app(
                    &window,
                    format!("Version {} is up to date.", check.current),
                    "Check again",
                );
                return;
            };

            let target_version = release.version.to_string();
            let artifact_name = release.artifact_name.clone();
            let progress_window = window.clone();
            let install = trouve_update::install_release_with_progress(&release, move |progress| {
                publish_install_progress(
                    &progress_window,
                    &target_version,
                    &artifact_name,
                    progress,
                );
            })
            .await;

            if let Err(error) = install {
                show_failure(&window, format!("{error:#}"));
                return;
            }

            set_stage(
                &window,
                "Update installed",
                &format!("Restarting into version {}…", release.version),
                1.0,
            );
            std::thread::sleep(Duration::from_millis(250));
            match restart_updated_app(&release.version.to_string()) {
                Ok(()) => {
                    let _ = window.upgrade_in_event_loop(|window| {
                        let _ = window.hide();
                        let _ = slint::quit_event_loop();
                    });
                }
                Err(error) => show_failure(
                    &window,
                    format!(
                        "version {} was installed, but the app could not restart: {error:#}",
                        release.version
                    ),
                ),
            }
        });
    });
}

fn publish_install_progress(
    window: &slint::Weak<StartupWindow>,
    version: &str,
    artifact_name: &str,
    progress: trouve_update::InstallProgress,
) {
    match progress {
        trouve_update::InstallProgress::FetchingChecksums => set_stage(
            window,
            "Preparing update…",
            "Fetching release checksums",
            0.05,
        ),
        trouve_update::InstallProgress::Downloading {
            received_bytes,
            total_bytes,
        } => {
            let (detail, overall) = match total_bytes.filter(|total| *total > 0) {
                Some(total) => {
                    let ratio = (received_bytes as f64 / total as f64).clamp(0.0, 1.0);
                    (
                        format!(
                            "{} of {} · {:.0}%",
                            human_bytes(received_bytes),
                            human_bytes(total),
                            ratio * 100.0
                        ),
                        0.08 + ratio as f32 * 0.78,
                    )
                }
                None => (format!("{} downloaded", human_bytes(received_bytes)), -1.0),
            };
            set_stage(
                window,
                &format!("Downloading version {version}…"),
                &detail,
                overall,
            );
        }
        trouve_update::InstallProgress::Verifying => {
            set_stage(window, "Verifying download…", artifact_name, 0.90)
        }
        trouve_update::InstallProgress::Extracting => set_stage(
            window,
            "Preparing installation…",
            "Unpacking the verified application",
            0.95,
        ),
        trouve_update::InstallProgress::Installing => set_stage(
            window,
            "Installing update…",
            "Replacing the application safely",
            0.99,
        ),
    }
}

fn set_stage(window: &slint::Weak<StartupWindow>, status: &str, detail: &str, progress: f32) {
    let status = status.to_string();
    let detail = detail.to_string();
    let _ = window.upgrade_in_event_loop(move |window| {
        window.set_failed(false);
        window.set_status(status.into());
        window.set_detail(detail.into());
        window.set_progress(progress);
    });
}

fn show_failure(window: &slint::Weak<StartupWindow>, error: String) {
    let error = concise_error(&error);
    let settings_status = format!("Startup update failed: {error}");
    let _ = window.upgrade_in_event_loop(move |window| {
        window.set_status("Couldn't update trouve".into());
        window.set_detail(error.into());
        window.set_progress(0.0);
        window.set_failed(true);
        window.set_continuation_status(settings_status.into());
        window.set_continuation_action("Try again".into());
    });
}

fn continue_to_app(
    window: &slint::Weak<StartupWindow>,
    settings_status: String,
    settings_action: &str,
) {
    let settings_action = settings_action.to_string();
    let _ = window.upgrade_in_event_loop(move |window| {
        window.invoke_continue_startup(settings_status.into(), settings_action.into());
    });
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
    fn download_sizes_use_readable_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn updater_errors_are_single_line_and_bounded() {
        assert_eq!(concise_error("one\n two"), "one two");
        assert_eq!(concise_error(&"x".repeat(300)).chars().count(), 240);
    }
}
