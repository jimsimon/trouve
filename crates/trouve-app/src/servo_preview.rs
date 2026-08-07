//! External Servo qualification runner for the Lit desktop frontend (ADR 0023).
//!
//! This binary is deliberately not a shipping desktop host. It launches the
//! pinned Servo shell as a separate process so the real packaged frontend can
//! be exercised while Servo's embedding, accessibility, lifecycle, and native
//! capability gaps are evaluated. Wry remains the product frontend.
//!
//! Servo 0.4.0's experimental web-platform bundle is an explicit qualification
//! condition: without it CSS Grid is disabled and the Lit shell's layout is not
//! representative. A future Servo version must be requalified before changing
//! either the pinned version or this platform-feature requirement.

mod opener;
mod web_preview_support;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use trouve_desktop_host::FrontendSource;
use web_preview_support::WebPreviewHost;

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

const SERVO_BINARY_ENV: &str = "TROUVE_SERVO_BIN";
const EXPECTED_SERVO_VERSION: &str = "Servo 0.4.0-e8dbc1dfb";
const INTERRUPT_GRACE_PERIOD: Duration = Duration::from_secs(2);

// Servo 0.4.0 leaves CSS Grid and several related APIs disabled by default.
// The Lit shell relies on Grid for its primary layout, so the experimental
// platform bundle is required for a meaningful qualification run.
const SERVO_ARGUMENTS: &[&str] = &[
    "--temporary-storage",
    "--user-agent=desktop",
    "--window-size=1400x900",
    "--enable-experimental-web-platform-features",
];

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let bundled = WEB_ASSETS_BUNDLED.then(bundled_web_assets).transpose()?;
    let frontend = FrontendSource::from_preview_environment(bundled, true)?;

    let servo_binary = required_servo_binary(std::env::var_os(SERVO_BINARY_ENV))?;
    verify_servo_version(&servo_binary)?;

    let host = WebPreviewHost::start(frontend)?;
    let gateway_origin = host.gateway_origin().to_owned();
    tracing::warn!(
        servo = %servo_binary.display(),
        %gateway_origin,
        experimental_web_platform_features = true,
        "launching qualification-only Servo shell with required experimental web-platform features; this is not the shipping desktop host"
    );

    let run_result = run_servoshell(&servo_binary, &gateway_origin);
    host.shutdown();
    run_result
}

fn required_servo_binary(value: Option<std::ffi::OsString>) -> Result<PathBuf> {
    let Some(value) = value else {
        bail!(
            "{SERVO_BINARY_ENV} is required and must point to the Servo 0.4.0 servoshell executable"
        );
    };
    if value.is_empty() {
        bail!("{SERVO_BINARY_ENV} cannot be empty");
    }

    let configured = PathBuf::from(value);
    let binary = configured
        .canonicalize()
        .with_context(|| format!("resolving {SERVO_BINARY_ENV} path {}", configured.display()))?;
    let metadata = binary
        .metadata()
        .with_context(|| format!("reading Servo binary metadata at {}", binary.display()))?;
    if !metadata.is_file() {
        bail!(
            "{SERVO_BINARY_ENV} must point to a servoshell executable file; got {}",
            binary.display()
        );
    }
    Ok(binary)
}

fn verify_servo_version(binary: &Path) -> Result<()> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", binary.display()))?;
    let actual = combined_output(&output.stdout, &output.stderr);

    if !output.status.success() {
        bail!(
            "Servo version check failed with status {}; expected {EXPECTED_SERVO_VERSION}, actual: {}",
            output.status,
            display_output(&actual)
        );
    }
    ensure_supported_servo_version(&actual)
}

fn ensure_supported_servo_version(actual: &str) -> Result<()> {
    let matches = actual
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("Version: ").unwrap_or(line))
        .any(|version| version == EXPECTED_SERVO_VERSION);
    if !matches {
        bail!(
            "unsupported Servo version; expected {EXPECTED_SERVO_VERSION}, actual: {}",
            display_output(actual)
        );
    }
    Ok(())
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    match (stdout.trim(), stderr.trim()) {
        ("", "") => String::new(),
        (stdout, "") => stdout.to_owned(),
        ("", stderr) => stderr.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn display_output(actual: &str) -> String {
    const LIMIT: usize = 240;

    let escaped = actual.trim().replace(['\r', '\n'], " ");
    if escaped.is_empty() {
        return "<no output>".to_owned();
    }
    let mut chars = escaped.chars();
    let prefix = chars.by_ref().take(LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn run_servoshell(binary: &Path, gateway_origin: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("creating the Servo qualification runtime")?;
    runtime.block_on(run_servoshell_async(binary, gateway_origin))
}

async fn run_servoshell_async(binary: &Path, gateway_origin: &str) -> Result<()> {
    let mut command = Command::new(binary);
    let display_backend = configure_display_backend(&mut command);
    command
        .args(SERVO_ARGUMENTS)
        .arg(gateway_origin)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if display_backend == "xwayland" {
        tracing::warn!(
            "Servo 0.4.0's native Wayland window is unresponsive on the qualification host; using the X11/XWayland fallback (native Wayland remains unqualified)"
        );
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("launching Servo from {}", binary.display()))?;

    tokio::select! {
        status = child.wait() => {
            return ensure_successful_exit(status.context("waiting for Servo to exit")?);
        }
        signal = tokio::signal::ctrl_c() => {
            signal.context("installing the Ctrl-C handler for Servo")?;
        }
    }

    tracing::info!(
        grace_period_seconds = INTERRUPT_GRACE_PERIOD.as_secs(),
        "interrupt received; waiting for Servo to exit"
    );
    match tokio::time::timeout(INTERRUPT_GRACE_PERIOD, child.wait()).await {
        Ok(status) => {
            status.context("waiting for Servo to exit after Ctrl-C")?;
        }
        Err(_) => {
            tracing::warn!("Servo did not exit after Ctrl-C; terminating it");
            child
                .kill()
                .await
                .context("terminating Servo after the Ctrl-C grace period")?;
        }
    }
    Ok(())
}

fn configure_display_backend(command: &mut Command) -> &'static str {
    if should_force_x11(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    ) {
        command
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_SESSION_TYPE", "x11")
            .env("WINIT_UNIX_BACKEND", "x11");
        "xwayland"
    } else {
        "native"
    }
}

fn should_force_x11(wayland_available: bool, x11_available: bool) -> bool {
    cfg!(target_os = "linux") && wayland_available && x11_available
}

fn ensure_successful_exit(status: std::process::ExitStatus) -> Result<()> {
    if !status.success() {
        bail!("Servo exited with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualification_arguments_are_fixed_and_ephemeral() {
        assert_eq!(
            SERVO_ARGUMENTS,
            [
                "--temporary-storage",
                "--user-agent=desktop",
                "--window-size=1400x900",
                "--enable-experimental-web-platform-features",
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn xwayland_fallback_requires_both_linux_display_transports() {
        assert!(should_force_x11(true, true));
        assert!(!should_force_x11(true, false));
        assert!(!should_force_x11(false, true));
        assert!(!should_force_x11(false, false));
    }

    #[test]
    fn official_servo_version_output_is_accepted() {
        ensure_supported_servo_version("Version: Servo 0.4.0-e8dbc1dfb").unwrap();
    }

    #[test]
    fn unlabelled_servo_version_output_is_accepted() {
        ensure_supported_servo_version("Servo 0.4.0-e8dbc1dfb").unwrap();
    }

    #[test]
    fn another_servo_0_4_revision_is_rejected() {
        let error = ensure_supported_servo_version("Servo 0.4.0-4efde8d")
            .unwrap_err()
            .to_string();
        assert!(error.contains(EXPECTED_SERVO_VERSION));
        assert!(error.contains("Servo 0.4.0-4efde8d"));
    }

    #[test]
    fn wrong_servo_version_reports_expected_and_actual() {
        let error = ensure_supported_servo_version("Servo 0.3.0-deadbee")
            .unwrap_err()
            .to_string();
        assert!(error.contains(EXPECTED_SERVO_VERSION));
        assert!(error.contains("Servo 0.3.0-deadbee"));
    }

    #[test]
    fn missing_version_output_is_explicit() {
        let error = ensure_supported_servo_version("").unwrap_err().to_string();
        assert!(error.contains("<no output>"));
    }

    #[test]
    fn output_display_is_bounded() {
        let displayed = display_output(&"x".repeat(300));
        assert_eq!(displayed.chars().count(), 241);
        assert!(displayed.ends_with('…'));
    }
}
