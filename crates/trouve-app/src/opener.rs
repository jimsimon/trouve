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
    let deadline = Instant::now() + WORKER_TIMEOUT;
    let mut worker = tokio::task::spawn_blocking(move || open_and_reap_until(&path, deadline));
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut worker).await {
        Ok(Ok(result)) => result.map_err(|error| error.to_string()),
        Ok(Err(error)) => Err(format!("system opener worker was interrupted: {error}")),
        Err(_) => {
            // This cancels a queued blocking task. A task already inside the
            // synchronous process-launch boundary observes the same absolute
            // deadline as soon as that boundary returns and reaps any child.
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
    open_and_reap_until(path, Instant::now() + LAUNCHER_TIMEOUT)
}

fn open_and_reap_until(path: &OsStr, deadline: Instant) -> std::io::Result<()> {
    try_opener_candidates(open::commands(path), deadline, wait_for_launcher)
}

fn try_opener_candidates<I, F>(commands: I, deadline: Instant, mut wait: F) -> std::io::Result<()>
where
    I: IntoIterator<Item = std::process::Command>,
    F: FnMut(&mut std::process::Command, Instant) -> std::io::Result<std::process::ExitStatus>,
{
    let mut last_error = None;
    for mut command in commands {
        if Instant::now() >= deadline {
            return Err(launcher_timeout_error());
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match wait(&mut command, deadline) {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                last_error = Some(std::io::Error::other(format!(
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
    deadline: Instant,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = trouve_process::spawn(command)?;
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
            return Err(launcher_timeout_error());
        }
        std::thread::sleep(LAUNCHER_POLL_INTERVAL);
    }
}

fn launcher_timeout_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "system launcher timed out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(code as u32)
    }

    #[test]
    fn opener_falls_back_after_a_nonzero_launcher_exit() {
        let commands = [
            std::process::Command::new("first"),
            std::process::Command::new("second"),
        ];
        let mut attempts = 0;
        let result =
            try_opener_candidates(commands, Instant::now() + Duration::from_secs(1), |_, _| {
                attempts += 1;
                Ok(exit_status(if attempts == 1 { 1 } else { 0 }))
            });

        assert!(result.is_ok());
        assert_eq!(attempts, 2);
    }

    #[test]
    fn opener_stops_immediately_after_a_timeout() {
        let commands = [
            std::process::Command::new("first"),
            std::process::Command::new("second"),
        ];
        let mut attempts = 0;
        let result =
            try_opener_candidates(commands, Instant::now() + Duration::from_secs(1), |_, _| {
                attempts += 1;
                Err(launcher_timeout_error())
            });

        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(attempts, 1);
    }
}
