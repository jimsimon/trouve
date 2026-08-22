//! Non-blocking system opener that still reaps its launcher process.

use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 16;
const LAUNCHER_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_TIMEOUT: Duration = Duration::from_secs(12);
const LAUNCHER_POLL_INTERVAL: Duration = Duration::from_millis(10);
static WORKER: OnceLock<Option<SyncSender<OsString>>> = OnceLock::new();

/// Open a URL or path with the system handler without blocking the caller.
///
/// `open::that_detached` double-forks on Unix and leaves its intermediate child
/// for this long-lived process to reap. A bounded worker queue runs the
/// ordinary, waiting opener off the UI thread while ensuring the launcher is
/// collected when it exits.
pub fn open(path: impl AsRef<OsStr>) -> Result<(), String> {
    enqueue(path.as_ref().to_owned())
}

/// Open a path and report whether the system launcher accepted it.
///
/// Video playback uses this completion-aware variant so a failed association
/// does not look successful or retain an unusable cache entry. The blocking
/// launcher runs outside the gateway workers and is killed if it does not
/// hand playback off promptly; the external player remains independent.
pub async fn open_confirmed(path: impl AsRef<OsStr>) -> Result<(), String> {
    let path = path.as_ref().to_owned();
    let mut worker = tokio::task::spawn_blocking(move || open_and_reap(&path));
    match tokio::time::timeout(WORKER_TIMEOUT, &mut worker).await {
        Ok(Ok(result)) => result.map_err(|error| error.to_string()),
        Ok(Err(error)) => Err(format!("system opener worker was interrupted: {error}")),
        Err(_) => {
            // This cancels a queued blocking task. A task that has already
            // started is independently bounded by `LAUNCHER_TIMEOUT` below.
            worker.abort();
            Err("system opener worker timed out".to_string())
        }
    }
}

fn enqueue(request: OsString) -> Result<(), String> {
    let Some(sender) = WORKER.get_or_init(start_worker) else {
        return Err("system opener worker is unavailable".into());
    };
    match sender.try_send(request) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err("system opener queue is full".into()),
        Err(TrySendError::Disconnected(_)) => Err("system opener worker disconnected".into()),
    }
}

fn start_worker() -> Option<SyncSender<OsString>> {
    let (sender, receiver) = sync_channel::<OsString>(QUEUE_CAPACITY);
    match std::thread::Builder::new()
        .name("trouve-opener".into())
        .spawn(move || {
            while let Ok(path) = receiver.recv() {
                if let Err(error) = open_and_reap(&path) {
                    tracing::warn!(
                        %error,
                        path = %path.to_string_lossy(),
                        "could not open system handler"
                    );
                }
            }
        }) {
        Ok(_) => Some(sender),
        Err(error) => {
            tracing::warn!(%error, "could not start system opener worker");
            None
        }
    }
}

fn open_and_reap(path: &OsStr) -> std::io::Result<()> {
    let mut last_error = None;
    for mut command in open::commands(path) {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match wait_for_launcher(&mut command, LAUNCHER_TIMEOUT) {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(std::io::Error::other(format!(
                    "launcher {command:?} failed with {status:?}"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return Err(error),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no system launcher is available",
        )
    }))
}

fn wait_for_launcher(
    command: &mut std::process::Command,
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = trouve_process::spawn(command)?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "system launcher timed out",
            ));
        }
        std::thread::sleep(LAUNCHER_POLL_INTERVAL);
    }
}
