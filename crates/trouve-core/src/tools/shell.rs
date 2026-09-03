//! Shell tools: run a command inside the session worktree, either blocking
//! (the classic one-shot) or as a background job the model can poll with
//! `shell_output` and stop with `shell_kill` — dev servers, long builds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use trouve_agents::process_env::{
    DetachedProcess, ProcessTreeChild, TerminatedEscapee, spawn_process_tree,
};

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
/// A cleanup that cannot be acknowledged is retried this many times (each
/// attempt is itself bounded by the process-tree reap timeout) before the
/// call reports the failure instead of holding the mutation lane for as long
/// as the unowned process lives.
const CLEANUP_ACKNOWLEDGEMENT_ATTEMPTS: u32 = 3;
/// How long a foreground call keeps draining stdout/stderr once its tree is
/// done but a released daemon may still hold the pipes open.
const RELEASED_PIPE_DRAIN: Duration = Duration::from_millis(200);
/// Time a released daemon gets to exit after SIGTERM at worktree eviction
/// before it is killed.
const DETACHED_EXIT_GRACE: Duration = Duration::from_secs(2);
const DETACHED_EXIT_POLL: Duration = Duration::from_millis(50);
/// Soft cap on remembered released daemons; dead entries are pruned first.
const MAX_DETACHED: usize = 512;

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

#[derive(Default)]
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

/// What a process tree left behind once the part it owned was gone.
#[derive(Clone, Debug, Default)]
struct TreeRemnants {
    /// Daemons that moved to their own session and were released.
    detached: Vec<DetachedProcess>,
    /// Descendants that left the process group but not the session and were
    /// killed with the tree.
    terminated_escapees: Vec<TerminatedEscapee>,
    /// The platform released sentinel holders it could not enumerate.
    released_untracked: bool,
}

impl TreeRemnants {
    fn take_from(child: &mut ProcessTreeChild) -> Self {
        Self {
            detached: child.take_detached(),
            terminated_escapees: child.take_terminated_escapees(),
            released_untracked: child.released_untracked(),
        }
    }

    fn is_empty(&self) -> bool {
        self.detached.is_empty() && self.terminated_escapees.is_empty() && !self.released_untracked
    }

    /// Whether something outside the tree may still hold its stdio pipes.
    fn may_hold_pipes(&self) -> bool {
        !self.detached.is_empty() || self.released_untracked
    }

    fn absorb(&mut self, other: Self) {
        for process in other.detached {
            let known = self
                .detached
                .iter()
                .any(|known| known.pid == process.pid && known.start_time == process.start_time);
            if !known {
                self.detached.push(process);
            }
        }
        for escapee in other.terminated_escapees {
            if !self
                .terminated_escapees
                .iter()
                .any(|known| known.pid == escapee.pid)
            {
                self.terminated_escapees.push(escapee);
            }
        }
        self.released_untracked |= other.released_untracked;
    }

    /// Add the structured fields and a human-readable `note` to a tool
    /// result. Nothing is added when there is nothing to report, so the
    /// result of a command that leaves no process behind is unchanged.
    fn annotate(&self, result: &mut Value, cleanup_warning: Option<&str>) {
        let Some(result) = result.as_object_mut() else {
            return;
        };
        let mut note = Vec::new();
        if !self.detached.is_empty() {
            let detached = self.detached.iter().map(|p| (p.pid, p.name.as_str()));
            result.insert("detached".into(), process_list(detached.clone()));
            note.push(format!(
                "Released {} ({}); {} until the session worktree is removed.",
                process_count(self.detached.len(), "detached"),
                describe_processes(detached),
                if self.detached.len() == 1 {
                    "it keeps running"
                } else {
                    "they keep running"
                },
            ));
        }
        if !self.terminated_escapees.is_empty() {
            let escapees = self
                .terminated_escapees
                .iter()
                .map(|p| (p.pid, p.name.as_str()));
            result.insert("killed_escaped".into(), process_list(escapees.clone()));
            note.push(format!(
                "Killed {} ({}) that left the process group but not the session.",
                process_count(self.terminated_escapees.len(), "escaped"),
                describe_processes(escapees),
            ));
        }
        if self.released_untracked {
            note.push(
                "Released descendants outside the process group without tracking them; they \
                 are not stopped when the session worktree is removed."
                    .to_string(),
            );
        }
        if let Some(warning) = cleanup_warning {
            result.insert("cleanup_warning".into(), json!(warning));
            note.push(format!("Warning: {warning}."));
        }
        if !note.is_empty() {
            result.insert("note".into(), json!(note.join(" ")));
        }
    }
}

fn process_list<'a>(processes: impl Iterator<Item = (i32, &'a str)>) -> Value {
    Value::Array(
        processes
            .map(|(pid, name)| json!({"pid": pid, "name": name}))
            .collect(),
    )
}

fn describe_processes<'a>(processes: impl Iterator<Item = (i32, &'a str)>) -> String {
    processes
        .map(|(pid, name)| format!("{name} pid {pid}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn process_count(count: usize, adjective: &str) -> String {
    let plural = if count == 1 { "" } else { "es" };
    format!("{count} {adjective} process{plural}")
}

/// A daemon released from a shell call. The session worktree that started it
/// owns it now: evicting the worktree terminates it.
struct DetachedEntry {
    process: DetachedProcess,
    worktree: PathBuf,
    /// The command that started it, for logs.
    command: String,
}

/// Released daemons, keyed by the worktree whose eviction terminates them.
#[derive(Default)]
struct DetachedRegistry {
    entries: Mutex<Vec<DetachedEntry>>,
}

impl DetachedRegistry {
    fn register(&self, worktree: &Path, command: &str, processes: &[DetachedProcess]) {
        let mut entries = self.entries.lock().unwrap();
        for process in processes {
            let known = entries.iter().any(|entry| {
                entry.process.pid == process.pid && entry.process.start_time == process.start_time
            });
            if known {
                continue;
            }
            tracing::info!(
                pid = process.pid,
                name = %process.name,
                command,
                worktree = %worktree.display(),
                "released a detached process from a shell call"
            );
            entries.push(DetachedEntry {
                process: process.clone(),
                worktree: worktree.to_path_buf(),
                command: command.to_string(),
            });
        }
        if entries.len() > MAX_DETACHED {
            entries.retain(|entry| entry.process.is_alive());
            let excess = entries.len().saturating_sub(MAX_DETACHED);
            entries.drain(..excess);
        }
    }

    /// Terminate every daemon released by `worktree`: ask it to exit, give it
    /// a short grace period, then kill the survivors. Returns the failures.
    async fn terminate_worktree(&self, worktree: &Path) -> Vec<String> {
        let entries: Vec<DetachedEntry> = {
            let mut entries = self.entries.lock().unwrap();
            let (mine, others) = entries
                .drain(..)
                .partition(|entry| entry.worktree == worktree);
            *entries = others;
            mine
        };
        let mut failures = Vec::new();
        let mut pending = Vec::new();
        for entry in entries {
            match entry.process.request_exit() {
                Ok(true) => pending.push(entry),
                Ok(false) => {}
                Err(error) => failures.push(detached_failure(&entry, &error)),
            }
        }
        let deadline = tokio::time::Instant::now() + DETACHED_EXIT_GRACE;
        while !pending.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(DETACHED_EXIT_POLL).await;
            pending.retain(|entry| entry.process.is_alive());
        }
        for entry in pending {
            tracing::warn!(
                pid = entry.process.pid,
                name = %entry.process.name,
                command = entry.command,
                "released process ignored SIGTERM at worktree eviction; killing it"
            );
            if let Err(error) = entry.process.kill() {
                failures.push(detached_failure(&entry, &error));
            }
        }
        failures
    }
}

fn detached_failure(entry: &DetachedEntry, error: &std::io::Error) -> String {
    format!(
        "released process {} (pid {}): {error}",
        entry.process.name, entry.process.pid
    )
}

impl Drop for DetachedRegistry {
    fn drop(&mut self) {
        // The sessions that owned these daemons are going away with the
        // registry; ask them to exit without waiting.
        if let Ok(entries) = self.entries.get_mut() {
            for entry in entries.drain(..) {
                let _ = entry.process.request_exit();
            }
        }
    }
}

/// One background job's process tree and captured output, shared with the
/// waiter and lifetime-cap tasks.
#[derive(Clone)]
struct JobHandle {
    child: Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    output: Arc<Mutex<JobOutput>>,
    /// Worktree the job was started from; other sessions cannot touch it.
    worktree: PathBuf,
    command: String,
}

impl JobHandle {
    /// Move what the tree released or killed into the job output and hand
    /// released daemons to the session registry.
    async fn collect_remnants(&self, registry: &DetachedRegistry) {
        let remnants = TreeRemnants::take_from(&mut *self.child.lock().await);
        self.record_remnants(registry, remnants);
    }

    fn record_remnants(&self, registry: &DetachedRegistry, remnants: TreeRemnants) {
        if remnants.is_empty() {
            return;
        }
        registry.register(&self.worktree, &self.command, &remnants.detached);
        self.output.lock().unwrap().remnants.absorb(remnants);
    }
}

/// One background job: its shared handle and the model's read cursor.
struct Job {
    handle: JobHandle,
    /// How far the model has read (byte offset into `output.bytes`).
    cursor: usize,
}

#[derive(Default)]
struct JobOutput {
    bytes: Vec<u8>,
    truncated: bool,
    exit_code: Option<i32>,
    killed: bool,
    remnants: TreeRemnants,
    /// Set when the process tree could not be cleaned up within the
    /// acknowledgement bound and the job was closed regardless.
    cleanup_warning: Option<String>,
}

/// Shared by the three shell tools; owns every background job and every
/// daemon released from a shell call.
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
    cleanup: Arc<CleanupController>,
    detached: Arc<DetachedRegistry>,
}

static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

async fn terminate_background_job(
    cleanup: &CleanupController,
    detached: &DetachedRegistry,
    job: &JobHandle,
) -> std::io::Result<()> {
    let result = cleanup.terminate_and_reap(&job.child).await;
    // Even a failed attempt has classified the holders it found.
    job.collect_remnants(detached).await;
    let status = result?;
    let mut output = job.output.lock().unwrap();
    output.killed = true;
    output.exit_code.get_or_insert(status.code().unwrap_or(-1));
    Ok(())
}

/// [`terminate_background_job`] for tasks with nobody to report to: retry
/// an unacknowledged cleanup up to the bound, then close the job with a
/// warning rather than retrying for as long as the unowned process lives.
async fn terminate_background_job_bounded(
    cleanup: &CleanupController,
    detached: &DetachedRegistry,
    job: &JobHandle,
) {
    let mut attempts = 0;
    let error = loop {
        match terminate_background_job(cleanup, detached, job).await {
            Ok(()) => return,
            Err(error) => {
                attempts += 1;
                if attempts >= CLEANUP_ACKNOWLEDGEMENT_ATTEMPTS {
                    break error;
                }
                tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
            }
        }
    };
    let warning = unacknowledged_cleanup_warning(&error);
    tracing::warn!(
        command = job.command,
        %warning,
        "closing a background shell job whose process-tree cleanup was not acknowledged"
    );
    let mut output = job.output.lock().unwrap();
    output.killed = true;
    output.exit_code.get_or_insert(-1);
    output.cleanup_warning = Some(warning);
}

fn unacknowledged_cleanup_warning(error: &impl std::fmt::Display) -> String {
    format!(
        "process-tree cleanup was not acknowledged after \
         {CLEANUP_ACKNOWLEDGEMENT_ATTEMPTS} attempts: {error}"
    )
}

/// Outcome of terminating a foreground call's process tree.
enum ForegroundCleanup {
    /// The tree is empty. `retried_after` carries the first failure when it
    /// took more than one attempt.
    Acknowledged {
        status: std::process::ExitStatus,
        retried_after: Option<String>,
    },
    /// The attempt bound was exhausted; the tree may still hold processes.
    Unacknowledged { error: String },
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
            .filter(|(_, j)| j.handle.output.lock().unwrap().exit_code.is_some())
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

    /// Stop every running job, and every daemon released from a shell call,
    /// belonging to a worktree being removed.
    pub async fn kill_worktree(&self, worktree: &Path) -> Result<(), String> {
        let jobs: Vec<(String, JobHandle)> = {
            let jobs = self.jobs.lock().unwrap();
            jobs.iter()
                .filter(|(_, job)| job.handle.worktree == worktree)
                .filter(|(_, job)| job.handle.output.lock().unwrap().exit_code.is_none())
                .map(|(id, job)| (id.clone(), job.handle.clone()))
                .collect()
        };
        let mut failures = Vec::new();
        for (id, job) in jobs {
            if let Err(error) = terminate_background_job(&self.cleanup, &self.detached, &job).await
            {
                failures.push(format!("{id}: {error}"));
            }
        }
        // Jobs first: stopping one can release further daemons for this
        // worktree.
        failures.extend(self.detached.terminate_worktree(worktree).await);
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

/// Foreground capture of one stream. Bytes accumulate in a shared buffer so
/// the call can return what it has when the pipe stays open after the tree
/// is done — a released daemon may have inherited it.
struct Capture {
    buffer: Arc<Mutex<CapturedOutput>>,
    reader: tokio::task::JoinHandle<()>,
}

impl Capture {
    fn start(stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>) -> Self {
        let buffer = Arc::new(Mutex::new(CapturedOutput::default()));
        let reader = tokio::spawn(read_capped(stream, buffer.clone()));
        Self { buffer, reader }
    }

    fn abort(&self) {
        self.reader.abort();
    }

    /// Wait for end-of-file — at most `drain_limit` when something outside
    /// the tree may hold the pipe — then take what was captured.
    async fn finish(mut self, drain_limit: Option<Duration>) -> CapturedOutput {
        match drain_limit {
            None => {
                let _ = (&mut self.reader).await;
            }
            Some(limit) => {
                if tokio::time::timeout(limit, &mut self.reader).await.is_err() {
                    self.reader.abort();
                }
            }
        }
        std::mem::take(&mut *self.buffer.lock().unwrap())
    }
}

async fn read_capped(
    stream: Option<impl tokio::io::AsyncRead + Unpin>,
    sink: Arc<Mutex<CapturedOutput>>,
) {
    use tokio::io::AsyncReadExt as _;

    let Some(mut stream) = stream else { return };
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let mut captured = sink.lock().unwrap();
        let room = MAX_CAPTURE_BYTES.saturating_sub(captured.bytes.len());
        let retained = read.min(room);
        captured.bytes.extend_from_slice(&buffer[..retained]);
        captured.truncated |= retained < read;
    }
}

async fn foreground_result(
    status: std::process::ExitStatus,
    stdout: Capture,
    stderr: Capture,
    remnants: &TreeRemnants,
    cleanup_warning: Option<&str>,
) -> ToolResult {
    let drain_limit = remnants.may_hold_pipes().then_some(RELEASED_PIPE_DRAIN);
    let (stdout, stderr) = tokio::join!(stdout.finish(drain_limit), stderr.finish(drain_limit));
    let (stdout, stdout_truncated) = stdout.into_string();
    let (stderr, stderr_truncated) = stderr.into_string();
    let mut result = json!({
        "exit_code": status.code(),
        "stdout": stdout,
        "stderr": stderr,
        "truncated": stdout_truncated || stderr_truncated,
    });
    remnants.annotate(&mut result, cleanup_warning);
    ToolResult::ok(result)
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
         shell_output and stop it with shell_kill. Processes the command leaves behind are \
         stopped with it, except daemons that detach into their own session (build caches, \
         package-manager daemons): those keep running, are reported in the result, and are \
         stopped when the session worktree is removed."
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
        // A daemon that moves to its own session (build cache, package
        // manager) belongs to the session worktree, not to this call.
        child.release_detached_descendants();
        // Drain both pipes while the process runs; waiting first can
        // deadlock once a pipe fills its kernel buffer.
        let stdout = Capture::start(child.take_stdout());
        let stderr = Capture::start(child.take_stderr());
        let child = Arc::new(tokio::sync::Mutex::new(child));
        let wait = {
            let child = child.clone();
            async move { child.lock().await.wait_and_cleanup().await }
        };
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                let cleanup = self.cleanup_foreground_until_acknowledged(&child).await;
                stdout.abort();
                stderr.abort();
                self.interrupted_result(ctx, command, &child, "command cancelled".to_string(), cleanup)
                    .await
            }
            outcome = tokio::time::timeout(timeout, wait) => match outcome {
            Err(_) => {
                let cleanup = self.cleanup_foreground_until_acknowledged(&child).await;
                stdout.abort();
                stderr.abort();
                let message = format!("command timed out after {}s", timeout.as_secs());
                self.interrupted_result(ctx, command, &child, message, cleanup).await
            }
            Ok(Err(error)) => {
                let completed_status = child.lock().await.leader_status();
                let cleanup = self.cleanup_foreground_until_acknowledged(&child).await;
                if let Some(status) = completed_status {
                    let remnants = self.collect_foreground_remnants(ctx, command, &child).await;
                    let (retry_error, cleanup_warning) = match &cleanup {
                        ForegroundCleanup::Acknowledged { retried_after, .. } => {
                            (retried_after.clone(), None)
                        }
                        ForegroundCleanup::Unacknowledged { error } => {
                            (Some(error.clone()), Some(unacknowledged_cleanup_warning(error)))
                        }
                    };
                    tracing::warn!(
                        %error,
                        retry_error = retry_error.as_deref(),
                        "shell process completed before a transient cleanup acknowledgement failure"
                    );
                    return foreground_result(
                        status,
                        stdout,
                        stderr,
                        &remnants,
                        cleanup_warning.as_deref(),
                    )
                    .await;
                }
                stdout.abort();
                stderr.abort();
                let message = match &cleanup {
                    ForegroundCleanup::Acknowledged {
                        status,
                        retried_after: None,
                    } => format!("shell failed: {error}; cleanup exit status: {status}"),
                    _ => format!("shell failed: {error}"),
                };
                self.interrupted_result(ctx, command, &child, message, cleanup).await
            }
            Ok(Ok(status)) => {
                let remnants = self.collect_foreground_remnants(ctx, command, &child).await;
                foreground_result(status, stdout, stderr, &remnants, None).await
            }
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
    /// keeping the future pending therefore quarantines the lane while
    /// cleanup cannot be acknowledged. The retries are bounded: a tree that
    /// still cannot be proven empty after them is reported and abandoned
    /// rather than freezing the lane for as long as its stragglers live.
    async fn cleanup_foreground_until_acknowledged(
        &self,
        child: &Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    ) -> ForegroundCleanup {
        let mut first_failure = None;
        let mut attempts = 0;
        loop {
            match self.jobs.cleanup.terminate_and_reap(child).await {
                Ok(status) => {
                    return ForegroundCleanup::Acknowledged {
                        status,
                        retried_after: first_failure,
                    };
                }
                Err(error) => {
                    attempts += 1;
                    let error = error.to_string();
                    if attempts >= CLEANUP_ACKNOWLEDGEMENT_ATTEMPTS {
                        tracing::warn!(
                            %error,
                            attempts,
                            "abandoning foreground shell process-tree cleanup"
                        );
                        return ForegroundCleanup::Unacknowledged { error };
                    }
                    first_failure.get_or_insert(error);
                    tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
                }
            }
        }
    }

    /// Drain what the tree released or killed and hand released daemons to
    /// the session registry.
    async fn collect_foreground_remnants(
        &self,
        ctx: &ToolCtx,
        command: &str,
        child: &Arc<tokio::sync::Mutex<ProcessTreeChild>>,
    ) -> TreeRemnants {
        let remnants = TreeRemnants::take_from(&mut *child.lock().await);
        if !remnants.detached.is_empty() {
            self.jobs
                .detached
                .register(&ctx.worktree, command, &remnants.detached);
        }
        remnants
    }

    /// The error result of a call stopped before its command completed:
    /// what the cleanup reported, plus anything the tree left behind.
    async fn interrupted_result(
        &self,
        ctx: &ToolCtx,
        command: &str,
        child: &Arc<tokio::sync::Mutex<ProcessTreeChild>>,
        message: String,
        cleanup: ForegroundCleanup,
    ) -> ToolResult {
        let remnants = self.collect_foreground_remnants(ctx, command, child).await;
        let message = match cleanup {
            ForegroundCleanup::Acknowledged {
                retried_after: None,
                ..
            } => message,
            ForegroundCleanup::Acknowledged {
                retried_after: Some(error),
                ..
            } => format!("{message}; process-tree cleanup required a retry after: {error}"),
            ForegroundCleanup::Unacknowledged { error } => {
                format!("{message}; {}", unacknowledged_cleanup_warning(&error))
            }
        };
        let mut result = ToolResult::error(message);
        remnants.annotate(&mut result.result, None);
        result
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
        child.release_detached_descendants();
        let pid = child.id();
        let output = Arc::new(Mutex::new(JobOutput::default()));
        pump(child.take_stdout(), output.clone());
        pump(child.take_stderr(), output.clone());
        let job = JobHandle {
            child: Arc::new(tokio::sync::Mutex::new(child)),
            output,
            worktree: ctx.worktree.clone(),
            command: command.to_string(),
        };
        // Waiter: the job is complete only when the leader and every
        // descendant it still owns has exited. Process-tree ownership remains
        // independent of the session mutation lane, which covers the launch
        // call rather than the lifetime of a service intentionally left
        // running in the background.
        {
            let job = job.clone();
            let cleanup = self.jobs.cleanup.clone();
            let detached = self.jobs.detached.clone();
            tokio::spawn(async move {
                loop {
                    let (status, remnants) = {
                        let mut child = job.child.lock().await;
                        let status = child.try_wait_tree();
                        (status, TreeRemnants::take_from(&mut child))
                    };
                    job.record_remnants(&detached, remnants);
                    match status {
                        Ok(Some(status)) => {
                            job.output
                                .lock()
                                .unwrap()
                                .exit_code
                                .get_or_insert(status.code().unwrap_or(-1));
                            break;
                        }
                        Ok(None) => {
                            // Closed without acknowledgement by the lifetime
                            // cap or a kill: nothing left to wait for.
                            if job.output.lock().unwrap().exit_code.is_some() {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(_) => {
                            // A liveness-query failure must not leave an
                            // untracked descendant running behind a job that
                            // looks finished.
                            terminate_background_job_bounded(&cleanup, &detached, &job).await;
                            break;
                        }
                    }
                }
            });
        }
        // Lifetime cap: terminate and reap the complete owned tree, including
        // descendants whose original shell leader has already exited.
        {
            let job = job.clone();
            let cleanup = self.jobs.cleanup.clone();
            let detached = self.jobs.detached.clone();
            tokio::spawn(async move {
                tokio::time::sleep(lifetime).await;
                if job.output.lock().unwrap().exit_code.is_none() {
                    terminate_background_job_bounded(&cleanup, &detached, &job).await;
                }
            });
        }

        let id = format!("bg-{}", JOB_SEQ.fetch_add(1, Ordering::SeqCst));
        {
            let mut jobs = self.jobs.jobs.lock().unwrap();
            if let Err(e) = self.jobs.make_room(&mut jobs) {
                // Over the cap: don't leak the process we just started.
                let job = job.clone();
                let cleanup = self.jobs.cleanup.clone();
                let detached = self.jobs.detached.clone();
                tokio::spawn(async move {
                    terminate_background_job_bounded(&cleanup, &detached, &job).await;
                });
                return ToolResult::error(e);
            }
            jobs.insert(
                id.clone(),
                Job {
                    handle: job,
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

/// One `shell_output` read, snapshotted under the registry lock.
struct OutputPage {
    new_output: String,
    exit_code: Option<i32>,
    truncated: bool,
    killed: bool,
    more_available: bool,
    remnants: TreeRemnants,
    cleanup_warning: Option<String>,
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
            let page = {
                let mut jobs = self.jobs.jobs.lock().unwrap();
                let Some(job) = jobs.get_mut(id) else {
                    return ToolResult::error(format!("unknown job: {id}"));
                };
                if job.handle.worktree != ctx.worktree {
                    return ToolResult::error(format!("unknown job: {id}"));
                }
                let out = job.handle.output.lock().unwrap();
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
                    let new_output = String::from_utf8_lossy(&capped[..take]).into_owned();
                    job.cursor += take;
                    Some(OutputPage {
                        new_output,
                        exit_code: out.exit_code,
                        truncated: out.truncated,
                        killed: out.killed,
                        more_available: job.cursor < out.bytes.len(),
                        remnants: out.remnants.clone(),
                        cleanup_warning: out.cleanup_warning.clone(),
                    })
                } else {
                    None
                }
            };
            match page {
                Some(page) => {
                    let mut result = json!({
                        "job_id": id,
                        "running": page.exit_code.is_none(),
                        "exit_code": page.exit_code,
                        "new_output": page.new_output,
                        "truncated": page.truncated,
                        "killed": page.killed,
                        "more_available": page.more_available,
                    });
                    page.remnants
                        .annotate(&mut result, page.cleanup_warning.as_deref());
                    return ToolResult::ok(result);
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
        let job = {
            let jobs = self.jobs.jobs.lock().unwrap();
            let Some(job) = jobs.get(id) else {
                return ToolResult::error(format!("unknown job: {id}"));
            };
            if job.handle.worktree != ctx.worktree {
                return ToolResult::error(format!("unknown job: {id}"));
            }
            job.handle.clone()
        };
        if job.output.lock().unwrap().exit_code.is_some() {
            return ToolResult::ok(json!({
                "job_id": id,
                "command": job.command,
                "already_finished": true,
            }));
        }
        if let Err(e) =
            terminate_background_job(&self.jobs.cleanup, &self.jobs.detached, &job).await
        {
            return ToolResult::error(format!("cannot kill {id}: {e}"));
        }
        let mut result = json!({
            "job_id": id,
            "command": job.command,
            "killed": true,
        });
        let remnants = job.output.lock().unwrap().remnants.clone();
        remnants.annotate(&mut result, None);
        ToolResult::ok(result)
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

    /// Poll `shell_output` until the job reports that it is no longer running.
    async fn wait_for_job_exit(output: &ShellOutput, ctx: &ToolCtx, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = output.run(ctx, &json!({"job_id": id})).await;
                if state.result["running"] == false {
                    break state.result;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("background job did not finish")
    }

    #[cfg(target_os = "linux")]
    fn reported_pids(result: &Value, field: &str) -> Vec<u32> {
        result[field]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|process| process["pid"].as_u64())
                    .map(|pid| pid as u32)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    fn process_exists(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    #[cfg(target_os = "linux")]
    fn require_setsid() {
        assert!(
            trouve_agents::process_env::find_executable("setsid").is_some(),
            "setsid is required"
        );
    }

    /// Start a daemon in its own session and return only once it is there:
    /// until it has called `setsid()` it is still a member of the call's
    /// process group and is stopped with the call.
    #[cfg(target_os = "linux")]
    const SETSID_DAEMON: &str = "setsid sh -c 'echo $$ > child.pid; exec sleep 60' \
        </dev/null >/dev/null 2>&1 & while [ ! -s child.pid ]; do sleep 0.01; done";

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn background_job_releases_setsid_descendant_until_worktree_eviction() {
        require_setsid();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, kill) = tools();
        let launched = shell
            .run(
                &ctx,
                &json!({"command": SETSID_DAEMON, "run_in_background": true}),
            )
            .await;
        let id = launched.result["job_id"].as_str().unwrap().to_string();
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;

        // The daemon left the session: the job completes without it.
        let state = wait_for_job_exit(&output, &ctx, &id).await;
        assert_eq!(state["exit_code"], 0, "{state:?}");
        assert_eq!(state["killed"], false);
        assert_eq!(reported_pids(&state, "detached"), vec![child_pid]);
        assert!(
            state["note"]
                .as_str()
                .is_some_and(|note| note.starts_with("Released 1 detached process (")),
            "{state:?}"
        );
        assert!(process_exists(child_pid));

        let killed = kill.run(&ctx, &json!({"job_id": id})).await;
        assert_eq!(killed.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(killed.result["already_finished"], true);
        assert!(process_exists(child_pid));

        // Evicting the worktree stops what its session released.
        shell.jobs.kill_worktree(tmp.path()).await.unwrap();
        wait_for_process_exit(child_pid).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn foreground_call_releases_setsid_descendant_and_reports_it() {
        require_setsid();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        let started = std::time::Instant::now();
        let res = shell
            .run(&ctx, &json!({"command": SETSID_DAEMON, "timeout_secs": 5}))
            .await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the call waited for the released daemon"
        );
        assert_eq!(res.result["exit_code"], 0);
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        assert_eq!(reported_pids(&res.result, "detached"), vec![child_pid]);
        assert!(
            res.result["note"]
                .as_str()
                .is_some_and(|note| note.starts_with("Released 1 detached process (")),
            "{:?}",
            res.result
        );
        assert!(process_exists(child_pid));

        shell.jobs.kill_worktree(tmp.path()).await.unwrap();
        wait_for_process_exit(child_pid).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn foreground_call_does_not_wait_for_pipes_held_by_a_released_daemon() {
        require_setsid();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        // No stdout/stderr redirection: the daemon inherits the call's pipes.
        let command = "echo started; setsid sh -c 'echo $$ > child.pid; exec sleep 60' \
            </dev/null & while [ ! -s child.pid ]; do sleep 0.01; done";
        let started = std::time::Instant::now();
        let res = shell
            .run(&ctx, &json!({"command": command, "timeout_secs": 5}))
            .await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the call waited for a pipe the released daemon holds"
        );
        assert_eq!(res.result["exit_code"], 0);
        assert_eq!(res.result["stdout"], "started\n");
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        assert_eq!(reported_pids(&res.result, "detached"), vec![child_pid]);

        shell.jobs.kill_worktree(tmp.path()).await.unwrap();
        wait_for_process_exit(child_pid).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn same_session_escapee_is_killed_with_the_call_and_reported() {
        assert!(
            trouve_agents::process_env::find_executable("bash").is_some(),
            "bash is required"
        );
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        // Job control moves the background job into its own process group
        // without starting a new session.
        let command = "bash -c 'set -m; sleep 60 </dev/null >/dev/null 2>&1 & echo $! > child.pid'";
        let res = shell
            .run(&ctx, &json!({"command": command, "timeout_secs": 5}))
            .await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        assert_eq!(
            reported_pids(&res.result, "killed_escaped"),
            vec![child_pid]
        );
        assert!(reported_pids(&res.result, "detached").is_empty());
        assert!(
            res.result["note"]
                .as_str()
                .is_some_and(|note| note.starts_with("Killed 1 escaped process (")),
            "{:?}",
            res.result
        );
        wait_for_process_exit(child_pid).await;
    }

    #[tokio::test]
    async fn foreground_cleanup_gives_up_after_the_acknowledgement_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        shell.jobs.cleanup.injected_failures.store(
            u64::from(CLEANUP_ACKNOWLEDGEMENT_ATTEMPTS),
            Ordering::SeqCst,
        );

        let res = tokio::time::timeout(
            Duration::from_secs(2),
            shell.run(&ctx, &json!({"command": "sleep 5", "timeout_secs": 0})),
        )
        .await
        .expect("an unacknowledged cleanup must not hold the call indefinitely");

        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        let error = res.result["error"].as_str().unwrap();
        assert!(
            error.contains("was not acknowledged after 3 attempts"),
            "{error}"
        );
        assert!(
            error.contains("injected shell process-tree cleanup failure"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn lifetime_cap_closes_the_job_after_the_acknowledgement_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, output, _) = tools();
        shell.jobs.cleanup.injected_failures.store(
            u64::from(CLEANUP_ACKNOWLEDGEMENT_ATTEMPTS),
            Ordering::SeqCst,
        );
        let launched = shell
            .spawn_background_with_lifetime(&ctx, "sleep 60", Duration::from_millis(100))
            .await;
        let id = launched.result["job_id"].as_str().unwrap().to_string();

        let state = wait_for_job_exit(&output, &ctx, &id).await;
        assert_eq!(state["killed"], true, "{state:?}");
        assert_eq!(state["exit_code"], -1);
        assert!(
            state["cleanup_warning"]
                .as_str()
                .is_some_and(|warning| warning.contains("not acknowledged after 3 attempts")),
            "{state:?}"
        );
        assert!(
            state["note"]
                .as_str()
                .is_some_and(|note| note.starts_with("Warning: process-tree cleanup")),
            "{state:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn worktree_eviction_tolerates_a_released_daemon_that_already_exited() {
        require_setsid();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let (shell, _, _) = tools();
        let res = shell
            .run(&ctx, &json!({"command": SETSID_DAEMON, "timeout_secs": 5}))
            .await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
        assert_eq!(reported_pids(&res.result, "detached"), vec![child_pid]);

        assert_eq!(unsafe { libc::kill(child_pid as i32, libc::SIGKILL) }, 0);
        wait_for_process_exit(child_pid).await;

        shell.jobs.kill_worktree(tmp.path()).await.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_the_registry_asks_released_daemons_to_exit() {
        require_setsid();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let child_pid = {
            let (shell, _, _) = tools();
            let res = shell
                .run(&ctx, &json!({"command": SETSID_DAEMON, "timeout_secs": 5}))
                .await;
            assert_eq!(
                res.status,
                trouve_protocol::ToolStatus::Ok,
                "{:?}",
                res.result
            );
            let child_pid = wait_for_pid_file(&tmp.path().join("child.pid")).await;
            assert!(process_exists(child_pid));
            child_pid
        };
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
