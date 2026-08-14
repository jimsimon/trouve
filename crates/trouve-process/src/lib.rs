//! Shared child-process launch primitives.
//!
//! macOS does not provide `pipe2(O_CLOEXEC)`. Process-tree ownership therefore
//! has a short `pipe`-then-`fcntl` setup window in which another child launch
//! could inherit a sentinel descriptor. Every trouve-owned process launch goes
//! through [`with_spawn_lock`] so that setup and the corresponding spawn are
//! atomic with respect to other launches in the process.

#[cfg(target_os = "macos")]
static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run the syscall that creates a child process under trouve's process-wide
/// macOS spawn lock.
///
/// Keep the closure limited to process creation. Waiting for a child while
/// holding the lock would unnecessarily serialize unrelated long-running
/// commands.
#[inline]
pub fn with_spawn_lock<T>(spawn: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    let _guard = SPAWN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    spawn()
}

/// Spawn a standard-library command under the shared launch lock.
#[inline]
pub fn spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    with_spawn_lock(|| command.spawn())
}

/// Spawn a command with closed stdin and both output streams captured, matching
/// [`std::process::Command::output`]. The launch lock is released before waiting.
#[inline]
pub fn output(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    command.stdin(std::process::Stdio::null());
    output_with_stdin(command)
}

/// Spawn a command with its configured stdin and both output streams captured.
/// Use this only when a caller intentionally supplies inherited or piped input.
#[inline]
pub fn output_with_stdin(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    spawn(command)?.wait_with_output()
}

/// Spawn a command with its configured stdio, then wait without retaining the
/// launch lock for the lifetime of the child.
#[inline]
pub fn status(command: &mut std::process::Command) -> std::io::Result<std::process::ExitStatus> {
    spawn(command)?.wait()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn output_closes_configured_stdin_by_default() {
        let input = std::fs::File::open("/etc/hosts").unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "cat"])
            .stdin(std::process::Stdio::from(input));

        let output = super::output(&mut command).unwrap();

        assert!(output.status.success());
        assert!(output.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn output_with_stdin_preserves_explicit_input() {
        let input = std::fs::File::open("/etc/hosts").unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "cat"])
            .stdin(std::process::Stdio::from(input));

        let output = super::output_with_stdin(&mut command).unwrap();

        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn spawn_lock_serializes_launch_sections() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = std::thread::spawn(move || {
            super::with_spawn_lock(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        entered_rx.recv().unwrap();

        let (second_tx, second_rx) = std::sync::mpsc::channel();
        let (second_ready_tx, second_ready_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            second_ready_tx.send(()).unwrap();
            super::with_spawn_lock(|| second_tx.send(()).unwrap());
        });
        second_ready_rx.recv().unwrap();
        assert!(
            second_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "a second launch entered while the process-wide lock was held"
        );

        release_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        second_rx.recv().unwrap();
    }
}
