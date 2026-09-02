//! Shell tools: run a command inside the session worktree, either blocking
//! (the classic one-shot) or as a background job the model can poll with
//! `shell_output` and stop with `shell_kill` — dev servers, long builds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use trouve_agents::process_env::{ProcessTreeChild, spawn_process_tree};

use super::{Tool, ToolCtx, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_CAPTURE_BYTES: usize = 32 * 1024;
/// Background jobs keep more output than the one-shot capture: they live
/// longer and are read incrementally.
const MAX_JOB_BYTES: usize = 1024 * 1024;
/// Hard lifetime cap for a background job; runaway processes die with it.
const MAX_JOB_SECS: u64 = 3600;
const MAX_JOBS: usize = 16;
const CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Default)]
struct CleanupController {
    #[cfg(test)]
    injected_failures: AtomicU64,
}

impl CleanupController {
    async fn terminate_and_reap(
        &self,
        child: &Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    ) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(test)]
        if self
            .injected_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(std::io::Error::other(
                "injected shell process-tree cleanup failure",
            ));
        }
        child.lock().await.terminate_and_reap().await
    }
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedOutput {
    fn into_string(self) -> (String, bool) {
        (
            String::from_utf8_lossy(&self.bytes).into_owned(),
            self.truncated,
        )
    }
}

/// One background job: the child (for kill/wait), its captured output, and
/// the model's read cursor.
struct Job {
    child: Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    output: Arc<Mutex<JobOutput>>,
    /// Worktree the job was started from; other sessions cannot touch it.
    worktree: std::path::PathBuf,
    command: String,
    /// How far the model has read (byte offset into `output.bytes`).
    cursor: usize,
}

#[derive(Default)]
struct JobOutput {
    bytes: Vec<u8>,
    truncated: bool,
    exit_code: Option<i32>,
    killed: bool,
}

/// Shared by the three shell tools; owns every background job.
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
    cleanup: Arc<CleanupController>,
}

static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

async fn terminate_background_job(
    cleanup: &CleanupController,
    child: &Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    output: &Arc<Mutex<JobOutput>>,
) -> std::io::Result<()> {
    let status = cleanup.terminate_and_reap(child).await?;
    let mut output = output.lock().unwrap();
    output.killed = true;
    output.exit_code.get_or_insert(status.code().unwrap_or(-1));
    Ok(())
}

impl JobRegistry {
    /// Drop finished jobs (oldest first) until a slot is free; running jobs
    /// are never evicted. Errors when every slot holds a running job.
    fn make_room(&self, jobs: &mut HashMap<String, Job>) -> Result<(), String> {
        if jobs.len() < MAX_JOBS {
            return Ok(());
        }
        let finished: Vec<String> = jobs
            .iter()
            .filter(|(_, j)| j.output.lock().unwrap().exit_code.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        match finished.first() {
            Some(id) => {
                jobs.remove(id);
                Ok(())
            }
            None => Err(format!(
                "{MAX_JOBS} background jobs are already running; kill one with shell_kill first"
            )),
        }
    }

    /// Stop every running job belonging to a worktree being removed.
    pub async fn kill_worktree(&self, worktree: &std::path::Path) -> Result<(), String> {
        let jobs: Vec<_> = {
            let jobs = self.jobs.lock().unwrap();
            jobs.iter()
                .filter(|(_, job)| job.worktree == worktree)
                .filter(|(_, job)| job.output.lock().unwrap().exit_code.is_none())
                .map(|(id, job)| (id.clone(), job.child.clone(), job.output.clone()))
                .collect()
        };
        let mut failures = Vec::new();
        for (id, child, output) in jobs {
            if let Err(error) = terminate_background_job(&self.cleanup, &child, &output).await {
                failures.push(format!("{id}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "background shell cleanup was not acknowledged: {}",
                failures.join("; ")
            ))
        }
    }
}

/// Pump one stream into the shared buffer, respecting the size cap.
fn pump(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    output: Arc<Mutex<JobOutput>>,
) {
    let Some(mut stream) = stream else { return };
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let mut out = output.lock().unwrap();
                    let room = MAX_JOB_BYTES.saturating_sub(out.bytes.len());
                    if room < n {
                        out.truncated = true;
                    }
                    let take = n.min(room);
                    out.bytes.extend_from_slice(&buf[..take]);
                }
            }
        }
    });
}

async fn read_capped(
    stream: Option<impl tokio::io::AsyncRead + Unpin>,
) -> std::io::Result<CapturedOutput> {
    use tokio::io::AsyncReadExt as _;

    let Some(mut stream) = stream else {
        return Ok(CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        });
    };
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES);
    let mut truncated = false;
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let room = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
        let retained = read.min(room);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(CapturedOutput { bytes, truncated })
}

async fn foreground_result(
    status: std::process::ExitStatus,
    stdout_task: tokio::task::JoinHandle<std::io::Result<CapturedOutput>>,
    stderr_task: tokio::task::JoinHandle<std::io::Result<CapturedOutput>>,
) -> ToolResult {
    let stdout = stdout_task
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        });
    let stderr = stderr_task
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        });
    let (stdout, stdout_truncated) = stdout.into_string();
    let (stderr, stderr_truncated) = stderr.into_string();
    ToolResult::ok(json!({
        "exit_code": status.code(),
        "stdout": stdout,
        "stderr": stderr,
        "truncated": stdout_truncated || stderr_truncated,
    }))
}

pub struct Shell {
    pub jobs: Arc<JobRegistry>,
}

#[async_trait::async_trait]
impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn description(&self) -> &'static str {
        "Run a shell command in the workspace root. Captures stdout/stderr (truncated at 32KB \
         each); times out after 120s by default. Set run_in_background for long-running \
         processes (dev servers, builds): it returns a job id immediately — poll it with \
         shell_output and stop it with shell_kill."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Command passed to `sh -c`"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600},
                "run_in_background": {"type": "boolean", "description": "Return immediately with a job id instead of waiting (default: false)"}
            },
            "required": ["command"]
        })
    }
    fn mutates(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("command cancelled");
        }
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: command");
        };
        if args
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return self.spawn_background(ctx, command).await;
        }
        let timeout = Duration::from_secs(
            args.get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
        );
        let mut command_process = trouve_agents::process_env::tokio_command("sh");
        command_process
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.worktree)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = match spawn_process_tree(&mut command_process) {
            Ok(child) => child,
            Err(e) => return ToolResult::error(format!("failed to spawn: {e}")),
        };
        let stdout = child.take_stdout();
        let stderr = child.take_stderr();
        let child = Arc::new(tokio::sync::Mutex::new(child));
        // Drain both pipes while the process runs; waiting first can
        // deadlock once a pipe fills its kernel buffer.
        let stdout_task = tokio::spawn(read_capped(stdout));
        let stderr_task = tokio::spawn(read_capped(stderr));
        let wait = {
            let child = child.clone();
            async move { child.lock().await.wait_and_cleanup().await }
        };
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                let (_, cleanup_failure) = self
                    .cleanup_foreground_until_acknowledged(&child)
                    .await;
                stdout_task.abort();
                stderr_task.abort();
                match cleanup_failure {
                    Some(error) => ToolResult::error(format!(
                        "command cancelled; process-tree cleanup required a retry after: {error}"
                    )),
                    None => ToolResult::error("command cancelled"),
                }
            }
            outcome = tokio::time::timeout(timeout, wait) => match outcome {
            Err(_) => {
                let (_, cleanup_failure) = self
                    .cleanup_foreground_until_acknowledged(&child)
                    .await;
                stdout_task.abort();
                stderr_task.abort();
                let timeout_message = format!("command timed out after {}s", timeout.as_secs());
                match cleanup_failure {
                    Some(error) => ToolResult::error(format!(
                        "{timeout_message}; process-tree cleanup required a retry after: {error}"
                    )),
                    None => ToolResult::error(timeout_message),
                }
            }
            Ok(Err(error)) => {
                let completed_status = child.lock().await.leader_status();
                let (cleanup_status, cleanup_failure) = self
                    .cleanup_foreground_until_acknowledged(&child)
                    .await;
                if let Some(status) = completed_status {
                    tracing::warn!(
                        %error,
                        retry_error = cleanup_failure.as_deref(),
                        "shell process completed before a transient cleanup acknowledgement failure"
                    );
                    return foreground_result(status, stdout_task, stderr_task).await;
                }
                stdout_task.abort();
                stderr_task.abort();
                match cleanup_failure {
                    Some(cleanup_error) => ToolResult::error(format!(
                        "shell failed: {error}; process-tree cleanup required a retry after: {cleanup_error}"
                    )),
                    None => ToolResult::error(format!(
                        "shell failed: {error}; cleanup exit status: {cleanup_status}"
                    )),
                }
            }
            Ok(Ok(status)) => foreground_result(status, stdout_task, stderr_task).await,
            },
        }
    }
}

impl Shell {
    pub(super) fn new(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }

    /// Retry process-tree cleanup without returning control to the engine.
    /// The engine owns the session mutation lane while this future is live;
    /// keeping the future pending therefore quarantines the lane if cleanup
    /// cannot be acknowledged.
    async fn cleanup_foreground_until_acknowledged(
        &self,
        child: &Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    ) -> (std::process::ExitStatus, Option<String>) {
        let mut first_failure = None;
        loop {
            match self.jobs.cleanup.terminate_and_reap(child).await {
                Ok(status) => return (status, first_failure),
                Err(error) => {
                    first_failure.get_or_insert_with(|| error.to_string());
                    tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
                }
            }
        }
    }

    async fn spawn_background(&self, ctx: &ToolCtx, command: &str) -> ToolResult {
        self.spawn_background_with_lifetime(ctx, command, Duration::from_secs(MAX_JOB_SECS))
            .await
    }

    async fn spawn_background_with_lifetime(
        &self,
        ctx: &ToolCtx,
        command: &str,
        lifetime: Duration,
    ) -> ToolResult {
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("command cancelled");
        }
        let mut command_process = trouve_agents::process_env::tokio_command("sh");
        command_process
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.worktree)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = match spawn_process_tree(&mut command_process) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("failed to spawn: {e}")),
        };
        let pid = child.id();
        let output = Arc::new(Mutex::new(JobOutput::default()));
        pump(child.take_stdout(), output.clone());
        pump(child.take_stderr(), output.clone());
        let child = Arc::new(tokio::sync::Mutex::new(child));
        // Waiter: the job is complete only when the leader and every
        // descendant has exited. Process-tree ownership remains independent of
        // the session mutation lane, which covers the launch call rather than
        // the lifetime of a service intentionally left running in the
        // background.
        {
            let child = child.clone();
            let output = output.clone();
            let cleanup = self.jobs.cleanup.clone();
            tokio::spawn(async move {
                loop {
                    let status = child.lock().await.try_wait_tree();
                    match status {
                        Ok(Some(status)) => {
                            output
                                .lock()
                                .unwrap()
                                .exit_code
                                .get_or_insert(status.code().unwrap_or(-1));
                            break;
                        }
                        Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
                        Err(_) => {
                            // A liveness-query failure must not release the
                            // mutation lane while an untracked descendant may
                            // still be running.
                            if terminate_background_job(&cleanup, &child, &output)
                                .await
                                .is_ok()
                            {
                                output.lock().unwrap().exit_code.get_or_insert(-1);
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            });
        }
        // Lifetime cap: terminate and reap the complete owned tree, including
        // descendants whose original shell leader has already exited.
        {
            let child = child.clone();
            let output = output.clone();
            let cleanup = self.jobs.cleanup.clone();
            tokio::spawn(async move {
                tokio::time::sleep(lifetime).await;
                if output.lock().unwrap().exit_code.is_none() {
                    while terminate_background_job(&cleanup, &child, &output)
                        .await
                        .is_err()
                    {
                        tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
                    }
                }
            });
        }

        let id = format!("bg-{}", JOB_SEQ.fetch_add(1, Ordering::SeqCst));
        {
            let mut jobs = self.jobs.jobs.lock().unwrap();
            if let Err(e) = self.jobs.make_room(&mut jobs) {
                // Over the cap: don't leak the process we just started.
                let child = child.clone();
                let output = output.clone();
                let cleanup = self.jobs.cleanup.clone();
                tokio::spawn(async move {
                    while terminate_background_job(&cleanup, &child, &output)
                        .await
                        .is_err()
                    {
                        tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
                    }
                });
                return ToolResult::error(e);
            }
            jobs.insert(
                id.clone(),
                Job {
                    child,
                    output,
                    worktree: ctx.worktree.clone(),
                    command: command.to_string(),
                    cursor: 0,
                },
            );
        }
        ToolResult::ok(json!({
            "job_id": id,
            "pid": pid,
            "note": "running in background; read output with shell_output, stop with shell_kill",
        }))
    }
}

pub struct ShellOutput {
    pub jobs: Arc<JobRegistry>,
}

#[async_trait::async_trait]
impl Tool for ShellOutput {
    fn name(&self) -> &'static str {
        "shell_output"
    }
    fn description(&self) -> &'static str {
        "Read new output from a background shell job (started with run_in_background). Returns \
         only output produced since the previous read, plus the job's status. Optionally waits \
         up to wait_ms for the job to produce output or finish."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Id returned by shell with run_in_background"},
                "wait_ms": {"type": "integer", "description": "Block up to this many milliseconds for new output or completion (default: 0)", "minimum": 0, "maximum": 60000}
            },
            "required": ["job_id"]
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(id) = args.get("job_id").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: job_id");
        };
        let wait = Duration::from_millis(args.get("wait_ms").and_then(Value::as_u64).unwrap_or(0));
        let deadline = tokio::time::Instant::now() + wait;

        loop {
            // Snapshot under the registry lock; sleep outside it.
            let read = {
                let mut jobs = self.jobs.jobs.lock().unwrap();
                let Some(job) = jobs.get_mut(id) else {
                    return ToolResult::error(format!("unknown job: {id}"));
                };
                if job.worktree != ctx.worktree {
                    return ToolResult::error(format!("unknown job: {id}"));
                }
                let out = job.output.lock().unwrap();
                if out.bytes.len() > job.cursor || out.exit_code.is_some() {
                    let slice = &out.bytes[job.cursor..];
                    let capped_len = slice.len().min(MAX_CAPTURE_BYTES);
                    let capped = &slice[..capped_len];
                    // Decode only up to the last complete UTF-8 character so
                    // a multi-byte char split across two reads isn't mangled
                    // into replacement chars at the seam; keep the trailing
                    // partial bytes for the next read. Once the process has
                    // exited and this is the final page, flush the remainder.
                    let take = if out.exit_code.is_some() && capped_len == slice.len() {
                        capped.len()
                    } else {
                        match std::str::from_utf8(capped) {
                            Ok(s) => s.len(),
                            Err(e) => e.valid_up_to(),
                        }
                    };
                    let new = String::from_utf8_lossy(&capped[..take]).into_owned();
                    job.cursor += take;
                    Some((
                        new,
                        out.exit_code,
                        out.truncated,
                        out.killed,
                        job.cursor < out.bytes.len(),
                    ))
                } else {
                    None
                }
            };
            match read {
                Some((new_output, exit_code, truncated, killed, more_available)) => {
                    return ToolResult::ok(json!({
                        "job_id": id,
                        "running": exit_code.is_none(),
                        "exit_code": exit_code,
                        "new_output": new_output,
                        "truncated": truncated,
                        "killed": killed,
                        "more_available": more_available,
                    }));
                }
                None if tokio::time::Instant::now() >= deadline => {
                    return ToolResult::ok(json!({
                        "job_id": id,
                        "running": true,
                        "new_output": "",
                    }));
                }
                None => {
                    tokio::select! {
                        biased;
                        _ = ctx.cancel.cancelled() => {
                            return ToolResult::error("shell output wait cancelled");
                        }
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
            }
        }
    }
}

pub struct ShellKill {
    pub jobs: Arc<JobRegistry>,
}

#[async_trait::async_trait]
impl Tool for ShellKill {
    fn name(&self) -> &'static str {
        "shell_kill"
    }
    fn description(&self) -> &'static str {
        "Stop a background shell job started with run_in_background."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Id returned by shell with run_in_background"}
            },
            "required": ["job_id"]
        })
    }
    fn mutates(&self) -> bool {
        // Only reaches processes the (already gated) shell tool started.
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(id) = args.get("job_id").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: job_id");
        };
        let (child, output, command) = {
            let jobs = self.jobs.jobs.lock().unwrap();
            let Some(job) = jobs.get(id) else {
                return ToolResult::error(format!("unknown job: {id}"));
            };
            if job.worktree != ctx.worktree {
                return ToolResult::error(format!("unknown job: {id}"));
            }
            (job.child.clone(), job.output.clone(), job.command.clone())
        };
        if output.lock().unwrap().exit_code.is_some() {
            return ToolResult::ok(json!({
                "job_id": id,
                "command": command,
                "already_finished": true,
            }));
        }
        if let Err(e) = terminate_background_job(&self.jobs.cleanup, &child, &output).await {
            return ToolResult::error(format!("cannot kill {id}: {e}"));
        }
        ToolResult::ok(json!({
            "job_id": id,
            "command": command,
            "killed": true,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> (Shell, ShellOutput, ShellKill) {
        let jobs = Arc::new(JobRegistry::default());
        (
            Shell::new(jobs.clone()),
            ShellOutput { jobs: jobs.clone() },
            ShellKill { jobs },
        )
    }

    #[tokio::test]
    async fn runs_in_worktree_and_reports_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "hi").unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();

        let res = shell.run(&ctx, &json!({"command": "ls"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert!(res.result["stdout"].as_str().unwrap().contains("hello.txt"));
        assert_eq!(res.result["exit_code"], 0);

        let res = shell.run(&ctx, &json!({"command": "exit 3"})).await;
        assert_eq!(res.result["exit_code"], 3);
    }

    #[tokio::test]
    async fn foreground_capture_drains_but_does_not_retain_unbounded_output() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();

        let res = shell
            .run(&ctx, &json!({"command": "yes x | head -c 131072"}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(
            res.result["stdout"].as_str().unwrap().len(),
            MAX_CAPTURE_BYTES
        );
        assert_eq!(res.result["truncated"], true);
    }

    #[tokio::test]
    async fn times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        let res = shell
            .run(&ctx, &json!({"command": "sleep 5", "timeout_secs": 1}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
    }

    #[tokio::test]
    async fn timeout_reports_cleanup_failure_only_after_a_retry_is_acknowledged() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        shell
            .jobs
            .cleanup
            .injected_failures
            .store(1, Ordering::SeqCst);

        let res = shell
            .run(&ctx, &json!({"command": "sleep 5", "timeout_secs": 0}))
            .await;

        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(
            res.result["error"]
                .as_str()
                .is_some_and(|error| error.contains("injected shell process-tree cleanup failure")),
            "the discarded cleanup failure was not propagated: {:?}",
            res.result
        );
    }

    #[tokio::test]
    async fn cancellation_reports_cleanup_failure_only_after_a_retry_is_acknowledged() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        shell
            .jobs
            .cleanup
            .injected_failures
            .store(1, Ordering::SeqCst);
        let running =
            tokio::spawn(async move { shell.run(&ctx, &json!({"command": "sleep 60"})).await });

        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("cleanup retry should eventually be acknowledged")
            .unwrap();

        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(
            res.result["error"]
                .as_str()
                .is_some_and(|error| error.contains("injected shell process-tree cleanup failure")),
            "the discarded cleanup failure was not propagated: {:?}",
            res.result
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancellation_terminates_foreground_process_group_and_reaps_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        let worktree = tmp.path().to_path_buf();
        let running = tokio::spawn(async move {
            shell
                .run(
                    &ctx,
                    &json!({
                        "command": "sleep 60 & child=$!; echo $child > child.pid; wait $child"
                    }),
                )
                .await
        });

        let child_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(worktree.join("child.pid")) {
                    break pid.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("foreground command should start its child");

        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("cancelled shell should acknowledge cleanup promptly")
            .unwrap();
        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert!(
            result.result["error"]
                .as_str()
                .is_some_and(|error| error.contains("cancelled"))
        );
        let descendant = std::fs::read_to_string(format!("/proc/{child_pid}/stat"));
        assert!(
            descendant.is_err()
                || descendant
                    .as_deref()
                    .ok()
                    .and_then(|stat| stat.rsplit_once(") "))
                    .is_some_and(|(_, fields)| fields.starts_with('Z')),
            "shell cancellation returned while its descendant was still running"
        );
    }

    #[tokio::test]
    async fn background_job_streams_output_and_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, _) = tools();

        let res = shell
            .run(
                &ctx,
                &json!({"command": "echo one; sleep 0.2; echo two", "run_in_background": true}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        let id = res.result["job_id"].as_str().unwrap().to_string();

        // Wait for completion; incremental reads never repeat output.
        let mut seen = String::new();
        for _ in 0..100 {
            let res = output
                .run(&ctx, &json!({"job_id": id, "wait_ms": 500}))
                .await;
            seen.push_str(res.result["new_output"].as_str().unwrap());
            if res.result["running"] == false {
                assert_eq!(res.result["exit_code"], 0);
                break;
            }
        }
        assert_eq!(seen, "one\ntwo\n");

        // A follow-up read reports the finished job with no new output.
        let res = output.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(res.result["running"], false);
        assert_eq!(res.result["new_output"], "");
    }

    #[tokio::test]
    async fn background_output_is_paged_into_bounded_tool_results() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, _) = tools();
        let res = shell
            .run(
                &ctx,
                &json!({
                    "command": "yes x | head -c 100000",
                    "run_in_background": true
                }),
            )
            .await;
        let id = res.result["job_id"].as_str().unwrap().to_string();

        let mut received = 0usize;
        for _ in 0..20 {
            let page = output
                .run(&ctx, &json!({"job_id": id, "wait_ms": 500}))
                .await;
            let content = page.result["new_output"].as_str().unwrap();
            assert!(content.len() <= MAX_CAPTURE_BYTES);
            received += content.len();
            if page.result["running"] == false && page.result["more_available"] == false {
                break;
            }
        }
        assert_eq!(received, 100_000);
    }

    #[tokio::test]
    async fn background_job_can_be_killed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, kill) = tools();

        let res = shell
            .run(
                &ctx,
                &json!({"command": "sleep 60", "run_in_background": true}),
            )
            .await;
        let id = res.result["job_id"].as_str().unwrap().to_string();

        let res = kill.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["killed"], true);

        // The waiter records the kill as an exit.
        for _ in 0..100 {
            let res = output
                .run(&ctx, &json!({"job_id": id, "wait_ms": 500}))
                .await;
            if res.result["running"] == false {
                assert_eq!(res.result["killed"], true);
                return;
            }
        }
        panic!("job never reported finished after kill");
    }

    #[tokio::test]
    async fn worktree_eviction_attempts_every_job_and_aggregates_cleanup_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        let first = shell
            .run(
                &ctx,
                &json!({"command": "sleep 60", "run_in_background": true}),
            )
            .await;
        let second = shell
            .run(
                &ctx,
                &json!({"command": "sleep 60", "run_in_background": true}),
            )
            .await;
        let first_id = first.result["job_id"].as_str().unwrap();
        let second_id = second.result["job_id"].as_str().unwrap();
        shell
            .jobs
            .cleanup
            .injected_failures
            .store(2, Ordering::SeqCst);

        let error = shell.jobs.kill_worktree(tmp.path()).await.unwrap_err();

        assert!(
            error.contains(first_id),
            "first job was not attempted: {error}"
        );
        assert!(
            error.contains(second_id),
            "second job was not attempted: {error}"
        );
        shell.jobs.kill_worktree(tmp.path()).await.unwrap();
    }

    #[tokio::test]
    async fn background_job_releases_callers_mutation_lane_after_launch() {
        let tmp = tempfile::tempdir().unwrap();
        let started = tmp.path().join("started");
        let lane = Arc::new(tokio::sync::RwLock::new(()));
        let launch_guard = lane.clone().write_owned().await;
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, kill) = tools();
        let launched = shell
            .run(
                &ctx,
                &json!({
                    "command": "touch started; sleep 60",
                    "run_in_background": true
                }),
            )
            .await;
        let id = launched.result["job_id"].as_str().unwrap().to_string();
        drop(launch_guard);

        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background shell did not reach its blocking command");

        let next_call = tokio::time::timeout(Duration::from_millis(100), lane.write_owned())
            .await
            .expect("a live background job retained the completed launch call's mutation lane");
        drop(next_call);
        let killed = kill.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(killed.status, trouve_protocol::ToolStatus::Ok);
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_pid_file(path: &std::path::Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(path)
                    && let Ok(pid) = pid.trim().parse::<u32>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("background shell should publish its descendant pid")
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_process_exit(pid: u32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("owned process did not exit");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shell_kill_owns_daemonized_descendant_after_leader_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, kill) = tools();
        let launched = shell
            .run(
                &ctx,
                &json!({
                    "command": "nohup sleep 60 </dev/null >/dev/null 2>&1 & echo $! > child.pid",
                    "run_in_background": true
                }),
            )
            .await;
        let id = launched.result["job_id"].as_str().unwrap().to_string();
        let leader_pid = launched.result["pid"].as_u64().unwrap() as u32;
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        wait_for_process_exit(leader_pid).await;

        let state = output.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(state.result["running"], true);

        let killed = kill.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(killed.status, trouve_protocol::ToolStatus::Ok);
        wait_for_process_exit(child_pid).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shell_kill_owns_setsid_descendant_after_leader_exit() {
        assert!(
            trouve_agents::process_env::find_executable("setsid").is_some(),
            "setsid is required"
        );
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, kill) = tools();
        let launched = shell
            .run(
                &ctx,
                &json!({
                    "command": "setsid sh -c 'echo $$ > child.pid; exec sleep 60' </dev/null >/dev/null 2>&1 &",
                    "run_in_background": true
                }),
            )
            .await;
        let id = launched.result["job_id"].as_str().unwrap().to_string();
        let leader_pid = launched.result["pid"].as_u64().unwrap() as u32;
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        wait_for_process_exit(leader_pid).await;

        let state = output.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(state.result["running"], true);

        let killed = kill.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(killed.status, trouve_protocol::ToolStatus::Ok);
        wait_for_process_exit(child_pid).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn lifetime_cap_owns_daemonized_descendant_after_leader_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, _) = tools();
        let launched = shell
            .spawn_background_with_lifetime(
                &ctx,
                "nohup sleep 60 </dev/null >/dev/null 2>&1 & echo $! > child.pid",
                Duration::from_secs(1),
            )
            .await;
        let id = launched.result["job_id"].as_str().unwrap().to_string();
        let leader_pid = launched.result["pid"].as_u64().unwrap() as u32;
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        wait_for_process_exit(leader_pid).await;

        wait_for_process_exit(child_pid).await;
        let state = output.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(state.result["running"], false);
        assert_eq!(state.result["killed"], true);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn killing_job_terminates_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, kill) = tools();
        let res = shell
            .run(
                &ctx,
                &json!({
                    "command": "sleep 60 & child=$!; echo $child > child.pid; wait $child",
                    "run_in_background": true
                }),
            )
            .await;
        let id = res.result["job_id"].as_str().unwrap().to_string();
        let child_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(tmp.path().join("child.pid")) {
                    break pid.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        let res = kill.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::path::Path::new(&format!("/proc/{child_pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("descendant survived process-group kill");
    }

    #[tokio::test]
    async fn jobs_are_scoped_to_their_worktree() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        let ctx1 = ToolCtx {
            worktree: tmp1.path().to_path_buf(),
            ..Default::default()
        };
        let ctx2 = ToolCtx {
            worktree: tmp2.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, kill) = tools();

        let res = shell
            .run(
                &ctx1,
                &json!({"command": "sleep 60", "run_in_background": true}),
            )
            .await;
        let id = res.result["job_id"].as_str().unwrap().to_string();

        let res = output.run(&ctx2, &json!({"job_id": id})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        let res = kill.run(&ctx2, &json!({"job_id": id})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        // Clean up.
        let res = kill.run(&ctx1, &json!({"job_id": id})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
    }
}
