//! Tools and the `ToolExecutor` chokepoint (invariant 3).
//!
//! The agent loop never performs side effects itself: it gates each call
//! through the permission layer and hands execution to a `ToolExecutor`.
//! Local mode uses [`LocalToolExecutor`]; cloud isolation later swaps in a
//! container-backed implementation without touching the loop.

mod diff;
mod edit_strategy;
mod fs;
mod glob;
mod grep;
mod hashline;
mod patch;
mod search;
mod shell;
mod todo;
mod web;

pub use search::{
    VENDOR_SEARCH_GUIDANCE, VENDOR_TOOL_BRIDGE_GUIDANCE, gc_index_store_in_background,
    warm_index_in_background,
};

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use futures::StreamExt as _;
use serde_json::Value;
use trouve_protocol::ToolStatus;
use trouve_providers::ToolSpec;

pub use edit_strategy::EditStrategy;
pub use edit_strategy::for_model as edit_strategy_for_model;

/// Execution context: everything a tool may touch. All paths resolve inside
/// the session worktree.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    /// Cancellation for the turn that owns this call. Long-running tools
    /// must finish process/protocol cleanup before returning from it.
    pub cancel: tokio_util::sync::CancellationToken,
    pub worktree: PathBuf,
    /// Canonicalized once when the engine builds the turn context. Isolated
    /// tool tests may omit it and pay the one-off fallback canonicalization.
    pub canonical_worktree: Option<PathBuf>,
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
    /// Model-specific editing policy used for both tool advertisement and
    /// execution enforcement.
    pub edit_strategy: EditStrategy,
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
        let root = match &self.canonical_worktree {
            Some(root) => root.clone(),
            None => self
                .worktree
                .canonicalize()
                .with_context(|| format!("worktree unavailable: {}", self.worktree.display()))?,
        };
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
    /// Execute one call. Long-running implementations must observe
    /// `ctx.cancel` and return only after any owned process or protocol
    /// request is stopped/reaped; the engine retains the session execution
    /// lane until this future acknowledges that cleanup.
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
    /// Release any per-worktree resources (e.g. spawned MCP server
    /// processes) when a session/worktree is going away. Default no-op.
    async fn evict_worktree(&self, _worktree: &Path) {}
}

/// Inputs for one authenticated GitHub App fetch. Tokens are passed through
/// process environment, never embedded in a remote URL or persisted config.
pub struct ReviewRepositorySync {
    pub root: PathBuf,
    pub repository: String,
    pub pull_number: u64,
    pub base_sha: String,
    pub head_sha: String,
    pub token: String,
    pub cancel: tokio_util::sync::CancellationToken,
}

pub struct ReviewRepositoryDiff {
    pub worktree: PathBuf,
    pub base_sha: String,
}

const REVIEW_GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_REVIEW_GIT_MESSAGE_BYTES: usize = 64 * 1024;

fn bounded_review_git_message(bytes: &[u8]) -> String {
    let end = bytes.len().min(MAX_REVIEW_GIT_MESSAGE_BYTES);
    let mut message = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    if end < bytes.len() {
        message.push_str("\n… output truncated");
    }
    message
}

async fn run_review_git(
    repository_path: &Path,
    auth: &str,
    args: Vec<String>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(&args)
        .current_dir(repository_path)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {auth}"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("review repository sync cancelled".into()),
        result = tokio::time::timeout(REVIEW_GIT_TIMEOUT, command.output()) => {
            match result {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => return Err(format!("running git: {error}")),
                Err(_) => return Err(format!(
                    "git {} timed out after {}s",
                    args.join(" "),
                    REVIEW_GIT_TIMEOUT.as_secs(),
                )),
            }
        }
    };
    if output.status.success() {
        if output.stdout.len() > MAX_REVIEW_GIT_MESSAGE_BYTES {
            return Err(format!(
                "git {} returned more than {MAX_REVIEW_GIT_MESSAGE_BYTES} bytes",
                args.join(" ")
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(bounded_review_git_message(&output.stderr))
    }
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
    built_in_specs: Vec<ToolSpec>,
    mcp: crate::mcp::McpManager,
    jobs: Arc<shell::JobRegistry>,
    hashline_failures: Mutex<HashMap<String, u8>>,
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
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(fs::ReadFile),
            Arc::new(fs::WriteFile),
            Arc::new(fs::EditFile),
            Arc::new(fs::DeleteFile),
            Arc::new(hashline::HashlineEdit),
            Arc::new(patch::ApplyPatch),
            Arc::new(patch::ApplyPatchFallback),
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
        ];
        let built_in_specs = tools
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters(),
            })
            .collect();
        Self {
            tools,
            built_in_specs,
            mcp: crate::mcp::McpManager::with_logs(logs),
            jobs,
            hashline_failures: Mutex::new(HashMap::new()),
        }
    }

    fn find(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    fn failure_key(ctx: &ToolCtx) -> String {
        if ctx.thread_id.is_empty() {
            format!("worktree:{}", ctx.worktree.display())
        } else {
            ctx.thread_id.clone()
        }
    }

    fn hashline_failure_count(&self, ctx: &ToolCtx) -> u8 {
        self.hashline_failures
            .lock()
            .unwrap()
            .get(&Self::failure_key(ctx))
            .copied()
            .unwrap_or(0)
    }

    fn record_hashline_result(&self, ctx: &ToolCtx, result: &ToolResult) -> u8 {
        let key = Self::failure_key(ctx);
        let mut failures = self.hashline_failures.lock().unwrap();
        if result.status == ToolStatus::Ok {
            failures.remove(&key);
            return 0;
        }
        let cancelled = result
            .result
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("cancelled"));
        if cancelled {
            return failures.get(&key).copied().unwrap_or(0);
        }
        let count = failures.entry(key).or_default();
        *count = count.saturating_add(1);
        *count
    }

    fn edit_policy_denial(&self, ctx: &ToolCtx, name: &str, args: &Value) -> Option<ToolResult> {
        if name == "apply_patch_fallback" {
            if ctx.edit_strategy != EditStrategy::EnforceHashline {
                return Some(ToolResult::error(
                    "apply_patch_fallback is not available for this model's edit strategy",
                ));
            }
            let failures = self.hashline_failure_count(ctx);
            if failures < edit_strategy::HASHLINE_FALLBACK_FAILURES {
                return Some(ToolResult::error(format!(
                    "apply_patch_fallback is locked until {} hashline_edit attempts fail (currently {failures})",
                    edit_strategy::HASHLINE_FALLBACK_FAILURES
                )));
            }
        }
        if ctx.edit_strategy != EditStrategy::EnforceHashline {
            return None;
        }
        if matches!(name, "edit_file" | "apply_patch") {
            return Some(ToolResult::error(format!(
                "{name} is unavailable for this model; read the file with format=\"hashline\" and use hashline_edit"
            )));
        }
        if name == "write_file"
            && let Some(path) = args.get("path").and_then(Value::as_str)
            && ctx.resolve(path).is_ok_and(|path| path.exists())
        {
            return Some(ToolResult::error(
                "write_file may only create new files under the enforced hashline strategy; use hashline_edit for an existing file",
            ));
        }
        None
    }
}

#[async_trait::async_trait]
impl ToolExecutor for LocalToolExecutor {
    async fn specs(&self, ctx: &ToolCtx) -> Vec<ToolSpec> {
        let mut specs = self
            .built_in_specs
            .iter()
            .cloned()
            .filter_map(|spec| edit_strategy::advertise(ctx.edit_strategy, spec))
            .collect::<Vec<_>>();
        specs.extend(
            self.mcp
                .specs(
                    ctx.config_dir.as_deref(),
                    ctx.workspace_root.as_deref(),
                    &ctx.worktree,
                    &ctx.cancel,
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
                    &ctx.cancel,
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
        if let Some(denial) = self.edit_policy_denial(ctx, name, args) {
            return denial;
        }
        match self.find(name) {
            Some(tool) => {
                let started = std::time::Instant::now();
                let result = tool.run(ctx, args).await;
                let failure_count = if name == "hashline_edit"
                    && ctx.edit_strategy == EditStrategy::EnforceHashline
                {
                    self.record_hashline_result(ctx, &result)
                } else if name == "apply_patch_fallback" && result.status == ToolStatus::Ok {
                    self.hashline_failures
                        .lock()
                        .unwrap()
                        .remove(&Self::failure_key(ctx));
                    0
                } else {
                    self.hashline_failure_count(ctx)
                };
                if matches!(
                    name,
                    "edit_file"
                        | "hashline_edit"
                        | "apply_patch"
                        | "apply_patch_fallback"
                        | "write_file"
                        | "delete_file"
                ) {
                    tracing::info!(
                        target: "trouve::edit_strategy",
                        thread_id = %ctx.thread_id,
                        strategy = ?ctx.edit_strategy,
                        tool = name,
                        status = ?result.status,
                        execution_ms = started.elapsed().as_millis(),
                        hashline_failures = failure_count,
                        "model edit strategy tool result"
                    );
                }
                result
            }
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
            let cancel = request.cancel.clone();
            async move { run_review_git(&repository_path, &auth, args, &cancel).await }
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
        if base_present && head_present {
            return Ok(repository_path);
        }

        let pull_ref = format!("refs/remotes/origin/trouve-pr-{}", request.pull_number);
        run(vec![
            "fetch".into(),
            "--quiet".into(),
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
        Ok(repository_path)
    }

    async fn review_repository_diff(
        &self,
        request: &ReviewRepositoryDiff,
    ) -> Result<Vec<ReviewDiffFile>, String> {
        let worktree = request.worktree.clone();
        let base_sha = request.base_sha.clone();
        let paths = tokio::task::spawn_blocking({
            let worktree = worktree.clone();
            let base_sha = base_sha.clone();
            move || crate::git::session_diff_files(&worktree, &base_sha)
        })
        .await
        .map_err(|error| format!("review diff manifest task failed: {error}"))?
        .map_err(|error| error.to_string())?;

        futures::stream::iter(paths.into_iter().map(|path| {
            let worktree = worktree.clone();
            let base_sha = base_sha.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    let diff = crate::git::session_diff_path(&worktree, &base_sha, &path)
                        .map_err(|error| error.to_string())?;
                    Ok(ReviewDiffFile { path, diff })
                })
                .await
                .map_err(|error| format!("review file diff task failed: {error}"))?
            }
        }))
        .buffered(4)
        .collect::<Vec<Result<ReviewDiffFile, String>>>()
        .await
        .into_iter()
        .collect()
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
    fn executor_classifies_hashline_edits_as_mutations() {
        let exec = LocalToolExecutor::default();
        assert_eq!(exec.tool_mutates("hashline_edit"), Some(true));
    }

    fn spec_names(specs: Vec<ToolSpec>) -> Vec<String> {
        specs.into_iter().map(|spec| spec.name).collect()
    }

    #[tokio::test]
    async fn enforced_hashline_catalog_retains_create_delete_and_controlled_fallback() {
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            edit_strategy: EditStrategy::EnforceHashline,
            ..Default::default()
        };
        let names = spec_names(exec.specs(&ctx).await);
        assert!(names.contains(&"hashline_edit".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"delete_file".to_string()));
        assert!(names.contains(&"apply_patch_fallback".to_string()));
        assert!(!names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"apply_patch".to_string()));
    }

    #[tokio::test]
    async fn ordinary_catalogs_do_not_expose_the_controlled_fallback_alias() {
        let exec = LocalToolExecutor::default();
        for edit_strategy in [
            EditStrategy::Auto,
            EditStrategy::PreferApplyPatch,
            EditStrategy::PreferHashline,
        ] {
            let ctx = ToolCtx {
                worktree: std::env::temp_dir(),
                edit_strategy,
                ..Default::default()
            };
            let names = spec_names(exec.specs(&ctx).await);
            assert!(names.contains(&"apply_patch".to_string()));
            assert!(!names.contains(&"apply_patch_fallback".to_string()));
        }
    }

    #[tokio::test]
    async fn enforced_hashline_fallback_unlocks_after_repeated_failures() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "old\n").unwrap();
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            thread_id: "thread-fallback".into(),
            edit_strategy: EditStrategy::EnforceHashline,
            ..Default::default()
        };
        let fallback = "*** Begin Patch\n*** Update File: f.txt\n-old\n+new\n*** End Patch";
        let locked = exec
            .execute(
                &ctx,
                "apply_patch_fallback",
                &serde_json::json!({"input": fallback}),
            )
            .await;
        assert_eq!(locked.status, ToolStatus::Error);

        for _ in 0..edit_strategy::HASHLINE_FALLBACK_FAILURES {
            let failed = exec
                .execute(
                    &ctx,
                    "hashline_edit",
                    &serde_json::json!({"input": "not hashline"}),
                )
                .await;
            assert_eq!(failed.status, ToolStatus::Error);
        }
        let applied = exec
            .execute(
                &ctx,
                "apply_patch_fallback",
                &serde_json::json!({"input": fallback}),
            )
            .await;
        assert_eq!(applied.status, ToolStatus::Ok, "{:?}", applied.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "new\n"
        );
    }

    #[tokio::test]
    async fn enforced_hashline_write_file_cannot_overwrite_existing_content() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "keep").unwrap();
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            edit_strategy: EditStrategy::EnforceHashline,
            ..Default::default()
        };
        let denied = exec
            .execute(
                &ctx,
                "write_file",
                &serde_json::json!({"path": "existing.txt", "content": "replace"}),
            )
            .await;
        assert_eq!(denied.status, ToolStatus::Error);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("existing.txt")).unwrap(),
            "keep"
        );
    }
}
