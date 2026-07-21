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

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use trouve_protocol::ToolStatus;
use trouve_providers::ToolSpec;

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
    /// Prepare the trusted local mirror used by the headless review service.
    /// This is intentionally part of the executor rather than review runtime
    /// code so git/network/filesystem mutations retain one chokepoint.
    async fn sync_review_repository(
        &self,
        _request: &ReviewRepositorySync,
    ) -> Result<PathBuf, String> {
        Err("review repository sync is unavailable in this executor".into())
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
}

/// Runs tools in-process against the local filesystem/shell, plus any MCP
/// servers configured for the workspace.
pub struct LocalToolExecutor {
    tools: Vec<Arc<dyn Tool>>,
    mcp: crate::mcp::McpManager,
    jobs: Arc<shell::JobRegistry>,
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
            async move {
                let output = tokio::process::Command::new("git")
                    .args(args)
                    .current_dir(&repository_path)
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
                    .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: basic {auth}"))
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .output()
                    .await
                    .map_err(|error| format!("running git: {error}"))?;
                if output.status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
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
        Ok(repository_path)
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
}
