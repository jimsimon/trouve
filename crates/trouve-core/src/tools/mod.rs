//! Tools and the `ToolExecutor` chokepoint (invariant 3).
//!
//! The agent loop never performs side effects itself: it gates each call
//! through the permission layer and hands execution to a `ToolExecutor`.
//! Local mode uses [`LocalToolExecutor`]; cloud isolation later swaps in a
//! container-backed implementation without touching the loop.

mod diff;
mod fs;
mod glob;
mod grep;
mod patch;
mod search;
mod shell;
mod todo;
mod web;

pub use search::{VENDOR_SEARCH_GUIDANCE, gc_index_store_in_background, warm_index_in_background};

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use trouve_protocol::ToolStatus;
use trouve_providers::ToolSpec;

const REVIEW_OPTIONAL_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const REVIEW_FETCH_TERMINATION_GRACE: Duration = Duration::from_secs(2);
const REVIEW_HISTORY_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const REVIEW_FETCH_STDERR_MAX_BYTES: usize = 8 * 1024;
const REVIEW_HISTORY_REF_LIMIT: usize = 3;
const REVIEW_GIT_TRACE_ENV_VARS: &[&str] = &[
    "GIT_TRACE",
    "GIT_TRACE2",
    "GIT_TRACE2_EVENT",
    "GIT_TRACE2_PERF",
    "GIT_TRACE_CURL",
    "GIT_TRACE_CURL_NO_DATA",
    "GIT_TRACE_PACKET",
    "GIT_TRACE_REDACT",
];

fn harden_authenticated_review_git_command(command: &mut tokio::process::Command) {
    for variable in REVIEW_GIT_TRACE_ENV_VARS {
        command.env_remove(variable);
    }
}

fn sanitize_review_fetch_stderr(stderr: &str, auth: &str) -> String {
    stderr
        .lines()
        .map(|line| {
            if line.to_ascii_lowercase().contains("authorization:") {
                "[redacted git authorization trace]".to_owned()
            } else {
                line.replace(auth, "[redacted]")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn review_history_ref_name(
    pull_number: u64,
    job_id: &str,
    index: usize,
) -> std::result::Result<String, String> {
    if job_id.is_empty()
        || !job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid review job id for temporary ref: {job_id}"));
    }
    Ok(format!("trouve-history-{pull_number}-{job_id}-{index}"))
}

#[cfg(unix)]
fn isolate_review_git_process(command: &mut tokio::process::Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_review_git_process(_command: &mut tokio::process::Command) {}

#[cfg(windows)]
async fn terminate_windows_review_git_tree(pid: Option<u32>) -> std::io::Result<()> {
    let Some(pid) = pid else {
        return Ok(());
    };
    let status = tokio::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "taskkill exited with {status}"
        )))
    }
}

#[cfg(unix)]
fn signal_review_git_process_group(pid: Option<u32>, signal: i32) -> std::io::Result<()> {
    let Some(pid) = pid else {
        return Ok(());
    };
    let result = unsafe { libc::kill(-(pid as i32), signal) };
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

#[cfg(unix)]
fn review_git_process_group_exists(pid: Option<u32>) -> bool {
    let Some(pid) = pid else {
        return false;
    };
    let result = unsafe { libc::kill(-(pid as i32), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(unix)]
async fn wait_for_review_git_process_group_exit(pid: Option<u32>) -> bool {
    let deadline = tokio::time::Instant::now() + REVIEW_FETCH_TERMINATION_GRACE;
    while review_git_process_group_exists(pid) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

#[cfg(unix)]
async fn quiesce_review_git_process_group(pid: Option<u32>) -> std::io::Result<()> {
    if !review_git_process_group_exists(pid) {
        return Ok(());
    }
    let _ = signal_review_git_process_group(pid, libc::SIGTERM);
    if wait_for_review_git_process_group_exit(pid).await {
        return Ok(());
    }
    signal_review_git_process_group(pid, libc::SIGKILL)?;
    if wait_for_review_git_process_group_exit(pid).await {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "review git process group remained alive after SIGKILL",
        ))
    }
}

struct ReviewGitChildGuard {
    child: Option<tokio::process::Child>,
    pid: Option<u32>,
    repository_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    armed: bool,
}

impl ReviewGitChildGuard {
    fn new(
        child: tokio::process::Child,
        repository_guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
            repository_guard: Some(repository_guard),
            armed: true,
        }
    }

    fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("review git child is present")
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn take_repository_guard(&mut self) -> tokio::sync::OwnedMutexGuard<()> {
        self.repository_guard
            .take()
            .expect("review repository guard is present")
    }
}

impl Drop for ReviewGitChildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        let _ = signal_review_git_process_group(self.pid, libc::SIGKILL);
        #[cfg(not(any(unix, windows)))]
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let pid = self.pid;
        let repository_guard = self.repository_guard.take();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                #[cfg(windows)]
                let _ = terminate_windows_review_git_tree(pid).await;
                let _ = child.wait().await;
                #[cfg(unix)]
                let _ = quiesce_review_git_process_group(pid).await;
                #[cfg(not(any(unix, windows)))]
                let _ = pid;
                drop(repository_guard);
            });
        }
    }
}

async fn terminate_review_git_process(guard: &mut ReviewGitChildGuard) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let pid = guard.pid;
        let _ = signal_review_git_process_group(pid, libc::SIGTERM);
        match tokio::time::timeout(REVIEW_FETCH_TERMINATION_GRACE, guard.child_mut().wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => {
                let _ = signal_review_git_process_group(pid, libc::SIGKILL);
                guard.child_mut().wait().await?;
            }
        }
        quiesce_review_git_process_group(pid).await
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        terminate_windows_review_git_tree(guard.pid).await?;
        #[cfg(not(windows))]
        guard.child_mut().start_kill()?;
        guard.child_mut().wait().await?;
        Ok(())
    }
}

fn cleanup_review_fetch_locks(repository_path: &Path, history_refs: &[String]) {
    let git_dir = repository_path.join(".git");
    for history_ref in history_refs {
        let lock = git_dir
            .join("refs/remotes/origin")
            .join(format!("{history_ref}.lock"));
        match std::fs::remove_file(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %lock.display(), %error, "could not clean stale review fetch lock")
            }
        }
    }
}

async fn read_bounded_review_fetch_stderr(mut stderr: tokio::process::ChildStderr) -> String {
    use tokio::io::AsyncReadExt as _;

    let mut captured = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let remaining = REVIEW_FETCH_STDERR_MAX_BYTES.saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..read.min(remaining)]);
            }
        }
    }
    String::from_utf8_lossy(&captured).trim().to_owned()
}

async fn finish_review_fetch_stderr(
    guard: &mut ReviewGitChildGuard,
    stderr_task: &mut tokio::task::JoinHandle<String>,
    wait_timeout: Duration,
) -> std::result::Result<String, String> {
    match tokio::time::timeout(wait_timeout, &mut *stderr_task).await {
        Ok(Ok(stderr)) => return Ok(stderr),
        Ok(Err(_)) => return Ok(String::new()),
        Err(_) => {}
    }
    terminate_review_git_process(guard)
        .await
        .map_err(|error| format!("terminating fetch descendants holding stderr: {error}"))?;
    match tokio::time::timeout(wait_timeout, &mut *stderr_task).await {
        Ok(Ok(stderr)) => Ok(stderr),
        Ok(Err(_)) => Ok(String::new()),
        Err(_) => {
            stderr_task.abort();
            Err("optional git fetch stderr remained open after process termination".into())
        }
    }
}

struct ReviewHistoryFetch {
    sha: String,
    history_ref: String,
    refspec: String,
}

fn optional_review_fetch_command(
    repository_path: &Path,
    auth: &str,
    refspec: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "fetch",
            "--force",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "origin",
            refspec,
        ])
        .current_dir(repository_path)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {auth}"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    harden_authenticated_review_git_command(&mut command);
    isolate_review_git_process(&mut command);
    command
}

/// Fetch each historical SHA independently while sharing one total timeout
/// budget and one repository lock. A missing force-pushed commit therefore
/// cannot roll back refs already fetched for the other history points.
async fn run_optional_review_fetches(
    repository_path: &Path,
    auth: &str,
    fetches: &[ReviewHistoryFetch],
    repository_guard: tokio::sync::OwnedMutexGuard<()>,
) -> Vec<(String, String)> {
    let deadline = tokio::time::Instant::now() + REVIEW_OPTIONAL_FETCH_TIMEOUT;
    let mut repository_guard = Some(repository_guard);
    let mut failures = Vec::new();
    for (index, fetch) in fetches.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            failures.push((
                fetch.sha.clone(),
                "optional review-history fetch budget was exhausted".into(),
            ));
            continue;
        }
        let attempts_left = u32::try_from(fetches.len() - index).unwrap_or(1);
        let attempt_timeout = remaining / attempts_left;
        let mut command = optional_review_fetch_command(repository_path, auth, &fetch.refspec);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                failures.push((
                    fetch.sha.clone(),
                    format!("running optional git fetch: {error}"),
                ));
                continue;
            }
        };
        let mut guard = ReviewGitChildGuard::new(
            child,
            repository_guard
                .take()
                .expect("review repository guard is available"),
        );
        let stderr = match guard.child_mut().stderr.take() {
            Some(stderr) => stderr,
            None => {
                failures.push((
                    fetch.sha.clone(),
                    "optional git fetch stderr was not captured".into(),
                ));
                return failures;
            }
        };
        let mut stderr_task = tokio::spawn(read_bounded_review_fetch_stderr(stderr));
        let result = match tokio::time::timeout(attempt_timeout, guard.child_mut().wait()).await {
            Ok(Ok(status)) => {
                #[cfg(unix)]
                if let Err(error) = quiesce_review_git_process_group(guard.pid).await {
                    Err(format!("stopping optional git fetch helpers: {error}"))
                } else if status.success() {
                    Ok(())
                } else {
                    Err(format!("optional git fetch exited with {status}"))
                }
                #[cfg(not(unix))]
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("optional git fetch exited with {status}"))
                }
            }
            Ok(Err(error)) => match terminate_review_git_process(&mut guard).await {
                Ok(()) => Err(format!("waiting for optional git fetch: {error}")),
                Err(terminate_error) => {
                    failures.push((
                        fetch.sha.clone(),
                        format!(
                            "waiting for optional git fetch: {error}; terminating it: {terminate_error}"
                        ),
                    ));
                    return failures;
                }
            },
            Err(_) => match terminate_review_git_process(&mut guard).await {
                Ok(()) => {
                    cleanup_review_fetch_locks(
                        repository_path,
                        std::slice::from_ref(&fetch.history_ref),
                    );
                    Err(format!(
                        "optional git fetch timed out after {:.1}s",
                        attempt_timeout.as_secs_f64()
                    ))
                }
                Err(error) => {
                    failures.push((
                        fetch.sha.clone(),
                        format!("terminating timed-out optional git fetch: {error}"),
                    ));
                    return failures;
                }
            },
        };
        let stderr = match finish_review_fetch_stderr(
            &mut guard,
            &mut stderr_task,
            REVIEW_FETCH_TERMINATION_GRACE,
        )
        .await
        {
            Ok(stderr) => sanitize_review_fetch_stderr(&stderr, auth),
            Err(stderr_error) => {
                cleanup_review_fetch_locks(
                    repository_path,
                    std::slice::from_ref(&fetch.history_ref),
                );
                let error = match result {
                    Ok(()) => stderr_error,
                    Err(error) => format!("{error}; {stderr_error}"),
                };
                failures.push((fetch.sha.clone(), error));
                return failures;
            }
        };
        guard.disarm();
        repository_guard = Some(guard.take_repository_guard());
        if let Err(mut error) = result {
            if !stderr.is_empty() {
                error.push_str(": ");
                error.push_str(&stderr);
            }
            failures.push((fetch.sha.clone(), error));
        }
    }
    failures
}

/// Execution context: everything a tool may touch. All paths resolve inside
/// the session worktree.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    pub worktree: PathBuf,
    /// Stable owner for thread-scoped tool artifacts. Empty only in isolated
    /// tool tests that do not exercise thread state.
    pub thread_id: String,
    /// Mutable todo snapshot shared by every tool call in one turn. The
    /// engine seeds it from persistence and commits successful updates.
    pub todos: Arc<Mutex<Vec<trouve_protocol::TodoItem>>>,
    /// Config dir for global tool discovery (MCP servers); None in tests.
    pub config_dir: Option<PathBuf>,
    /// Registered workspace repo root: its `.agents/.mcp.json` applies even
    /// before it is committed to the session branch.
    pub workspace_root: Option<PathBuf>,
}

impl ToolCtx {
    /// Resolve a model-supplied path inside the worktree, rejecting absolute
    /// paths, traversal, and symlinks that point outside the worktree.
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let p = Path::new(path);
        if p.is_absolute() {
            bail!("absolute paths are not allowed: {path}");
        }
        for comp in p.components() {
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                _ => bail!("path escapes the worktree: {path}"),
            }
        }
        let joined = self.worktree.join(p);
        // The lexical checks above don't stop symlinks committed to the
        // worktree (git stores arbitrary targets, including absolute paths)
        // from pointing outside it. Canonicalize the deepest existing
        // ancestor — which resolves every symlink on the way, including the
        // target itself when it exists — and require it to stay under the
        // canonicalized worktree. The not-yet-created remainder is safe: it
        // contains only `Normal` components (checked above) and dangling
        // symlinks fail canonicalization rather than being written through.
        let root = self
            .worktree
            .canonicalize()
            .with_context(|| format!("worktree unavailable: {}", self.worktree.display()))?;
        let mut existing = joined.clone();
        while existing.symlink_metadata().is_err() {
            if !existing.pop() {
                bail!("path escapes the worktree: {path}");
            }
        }
        let canon = existing
            .canonicalize()
            .with_context(|| format!("cannot resolve {path}"))?;
        if !canon.starts_with(&root) {
            bail!("path escapes the worktree: {path}");
        }
        Ok(joined)
    }
}

pub struct ToolResult {
    pub status: ToolStatus,
    pub result: Value,
}

impl ToolResult {
    pub fn ok(result: Value) -> Self {
        Self {
            status: ToolStatus::Ok,
            result,
        }
    }
    pub fn error(message: impl std::fmt::Display) -> Self {
        Self {
            status: ToolStatus::Error,
            result: serde_json::json!({"error": message.to_string()}),
        }
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema of the arguments object.
    fn parameters(&self) -> Value;
    /// Whether the tool can change worktree or system state (drives the
    /// permission gate).
    fn mutates(&self) -> bool;
    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult;
}

/// The single chokepoint every side effect flows through.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Tool specs visible from this context (built-ins + workspace MCP
    /// tools, hence async and context-dependent).
    async fn specs(&self, ctx: &ToolCtx) -> Vec<ToolSpec>;
    /// `None` when the tool is unknown.
    fn tool_mutates(&self, name: &str) -> Option<bool>;
    async fn execute(&self, ctx: &ToolCtx, name: &str, args: &Value) -> ToolResult;
    /// Create an engine-owned worktree checkpoint through the same trusted
    /// execution boundary as other Git mutations.
    async fn checkpoint_worktree(
        &self,
        _worktree: &Path,
        _session_id: &str,
        _seq: i64,
        _message: &str,
    ) -> Result<String, String> {
        Err("worktree checkpointing is unavailable in this executor".into())
    }
    /// Prepare the trusted local mirror used by the headless review service.
    /// This is intentionally part of the executor rather than review runtime
    /// code so git/network/filesystem mutations retain one chokepoint.
    async fn sync_review_repository(
        &self,
        _request: &ReviewRepositorySync,
    ) -> Result<PathBuf, String> {
        Err("review repository sync is unavailable in this executor".into())
    }
    /// Read the complete base-to-head diff as per-file segments for the
    /// headless review orchestrator. This remains behind the executor so git
    /// access follows the same audited chokepoint as model-visible tools.
    async fn review_repository_diff(
        &self,
        _request: &ReviewRepositoryDiff,
    ) -> Result<Vec<ReviewDiffFile>, String> {
        Err("review repository diff is unavailable in this executor".into())
    }
    async fn review_repository_merge_base(
        &self,
        _request: &ReviewRepositoryMergeBase,
    ) -> Result<String, String> {
        Err("review repository merge-base is unavailable in this executor".into())
    }
    /// Drop temporary per-job refs after rewritten-history comparison has
    /// consumed the historical objects they kept reachable.
    async fn cleanup_review_repository_history(
        &self,
        _request: &ReviewRepositoryHistoryCleanup,
    ) -> Result<(), String> {
        Err("review repository history cleanup is unavailable in this executor".into())
    }
    /// Release any per-worktree resources (e.g. spawned MCP server
    /// processes) when a session/worktree is going away. Default no-op.
    async fn evict_worktree(&self, _worktree: &Path) {}
}

/// Inputs for one authenticated GitHub App fetch. Tokens are passed through
/// process environment, never embedded in a remote URL or persisted config.
pub struct ReviewRepositorySync {
    pub root: PathBuf,
    pub repository: String,
    pub job_id: String,
    pub pull_number: u64,
    pub base_sha: String,
    pub head_sha: String,
    /// Historical commits used only to reduce rewritten-history review
    /// scope. Failure to fetch one must not prevent the current review.
    pub optional_shas: Vec<String>,
    pub token: String,
}

pub struct ReviewRepositoryDiff {
    pub worktree: PathBuf,
    pub base_sha: String,
    pub head_sha: String,
}

pub struct ReviewRepositoryMergeBase {
    pub worktree: PathBuf,
    pub base_sha: String,
    pub head_sha: String,
}

pub struct ReviewRepositoryHistoryCleanup {
    pub worktree: PathBuf,
    pub job_id: String,
    pub pull_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDiffFile {
    pub path: String,
    pub diff: String,
}

/// Runs tools in-process against the local filesystem/shell, plus any MCP
/// servers configured for the workspace.
pub struct LocalToolExecutor {
    tools: Vec<Arc<dyn Tool>>,
    mcp: crate::mcp::McpManager,
    jobs: Arc<shell::JobRegistry>,
    review_repository_locks: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for LocalToolExecutor {
    fn default() -> Self {
        Self::with_mcp_logs(crate::mcp::McpLogStore::default())
    }
}

impl LocalToolExecutor {
    /// Build with an externally-owned MCP log store so the engine can serve
    /// "view logs" for runtime connections too.
    pub fn with_mcp_logs(logs: crate::mcp::McpLogStore) -> Self {
        // Both search tools share one index cache (indexes are expensive to
        // build, cheap to re-validate, and identical across tools).
        let search_cache = search::shared_cache();
        // The three shell tools share one background-job registry.
        let jobs = Arc::new(shell::JobRegistry::default());
        Self {
            tools: vec![
                Arc::new(fs::ReadFile),
                Arc::new(fs::WriteFile),
                Arc::new(fs::EditFile),
                Arc::new(patch::ApplyPatch),
                Arc::new(fs::ListDir),
                Arc::new(diff::GitDiff),
                Arc::new(glob::Glob),
                Arc::new(shell::Shell { jobs: jobs.clone() }),
                Arc::new(shell::ShellOutput { jobs: jobs.clone() }),
                Arc::new(shell::ShellKill { jobs: jobs.clone() }),
                Arc::new(grep::Grep),
                Arc::new(web::WebFetch::default()),
                Arc::new(todo::TodoWrite),
                Arc::new(search::Search {
                    cache: search_cache.clone(),
                }),
                Arc::new(search::FindRelated {
                    cache: search_cache,
                }),
            ],
            mcp: crate::mcp::McpManager::with_logs(logs),
            jobs,
            review_repository_locks: Mutex::new(HashMap::new()),
        }
    }

    fn find(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }
}

#[async_trait::async_trait]
impl ToolExecutor for LocalToolExecutor {
    async fn specs(&self, ctx: &ToolCtx) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect();
        specs.extend(
            self.mcp
                .specs(
                    ctx.config_dir.as_deref(),
                    ctx.workspace_root.as_deref(),
                    &ctx.worktree,
                )
                .await,
        );
        specs
    }

    fn tool_mutates(&self, name: &str) -> Option<bool> {
        if name.starts_with(crate::mcp::TOOL_PREFIX) {
            // MCP tools are external code: always treated as mutating so
            // the permission layer gates them (first-use approval in
            // non-read-only ask / allow-list modes; the mutating
            // classification makes read-only modes deny them outright).
            return Some(true);
        }
        self.find(name).map(|t| t.mutates())
    }

    async fn execute(&self, ctx: &ToolCtx, name: &str, args: &Value) -> ToolResult {
        if name.starts_with(crate::mcp::TOOL_PREFIX) {
            return match self
                .mcp
                .call(
                    ctx.config_dir.as_deref(),
                    ctx.workspace_root.as_deref(),
                    &ctx.worktree,
                    name,
                    args,
                )
                .await
            {
                Ok((false, value)) => ToolResult::ok(value),
                Ok((true, value)) => ToolResult {
                    status: ToolStatus::Error,
                    result: value,
                },
                Err(e) => ToolResult::error(format!("{e:#}")),
            };
        }
        match self.find(name) {
            Some(tool) => tool.run(ctx, args).await,
            None => ToolResult::error(format!("unknown tool: {name}")),
        }
    }

    async fn checkpoint_worktree(
        &self,
        worktree: &Path,
        session_id: &str,
        seq: i64,
        message: &str,
    ) -> Result<String, String> {
        let worktree = worktree.to_path_buf();
        let session_id = session_id.to_string();
        let message = message.to_string();
        tokio::task::spawn_blocking(move || {
            crate::git::checkpoint(&worktree, &session_id, seq, &message)
        })
        .await
        .map_err(|error| format!("checkpoint worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    async fn sync_review_repository(
        &self,
        request: &ReviewRepositorySync,
    ) -> Result<PathBuf, String> {
        use base64::Engine as _;

        let repository_path = request.root.join(&request.repository);
        let repository_lock = {
            let mut locks = self.review_repository_locks.lock().unwrap();
            locks
                .entry(repository_path.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let repository_guard = repository_lock.lock_owned().await;
        let parent = repository_path
            .parent()
            .ok_or_else(|| "invalid review repository path".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        if repository_path.exists() && !repository_path.join(".git").is_dir() {
            return Err(format!(
                "{} exists but is not a git repository",
                repository_path.display()
            ));
        }

        let auth = base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", request.token));
        let run = |args: Vec<String>| {
            let repository_path = repository_path.clone();
            let auth = auth.clone();
            async move {
                let mut command = tokio::process::Command::new("git");
                command
                    .args(args)
                    .current_dir(&repository_path)
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
                    .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {auth}"))
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .kill_on_drop(true);
                harden_authenticated_review_git_command(&mut command);
                let output = command
                    .output()
                    .await
                    .map_err(|error| format!("running git: {error}"))?;
                if output.status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
                } else {
                    Err(sanitize_review_fetch_stderr(
                        String::from_utf8_lossy(&output.stderr).trim(),
                        &auth,
                    ))
                }
            }
        };

        if !repository_path.exists() {
            std::fs::create_dir_all(&repository_path).map_err(|error| error.to_string())?;
            run(vec!["init".into()]).await?;
            run(vec![
                "remote".into(),
                "add".into(),
                "origin".into(),
                format!("https://github.com/{}.git", request.repository),
            ])
            .await?;
        }

        // Retries and duplicate requests for an immutable revision can reuse
        // objects already fetched into the shared review repository.
        let base_present = run(vec![
            "cat-file".into(),
            "-e".into(),
            format!("{}^{{commit}}", request.base_sha),
        ])
        .await
        .is_ok();
        let head_present = run(vec![
            "cat-file".into(),
            "-e".into(),
            format!("{}^{{commit}}", request.head_sha),
        ])
        .await
        .is_ok();
        if !base_present || !head_present {
            let pull_ref = format!("refs/remotes/origin/trouve-pr-{}", request.pull_number);
            run(vec![
                "fetch".into(),
                "--force".into(),
                "--no-tags".into(),
                "origin".into(),
                format!("+{}:refs/remotes/origin/trouve-base", request.base_sha),
                format!("+refs/pull/{}/head:{pull_ref}", request.pull_number),
            ])
            .await?;
            let actual = run(vec!["rev-parse".into(), pull_ref]).await?;
            if actual != request.head_sha {
                return Err(format!(
                    "pull request moved while fetching: expected {}, got {actual}",
                    request.head_sha
                ));
            }
        }
        // Give every job its own temporary refs. Even when an object is
        // already present, pin it under this job's ref so an older overlapping
        // job cannot make it unreachable during its cleanup.
        let mut fetches = Vec::new();
        for (index, sha) in request
            .optional_shas
            .iter()
            .take(REVIEW_HISTORY_REF_LIMIT)
            .enumerate()
        {
            let history_ref = review_history_ref_name(request.pull_number, &request.job_id, index)?;
            let full_history_ref = format!("refs/remotes/origin/{history_ref}");
            let present = run(vec![
                "cat-file".into(),
                "-e".into(),
                format!("{sha}^{{commit}}"),
            ])
            .await
            .is_ok();
            if present {
                if let Err(error) =
                    run(vec!["update-ref".into(), full_history_ref, sha.clone()]).await
                {
                    tracing::warn!(
                        repository = %request.repository,
                        pull_number = request.pull_number,
                        job_id = %request.job_id,
                        %sha,
                        %error,
                        "could not pin an already-present review-history commit; continuing with the full diff if reuse is unavailable"
                    );
                }
            } else {
                fetches.push(ReviewHistoryFetch {
                    sha: sha.clone(),
                    history_ref,
                    refspec: format!("+{sha}:{full_history_ref}"),
                });
            }
        }
        if fetches.is_empty() {
            drop(repository_guard);
        } else {
            for (sha, error) in
                run_optional_review_fetches(&repository_path, &auth, &fetches, repository_guard)
                    .await
            {
                tracing::warn!(
                    repository = %request.repository,
                    pull_number = request.pull_number,
                    job_id = %request.job_id,
                    %sha,
                    %error,
                    "optional review-history fetch failed; continuing with the full diff if reuse is unavailable"
                );
            }
        }
        Ok(repository_path)
    }

    async fn cleanup_review_repository_history(
        &self,
        request: &ReviewRepositoryHistoryCleanup,
    ) -> Result<(), String> {
        let references = (0..REVIEW_HISTORY_REF_LIMIT)
            .map(|index| {
                review_history_ref_name(request.pull_number, &request.job_id, index)
                    .map(|name| format!("refs/remotes/origin/{name}"))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let repository_lock = {
            let mut locks = self.review_repository_locks.lock().unwrap();
            locks
                .entry(request.worktree.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let repository_guard =
            tokio::time::timeout(REVIEW_HISTORY_CLEANUP_TIMEOUT, repository_lock.lock_owned())
                .await
                .map_err(|_| "timed out waiting to clean temporary review refs".to_owned())?;

        let mut command = tokio::process::Command::new("git");
        command
            .args(["update-ref", "--stdin"])
            .current_dir(&request.worktree)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        isolate_review_git_process(&mut command);
        let child = command
            .spawn()
            .map_err(|error| format!("starting temporary review ref cleanup: {error}"))?;
        let mut guard = ReviewGitChildGuard::new(child, repository_guard);
        let stderr = guard
            .child_mut()
            .stderr
            .take()
            .ok_or_else(|| "temporary review ref cleanup did not capture stderr".to_owned())?;
        let mut stdin = guard
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| "temporary review ref cleanup did not capture stdin".to_owned())?;
        let commands = references
            .iter()
            .map(|reference| format!("delete {reference}\n"))
            .collect::<String>();
        use tokio::io::AsyncWriteExt as _;
        stdin
            .write_all(commands.as_bytes())
            .await
            .map_err(|error| format!("writing temporary review ref cleanup: {error}"))?;
        drop(stdin);
        let stderr_task = tokio::spawn(read_bounded_review_fetch_stderr(stderr));
        let status = match tokio::time::timeout(
            REVIEW_HISTORY_CLEANUP_TIMEOUT,
            guard.child_mut().wait(),
        )
        .await
        {
            Ok(Ok(status)) => {
                #[cfg(unix)]
                quiesce_review_git_process_group(guard.pid)
                    .await
                    .map_err(|error| format!("stopping temporary ref cleanup helpers: {error}"))?;
                status
            }
            Ok(Err(error)) => {
                terminate_review_git_process(&mut guard)
                    .await
                    .map_err(|terminate_error| {
                        format!(
                            "waiting for temporary review ref cleanup: {error}; terminating it: {terminate_error}"
                        )
                    })?;
                return Err(format!("waiting for temporary review ref cleanup: {error}"));
            }
            Err(_) => {
                terminate_review_git_process(&mut guard)
                    .await
                    .map_err(|error| {
                        format!("terminating timed-out temporary review ref cleanup: {error}")
                    })?;
                return Err("temporary review ref cleanup timed out".to_owned());
            }
        };
        let stderr = stderr_task.await.unwrap_or_default();
        guard.disarm();
        if !status.success() {
            return Err(format!("deleting temporary review refs: {stderr}"));
        }
        Ok(())
    }

    async fn review_repository_diff(
        &self,
        request: &ReviewRepositoryDiff,
    ) -> Result<Vec<ReviewDiffFile>, String> {
        let worktree = request.worktree.clone();
        let base_sha = request.base_sha.clone();
        let head_sha = request.head_sha.clone();
        tokio::task::spawn_blocking(move || {
            let paths = crate::git::diff_files_between(&worktree, &base_sha, &head_sha)
                .map_err(|error| error.to_string())?;
            paths
                .into_iter()
                .map(|path| {
                    let diff =
                        crate::git::diff_path_between(&worktree, &base_sha, &head_sha, &path)
                            .map_err(|error| error.to_string())?;
                    Ok(ReviewDiffFile { path, diff })
                })
                .collect()
        })
        .await
        .map_err(|error| format!("review diff task failed: {error}"))?
    }

    async fn review_repository_merge_base(
        &self,
        request: &ReviewRepositoryMergeBase,
    ) -> Result<String, String> {
        let worktree = request.worktree.clone();
        let base_sha = request.base_sha.clone();
        let head_sha = request.head_sha.clone();
        tokio::task::spawn_blocking(move || crate::git::merge_base(&worktree, &base_sha, &head_sha))
            .await
            .map_err(|error| format!("review merge-base task failed: {error}"))?
            .map_err(|error| error.to_string())
    }

    async fn evict_worktree(&self, worktree: &Path) {
        self.jobs.kill_worktree(worktree).await;
        self.mcp.evict_worktree(worktree).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_resolution_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let ctx = ToolCtx {
            worktree: dir.path().to_path_buf(),
            ..Default::default()
        };
        assert!(ctx.resolve("src/main.rs").is_ok());
        assert!(ctx.resolve("./a/b").is_ok());
        assert!(ctx.resolve("/etc/passwd").is_err());
        assert!(ctx.resolve("../outside").is_err());
        assert!(ctx.resolve("a/../../outside").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn path_resolution_rejects_symlink_escapes() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: dir.path().to_path_buf(),
            ..Default::default()
        };

        // A symlink whose target file exists outside the worktree.
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("leak")).unwrap();
        assert!(ctx.resolve("leak").is_err());

        // A symlinked directory: every path component is `Normal`, but the
        // resolved location is outside the worktree.
        std::os::unix::fs::symlink(outside.path(), dir.path().join("dir")).unwrap();
        assert!(ctx.resolve("dir/secret").is_err());
        assert!(ctx.resolve("dir/new-file").is_err());

        // A dangling symlink must not be written through either.
        std::os::unix::fs::symlink(outside.path().join("missing"), dir.path().join("dangle"))
            .unwrap();
        assert!(ctx.resolve("dangle").is_err());

        // Symlinks that stay inside the worktree are fine.
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/f"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
        assert!(ctx.resolve("alias/f").is_ok());
    }

    #[tokio::test]
    async fn executor_reports_unknown_tools() {
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            ..Default::default()
        };
        let res = exec.execute(&ctx, "nope", &serde_json::json!({})).await;
        assert_eq!(res.status, ToolStatus::Error);
    }

    #[test]
    fn review_fetch_cleanup_removes_only_known_lock_files() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        let refs = git_dir.join("refs/remotes/origin");
        std::fs::create_dir_all(&refs).unwrap();
        for lock in [
            git_dir.join("FETCH_HEAD.lock"),
            git_dir.join("packed-refs.lock"),
            git_dir.join("shallow.lock"),
            refs.join("trouve-history-42-rv_test-0.lock"),
        ] {
            std::fs::write(lock, "locked").unwrap();
        }
        let unrelated = git_dir.join("index.lock");
        std::fs::write(&unrelated, "keep").unwrap();

        cleanup_review_fetch_locks(dir.path(), &["trouve-history-42-rv_test-0".into()]);

        assert!(git_dir.join("FETCH_HEAD.lock").exists());
        assert!(git_dir.join("packed-refs.lock").exists());
        assert!(git_dir.join("shallow.lock").exists());
        assert!(!refs.join("trouve-history-42-rv_test-0.lock").exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn review_fetch_command_disables_credential_tracing_and_global_fetch_locks() {
        let command = optional_review_fetch_command(
            Path::new("."),
            "secret-auth",
            "+abc:refs/remotes/origin/trouve-history-42-rv_test-0",
        );
        let command = command.as_std();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "--no-write-fetch-head"));
        assert!(args.iter().any(|arg| arg == "--no-auto-maintenance"));
        let env = command.get_envs().collect::<HashMap<_, _>>();
        for variable in REVIEW_GIT_TRACE_ENV_VARS {
            assert_eq!(env.get(std::ffi::OsStr::new(variable)), Some(&None));
        }
    }

    #[test]
    fn review_fetch_stderr_redacts_authorization_traces_and_auth_value() {
        let stderr = "trace: Authorization: basic secret-auth\nfatal: secret-auth rejected";
        let sanitized = sanitize_review_fetch_stderr(stderr, "secret-auth");
        assert!(!sanitized.contains("secret-auth"));
        assert!(!sanitized.to_ascii_lowercase().contains("authorization:"));
        assert!(sanitized.contains("[redacted git authorization trace]"));
    }

    #[tokio::test]
    async fn optional_review_fetches_preserve_partial_success() {
        let origin = tempfile::tempdir().unwrap();
        let repository = tempfile::tempdir().unwrap();
        let git = |directory: &Path, args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(directory)
                .output()
                .unwrap()
        };
        assert!(git(origin.path(), &["init"]).status.success());
        assert!(
            git(origin.path(), &["config", "user.name", "Test"])
                .status
                .success()
        );
        assert!(
            git(origin.path(), &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        std::fs::write(origin.path().join("file"), "content").unwrap();
        assert!(git(origin.path(), &["add", "file"]).status.success());
        assert!(
            git(origin.path(), &["commit", "-m", "seed"])
                .status
                .success()
        );
        let valid_sha = String::from_utf8(git(origin.path(), &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert!(git(repository.path(), &["init"]).status.success());
        assert!(
            git(
                repository.path(),
                &["remote", "add", "origin", origin.path().to_str().unwrap()]
            )
            .status
            .success()
        );
        let missing_sha = "ffffffffffffffffffffffffffffffffffffffff".to_owned();
        let missing_ref = "trouve-history-42-rv_test-0".to_owned();
        let valid_ref = "trouve-history-42-rv_test-1".to_owned();
        let fetches = vec![
            ReviewHistoryFetch {
                sha: missing_sha.clone(),
                history_ref: missing_ref.clone(),
                refspec: format!("+{missing_sha}:refs/remotes/origin/{missing_ref}"),
            },
            ReviewHistoryFetch {
                sha: valid_sha.clone(),
                history_ref: valid_ref.clone(),
                refspec: format!("+{valid_sha}:refs/remotes/origin/{valid_ref}"),
            },
        ];
        let repository_lock = Arc::new(tokio::sync::Mutex::new(()));
        let failures = run_optional_review_fetches(
            repository.path(),
            "test-auth",
            &fetches,
            repository_lock.lock_owned().await,
        )
        .await;

        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, missing_sha);
        assert_eq!(
            String::from_utf8(
                git(
                    repository.path(),
                    &["rev-parse", &format!("refs/remotes/origin/{valid_ref}")]
                )
                .stdout
            )
            .unwrap()
            .trim(),
            valid_sha
        );
    }

    #[tokio::test]
    async fn optional_present_history_ref_failure_does_not_abort_sync() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("acme/widgets");
        std::fs::create_dir_all(&repository).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repository)
                .output()
                .unwrap()
        };
        assert!(git(&["init"]).status.success());
        assert!(git(&["config", "user.name", "Test"]).status.success());
        assert!(
            git(&["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        std::fs::write(repository.join("file"), "content").unwrap();
        assert!(git(&["add", "file"]).status.success());
        assert!(git(&["commit", "-m", "seed"]).status.success());
        let head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        let ref_dir = repository.join(".git/refs/remotes/origin");
        std::fs::create_dir_all(&ref_dir).unwrap();
        std::fs::write(ref_dir.join("trouve-history-42-rv_test-0.lock"), "locked").unwrap();

        let synced = LocalToolExecutor::default()
            .sync_review_repository(&ReviewRepositorySync {
                root: root.path().to_path_buf(),
                repository: "acme/widgets".into(),
                job_id: "rv_test".into(),
                pull_number: 42,
                base_sha: head.clone(),
                head_sha: head.clone(),
                optional_shas: vec![head],
                token: "test-token".into(),
            })
            .await
            .unwrap();

        assert_eq!(synced, repository);
    }

    #[tokio::test]
    async fn review_history_cleanup_deletes_only_the_jobs_bounded_refs() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
        };
        assert!(git(&["init"]).status.success());
        assert!(git(&["config", "user.name", "Test"]).status.success());
        assert!(
            git(&["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        std::fs::write(dir.path().join("file"), "content").unwrap();
        assert!(git(&["add", "file"]).status.success());
        assert!(git(&["commit", "-m", "seed"]).status.success());
        let head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        for job_id in ["rv_test", "rv_replacement"] {
            for index in 0..3 {
                let reference = format!("refs/remotes/origin/trouve-history-42-{job_id}-{index}");
                assert!(git(&["update-ref", &reference, &head]).status.success());
            }
        }

        LocalToolExecutor::default()
            .cleanup_review_repository_history(&ReviewRepositoryHistoryCleanup {
                worktree: dir.path().to_path_buf(),
                job_id: "rv_test".into(),
                pull_number: 42,
            })
            .await
            .unwrap();

        for index in 0..3 {
            let reference = format!("refs/remotes/origin/trouve-history-42-rv_test-{index}");
            assert!(!git(&["show-ref", "--verify", &reference]).status.success());
            let replacement =
                format!("refs/remotes/origin/trouve-history-42-rv_replacement-{index}");
            assert!(
                git(&["show-ref", "--verify", &replacement])
                    .status
                    .success()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_review_fetch_process_is_terminated_and_reaped() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("60").kill_on_drop(true);
        isolate_review_git_process(&mut command);
        let child = command.spawn().unwrap();
        let repository_lock = Arc::new(tokio::sync::Mutex::new(()));
        let mut guard = ReviewGitChildGuard::new(child, repository_lock.clone().lock_owned().await);
        let pid = guard.pid;

        terminate_review_git_process(&mut guard).await.unwrap();
        guard.disarm();

        assert!(!review_git_process_group_exists(pid));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_direct_child_does_not_leave_process_group_helpers() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 60 &").kill_on_drop(true);
        isolate_review_git_process(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        child.wait().await.unwrap();
        assert!(review_git_process_group_exists(pid));

        quiesce_review_git_process_group(pid).await.unwrap();

        assert!(!review_git_process_group_exists(pid));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stderr_collection_terminates_descendants_that_hold_the_pipe() {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 >&2 &")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        isolate_review_git_process(&mut command);
        let child = command.spawn().unwrap();
        let repository_lock = Arc::new(tokio::sync::Mutex::new(()));
        let mut guard = ReviewGitChildGuard::new(child, repository_lock.lock_owned().await);
        let pid = guard.pid;
        let stderr = guard.child_mut().stderr.take().unwrap();
        let mut stderr_task = tokio::spawn(read_bounded_review_fetch_stderr(stderr));
        assert!(guard.child_mut().wait().await.unwrap().success());

        finish_review_fetch_stderr(&mut guard, &mut stderr_task, Duration::from_millis(50))
            .await
            .unwrap();
        guard.disarm();

        assert!(!review_git_process_group_exists(pid));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_guard_reaps_descendants_before_releasing_repository() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 60 & wait").kill_on_drop(true);
        isolate_review_git_process(&mut command);
        let child = command.spawn().unwrap();
        let pid = child.id();
        let repository_lock = Arc::new(tokio::sync::Mutex::new(()));
        let guard = ReviewGitChildGuard::new(child, repository_lock.clone().lock_owned().await);

        drop(guard);
        let reacquired = tokio::time::timeout(
            REVIEW_FETCH_TERMINATION_GRACE * 2,
            repository_lock.clone().lock_owned(),
        )
        .await
        .unwrap();

        assert!(!review_git_process_group_exists(pid));
        drop(reacquired);
    }
}
