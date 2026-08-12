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

const PATH_MARKER: &str = "__TROUVE_LOGIN_SHELL_PATH__";
const PATH_CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);
const PROCESS_TREE_REAP_TIMEOUT: Duration = Duration::from_secs(5);
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

/// A Tokio child whose descendants share an owned operating-system process
/// tree boundary.
///
/// On Unix the child leads a new process group. On Windows it is assigned to
/// a kill-on-close Job Object. Call [`Self::terminate_and_reap`] on normal
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
    /// process-group id after the complete tree has exited.
    tree_active: bool,
    #[cfg(unix)]
    process_group: i32,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
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
    /// descendant has left the owned process group / Job Object.
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

    /// Terminate the complete tree and reap its leader before returning.
    pub async fn terminate_and_reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let terminate_result = self.terminate_now();
        let status = tokio::time::timeout(PROCESS_TREE_REAP_TIMEOUT, self.wait_for_leader())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out reaping terminated child process",
                )
            })??;
        let empty_result = wait_for_platform_process_tree_exit(self).await;
        if empty_result.is_ok() {
            self.tree_active = false;
        }
        terminate_result.and(empty_result).map(|()| status)
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
    command.kill_on_drop(true);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }

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
        #[cfg(unix)]
        process_group,
        #[cfg(windows)]
        job,
    })
}

async fn wait_for_platform_process_tree_exit(child: &ProcessTreeChild) -> std::io::Result<()> {
    let deadline = tokio::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT;
    while platform_process_tree_active(child)? {
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for terminated process tree",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[cfg(unix)]
fn platform_process_tree_active(child: &ProcessTreeChild) -> std::io::Result<bool> {
    if !child.tree_active {
        return Ok(false);
    }
    let result = unsafe { libc::kill(-child.process_group, 0) };
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

#[cfg(windows)]
fn platform_process_tree_active(child: &ProcessTreeChild) -> std::io::Result<bool> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
        QueryInformationJobObject,
    };

    if !child.tree_active {
        return Ok(false);
    }
    let mut accounting = unsafe { std::mem::zeroed::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() };
    let queried = unsafe {
        QueryInformationJobObject(
            child.job.as_raw_handle().cast(),
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
fn platform_process_tree_active(child: &ProcessTreeChild) -> std::io::Result<bool> {
    Ok(child.tree_active && child.leader_status.is_none())
}

#[cfg(unix)]
fn terminate_platform_process_tree(child: &mut ProcessTreeChild) -> std::io::Result<()> {
    if !child.tree_active {
        return Ok(());
    }
    let result = unsafe { libc::kill(-child.process_group, libc::SIGKILL) };
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

#[cfg(windows)]
fn terminate_platform_process_tree(child: &mut ProcessTreeChild) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    if !child.tree_active {
        return Ok(());
    }
    let terminated = unsafe { TerminateJobObject(child.job.as_raw_handle().cast(), 1) };
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
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("spawned child did not expose a process id"))?;
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
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("spawned child did not expose a process id"))?;
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
    let mut child = command.spawn().ok()?;
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
    std::env::split_paths(path)
        .map(|directory| directory.join(command))
        .find(|candidate| executable_file(candidate))
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
        let mut child = command.spawn().unwrap();

        assert!(
            wait_for_capture(&mut child, Duration::from_millis(100)).is_none(),
            "test shell unexpectedly exited before the timeout"
        );
        let descendant = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let state = std::fs::read_to_string(format!("/proc/{descendant}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, tail)| tail.to_owned()))
            .and_then(|tail| tail.chars().next());
        assert!(
            state.is_none() || state == Some('Z'),
            "login-shell descendant survived timeout cleanup in state {state:?}"
        );
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
    fn process_state(pid: u32) -> Option<char> {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, tail)| tail.to_owned()))
            .and_then(|tail| tail.chars().next())
    }

    #[cfg(target_os = "linux")]
    async fn assert_process_tree_member_stopped(pid: u32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = process_state(pid);
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
        let mut member = member_command.spawn().unwrap();

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
        assert!(platform_process_tree_active(&child).unwrap());

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
            process_state(descendant).is_some_and(|state| state != 'Z'),
            "tree wait terminated a live background descendant"
        );

        child.terminate_and_reap().await.unwrap();
        assert_process_tree_member_stopped(descendant).await;
    }
}
