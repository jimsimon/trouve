//! Tools and the `ToolExecutor` chokepoint (invariant 3).
//!
//! Trouve's native agent loop never performs side effects itself: it gates
//! each call through the permission layer and hands execution to a
//! `ToolExecutor`. Supplemental capabilities mounted into subscription CLIs
//! return through this same boundary. Certified vendor-native core tools are
//! the deliberate exception recorded by ADR 0019; their adapters normalize
//! lifecycle and approval events without re-executing the operation here.
//! Local mode uses [`LocalToolExecutor`]; cloud isolation later swaps in a
//! container-backed implementation without touching the native loop.

mod diff;
mod fs;
mod glob;
mod grep;
mod patch;
mod search;
mod shell;
mod skill;
mod todo;
mod web;

pub use search::{VENDOR_SEARCH_GUIDANCE, gc_index_store_in_background, warm_index_in_background};

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use trouve_protocol::ToolStatus;
use trouve_providers::ToolSpec;

/// Execution context: everything a tool may touch. All paths resolve inside
/// the session worktree.
#[derive(Debug, Clone)]
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
    /// Snapshot of the global built-in skill setting for this turn. User and
    /// workspace skills are always available.
    pub builtin_skills_enabled: bool,
}

impl Default for ToolCtx {
    fn default() -> Self {
        Self {
            worktree: PathBuf::new(),
            thread_id: String::new(),
            todos: Arc::new(Mutex::new(Vec::new())),
            config_dir: None,
            workspace_root: None,
            builtin_skills_enabled: true,
        }
    }
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
    /// Read the complete base-to-head diff as per-file segments for the
    /// headless review orchestrator. This remains behind the executor so git
    /// access follows the same audited chokepoint as model-visible tools.
    async fn review_repository_diff(
        &self,
        _request: &ReviewRepositoryDiff,
    ) -> Result<Vec<ReviewDiffFile>, String> {
        Err("review repository diff is unavailable in this executor".into())
    }
    /// Stage user-provided non-image attachments inside the session
    /// worktree so provider-native file tools can read them. This controlled
    /// filesystem mutation stays behind the same executor boundary as every
    /// other Trouve-owned worktree write.
    async fn stage_attachments(
        &self,
        _ctx: &ToolCtx,
        _files: &[AttachmentStage],
    ) -> Result<Vec<AttachmentStage>, String> {
        Err("attachment staging is unavailable in this executor".into())
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

pub struct ReviewRepositoryDiff {
    pub worktree: PathBuf,
    pub base_sha: String,
}

#[derive(Debug, Clone)]
pub struct AttachmentStage {
    pub attachment: trouve_protocol::Attachment,
    pub source: PathBuf,
    pub relative_path: PathBuf,
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
    /// Shared bare repositories are reused across review jobs. Serialize the
    /// complete fetch/ref transaction per repository while allowing unrelated
    /// repositories to sync concurrently.
    review_repository_locks: Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>,
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
                Arc::new(skill::LoadSkill),
            ],
            mcp: crate::mcp::McpManager::with_logs(logs),
            jobs,
            review_repository_locks: Mutex::new(HashMap::new()),
        }
    }

    fn find(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    fn review_repository_lock(&self, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.review_repository_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
        lock
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

    async fn stage_attachments(
        &self,
        ctx: &ToolCtx,
        files: &[AttachmentStage],
    ) -> Result<Vec<AttachmentStage>, String> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let relative_dir = Path::new(".trouve").join("attachments");
        let attachment_dir = ctx
            .resolve(&relative_dir.to_string_lossy())
            .map_err(|error| error.to_string())?;
        tokio::fs::create_dir_all(&attachment_dir)
            .await
            .map_err(|error| {
                format!(
                    "cannot create attachment directory {}: {error}",
                    attachment_dir.display()
                )
            })?;
        let ignore_path = ctx
            .resolve(".trouve/.gitignore")
            .map_err(|error| error.to_string())?;
        if let Err(error) = tokio::fs::write(&ignore_path, "*\n").await {
            tracing::warn!(
                "cannot gitignore staged attachments at {}: {error}",
                ignore_path.display()
            );
        }

        let mut staged = Vec::new();
        for file in files {
            if !file.relative_path.starts_with(&relative_dir) {
                tracing::warn!(
                    "refusing attachment destination outside {}: {}",
                    relative_dir.display(),
                    file.relative_path.display()
                );
                continue;
            }
            let destination = match ctx.resolve(&file.relative_path.to_string_lossy()) {
                Ok(destination) => destination,
                Err(error) => {
                    tracing::warn!(
                        "cannot resolve attachment destination {}: {error}",
                        file.relative_path.display()
                    );
                    continue;
                }
            };
            match tokio::fs::copy(&file.source, destination).await {
                Ok(_) => staged.push(file.clone()),
                Err(error) => {
                    tracing::warn!("cannot stage attachment {}: {error}", file.attachment.name)
                }
            }
        }
        Ok(staged)
    }

    async fn sync_review_repository(
        &self,
        request: &ReviewRepositorySync,
    ) -> Result<PathBuf, String> {
        use base64::Engine as _;

        let repository_path = request.root.join(&request.repository);
        let repository_lock = self.review_repository_lock(&repository_path);
        let _repository_guard = repository_lock.lock().await;
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

        // Retries and duplicate requests for an immutable revision can reuse
        // objects already fetched into the shared review repository. Anchor
        // both objects before returning so later git maintenance cannot prune
        // commits that arrived only through a now-expired FETCH_HEAD.
        let base_ref = "refs/remotes/origin/trouve-base";
        let pull_ref = format!("refs/remotes/origin/trouve-pr-{}", request.pull_number);
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
            run(vec![
                "update-ref".into(),
                base_ref.into(),
                request.base_sha.clone(),
            ])
            .await?;
            run(vec![
                "update-ref".into(),
                pull_ref.clone(),
                request.head_sha.clone(),
            ])
            .await?;
            return Ok(repository_path);
        }

        run(vec![
            "fetch".into(),
            "--force".into(),
            "--no-tags".into(),
            "origin".into(),
            format!("+{}:{base_ref}", request.base_sha),
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
        tokio::task::spawn_blocking(move || {
            let paths = crate::git::session_diff_files(&worktree, &base_sha)
                .map_err(|error| error.to_string())?;
            paths
                .into_iter()
                .map(|path| {
                    let diff = crate::git::session_diff_path(&worktree, &base_sha, &path)
                        .map_err(|error| error.to_string())?;
                    Ok(ReviewDiffFile { path, diff })
                })
                .collect()
        })
        .await
        .map_err(|error| format!("review diff task failed: {error}"))?
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
    fn review_repository_locks_are_shared_only_within_one_repository() {
        let executor = LocalToolExecutor::default();
        let first = executor.review_repository_lock(Path::new("/reviews/owner/one"));
        let same = executor.review_repository_lock(Path::new("/reviews/owner/one"));
        let other = executor.review_repository_lock(Path::new("/reviews/owner/two"));

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[tokio::test]
    async fn review_repository_reuse_anchors_existing_commits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("reviews");
        let repository = root.join("owner/repo");
        let git_config = temp.path().join("empty-gitconfig");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::write(&git_config, "").unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repository)
                .env("GIT_CONFIG_GLOBAL", &git_config)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_CONFIG_COUNT")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init"]);
        git(&[
            "-c",
            "user.name=Trouve Test",
            "-c",
            "user.email=trouve@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ]);
        let base_sha = git(&["rev-parse", "HEAD"]);
        git(&[
            "-c",
            "user.name=Trouve Test",
            "-c",
            "user.email=trouve@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "head",
        ]);
        let head_sha = git(&["rev-parse", "HEAD"]);

        let synced = LocalToolExecutor::default()
            .sync_review_repository(&ReviewRepositorySync {
                root,
                repository: "owner/repo".into(),
                pull_number: 42,
                base_sha: base_sha.clone(),
                head_sha: head_sha.clone(),
                token: "unused-on-reuse".into(),
            })
            .await
            .unwrap();

        assert_eq!(synced, repository);
        assert_eq!(
            git(&["rev-parse", "refs/remotes/origin/trouve-base"]),
            base_sha
        );
        assert_eq!(
            git(&["rev-parse", "refs/remotes/origin/trouve-pr-42"]),
            head_sha
        );
    }
}
