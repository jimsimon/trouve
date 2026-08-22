//! Non-blocking system opener that still reaps its launcher process.

use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 16;
const LAUNCHER_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_TIMEOUT: Duration = Duration::from_secs(12);
const HANDOFF_CONFIRMATION_INTERVAL: Duration = Duration::from_millis(250);
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
/// launcher runs outside the gateway workers. A launcher that remains alive
/// beyond a short confirmation interval is treated as having accepted the
/// handoff and is reaped asynchronously without terminating it.
pub async fn open_confirmed(path: impl AsRef<OsStr>) -> Result<(), String> {
    let path = path.as_ref().to_owned();
    let deadline = Instant::now() + WORKER_TIMEOUT;
    let mut worker = tokio::task::spawn_blocking(move || open_and_reap_until(&path, deadline));
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut worker).await {
        Ok(Ok(result)) => result.map_err(|error| error.to_string()),
        Ok(Err(error)) => Err(format!("system opener worker was interrupted: {error}")),
        Err(_) => {
            // A synchronous launch boundary cannot be cancelled safely: it
            // may still start a player that needs the retained cache path.
            // Detach the worker and accept the queued handoff so cache
            // reconciliation cannot remove the file underneath that player.
            drop(worker);
            Ok(())
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
    F: FnMut(&mut std::process::Command, Instant) -> std::io::Result<LauncherOutcome>,
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
            Ok(LauncherOutcome::HandedOff) => return Ok(()),
            Ok(LauncherOutcome::Exited(status)) if status.success() => return Ok(()),
            Ok(LauncherOutcome::Exited(status)) => {
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

#[derive(Debug)]
enum LauncherOutcome {
    Exited(std::process::ExitStatus),
    HandedOff,
}

fn wait_for_launcher(
    command: &mut std::process::Command,
    deadline: Instant,
) -> std::io::Result<LauncherOutcome> {
    let mut child = trouve_process::spawn(command)?;
    let handoff_deadline = (Instant::now() + HANDOFF_CONFIRMATION_INTERVAL).min(deadline);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(LauncherOutcome::Exited(status)),
            Ok(None) => {}
            Err(error) => {
                // Once the platform command has started, an ambiguous wait
                // error is not authority to terminate what may be the active
                // player. Preserve the handoff and let the reaper make the
                // best effort to collect it later.
                tracing::warn!(%error, "could not inspect system launcher");
                reap_launcher_in_background(child);
                return Ok(LauncherOutcome::HandedOff);
            }
        }
        if Instant::now() >= handoff_deadline {
            reap_launcher_in_background(child);
            return Ok(LauncherOutcome::HandedOff);
        }
        std::thread::sleep(LAUNCHER_POLL_INTERVAL);
    }
}

fn reap_launcher_in_background(mut child: std::process::Child) {
    if std::thread::Builder::new()
        .name("trouve-opener-reaper".into())
        .spawn(move || {
            if let Err(error) = child.wait() {
                tracing::warn!(%error, "could not reap system launcher");
            }
        })
        .is_err()
    {
        // Thread creation failure is exceptional. The child must not be
        // killed because it may be the active player; dropping the handle is
        // safer than interrupting playback, and the OS will reclaim it when
        // the host exits.
        tracing::warn!("could not start system launcher reaper");
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
                Ok(LauncherOutcome::Exited(exit_status(if attempts == 1 {
                    1
                } else {
                    0
                })))
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

    #[test]
    fn opener_accepts_a_live_launcher_handoff_without_trying_fallbacks() {
        let commands = [
            std::process::Command::new("first"),
            std::process::Command::new("second"),
        ];
        let mut attempts = 0;
        let result =
            try_opener_candidates(commands, Instant::now() + Duration::from_secs(1), |_, _| {
                attempts += 1;
                Ok(LauncherOutcome::HandedOff)
            });

        assert!(result.is_ok());
        assert_eq!(attempts, 1);
    }
}
