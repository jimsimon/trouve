//! Shell tools: run a command inside the session worktree, either blocking
//! (the classic one-shot) or as a background job the model can poll with
//! `shell_output` and stop with `shell_kill` — dev servers, long builds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_CAPTURE_BYTES: usize = 32 * 1024;
/// Background jobs keep more output than the one-shot capture: they live
/// longer and are read incrementally.
const MAX_JOB_BYTES: usize = 1024 * 1024;
/// Hard lifetime cap for a background job; runaway processes die with it.
const MAX_JOB_SECS: u64 = 3600;
const MAX_JOBS: usize = 16;

fn truncate_utf8(mut bytes: Vec<u8>) -> (String, bool) {
    let truncated = bytes.len() > MAX_CAPTURE_BYTES;
    if truncated {
        bytes.truncate(MAX_CAPTURE_BYTES);
    }
    (String::from_utf8_lossy(&bytes).into_owned(), truncated)
}

/// One background job: the child (for kill/wait), its captured output, and
/// the model's read cursor.
struct Job {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
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
}

static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

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
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.worktree)
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(timeout, child).await {
            Err(_) => ToolResult::error(format!("command timed out after {}s", timeout.as_secs())),
            Ok(Err(e)) => ToolResult::error(format!("failed to spawn: {e}")),
            Ok(Ok(output)) => {
                let (stdout, stdout_truncated) = truncate_utf8(output.stdout);
                let (stderr, stderr_truncated) = truncate_utf8(output.stderr);
                ToolResult::ok(json!({
                    "exit_code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                    "truncated": stdout_truncated || stderr_truncated,
                }))
            }
        }
    }
}

impl Shell {
    async fn spawn_background(&self, ctx: &ToolCtx, command: &str) -> ToolResult {
        let mut child = match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.worktree)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("failed to spawn: {e}")),
        };
        let pid = child.id();
        let output = Arc::new(Mutex::new(JobOutput::default()));
        pump(child.stdout.take(), output.clone());
        pump(child.stderr.take(), output.clone());
        let child = Arc::new(tokio::sync::Mutex::new(child));

        // Waiter: record the exit code when the process ends (also fired by
        // shell_kill and the lifetime cap below).
        {
            let child = child.clone();
            let output = output.clone();
            tokio::spawn(async move {
                let status = child.lock().await.wait().await;
                let mut out = output.lock().unwrap();
                out.exit_code = Some(status.ok().and_then(|s| s.code()).unwrap_or(-1));
            });
        }
        // Lifetime cap: kill anything still running after MAX_JOB_SECS.
        {
            let child = child.clone();
            let output = output.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(MAX_JOB_SECS)).await;
                if output.lock().unwrap().exit_code.is_none() {
                    output.lock().unwrap().killed = true;
                    let _ = child.lock().await.start_kill();
                }
            });
        }

        let id = format!("bg-{}", JOB_SEQ.fetch_add(1, Ordering::SeqCst));
        {
            let mut jobs = self.jobs.jobs.lock().unwrap();
            if let Err(e) = self.jobs.make_room(&mut jobs) {
                // Over the cap: don't leak the process we just started.
                let child = child.clone();
                tokio::spawn(async move {
                    let _ = child.lock().await.start_kill();
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
                    let new = String::from_utf8_lossy(&out.bytes[job.cursor..]).into_owned();
                    job.cursor = out.bytes.len();
                    Some((new, out.exit_code, out.truncated, out.killed))
                } else {
                    None
                }
            };
            match read {
                Some((new_output, exit_code, truncated, killed)) => {
                    return ToolResult::ok(json!({
                        "job_id": id,
                        "running": exit_code.is_none(),
                        "exit_code": exit_code,
                        "new_output": new_output,
                        "truncated": truncated,
                        "killed": killed,
                    }));
                }
                None if tokio::time::Instant::now() >= deadline => {
                    return ToolResult::ok(json!({
                        "job_id": id,
                        "running": true,
                        "new_output": "",
                    }));
                }
                None => tokio::time::sleep(Duration::from_millis(50)).await,
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
        output.lock().unwrap().killed = true;
        if let Err(e) = child.lock().await.start_kill() {
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
            Shell { jobs: jobs.clone() },
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
