//! Non-blocking system opener that still reaps its launcher process.

use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, channel, sync_channel,
};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 16;
const LAUNCHER_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_TIMEOUT: Duration = Duration::from_secs(12);
const HANDOFF_CONFIRMATION_INTERVAL: Duration = Duration::from_secs(2);
const LAUNCHER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const LAUNCHER_REAPER_POLL_INTERVAL: Duration = Duration::from_millis(25);
const LAUNCHER_REAPER_BATCH_SIZE: usize = 64;
const DISPATCH_QUEUED: u8 = 0;
const DISPATCH_ENTERED: u8 = 1;
const DISPATCH_CANCELLED: u8 = 2;
static WORKER: OnceLock<Option<SyncSender<OsString>>> = OnceLock::new();
static LAUNCHER_REAPER: OnceLock<Option<Sender<std::process::Child>>> = OnceLock::new();

#[derive(Debug)]
pub(super) struct OpenAttemptError {
    error: std::io::Error,
    retain_path: bool,
}

impl OpenAttemptError {
    fn discard(error: std::io::Error) -> Self {
        Self {
            error,
            retain_path: false,
        }
    }

    fn retain(error: std::io::Error) -> Self {
        Self {
            error,
            retain_path: true,
        }
    }

    pub(super) fn retain_path(&self) -> bool {
        self.retain_path
    }

    fn kind(&self) -> std::io::ErrorKind {
        self.error.kind()
    }
}

impl std::fmt::Display for OpenAttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for OpenAttemptError {}

#[derive(Debug, Default)]
struct LaunchDispatch {
    state: AtomicU8,
}

impl LaunchDispatch {
    fn enter_launch(&self) -> Result<(), OpenAttemptError> {
        match self.state.compare_exchange(
            DISPATCH_QUEUED,
            DISPATCH_ENTERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(DISPATCH_ENTERED) => Ok(()),
            Err(_) => Err(launcher_timeout_error()),
        }
    }

    fn cancel_if_queued(&self) -> bool {
        self.state
            .compare_exchange(
                DISPATCH_QUEUED,
                DISPATCH_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

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
pub async fn open_confirmed(path: impl AsRef<OsStr>) -> Result<(), OpenAttemptError> {
    let path = path.as_ref().to_owned();
    let deadline = Instant::now() + WORKER_TIMEOUT;
    let dispatch = Arc::new(LaunchDispatch::default());
    let worker_dispatch = Arc::clone(&dispatch);
    let mut worker = tokio::task::spawn_blocking(move || {
        open_and_reap_until_with_dispatch(&path, deadline, &worker_dispatch)
    });
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut worker).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(OpenAttemptError::discard(std::io::Error::other(format!(
            "system opener worker was interrupted: {error}"
        )))),
        Err(_) => {
            if dispatch.cancel_if_queued() {
                worker.abort();
                Err(OpenAttemptError::discard(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "system opener worker timed out before launching",
                )))
            } else {
                // A synchronous process-launch boundary cannot be cancelled.
                // Report the timeout, but retain the bounded cache path while
                // the detached worker may still hand it to a player.
                drop(worker);
                Err(OpenAttemptError::retain(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "system opener worker timed out during launch",
                )))
            }
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

fn open_and_reap(path: &OsStr) -> Result<(), OpenAttemptError> {
    open_and_reap_until(path, Instant::now() + LAUNCHER_TIMEOUT)
}

fn open_and_reap_until(path: &OsStr, deadline: Instant) -> Result<(), OpenAttemptError> {
    open_and_reap_until_with_dispatch(path, deadline, &LaunchDispatch::default())
}

fn open_and_reap_until_with_dispatch(
    path: &OsStr,
    deadline: Instant,
    dispatch: &LaunchDispatch,
) -> Result<(), OpenAttemptError> {
    try_opener_candidates(open::commands(path), deadline, |command, deadline| {
        wait_for_launcher(command, deadline, dispatch)
    })
}

fn try_opener_candidates<I, F>(
    commands: I,
    deadline: Instant,
    mut wait: F,
) -> Result<(), OpenAttemptError>
where
    I: IntoIterator<Item = std::process::Command>,
    F: FnMut(&mut std::process::Command, Instant) -> Result<LauncherOutcome, OpenAttemptError>,
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
                last_error = Some(OpenAttemptError::discard(std::io::Error::other(format!(
                    "launcher {command:?} failed with {status:?}"
                ))));
            }
            Err(error) if error.retain_path() => return Err(error),
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return Err(error),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        OpenAttemptError::discard(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no system launcher is available",
        ))
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
    dispatch: &LaunchDispatch,
) -> Result<LauncherOutcome, OpenAttemptError> {
    let reaper = launcher_reaper()?;
    dispatch.enter_launch()?;
    let mut child = trouve_process::spawn(command).map_err(OpenAttemptError::discard)?;
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
                reap_launcher_in_background(child, reaper);
                return Err(OpenAttemptError::retain(error));
            }
        }
        if Instant::now() >= handoff_deadline {
            reap_launcher_in_background(child, reaper);
            return Ok(LauncherOutcome::HandedOff);
        }
        std::thread::sleep(LAUNCHER_POLL_INTERVAL);
    }
}

fn launcher_reaper() -> Result<&'static Sender<std::process::Child>, OpenAttemptError> {
    LAUNCHER_REAPER
        .get_or_init(start_launcher_reaper)
        .as_ref()
        .ok_or_else(|| {
            OpenAttemptError::discard(std::io::Error::other(
                "system launcher reaper is unavailable",
            ))
        })
}

fn start_launcher_reaper() -> Option<Sender<std::process::Child>> {
    let (sender, receiver) = channel::<std::process::Child>();
    std::thread::Builder::new()
        .name("trouve-opener-reaper".into())
        .spawn(move || supervise_launchers(receiver))
        .map(|_| sender)
        .map_err(|error| {
            tracing::warn!(%error, "could not start system launcher reaper");
        })
        .ok()
}

fn supervise_launchers(receiver: Receiver<std::process::Child>) {
    let mut children = Vec::new();
    loop {
        let mut intake_remaining = LAUNCHER_REAPER_BATCH_SIZE;
        match receiver.recv_timeout(LAUNCHER_REAPER_POLL_INTERVAL) {
            Ok(child) => {
                children.push(SupervisedLauncher::new(child));
                intake_remaining -= 1;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        drain_ready(&receiver, intake_remaining, |child| {
            children.push(SupervisedLauncher::new(child))
        });
        poll_launchers(&mut children);
    }
}

fn drain_ready<T>(receiver: &Receiver<T>, limit: usize, mut accept: impl FnMut(T)) {
    for _ in 0..limit {
        let Ok(value) = receiver.try_recv() else {
            return;
        };
        accept(value);
    }
}

struct SupervisedLauncher {
    child: std::process::Child,
    inspection_error_reported: bool,
}

impl SupervisedLauncher {
    fn new(child: std::process::Child) -> Self {
        Self {
            child,
            inspection_error_reported: false,
        }
    }
}

fn poll_launchers(children: &mut Vec<SupervisedLauncher>) {
    children.retain_mut(|launcher| match launcher.child.try_wait() {
        Ok(Some(_)) => false,
        Ok(None) => true,
        Err(error) => {
            // Keep retrying this child without blocking progress for other
            // launchers. A later successful poll will still collect it.
            if !launcher.inspection_error_reported {
                tracing::warn!(%error, "could not inspect system launcher in reaper");
                launcher.inspection_error_reported = true;
            }
            true
        }
    });
}

fn reap_launcher_in_background(child: std::process::Child, reaper: &Sender<std::process::Child>) {
    if let Err(error) = reaper.send(child) {
        // The shared sender remains live for the process lifetime, so this is
        // only a defensive fallback. Wait without killing a possible player.
        let mut child = error.0;
        if let Err(error) = child.wait() {
            tracing::warn!(%error, "could not synchronously reap system launcher");
        }
    }
}

fn launcher_timeout_error() -> OpenAttemptError {
    OpenAttemptError::discard(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "system launcher timed out",
    ))
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

    #[test]
    fn queued_dispatch_can_be_cancelled_before_a_late_launch() {
        let dispatch = LaunchDispatch::default();

        assert!(dispatch.cancel_if_queued());
        assert_eq!(
            dispatch.enter_launch().unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn entered_launch_is_retained_when_the_caller_times_out() {
        let dispatch = LaunchDispatch::default();

        dispatch.enter_launch().unwrap();
        assert!(!dispatch.cancel_if_queued());
    }

    #[cfg(unix)]
    #[test]
    fn reaper_poll_collects_a_later_exit_while_an_earlier_launcher_is_live() {
        let mut long_command = std::process::Command::new("sleep");
        long_command.arg("2");
        let long_child = trouve_process::spawn(&mut long_command).unwrap();
        let long_id = long_child.id();
        let mut short_command = std::process::Command::new("true");
        let short_child = trouve_process::spawn(&mut short_command).unwrap();
        let mut children = vec![
            SupervisedLauncher::new(long_child),
            SupervisedLauncher::new(short_child),
        ];
        let deadline = Instant::now() + Duration::from_secs(1);

        while children.len() > 1 && Instant::now() < deadline {
            poll_launchers(&mut children);
            std::thread::sleep(LAUNCHER_POLL_INTERVAL);
        }

        let remaining_ids = children
            .iter()
            .map(|launcher| launcher.child.id())
            .collect::<Vec<_>>();
        for mut launcher in children {
            launcher.child.kill().unwrap();
            launcher.child.wait().unwrap();
        }
        assert_eq!(remaining_ids, vec![long_id]);
    }

    #[test]
    fn reaper_intake_batch_is_bounded_before_the_next_poll() {
        let (sender, receiver) = channel();
        for value in 0..=LAUNCHER_REAPER_BATCH_SIZE {
            sender.send(value).unwrap();
        }
        let mut accepted = Vec::new();

        drain_ready(&receiver, LAUNCHER_REAPER_BATCH_SIZE, |value| {
            accepted.push(value);
        });

        assert_eq!(accepted.len(), LAUNCHER_REAPER_BATCH_SIZE);
        assert_eq!(receiver.try_recv(), Ok(LAUNCHER_REAPER_BATCH_SIZE));
    }
}
