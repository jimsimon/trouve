//! Child-process environment normalization for desktop-launched trouve.
//!
//! Graphical launchers commonly start the desktop app with a minimal `PATH`,
//! while a user's interactive/login shell adds language managers such as NVM.
//! Capture that shell's `PATH` once, merge it with the inherited value, and
//! use the cached result for direct child execution. The child itself is still
//! spawned without a shell, which is required for stdio protocols such as MCP.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

const PATH_MARKER: &str = "__TROUVE_LOGIN_SHELL_PATH__";
const PATH_CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);
const PROCESS_TREE_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Enumerating sentinel holders walks every `/proc/*/fd` directory. Under
/// [`DetachedPolicy::Release`] a tree whose group is already empty re-scans at
/// most this often while it waits for a same-session holder to exit.
#[cfg(any(target_os = "linux", target_os = "android"))]
const HOLDER_SCAN_INTERVAL: Duration = Duration::from_millis(250);
/// Unix platforms without a holder query cannot tell a detached daemon from a
/// dying group member. Under [`DetachedPolicy::Release`] they wait this long
/// for the sentinel to close after the group emptied, then release whatever
/// still holds it.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const DETACHED_RELEASE_GRACE: Duration = Duration::from_millis(500);
/// Upper bound for the per-descriptor close-on-exec fallback in `pre_exec`.
#[cfg(unix)]
const MAX_INHERITABLE_DESCRIPTORS: libc::c_int = 65_536;
#[cfg(windows)]
const WINDOWS_PROCESS_TREE_CREATION_FLAGS: u32 =
    windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
const PATH_CAPTURE_SCRIPT: &str =
    "exec /bin/sh -c 'printf \"__TROUVE_LOGIN_SHELL_PATH__%s\\n\" \"$PATH\"'";

static EFFECTIVE_PATH: OnceLock<Option<OsString>> = OnceLock::new();

/// The merged login-shell and inherited process path, captured at most once.
pub fn effective_path() -> Option<&'static OsStr> {
    EFFECTIVE_PATH
        .get_or_init(|| {
            let inherited = std::env::var_os("PATH");
            let shell = login_shell_path().and_then(|shell| {
                let captured = capture_shell_path(&shell);
                if captured.is_some() {
                    tracing::debug!(shell = %shell.display(), "captured login-shell PATH for child processes");
                } else {
                    tracing::debug!(shell = %shell.display(), "login-shell PATH capture failed; using inherited PATH");
                }
                captured
            });
            merge_paths(shell.as_deref(), inherited.as_deref())
        })
        .as_deref()
}

/// Resolve a command through the normalized child path.
pub fn find_executable(command: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(command);
    if direct.components().count() > 1 {
        return executable_file(&direct).then_some(direct);
    }
    find_executable_in_path(command, effective_path()?)
}

/// Build a Tokio command with normalized executable lookup and child `PATH`.
pub fn tokio_command(command: &str) -> tokio::process::Command {
    let mut child = tokio::process::Command::new(
        find_executable(command).unwrap_or_else(|| PathBuf::from(command)),
    );
    apply_path_to_tokio(&mut child);
    child
}

/// Build a standard-library command with normalized lookup and child `PATH`.
pub fn std_command(command: &str) -> std::process::Command {
    let mut child = std::process::Command::new(
        find_executable(command).unwrap_or_else(|| PathBuf::from(command)),
    );
    if let Some(path) = effective_path() {
        child.env("PATH", path);
    }
    child
}

/// Apply the normalized `PATH` to an existing Tokio command.
pub fn apply_path_to_tokio(command: &mut tokio::process::Command) {
    if let Some(path) = effective_path() {
        command.env("PATH", path);
    }
}

/// How a process tree treats a descendant that left the leader's session.
///
/// A descendant inherits the tree's sentinel across `fork`, `exec`, and
/// `setsid()`. One that calls `setsid()` (build caches, package-manager
/// daemons, anything started with `detached: true`) is designed to outlive
/// the command that started it. [`Self::Terminate`] kills it with the rest of
/// the tree; [`Self::Release`] hands it to the caller as a
/// [`DetachedProcess`] so the session, not the call, decides when it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetachedPolicy {
    /// Every sentinel holder dies with the tree.
    #[default]
    Terminate,
    /// Holders in another session keep running and are reported instead.
    Release,
}

/// A descendant that left the tree's session and was released from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachedProcess {
    pub pid: i32,
    /// Kernel start time of the pid when it was released. Every later signal
    /// re-checks it so a recycled pid is never signalled by mistake.
    pub start_time: u64,
    /// The process's short command name (`/proc/<pid>/comm`).
    pub name: String,
}

/// A descendant that left the tree's process group but not its session and
/// was therefore terminated with the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminatedEscapee {
    pub pid: i32,
    pub name: String,
}

impl DetachedProcess {
    /// Whether the released process still exists under the same identity.
    pub fn is_alive(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            linux_process_stat(self.pid).is_some_and(|stat| {
                stat.start_time == self.start_time && !matches!(stat.state, 'Z' | 'X')
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            false
        }
    }

    /// Ask the process to exit (`SIGTERM`). `Ok(false)` means it was already
    /// gone or its pid has been recycled.
    pub fn request_exit(&self) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            self.signal(libc::SIGTERM)
        }
        #[cfg(not(unix))]
        {
            Ok(false)
        }
    }

    /// Kill the process (`SIGKILL`). `Ok(false)` means it was already gone or
    /// its pid has been recycled.
    pub fn kill(&self) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            self.signal(libc::SIGKILL)
        }
        #[cfg(not(unix))]
        {
            Ok(false)
        }
    }

    #[cfg(unix)]
    fn signal(&self, signal: libc::c_int) -> std::io::Result<bool> {
        if !self.is_alive() {
            return Ok(false);
        }
        if unsafe { libc::kill(self.pid, signal) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

/// A Tokio child whose descendants share an owned operating-system process
/// tree boundary.
///
/// On Unix the child leads a new process group and inherits a private lifetime
/// sentinel, so descendants that call `setsid()` remain owned unless the tree
/// opted into [`DetachedPolicy::Release`]. On Windows it is assigned to a
/// kill-on-close Job Object. Call [`Self::terminate_and_reap`] on normal
/// cleanup paths; `Drop` still signals the complete tree as a last resort.
/// This wrapper deliberately accepts an already-configured `Command`, so
/// callers can set argv, environment, cwd, and stdio without invoking a shell.
pub struct ProcessTreeChild {
    child: tokio::process::Child,
    /// Cached separately because a shell leader may exit while a legitimate
    /// background descendant continues inside the owned process tree.
    leader_status: Option<std::process::ExitStatus>,
    /// Whether this wrapper still owns a live or not-yet-proven-empty process
    /// tree. The flag prevents Drop from signalling a reused numeric Unix
    /// process-group id after the group and inherited sentinel are empty.
    tree_active: bool,
    detached_policy: DetachedPolicy,
    /// Descendants released under [`DetachedPolicy::Release`], in discovery
    /// order and without duplicates.
    detached: Vec<DetachedProcess>,
    /// Descendants that left the process group but stayed in the session and
    /// were killed with the tree.
    terminated_escapees: Vec<TerminatedEscapee>,
    /// Set when a platform without a holder query released sentinel holders
    /// it could not enumerate.
    released_untracked: bool,
    #[cfg(unix)]
    process_group: i32,
    #[cfg(unix)]
    descendant_sentinel: OwnedFd,
    /// Last holder classification while the group was empty: when it ran and
    /// whether a same-session holder still existed.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    holder_scan: Option<(Instant, bool)>,
    /// When the group first emptied while the sentinel stayed open.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    release_grace_started: Option<Instant>,
    #[cfg(target_os = "macos")]
    process_group_signalled: bool,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

/// A standard-library child whose descendants share the same owned process
/// tree boundary as [`ProcessTreeChild`]. Synchronous subsystems use this
/// variant when they already run on a dedicated blocking thread and need
/// polling, bounded pipe drains, or file-backed stdin without nesting a Tokio
/// runtime.
pub struct BlockingProcessTreeChild {
    child: std::process::Child,
    leader_status: Option<std::process::ExitStatus>,
    tree_active: bool,
    #[cfg(unix)]
    process_group: i32,
    #[cfg(unix)]
    descendant_sentinel: OwnedFd,
    #[cfg(target_os = "macos")]
    process_group_signalled: bool,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

impl BlockingProcessTreeChild {
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Reap the leader, terminate descendants that inherited its sentinel, and
    /// return completion only once the complete owned tree is empty.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.try_wait_until(Instant::now() + PROCESS_TREE_REAP_TIMEOUT)
    }

    /// [`Self::try_wait`] with an absolute deadline for descendant cleanup.
    ///
    /// Callers that impose an operation-wide timeout use this form so the
    /// process-tree cleanup budget is included in, rather than added after,
    /// that timeout.
    pub fn try_wait_until(
        &mut self,
        deadline: Instant,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        if self.leader_status.is_none() {
            self.leader_status = self.child.try_wait()?;
        }
        let Some(status) = self.leader_status else {
            return Ok(None);
        };
        terminate_blocking_process_tree(self)?;
        wait_for_blocking_process_tree_exit_until(self, deadline)?;
        self.tree_active = false;
        Ok(Some(status))
    }

    /// Terminate every descendant and synchronously reap the direct child.
    pub fn terminate_and_reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.terminate_and_reap_until(Instant::now() + PROCESS_TREE_REAP_TIMEOUT)
    }

    /// [`Self::terminate_and_reap`] with an absolute cleanup deadline.
    pub fn terminate_and_reap_until(
        &mut self,
        deadline: Instant,
    ) -> std::io::Result<std::process::ExitStatus> {
        let terminate_result = terminate_blocking_process_tree(self);
        if self.leader_status.is_none() {
            let _ = self.child.kill();
            loop {
                if let Some(status) = self.child.try_wait()? {
                    self.leader_status = Some(status);
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out reaping terminated blocking child process",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10).min(deadline - now));
            }
        }
        let empty_result = wait_for_blocking_process_tree_exit_until(self, deadline);
        if empty_result.is_ok() {
            self.tree_active = false;
        }
        terminate_result.and(empty_result)?;
        Ok(self.leader_status.expect("leader was reaped above"))
    }
}

impl Drop for BlockingProcessTreeChild {
    fn drop(&mut self) {
        if self.tree_active {
            let _ = terminate_blocking_process_tree(self);
            let _ = self.child.kill();
            // Drop must remain non-blocking even if the OS refuses or cannot
            // confirm tree termination. On Unix, arrange detached zombie
            // reaping without extending the caller's operation deadline.
            #[cfg(unix)]
            {
                let pid = self.child.id();
                std::thread::spawn(move || {
                    let Ok(pid) = i32::try_from(pid) else {
                        return;
                    };
                    let mut status = 0;
                    let _ = unsafe { libc::waitpid(pid, &mut status, 0) };
                });
            }
        }
    }
}

impl ProcessTreeChild {
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Return the direct child's terminal status once it has been observed.
    ///
    /// Callers can use this to distinguish a command failure from a cleanup
    /// acknowledgement failure that happened after the command completed.
    pub fn leader_status(&self) -> Option<std::process::ExitStatus> {
        self.leader_status
    }

    /// Opt into [`DetachedPolicy::Release`]: descendants that move to their
    /// own session no longer keep this tree alive and are not killed with it.
    /// Collect them with [`Self::take_detached`] once the tree completes.
    pub fn release_detached_descendants(&mut self) {
        self.detached_policy = DetachedPolicy::Release;
    }

    pub fn detached_policy(&self) -> DetachedPolicy {
        self.detached_policy
    }

    /// Descendants released so far under [`DetachedPolicy::Release`].
    pub fn take_detached(&mut self) -> Vec<DetachedProcess> {
        std::mem::take(&mut self.detached)
    }

    /// Descendants that escaped the process group and were killed with the
    /// tree.
    pub fn take_terminated_escapees(&mut self) -> Vec<TerminatedEscapee> {
        std::mem::take(&mut self.terminated_escapees)
    }

    /// Whether sentinel holders were released without being enumerated. Only
    /// Unix platforms without a per-process descriptor query report this.
    pub fn released_untracked(&self) -> bool {
        self.released_untracked
    }

    /// Reap the tree leader without terminating descendants.
    ///
    /// Most protocol subprocesses should continue using [`Self::try_wait`],
    /// which treats leader exit as the end of the complete tree. Long-lived
    /// shell jobs use this lower-level operation through [`Self::try_wait_tree`]
    /// so an intentionally backgrounded descendant retains its owner.
    pub fn try_wait_leader(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if self.leader_status.is_some() {
            return Ok(self.leader_status);
        }
        self.leader_status = self.child.try_wait()?;
        Ok(self.leader_status)
    }

    /// Report completion only after the leader has been reaped and every
    /// descendant has left the inherited Unix sentinel / Windows Job Object.
    ///
    /// Unlike [`Self::try_wait`], this does not terminate a live descendant
    /// merely because its original leader exited.
    pub fn try_wait_tree(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let Some(status) = self.try_wait_leader()? else {
            return Ok(None);
        };
        if platform_process_tree_active(self)? {
            return Ok(None);
        }
        self.tree_active = false;
        Ok(Some(status))
    }

    /// Check for natural leader exit without leaving descendants or an armed
    /// stale process-group id behind.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let Some(status) = self.try_wait_leader()? else {
            return Ok(None);
        };
        terminate_platform_process_tree(self)?;
        if platform_process_tree_active(self)? {
            return Ok(None);
        }
        self.tree_active = false;
        Ok(Some(status))
    }

    /// Wait for a natural leader exit, terminate any descendants it left in
    /// the tree, then disarm the Drop fallback.
    pub async fn wait_and_cleanup(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.wait_for_leader().await?;
        let cleanup_result = terminate_platform_process_tree(self);
        let empty_result = wait_for_platform_process_tree_exit(self).await;
        if empty_result.is_ok() {
            self.tree_active = false;
        }
        cleanup_result.and(empty_result).map(|()| status)
    }

    /// Wait for the leader only until `leader_deadline`, then include complete
    /// descendant cleanup in the caller's absolute `cleanup_deadline`.
    pub async fn wait_and_cleanup_until(
        &mut self,
        leader_deadline: tokio::time::Instant,
        cleanup_deadline: tokio::time::Instant,
    ) -> std::io::Result<std::process::ExitStatus> {
        let status = tokio::time::timeout_at(leader_deadline, self.wait_for_leader())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for child process",
                )
            })??;
        let cleanup_result = terminate_platform_process_tree(self);
        let cleanup_deadline =
            cleanup_deadline.min(tokio::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT);
        let empty_result = wait_for_platform_process_tree_exit_until(self, cleanup_deadline).await;
        if empty_result.is_ok() {
            self.tree_active = false;
        }
        cleanup_result.and(empty_result).map(|()| status)
    }

    /// Signal the complete process tree, and the leader as a fallback.
    ///
    /// This method is synchronous so `Drop` can use it. Prefer
    /// [`Self::terminate_and_reap`] whenever async cleanup is possible.
    pub fn terminate_now(&mut self) -> std::io::Result<()> {
        let tree_result = terminate_platform_process_tree(self);
        let child_result = match self.try_wait_leader() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => self.child.start_kill(),
            Err(error) => Err(error),
        };
        tree_result.and(child_result)
    }

    /// Once the leader has exited, signal the remaining process-tree boundary
    /// again. A child can fork in the brief interval after the first signal was
    /// sent but before the leader actually exits; this closes that race without
    /// repeatedly signalling a stale process-group id while an escaped child
    /// keeps the descendant sentinel open.
    pub fn retry_termination_after_leader_exit(&mut self) -> std::io::Result<bool> {
        if self.try_wait_leader()?.is_none() {
            return Ok(false);
        }
        terminate_platform_process_tree(self)?;
        Ok(true)
    }

    /// Terminate the complete tree and reap its leader before returning.
    pub async fn terminate_and_reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.terminate_and_reap_until(tokio::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT)
            .await
    }

    /// [`Self::terminate_and_reap`] bounded by an absolute deadline.
    pub async fn terminate_and_reap_until(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> std::io::Result<std::process::ExitStatus> {
        let terminate_result = self.terminate_now();
        let deadline = deadline.min(tokio::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT);
        let status = tokio::time::timeout_at(deadline, self.wait_for_leader())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out reaping terminated child process",
                )
            })??;
        let retry_result = self.retry_termination_after_leader_exit().map(|_| ());
        let empty_result = wait_for_platform_process_tree_exit_until(self, deadline).await;
        if empty_result.is_ok() {
            self.tree_active = false;
        }
        terminate_result
            .and(retry_result)
            .and(empty_result)
            .map(|()| status)
    }

    async fn wait_for_leader(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(status) = self.leader_status {
            return Ok(status);
        }
        let status = self.child.wait().await?;
        self.leader_status = Some(status);
        Ok(status)
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        if self.tree_active {
            let _ = self.terminate_now();
        }
    }
}

/// Spawn a directly configured command inside an owned process tree.
pub fn spawn_process_tree(
    command: &mut tokio::process::Command,
) -> std::io::Result<ProcessTreeChild> {
    trouve_process::with_spawn_lock(|| spawn_process_tree_locked(command))
}

fn spawn_process_tree_locked(
    command: &mut tokio::process::Command,
) -> std::io::Result<ProcessTreeChild> {
    command.kill_on_drop(true);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }

    #[cfg(unix)]
    let (descendant_sentinel, descendant_sentinel_writer) =
        install_unix_descendant_sentinel(command.as_std_mut())?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // A normally spawned process can create an escaping descendant before
        // AssignProcessToJobObject runs. Hold its primary thread at creation;
        // it is resumed only after the Job Object owns the process.
        command
            .as_std_mut()
            .creation_flags(WINDOWS_PROCESS_TREE_CREATION_FLAGS);
    }

    #[cfg(windows)]
    let job = create_kill_on_close_job()?;

    let mut child = command.spawn()?;

    #[cfg(unix)]
    drop(descendant_sentinel_writer);

    #[cfg(unix)]
    let process_group = match child.id().and_then(|pid| i32::try_from(pid).ok()) {
        Some(process_group) => process_group,
        None => {
            let _ = child.start_kill();
            return Err(std::io::Error::other(
                "spawned child did not expose a valid process id",
            ));
        }
    };

    #[cfg(windows)]
    if let Err(error) = assign_process_to_job(&job, &child) {
        abort_windows_spawn(&mut child, None);
        return Err(error);
    }

    #[cfg(windows)]
    if let Err(error) = resume_suspended_process(&child) {
        abort_windows_spawn(&mut child, Some(&job));
        return Err(error);
    }

    Ok(ProcessTreeChild {
        child,
        leader_status: None,
        tree_active: true,
        detached_policy: DetachedPolicy::Terminate,
        detached: Vec::new(),
        terminated_escapees: Vec::new(),
        released_untracked: false,
        #[cfg(unix)]
        process_group,
        #[cfg(unix)]
        descendant_sentinel,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        holder_scan: None,
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        release_grace_started: None,
        #[cfg(target_os = "macos")]
        process_group_signalled: false,
        #[cfg(windows)]
        job,
    })
}

/// Spawn a standard-library command inside an owned process group / Windows
/// Job Object. This is the blocking counterpart to [`spawn_process_tree`].
pub fn spawn_blocking_process_tree(
    command: &mut std::process::Command,
) -> std::io::Result<BlockingProcessTreeChild> {
    trouve_process::with_spawn_lock(|| spawn_blocking_process_tree_locked(command))
}

fn spawn_blocking_process_tree_locked(
    command: &mut std::process::Command,
) -> std::io::Result<BlockingProcessTreeChild> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(unix)]
    let (descendant_sentinel, descendant_sentinel_writer) =
        install_unix_descendant_sentinel(command)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(WINDOWS_PROCESS_TREE_CREATION_FLAGS);
    }

    #[cfg(windows)]
    let job = create_kill_on_close_job()?;
    let mut child = command.spawn()?;

    #[cfg(unix)]
    drop(descendant_sentinel_writer);

    #[cfg(unix)]
    let process_group = match i32::try_from(child.id()) {
        Ok(process_group) => process_group,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other(
                "spawned child did not expose a valid process id",
            ));
        }
    };

    #[cfg(windows)]
    if let Err(error) = assign_pid_to_job(&job, child.id()) {
        abort_blocking_windows_spawn(&mut child, None);
        return Err(error);
    }
    #[cfg(windows)]
    if let Err(error) = resume_suspended_pid(child.id()) {
        abort_blocking_windows_spawn(&mut child, Some(&job));
        return Err(error);
    }

    Ok(BlockingProcessTreeChild {
        child,
        leader_status: None,
        tree_active: true,
        #[cfg(unix)]
        process_group,
        #[cfg(unix)]
        descendant_sentinel,
        #[cfg(target_os = "macos")]
        process_group_signalled: false,
        #[cfg(windows)]
        job,
    })
}

#[cfg(unix)]
fn install_unix_descendant_sentinel(
    command: &mut std::process::Command,
) -> std::io::Result<(OwnedFd, OwnedFd)> {
    use std::os::unix::process::CommandExt as _;

    let mut descriptors = [-1; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let created = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let created = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
    if created != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let reader = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    for descriptor in [&writer, &reader] {
        // Fresh pipe descriptors have no descriptor flags. Mark the writer
        // first and use one syscall per end to minimize the unavoidable
        // `pipe`/`fcntl` gap for non-`pipe2` Unix platforms.
        if unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }

    let writer_fd = writer.as_raw_fd();
    let descriptor_limit = inheritable_descriptor_limit();
    // SAFETY: this closure only invokes async-signal-safe `close_range` and
    // `fcntl` between fork and exec. The parent copy stays close-on-exec and
    // is dropped immediately after spawn; the child copy deliberately survives
    // exec and is inherited across forks and `setsid()` calls until the last
    // descendant exits.
    unsafe {
        command.pre_exec(move || {
            // Libraries loaded into the desktop process (WebKitGTK, for one)
            // open descriptors without `O_CLOEXEC`. Nothing but stdio and the
            // sentinel should reach a child, so mark everything else first and
            // re-arm the sentinel afterwards.
            mark_descriptors_close_on_exec(descriptor_limit);
            let flags = libc::fcntl(writer_fd, libc::F_GETFD);
            if flags == -1 || libc::fcntl(writer_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok((reader, writer))
}

/// Highest descriptor number the per-descriptor close-on-exec fallback needs
/// to visit, computed in the parent because `getrlimit` is not
/// async-signal-safe.
#[cfg(unix)]
fn inheritable_descriptor_limit() -> libc::c_int {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let soft = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_cur
    } else {
        1024
    };
    libc::c_int::try_from(soft)
        .unwrap_or(MAX_INHERITABLE_DESCRIPTORS)
        .min(MAX_INHERITABLE_DESCRIPTORS)
}

/// Mark every descriptor above stdio close-on-exec. Runs between fork and
/// exec, so it is restricted to async-signal-safe calls and never allocates.
#[cfg(unix)]
fn mark_descriptors_close_on_exec(descriptor_limit: libc::c_int) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // `CLOSE_RANGE_CLOEXEC` (Linux 5.11+). The raw syscall avoids relying
        // on a libc wrapper that older glibc builds lack; on older kernels it
        // fails with EINVAL/ENOSYS and the loop below takes over.
        const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3 as libc::c_uint,
                libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            )
        };
        if marked == 0 {
            return;
        }
    }
    for descriptor in 3..descriptor_limit {
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags == -1 || flags & libc::FD_CLOEXEC != 0 {
            continue;
        }
        unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    }
}

fn wait_for_blocking_process_tree_exit_until(
    child: &BlockingProcessTreeChild,
    deadline: Instant,
) -> std::io::Result<()> {
    while blocking_process_tree_active(child)? {
        let now = Instant::now();
        if now >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for terminated blocking process tree",
            ));
        }
        std::thread::sleep(Duration::from_millis(10).min(deadline - now));
    }
    Ok(())
}

#[cfg(unix)]
fn blocking_process_tree_active(child: &BlockingProcessTreeChild) -> std::io::Result<bool> {
    if !child.tree_active {
        return Ok(false);
    }
    let sentinel_active = unix_descendant_sentinel_active(&child.descendant_sentinel)?;
    #[cfg(target_os = "macos")]
    let group_active =
        !child.process_group_signalled && unix_process_group_active(child.process_group)?;
    #[cfg(not(target_os = "macos"))]
    let group_active = unix_process_group_active(child.process_group)?;
    Ok(sentinel_active || group_active)
}

#[cfg(windows)]
fn blocking_process_tree_active(child: &BlockingProcessTreeChild) -> std::io::Result<bool> {
    if child.tree_active {
        windows_job_active(&child.job)
    } else {
        Ok(false)
    }
}

#[cfg(not(any(unix, windows)))]
fn blocking_process_tree_active(child: &BlockingProcessTreeChild) -> std::io::Result<bool> {
    Ok(child.tree_active && child.leader_status.is_none())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn terminate_blocking_process_tree(child: &mut BlockingProcessTreeChild) -> std::io::Result<()> {
    if !child.tree_active {
        return Ok(());
    }
    let group = signal_unix_process_group(child.process_group);
    // Blocking trees back synchronous subsystems (Git, MCP config) that never
    // intentionally leave a daemon behind; they keep terminating everything.
    let escaped = terminate_unix_sentinel_holders(
        &child.descendant_sentinel,
        child.process_group,
        DetachedPolicy::Terminate,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    group.and(escaped)
}

#[cfg(target_os = "macos")]
fn terminate_blocking_process_tree(child: &mut BlockingProcessTreeChild) -> std::io::Result<()> {
    if !child.tree_active {
        return Ok(());
    }
    if let Err(error) = signal_unix_process_group(child.process_group) {
        // Some Darwin sandboxes reject a group signal with EPERM even though
        // the owned leader can still be killed directly. The inherited
        // sentinel remains the authority for whether any descendant survived.
        if error.raw_os_error() != Some(libc::EPERM) {
            return Err(error);
        }
    }
    // Darwin may retain killed group members as zombies. Ignore that stale
    // group after signalling, but keep the sentinel armed: a `setsid()` child
    // that survived the group signal must continue to block acknowledgement.
    child.process_group_signalled = true;
    Ok(())
}

#[cfg(windows)]
fn terminate_blocking_process_tree(child: &mut BlockingProcessTreeChild) -> std::io::Result<()> {
    if child.tree_active {
        terminate_windows_job(&child.job)
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate_blocking_process_tree(_child: &mut BlockingProcessTreeChild) -> std::io::Result<()> {
    Ok(())
}

async fn wait_for_platform_process_tree_exit(child: &mut ProcessTreeChild) -> std::io::Result<()> {
    wait_for_platform_process_tree_exit_until(
        child,
        tokio::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT,
    )
    .await
}

async fn wait_for_platform_process_tree_exit_until(
    child: &mut ProcessTreeChild,
    deadline: tokio::time::Instant,
) -> std::io::Result<()> {
    while platform_process_tree_active(child)? {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for terminated process tree",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10).min(deadline - now)).await;
    }
    Ok(())
}

#[cfg(unix)]
fn platform_process_tree_active(child: &mut ProcessTreeChild) -> std::io::Result<bool> {
    if !child.tree_active {
        return Ok(false);
    }
    let sentinel_active = unix_descendant_sentinel_active(&child.descendant_sentinel)?;
    if sentinel_active && child.detached_policy == DetachedPolicy::Terminate {
        return Ok(true);
    }
    #[cfg(target_os = "macos")]
    let group_active =
        !child.process_group_signalled && unix_process_group_active(child.process_group)?;
    #[cfg(not(target_os = "macos"))]
    let group_active = unix_process_group_active(child.process_group)?;
    if group_active || !sentinel_active {
        return Ok(group_active);
    }
    // Release policy with an empty group: only processes outside the group
    // still hold the sentinel. Keep the tree alive for those that stayed in
    // the session; the rest are detached daemons the tree does not own.
    released_holders_remain(child)
}

/// Whether a same-session process still holds the sentinel of a tree whose
/// process group is already empty. Detached holders are recorded as a side
/// effect.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn released_holders_remain(child: &mut ProcessTreeChild) -> std::io::Result<bool> {
    if let Some((scanned_at, remain)) = child.holder_scan
        && scanned_at.elapsed() < HOLDER_SCAN_INTERVAL
    {
        return Ok(remain);
    }
    let own_session = unsafe { libc::getsid(0) };
    let mut remain = false;
    for holder in linux_sentinel_holders(&child.descendant_sentinel, child.process_group)? {
        if holder.stat.session == own_session {
            remain = true;
        } else {
            record_detached(&mut child.detached, &holder);
        }
    }
    child.holder_scan = Some((Instant::now(), remain));
    Ok(remain)
}

/// Without a holder query this platform cannot separate a detached daemon
/// from a group member that is still dying, so it grants the sentinel a short
/// grace period after the group empties and then releases the remaining
/// holders untracked.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn released_holders_remain(child: &mut ProcessTreeChild) -> std::io::Result<bool> {
    let started = *child.release_grace_started.get_or_insert_with(Instant::now);
    if started.elapsed() < DETACHED_RELEASE_GRACE {
        return Ok(true);
    }
    child.released_untracked = true;
    Ok(false)
}

#[cfg(windows)]
fn platform_process_tree_active(child: &mut ProcessTreeChild) -> std::io::Result<bool> {
    windows_job_active(&child.job)
}

#[cfg(windows)]
fn windows_job_active(job: &std::os::windows::io::OwnedHandle) -> std::io::Result<bool> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
        QueryInformationJobObject,
    };

    let mut accounting = unsafe { std::mem::zeroed::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() };
    let queried = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle().cast(),
            JobObjectBasicAccountingInformation,
            std::ptr::from_mut(&mut accounting).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    if queried == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(accounting.ActiveProcesses != 0)
    }
}

#[cfg(not(any(unix, windows)))]
fn platform_process_tree_active(child: &mut ProcessTreeChild) -> std::io::Result<bool> {
    Ok(child.tree_active && child.leader_status.is_none())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn terminate_platform_process_tree(child: &mut ProcessTreeChild) -> std::io::Result<()> {
    if !child.tree_active {
        return Ok(());
    }
    let group = signal_unix_process_group(child.process_group);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Holders killed here should be re-checked on the next liveness poll
        // rather than after the throttle interval.
        child.holder_scan = None;
    }
    let escaped = terminate_unix_sentinel_holders(
        &child.descendant_sentinel,
        child.process_group,
        child.detached_policy,
        &mut child.detached,
        &mut child.terminated_escapees,
    );
    group.and(escaped)
}

#[cfg(target_os = "macos")]
fn terminate_platform_process_tree(child: &mut ProcessTreeChild) -> std::io::Result<()> {
    if !child.tree_active {
        return Ok(());
    }
    if let Err(error) = signal_unix_process_group(child.process_group) {
        // Some Darwin sandboxes reject a group signal with EPERM even though
        // the owned leader can still be killed directly. The inherited
        // sentinel remains the authority for whether any descendant survived.
        if error.raw_os_error() != Some(libc::EPERM) {
            return Err(error);
        }
    }
    // Darwin can keep a killed group visible as zombies. Stop consulting that
    // group after signalling, but do not disarm the inherited sentinel: it is
    // the ownership proof for a live descendant that moved to another group.
    child.process_group_signalled = true;
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn unix_process_group_active(process_group: i32) -> std::io::Result<bool> {
    let result = unsafe { libc::kill(-process_group, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

#[cfg(unix)]
fn unix_descendant_sentinel_active(sentinel: &OwnedFd) -> std::io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: sentinel.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(std::ptr::from_mut(&mut descriptor), 1, 0) };
        if result == 0 {
            return Ok(true);
        }
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(std::io::Error::other(
                    "process-tree descendant sentinel became invalid",
                ));
            }
            return Ok(descriptor.revents & libc::POLLHUP == 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn signal_unix_process_group(process_group: i32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

/// The `/proc/<pid>/stat` fields the process-tree code relies on.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LinuxProcessStat {
    state: char,
    parent_id: i32,
    process_group: i32,
    session: i32,
    /// Start time in clock ticks since boot; with the pid it identifies one
    /// process incarnation.
    start_time: u64,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_process_stat(pid: i32) -> Option<LinuxProcessStat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_process_stat(&stat)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn parse_linux_process_stat(stat: &str) -> Option<LinuxProcessStat> {
    // The command name (field 2) is parenthesised and may itself contain
    // spaces or parentheses, so split after its closing parenthesis.
    let (_, tail) = stat.rsplit_once(") ")?;
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent_id = fields.next()?.parse().ok()?;
    let process_group = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    // Fields 7 (tty_nr) through 21 (itrealvalue) precede starttime (22).
    let start_time = fields.nth(15)?.parse().ok()?;
    Some(LinuxProcessStat {
        state,
        parent_id,
        process_group,
        session,
        start_time,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_process_name(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|name| name.trim().to_string())
        .unwrap_or_default()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_process_state_is_active(state: char, parent_id: i32, owner_pid: i32) -> bool {
    !matches!(state, 'Z' | 'X') || parent_id == owner_pid
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn unix_process_group_active(process_group: i32) -> std::io::Result<bool> {
    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    for process in std::fs::read_dir("/proc")? {
        let Ok(process) = process else { continue };
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if linux_process_stat(pid).is_some_and(|stat| {
            stat.process_group == process_group
                && linux_process_state_is_active(stat.state, stat.parent_id, own_pid)
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A live process that still holds a tree's inherited sentinel.
#[cfg(any(target_os = "linux", target_os = "android"))]
struct SentinelHolder {
    pid: i32,
    name: String,
    stat: LinuxProcessStat,
}

/// Enumerate the processes holding `sentinel`, excluding trouve itself and
/// unrelated direct children that inherited the writer mid-spawn.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_sentinel_holders(
    sentinel: &OwnedFd,
    tree_leader: i32,
) -> std::io::Result<Vec<SentinelHolder>> {
    let sentinel_target = std::fs::read_link(format!("/proc/self/fd/{}", sentinel.as_raw_fd()))?;
    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    let mut holders = Vec::new();
    for process in std::fs::read_dir("/proc")? {
        let Ok(process) = process else { continue };
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        let holds_sentinel = descriptors.filter_map(Result::ok).any(|descriptor| {
            std::fs::read_link(descriptor.path()).is_ok_and(|target| target == sentinel_target)
        });
        if !holds_sentinel {
            continue;
        }
        // The holder can exit between the descriptor scan and this read; it
        // then no longer needs terminating or reporting.
        let Some(stat) = linux_process_stat(pid) else {
            continue;
        };
        // Every sentinel writer is close-on-exec in the owner. A different
        // process-tree spawn can nevertheless fork while this writer is still
        // present in the shared parent, briefly inheriting it before exec
        // closes it. Such a process is another direct child of trouve, not a
        // descendant of this tree. Killing it here made unrelated short-lived
        // commands fail nondeterministically under concurrent test/turn load.
        // The actual tree leader remains eligible even though it is also a
        // direct child; its descendants either name that leader as their
        // parent or have already been reparented after escaping the group.
        if pid != tree_leader && stat.parent_id == own_pid {
            continue;
        }
        holders.push(SentinelHolder {
            pid,
            name: linux_process_name(pid),
            stat,
        });
    }
    Ok(holders)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn record_detached(detached: &mut Vec<DetachedProcess>, holder: &SentinelHolder) {
    if detached
        .iter()
        .any(|known| known.pid == holder.pid && known.start_time == holder.stat.start_time)
    {
        return;
    }
    detached.push(DetachedProcess {
        pid: holder.pid,
        start_time: holder.stat.start_time,
        name: holder.name.clone(),
    });
}

/// Kill the processes still holding the tree's sentinel. Under
/// [`DetachedPolicy::Release`] a holder in another session is recorded in
/// `detached` instead of being signalled. Holders outside the process group
/// that were killed are reported in `terminated`.
///
/// Session membership is compared against trouve's own session: the leader
/// was spawned with a new process group but never a new session, so every
/// descendant shares trouve's session until one of them calls `setsid()`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn terminate_unix_sentinel_holders(
    sentinel: &OwnedFd,
    tree_leader: i32,
    policy: DetachedPolicy,
    detached: &mut Vec<DetachedProcess>,
    terminated: &mut Vec<TerminatedEscapee>,
) -> std::io::Result<()> {
    let own_session = unsafe { libc::getsid(0) };
    let mut first_error = None;
    for holder in linux_sentinel_holders(sentinel, tree_leader)? {
        if policy == DetachedPolicy::Release && holder.stat.session != own_session {
            record_detached(detached, &holder);
            continue;
        }
        if unsafe { libc::kill(holder.pid, libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        // Group members die with the group signal; only a holder that left
        // the group is worth telling the caller about.
        if holder.stat.process_group != tree_leader
            && !terminated.iter().any(|known| known.pid == holder.pid)
        {
            terminated.push(TerminatedEscapee {
                pid: holder.pid,
                name: holder.name,
            });
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
fn terminate_unix_sentinel_holders(
    _sentinel: &OwnedFd,
    _tree_leader: i32,
    _policy: DetachedPolicy,
    _detached: &mut Vec<DetachedProcess>,
    _terminated: &mut Vec<TerminatedEscapee>,
) -> std::io::Result<()> {
    // The inherited sentinel still prevents a false cleanup acknowledgement
    // on these platforms. Without a portable process-holder query, an escaped
    // descendant remains quarantined until it exits (or, under
    // `DetachedPolicy::Release`, until the release grace period elapses)
    // rather than racing a new mutation.
    Ok(())
}

#[cfg(windows)]
fn terminate_platform_process_tree(child: &mut ProcessTreeChild) -> std::io::Result<()> {
    if !child.tree_active {
        return Ok(());
    }
    terminate_windows_job(&child.job)
}

#[cfg(windows)]
fn terminate_windows_job(job: &std::os::windows::io::OwnedHandle) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    let terminated = unsafe { TerminateJobObject(job.as_raw_handle().cast(), 1) };
    if terminated == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate_platform_process_tree(_child: &mut ProcessTreeChild) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn create_kill_on_close_job() -> std::io::Result<std::os::windows::io::OwnedHandle> {
    use std::ffi::c_void;
    use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
    let mut limits = unsafe { std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(job)
    }
}

#[cfg(windows)]
fn assign_process_to_job(
    job: &std::os::windows::io::OwnedHandle,
    child: &tokio::process::Child,
) -> std::io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("spawned child did not expose a process id"))?;
    assign_pid_to_job(job, pid)
}

#[cfg(windows)]
fn assign_pid_to_job(job: &std::os::windows::io::OwnedHandle, pid: u32) -> std::io::Result<()> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
    if process.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
    let assigned = unsafe {
        AssignProcessToJobObject(job.as_raw_handle().cast(), process.as_raw_handle().cast())
    };
    if assigned == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn resume_suspended_process(child: &tokio::process::Child) -> std::io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("spawned child did not expose a process id"))?;
    resume_suspended_pid(pid)
}

#[cfg(windows)]
fn resume_suspended_pid(pid: u32) -> std::io::Result<()> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.cast()) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    loop {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(thread.cast()) };
            let previous_count = unsafe { ResumeThread(thread.as_raw_handle().cast()) };
            if previous_count == u32::MAX {
                return Err(std::io::Error::last_os_error());
            }
            if previous_count == 0 {
                return Err(std::io::Error::other(
                    "spawned process primary thread was not suspended before job assignment",
                ));
            }
            return Ok(());
        }
        if unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "spawned process primary thread was not found",
            ));
        }
    }
}

#[cfg(windows)]
fn abort_windows_spawn(
    child: &mut tokio::process::Child,
    assigned_job: Option<&std::os::windows::io::OwnedHandle>,
) {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    if let Some(job) = assigned_job {
        let _ = unsafe { TerminateJobObject(job.as_raw_handle().cast(), 1) };
    }
    let _ = child.start_kill();
    let deadline = Instant::now() + PROCESS_TREE_REAP_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(windows)]
fn abort_blocking_windows_spawn(
    child: &mut std::process::Child,
    assigned_job: Option<&std::os::windows::io::OwnedHandle>,
) {
    if let Some(job) = assigned_job {
        let _ = terminate_windows_job(job);
    }
    let _ = child.kill();
    let deadline = Instant::now() + PROCESS_TREE_REAP_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn login_shell_path() -> Option<PathBuf> {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| shell.is_file())
        .or_else(login_shell_from_passwd)
}

#[cfg(unix)]
fn login_shell_from_passwd() -> Option<PathBuf> {
    let user = std::env::var("USER").ok()?;
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let fields = line.split(':').collect::<Vec<_>>();
        (fields.len() >= 7 && fields[0] == user)
            .then(|| PathBuf::from(fields[6]))
            .filter(|shell| shell.is_file())
    })
}

#[cfg(not(unix))]
fn login_shell_from_passwd() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn capture_shell_path(shell: &Path) -> Option<OsString> {
    let shell_name = shell.file_name()?.to_str()?;
    let mut command = std::process::Command::new(shell);
    if matches!(shell_name, "bash" | "fish" | "ksh" | "zsh") {
        command.args(["-l", "-i", "-c", PATH_CAPTURE_SCRIPT]);
    } else {
        command.args(["-l", "-c", PATH_CAPTURE_SCRIPT]);
    }
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("TERM", "dumb");
    isolate_process_group(&mut command);
    let mut output = tempfile::tempfile().ok()?;
    command.stdout(Stdio::from(output.try_clone().ok()?));
    let mut child = trouve_process::spawn(&mut command).ok()?;
    let status = wait_for_capture(&mut child, PATH_CAPTURE_TIMEOUT);
    status?.success().then_some(())?;
    output.rewind().ok()?;
    let mut bytes = Vec::new();
    output.take(1024 * 1024).read_to_end(&mut bytes).ok()?;
    extract_marked_path(&bytes)
}

#[cfg(unix)]
fn isolate_process_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(unix)]
fn wait_for_capture(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                terminate_capture_process_group(child);
                break None;
            }
        }
    }
}

#[cfg(unix)]
fn terminate_capture_process_group(child: &mut std::process::Child) {
    let pid = child.id();
    // The login shell is its process-group leader. Kill the complete group so
    // startup hooks and background descendants cannot survive a capture
    // timeout and keep files, pipes, or user processes alive.
    let killed_group = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } == 0;
    if !killed_group {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(not(unix))]
fn capture_shell_path(_shell: &Path) -> Option<OsString> {
    None
}

fn extract_marked_path(output: &[u8]) -> Option<OsString> {
    let text = String::from_utf8_lossy(output);
    let marked = text.rsplit_once(PATH_MARKER)?.1;
    let path = marked.lines().next()?.trim();
    (!path.is_empty()).then(|| OsString::from(path))
}

fn merge_paths(shell: Option<&OsStr>, inherited: Option<&OsStr>) -> Option<OsString> {
    let mut seen = HashSet::<OsString>::new();
    let mut paths = Vec::<PathBuf>::new();
    for value in [shell, inherited].into_iter().flatten() {
        for path in std::env::split_paths(value) {
            if seen.insert(path.as_os_str().to_os_string()) {
                paths.push(path);
            }
        }
    }
    (!paths.is_empty())
        .then(|| std::env::join_paths(paths).ok())
        .flatten()
}

fn find_executable_in_path(command: &str, path: &OsStr) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates = windows_command_candidates(
        command,
        std::env::var_os("PATHEXT")
            .filter(|value| !value.is_empty())
            .as_deref()
            .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD")),
    );
    #[cfg(not(windows))]
    let candidates = [OsString::from(command)];

    std::env::split_paths(path).find_map(|directory| {
        candidates
            .iter()
            .map(|candidate| directory.join(candidate))
            .find(|candidate| executable_file(candidate))
    })
}

#[cfg(windows)]
fn windows_command_candidates(command: &str, extensions: &OsStr) -> Vec<OsString> {
    let command_path = Path::new(command);
    if command_path.extension().is_some() {
        return vec![OsString::from(command)];
    }
    extensions
        .to_string_lossy()
        .split(';')
        .filter_map(|extension| {
            let extension = extension.trim();
            if extension.is_empty() {
                None
            } else if extension.starts_with('.') {
                Some(OsString::from(format!("{command}{extension}")))
            } else {
                Some(OsString::from(format!("{command}.{extension}")))
            }
        })
        .collect()
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_the_last_marked_path() {
        let output = b"shell startup noise\n__TROUVE_LOGIN_SHELL_PATH__/old\n\
                       __TROUVE_LOGIN_SHELL_PATH__/nvm/bin:/usr/bin\n";
        assert_eq!(
            extract_marked_path(output),
            Some(OsString::from("/nvm/bin:/usr/bin"))
        );
    }

    #[test]
    fn shell_paths_precede_inherited_paths_without_duplicates() {
        let shell = std::env::join_paths(["/nvm/bin", "/usr/bin"]).unwrap();
        let inherited = std::env::join_paths(["/usr/bin", "/opt/trouve/bin"]).unwrap();
        let merged = merge_paths(Some(&shell), Some(&inherited)).unwrap();
        assert_eq!(
            std::env::split_paths(&merged).collect::<Vec<_>>(),
            ["/nvm/bin", "/usr/bin", "/opt/trouve/bin"]
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_process_tree_creation_is_suspended_until_job_assignment() {
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

        assert_ne!(WINDOWS_PROCESS_TREE_CREATION_FLAGS & CREATE_SUSPENDED, 0);
    }

    #[cfg(windows)]
    #[test]
    fn executable_lookup_honors_pathext() {
        assert_eq!(
            windows_command_candidates("codex", OsStr::new(".EXE;.CMD")),
            [OsString::from("codex.EXE"), OsString::from("codex.CMD")]
        );
        assert_eq!(
            windows_command_candidates("codex.exe", OsStr::new(".EXE;.CMD")),
            [OsString::from("codex.exe")]
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_process_tree_resumes_after_job_assignment() {
        let mut command = tokio::process::Command::new("cmd.exe");
        command
            .args(["/C", "exit", "/B", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();

        assert!(child.wait_and_cleanup().await.unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn executable_lookup_uses_the_supplied_path() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("npx");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([directory.path()]).unwrap();
        assert_eq!(find_executable_in_path("npx", &path), Some(executable));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_group_liveness_classifies_zombies_by_reap_owner() {
        let own_pid = i32::try_from(std::process::id()).unwrap();

        assert!(
            linux_process_state_is_active('S', 1, own_pid),
            "a live orphan remains active"
        );
        assert!(
            linux_process_state_is_active('Z', own_pid, own_pid),
            "a direct zombie remains active until trouve can reap it"
        );
        assert!(
            !linux_process_state_is_active('Z', 1, own_pid),
            "an inert zombie owned by another reaper cannot quarantine the tree"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_stat_parser_reads_session_and_start_time_past_an_awkward_comm() {
        let stat = "4242 (sh -c (nested) name) S 4100 4242 3999 34817 4242 4194304 \
                    120 0 0 0 5 3 0 0 20 0 1 0 987654 2334720 210 18446744073709551615 \
                    1 1 0 0 0 0 0 0 65538 1 0 0 17 3 0 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(
            parse_linux_process_stat(stat),
            Some(LinuxProcessStat {
                state: 'S',
                parent_id: 4100,
                process_group: 4242,
                session: 3999,
                start_time: 987_654,
            })
        );
        assert_eq!(parse_linux_process_stat("4242 (truncated) S 1"), None);

        let own_pid = i32::try_from(std::process::id()).unwrap();
        let own = linux_process_stat(own_pid).expect("own /proc stat");
        assert_eq!(own.session, unsafe { libc::getsid(0) });
        assert_eq!(own.process_group, unsafe { libc::getpgid(0) });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timed_out_capture_reaps_the_shell_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"sleep 60 & echo $! > "$1"; wait"#,
                "trouve-test-shell",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        isolate_process_group(&mut command);
        let mut child = trouve_process::spawn(&mut command).unwrap();

        assert!(
            wait_for_capture(&mut child, Duration::from_millis(100)).is_none(),
            "test shell unexpectedly exited before the timeout"
        );
        let descendant = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let state = loop {
            let state = process_state(descendant).unwrap();
            if state.is_none() || state == Some('Z') || Instant::now() >= deadline {
                break state;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            state.is_none() || state == Some('Z'),
            "login-shell descendant survived timeout cleanup in state {state:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn blocking_process_tree_closes_descendant_held_pipes_after_leader_exit() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 &"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_blocking_process_tree(&mut command).unwrap();
        let mut stdout = child.take_stdout().unwrap();
        let mut stderr = child.take_stderr().unwrap();
        let started = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        stderr.read_to_end(&mut bytes).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sentinel_cleanup_preserves_an_unrelated_direct_child_during_exec() {
        use std::os::unix::process::CommandExt as _;

        // Model the narrow overlap between two concurrent spawns: the second
        // child forks while the first spawn's sentinel writer still exists in
        // the shared parent. The inherited descriptor is normally closed by
        // exec; retaining it here makes the race deterministic.
        let mut owner = std::process::Command::new("/bin/true");
        let (sentinel, writer) = install_unix_descendant_sentinel(&mut owner).unwrap();
        let writer_fd = writer.as_raw_fd();
        let mut unrelated = std::process::Command::new("/bin/sleep");
        unrelated
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: only async-signal-safe fcntl calls run between fork and exec.
        unsafe {
            unrelated.pre_exec(move || {
                let flags = libc::fcntl(writer_fd, libc::F_GETFD);
                if flags == -1
                    || libc::fcntl(writer_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut unrelated = trouve_process::spawn(&mut unrelated).unwrap();
        drop(writer);
        assert!(unix_descendant_sentinel_active(&sentinel).unwrap());

        terminate_unix_sentinel_holders(
            &sentinel,
            i32::MAX,
            DetachedPolicy::Terminate,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(50));
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "cleanup for one process tree killed an unrelated direct child"
        );
        let _ = unrelated.kill();
        let _ = unrelated.wait();
    }

    #[cfg(target_os = "linux")]
    async fn spawned_descendant_pid(pid_path: &Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(pid_path)
                    && let Ok(pid) = pid.trim().parse::<u32>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child did not publish its descendant pid")
    }

    #[cfg(target_os = "linux")]
    fn process_state(pid: u32) -> std::io::Result<Option<char>> {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let state = stat
            .rsplit_once(") ")
            .and_then(|(_, tail)| tail.chars().next())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed /proc/{pid}/stat"),
                )
            })?;
        Ok(Some(state))
    }

    #[cfg(target_os = "linux")]
    async fn assert_process_tree_member_stopped(pid: u32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = process_state(pid).unwrap();
                if state.is_none() || state == Some('Z') {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("process-tree descendant {pid} survived cleanup"));
    }

    #[cfg(target_os = "linux")]
    fn process_tree_fixture(pid_path: &Path) -> ProcessTreeChild {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"sleep 60 & echo $! > "$1"; wait"#,
                "trouve-process-tree-test",
            ])
            .arg(pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        spawn_process_tree(&mut command).unwrap()
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn process_tree_explicit_termination_reaps_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let mut child = process_tree_fixture(&pid_path);
        let descendant = spawned_descendant_pid(&pid_path).await;

        child.terminate_and_reap().await.unwrap();

        assert_process_tree_member_stopped(descendant).await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn process_tree_explicit_termination_acknowledges_killed_descendants() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();

        tokio::time::timeout(Duration::from_secs(2), child.terminate_and_reap())
            .await
            .expect("macOS process-group cleanup retained an inert zombie")
            .unwrap();
        assert!(
            !child.tree_active,
            "signalled process group must be disarmed"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_guarded_spawns_keep_sentinels_isolated() {
        const CHILD_COUNT: usize = 24;

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CHILD_COUNT * 2));
        let mut tree_spawns = Vec::with_capacity(CHILD_COUNT);
        let mut direct_spawns = Vec::with_capacity(CHILD_COUNT);
        for _ in 0..CHILD_COUNT {
            let tree_barrier = barrier.clone();
            tree_spawns.push(tokio::spawn(async move {
                tree_barrier.wait().await;
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .args(["-c", "sleep 60"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                spawn_process_tree(&mut command).unwrap()
            }));
            let direct_barrier = barrier.clone();
            direct_spawns.push(tokio::spawn(async move {
                direct_barrier.wait().await;
                let mut command = tokio::process::Command::new("/bin/sleep");
                command
                    .arg("60")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                trouve_process::with_spawn_lock(|| command.spawn()).unwrap()
            }));
        }

        let mut children = Vec::with_capacity(CHILD_COUNT);
        for spawn in tree_spawns {
            children.push(spawn.await.unwrap());
        }
        let mut direct_children = Vec::with_capacity(CHILD_COUNT);
        for spawn in direct_spawns {
            direct_children.push(spawn.await.unwrap());
        }
        for child in &mut children {
            tokio::time::timeout(Duration::from_secs(2), child.terminate_and_reap())
                .await
                .expect("concurrent macOS launch leaked a foreign sentinel writer")
                .unwrap();
        }
        for child in &mut direct_children {
            child.start_kill().unwrap();
            child.wait().await.unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn process_tree_drop_terminates_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let child = process_tree_fixture(&pid_path);
        let descendant = spawned_descendant_pid(&pid_path).await;

        drop(child);

        assert_process_tree_member_stopped(descendant).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn natural_exit_cleans_descendants_and_disarms_drop() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"sleep 60 & echo $! > "$1""#,
                "trouve-process-tree-test",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let descendant = spawned_descendant_pid(&pid_path).await;

        assert!(child.wait_and_cleanup().await.unwrap().success());
        assert!(!child.tree_active, "reaped PGID must be disarmed");
        assert_process_tree_member_stopped(descendant).await;

        // `Drop` must not signal the already-reaped leader's numeric PGID.
        drop(child);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn natural_try_wait_cleans_descendants_and_disarms_drop() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"sleep 60 & echo $! > "$1""#,
                "trouve-process-tree-test",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let descendant = spawned_descendant_pid(&pid_path).await;

        let status = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process-tree leader did not exit");
        assert!(status.success());
        assert!(!child.tree_active, "reaped PGID must be disarmed");
        assert_process_tree_member_stopped(descendant).await;

        drop(child);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn try_wait_keeps_tree_armed_until_signalled_member_is_reaped() {
        use std::os::unix::process::CommandExt as _;

        let mut leader_command = tokio::process::Command::new("/bin/sleep");
        leader_command
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut leader_command).unwrap();
        let leader = child.id().expect("leader pid");

        let mut member_command = std::process::Command::new("/bin/sleep");
        member_command
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        member_command.process_group(child.process_group);
        let mut member = trouve_process::with_spawn_lock(|| member_command.spawn()).unwrap();

        assert_eq!(unsafe { libc::kill(leader as i32, libc::SIGKILL) }, 0);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if child.try_wait_leader().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process-tree leader did not exit");

        assert!(child.try_wait().unwrap().is_none());
        assert!(child.tree_active, "live process group must remain armed");
        assert!(platform_process_tree_active(&mut child).unwrap());

        member.wait().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("empty process tree was not acknowledged");
        assert!(!status.success());
        assert!(!child.tree_active, "empty process group must be disarmed");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tree_wait_keeps_descendants_owned_after_leader_exit() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("descendant.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"sleep 60 & echo $! > "$1""#,
                "trouve-process-tree-test",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let descendant = spawned_descendant_pid(&pid_path).await;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if child.try_wait_leader().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process-tree leader did not exit");

        assert!(child.try_wait_tree().unwrap().is_none());
        assert!(child.tree_active);
        assert!(
            process_state(descendant)
                .unwrap()
                .is_some_and(|state| state != 'Z'),
            "tree wait terminated a live background descendant"
        );

        child.terminate_and_reap().await.unwrap();
        assert_process_tree_member_stopped(descendant).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tree_wait_owns_and_terminates_descendant_after_setsid() {
        assert!(find_executable("setsid").is_some(), "setsid is required");
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("setsid-descendant.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"setsid /bin/sh -c 'echo $$ > "$1"; exec /bin/sleep 60' detached "$1" </dev/null >/dev/null 2>&1 &"#,
                "trouve-process-tree-test",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let original_group = child.process_group;
        let descendant = spawned_descendant_pid(&pid_path).await;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if child.try_wait_leader().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process-tree leader did not exit");
        assert_ne!(
            unsafe { libc::getpgid(descendant as i32) },
            original_group,
            "fixture descendant did not leave the original process group"
        );
        assert!(
            child.try_wait_tree().unwrap().is_none(),
            "setsid descendant escaped process-tree ownership"
        );

        child.terminate_and_reap().await.unwrap();
        assert_process_tree_member_stopped(descendant).await;
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_process_name(pid: u32, name: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while linux_process_name(pid as i32) != name {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("process {pid} never became {name}"));
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_leader_exit(child: &mut ProcessTreeChild) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if child.try_wait_leader().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process-tree leader did not exit");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn release_policy_leaves_setsid_descendant_running_and_records_it() {
        assert!(find_executable("setsid").is_some(), "setsid is required");
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("setsid-descendant.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"setsid /bin/sh -c 'echo $$ > "$1"; exec /bin/sleep 60' detached "$1" </dev/null >/dev/null 2>&1 &"#,
                "trouve-process-tree-test",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        child.release_detached_descendants();
        let descendant = spawned_descendant_pid(&pid_path).await;
        wait_for_process_name(descendant, "sleep").await;
        wait_for_leader_exit(&mut child).await;

        let status = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(status) = child.try_wait_tree().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a released setsid descendant kept the tree alive");
        assert!(status.success());
        assert!(!child.tree_active, "released tree must be disarmed");
        assert!(
            process_state(descendant)
                .unwrap()
                .is_some_and(|state| state != 'Z'),
            "release policy terminated the detached descendant"
        );
        let detached = child.take_detached();
        assert_eq!(detached.len(), 1, "unexpected detached set: {detached:?}");
        assert_eq!(detached[0].pid, descendant as i32);
        assert_eq!(detached[0].name, "sleep");
        assert!(detached[0].is_alive());
        assert!(child.take_detached().is_empty(), "take_detached must drain");
        assert!(child.take_terminated_escapees().is_empty());

        // Drop must not reach into a session the tree no longer owns.
        drop(child);
        std::thread::sleep(Duration::from_millis(50));
        assert!(detached[0].is_alive());

        assert!(detached[0].request_exit().unwrap());
        assert_process_tree_member_stopped(descendant).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while detached[0].is_alive() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("detached process identity survived its exit");
        assert!(!detached[0].kill().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn release_policy_still_terminates_same_session_escapee() {
        assert!(find_executable("bash").is_some(), "bash is required");
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("escapee.pid");
        let mut command = tokio::process::Command::new("bash");
        // Job control moves the background job into its own process group
        // without starting a new session.
        command
            .args([
                "-c",
                r#"set -m; sleep 60 & echo $! > "$1"; wait"#,
                "trouve-process-tree-test",
            ])
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        child.release_detached_descendants();
        let descendant = spawned_descendant_pid(&pid_path).await;
        let stat = linux_process_stat(descendant as i32).expect("escapee stat");
        assert_ne!(
            stat.process_group, child.process_group,
            "fixture escapee did not leave the process group"
        );
        assert_eq!(
            stat.session,
            unsafe { libc::getsid(0) },
            "fixture escapee unexpectedly left the session"
        );

        child.terminate_and_reap().await.unwrap();

        assert_process_tree_member_stopped(descendant).await;
        let terminated = child.take_terminated_escapees();
        assert_eq!(
            terminated,
            vec![TerminatedEscapee {
                pid: descendant as i32,
                name: "sleep".to_string(),
            }]
        );
        assert!(child.take_detached().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn spawned_children_do_not_inherit_non_cloexec_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let leaked_path = directory.path().join("leaked-descriptor");
        let leaked = std::fs::File::create(&leaked_path).unwrap();
        // Model a library that opened a descriptor without O_CLOEXEC.
        assert_eq!(
            unsafe { libc::fcntl(leaked.as_raw_fd(), libc::F_SETFD, 0) },
            0
        );

        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "ls -l /proc/self/fd"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = spawn_process_tree(&mut command).unwrap();
        let mut stdout = child.take_stdout().unwrap();
        let listing = async {
            use tokio::io::AsyncReadExt as _;
            let mut listing = String::new();
            stdout.read_to_string(&mut listing).await.unwrap();
            listing
        };
        let (status, listing) = tokio::join!(child.wait_and_cleanup(), listing);
        assert!(status.unwrap().success());

        assert!(
            !listing.contains(leaked_path.to_str().unwrap()),
            "child inherited a descriptor the parent left inheritable:\n{listing}"
        );
        // The sentinel is a pipe above stdio; stdout itself is also a pipe.
        let sentinel_inherited = listing.lines().any(|line| {
            line.split_once(" -> ").is_some_and(|(prefix, target)| {
                target.starts_with("pipe:")
                    && prefix
                        .rsplit(' ')
                        .next()
                        .and_then(|descriptor| descriptor.parse::<i32>().ok())
                        .is_some_and(|descriptor| descriptor >= 3)
            })
        });
        assert!(
            sentinel_inherited,
            "descriptor hygiene removed the descendant sentinel:\n{listing}"
        );
        drop(leaked);
    }
}
