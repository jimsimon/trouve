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
use serde_json::Value;
use trouve_protocol::ToolStatus;
use trouve_providers::ToolSpec;

pub use edit_strategy::EditStrategy;
pub use edit_strategy::for_model as edit_strategy_for_model;

/// One session mutation lane that a tool may transfer to a background task.
///
/// The engine installs this only for a background `shell` call. The shell
/// waiter takes ownership after the process starts and holds the write guard
/// until that process exits or is killed and reaped. If the executor rejects
/// the call before taking it, the guard is released when the call context is
/// dropped.
pub struct BackgroundMutationLease {
    guard: Mutex<Option<tokio::sync::OwnedRwLockWriteGuard<()>>>,
}

impl std::fmt::Debug for BackgroundMutationLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundMutationLease")
            .field("available", &self.guard.lock().unwrap().is_some())
            .finish()
    }
}

impl BackgroundMutationLease {
    pub(crate) fn new(guard: tokio::sync::OwnedRwLockWriteGuard<()>) -> Self {
        Self {
            guard: Mutex::new(Some(guard)),
        }
    }

    pub(crate) fn take(&self) -> Option<tokio::sync::OwnedRwLockWriteGuard<()>> {
        self.guard.lock().unwrap().take()
    }
}

/// Execution context: everything a tool may touch. Mutation paths resolve
/// inside the session worktree; explicitly registered host resources are
/// additionally available to read-only filesystem tools.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    /// Cancellation for the turn that owns this call. Long-running tools
    /// must finish process/protocol cleanup before returning from it.
    pub cancel: tokio_util::sync::CancellationToken,
    pub worktree: PathBuf,
    /// Canonicalized once when the engine builds the turn context. Isolated
    /// tool tests may omit it and pay the one-off fallback canonicalization.
    pub canonical_worktree: Option<PathBuf>,
    /// Canonical files/directories exposed as read-only capabilities by the
    /// host. They never participate in mutation path resolution.
    pub read_only_roots: Arc<[PathBuf]>,
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
    /// Engine-owned session write lease available only to a background shell
    /// launch. Kept public for `ToolCtx` construction compatibility; custom
    /// executors must leave it untouched.
    #[doc(hidden)]
    pub background_mutation_lease: Option<Arc<BackgroundMutationLease>>,
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

    /// Resolve a path for a read-only filesystem operation.
    ///
    /// Relative paths retain ordinary worktree semantics. Absolute paths are
    /// accepted only when the existing canonical target is contained by a
    /// canonical root the host registered for this turn. Resolving the full
    /// target prevents symlinks inside an allowed package from escaping to a
    /// credential or unrelated checkout elsewhere on the host.
    pub fn resolve_read(&self, path: &str) -> Result<PathBuf> {
        let requested = Path::new(path);
        if !requested.is_absolute() {
            return self.resolve(path);
        }
        if requested
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            bail!("read path contains parent traversal: {path}");
        }
        let canonical = requested
            .canonicalize()
            .with_context(|| format!("cannot resolve read-only path {path}"))?;
        let supported = std::fs::metadata(&canonical)
            .is_ok_and(|metadata| metadata.is_file() || metadata.is_dir());
        if !supported {
            bail!("read-only path is not a regular file or directory: {path}");
        }
        if !self
            .read_only_roots
            .iter()
            .any(|root| canonical == *root || canonical.starts_with(root))
        {
            bail!("absolute path is not under a registered read-only root: {path}");
        }
        Ok(canonical)
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
    /// Native specs without consulting or launching external MCP servers.
    ///
    /// This deliberately fails closed. A custom executor that only implements
    /// [`Self::specs`] may perform external discovery while building that
    /// catalog, so callers that disabled bridge tools must not reach it through
    /// this default. Executors with a trusted static catalog opt in by
    /// overriding this method.
    async fn native_specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
        Vec::new()
    }
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
        _checkpoint_id: &str,
        _message: &str,
    ) -> Result<String, String> {
        Err("worktree checkpointing is unavailable in this executor".into())
    }
    /// Remove an immutable checkpoint ref when its matching database
    /// transaction could not be committed. The failed commit is a
    /// compare-and-swap guard.
    async fn rollback_checkpoint_worktree_ref(
        &self,
        _worktree: &Path,
        _session_id: &str,
        _checkpoint_id: &str,
        _failed_commit: &str,
    ) -> Result<(), String> {
        Err("worktree checkpoint rollback is unavailable in this executor".into())
    }
    /// Reconcile immutable checkpoint refs against the rows committed for one
    /// session. This safely removes crash orphans and truncated redo anchors.
    async fn reconcile_checkpoint_worktree_refs(
        &self,
        _worktree: &Path,
        _session_id: &str,
        _live_checkpoint_ids: &[String],
    ) -> Result<(), String> {
        Err("worktree checkpoint reconciliation is unavailable in this executor".into())
    }
    /// Persist one decoded attachment at an engine-selected opaque path.
    /// Implementations must confine the destination to the supplied root and
    /// create the file without replacing an existing artifact.
    fn prepare_attachment_file(
        &self,
        _root: &Path,
        _path: &Path,
        _bytes: &[u8],
    ) -> Result<(), String> {
        Err("attachment preparation is unavailable in this executor".into())
    }
    /// Synchronous rollback for files staged before their database
    /// transaction commits. This is synchronous so an aborted engine future
    /// can perform cleanup from its drop guard.
    fn rollback_attachment_files(&self, _root: &Path, _paths: &[PathBuf]) -> Result<(), String> {
        Err("attachment rollback is unavailable in this executor".into())
    }
    /// Read one durable opaque attachment without following any path link.
    /// Implementations must require a direct child of `root`, a regular file,
    /// and an exact match with the size committed in the attachment row.
    async fn read_attachment_file(
        &self,
        _root: &Path,
        _path: &Path,
        _expected_size: u64,
    ) -> Result<Vec<u8>, String> {
        Err("attachment reads are unavailable in this executor".into())
    }
    /// Verify durable attachment bytes and create deterministic opaque copies
    /// below a server-managed session worktree for path-only vendor clients.
    /// Returned paths are worktree-relative and safe to include in prompts.
    async fn materialize_attachments(
        &self,
        _request: &AttachmentMaterialization,
    ) -> Result<Vec<MaterializedAttachment>, String> {
        Err("attachment materialization is unavailable in this executor".into())
    }
    /// Remove one deleted session's immutable checkpoint namespace and
    /// attachment files through the trusted side-effect boundary.
    async fn cleanup_deleted_session(
        &self,
        _request: &DeletedSessionCleanup,
    ) -> Result<(), String> {
        Err("deleted session cleanup is unavailable in this executor".into())
    }
    /// Delete detached attachment files after their database rows have been
    /// rolled back. Implementations must confine every path to the root and
    /// stop before each destructive step when ownership is lost.
    async fn cleanup_attachment_files(
        &self,
        _root: &Path,
        _paths: Vec<PathBuf>,
        _ownership_lost: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        Err("attachment cleanup is unavailable in this executor".into())
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
    /// Read a bounded current-worktree diff through the trusted Git boundary.
    async fn session_diff(&self, _request: &SessionRepositoryDiff) -> Result<String, String> {
        Err("session diff is unavailable in this executor".into())
    }
    /// Read bounded changed-path metadata through the trusted Git boundary.
    async fn session_diff_summary(
        &self,
        _request: &SessionRepositoryDiff,
    ) -> Result<Vec<crate::git::SessionDiffStat>, String> {
        Err("session diff summary is unavailable in this executor".into())
    }
    /// Resolve a PR base and push one server-managed session branch.
    async fn push_session_branch(
        &self,
        _request: &SessionRepositoryPush,
    ) -> Result<String, String> {
        Err("session branch push is unavailable in this executor".into())
    }
    /// Atomically reserve and create a session worktree. The returned receipt
    /// is opaque outside the executor and is required for finalize/rollback.
    async fn create_session_worktree(
        &self,
        _request: &SessionWorktreeCreate,
    ) -> Result<SessionWorktreeCreation, String> {
        Err("session worktree creation is unavailable in this executor".into())
    }
    async fn finalize_session_worktree(
        &self,
        creation: SessionWorktreeCreation,
    ) -> Result<(), String> {
        creation.finalize()
    }
    async fn rollback_session_worktree(
        &self,
        request: SessionWorktreeRollback,
    ) -> Result<(), String> {
        request.creation.rollback()
    }
    /// Release any per-worktree resources (e.g. spawned shell and MCP server
    /// processes) when a session/worktree is going away. Returning success is
    /// the cleanup acknowledgement required before filesystem teardown.
    async fn evict_worktree(&self, _worktree: &Path) -> Result<(), String> {
        Ok(())
    }
    /// Persist one user-managed MCP config mutation and then quarantine every
    /// cached or in-flight instance of the affected server. Both effects
    /// remain behind the executor boundary; custom executors fail closed
    /// unless they explicitly own such configuration.
    async fn mutate_mcp_config(
        &self,
        _request: &McpConfigMutationRequest,
    ) -> Result<McpConfigMutationOutcome, String> {
        Err("MCP config mutation is unavailable in this executor".into())
    }
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
    pub managed_root: PathBuf,
    pub worktree: PathBuf,
    pub base_sha: String,
    pub cancel: tokio_util::sync::CancellationToken,
}

/// One confined, cancellable read of a server-managed session worktree.
/// The executor revalidates containment immediately before invoking Git so a
/// stale or tampered database path cannot turn a read endpoint into an
/// arbitrary-repository Git invocation.
pub struct SessionRepositoryDiff {
    pub managed_root: PathBuf,
    pub worktree: PathBuf,
    pub base_ref: String,
    pub path: Option<String>,
    pub cancel: tokio_util::sync::CancellationToken,
}

pub struct SessionRepositoryPush {
    pub managed_root: PathBuf,
    pub worktree: PathBuf,
    pub base_ref: String,
    pub requested_base: Option<String>,
    pub branch: String,
    pub cancel: tokio_util::sync::CancellationToken,
}

/// One attachment selected from durable metadata for trusted verification and
/// worktree materialization. `source` must be a direct child of `source_root`.
#[derive(Debug, Clone)]
pub struct AttachmentMaterializationFile {
    pub attachment: trouve_protocol::Attachment,
    pub source: PathBuf,
}

pub struct AttachmentMaterialization {
    pub source_root: PathBuf,
    pub managed_worktree_root: PathBuf,
    pub worktree: PathBuf,
    pub files: Vec<AttachmentMaterializationFile>,
    pub cancel: tokio_util::sync::CancellationToken,
}

#[derive(Debug, Clone)]
pub struct MaterializedAttachment {
    pub attachment: trouve_protocol::Attachment,
    pub bytes: Arc<[u8]>,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
}

pub struct SessionWorktreeCreate {
    pub repository: PathBuf,
    pub worktree: PathBuf,
    pub session_id: String,
    pub checkpoint_id: String,
    pub branch: String,
    pub base_ref: Option<String>,
    pub checkout_ref: Option<String>,
    pub fetch_latest: bool,
}

type SessionWorktreeAction = Box<dyn FnOnce() -> Result<(), String> + Send + 'static>;

/// Executor-owned proof that a newly-created worktree and its pristine
/// checkpoint still belong to this session-creation attempt.
///
/// Until [`Self::mark_durable`] is called, dropping this value synchronously
/// invokes its ownership-checked rollback. This is deliberately a synchronous
/// drop guard: aborting the engine future must not abandon Git artifacts.
pub struct SessionWorktreeCreation {
    pub base_ref: String,
    pub checkpoint_commit: String,
    rollback: Option<SessionWorktreeAction>,
    finalize: Option<SessionWorktreeAction>,
    durable: bool,
}

impl SessionWorktreeCreation {
    /// Build a guarded opaque receipt. Custom executors can use this without
    /// exposing their implementation-specific ownership token.
    pub fn guarded(
        base_ref: String,
        checkpoint_commit: String,
        rollback: impl FnOnce() -> Result<(), String> + Send + 'static,
        finalize: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            base_ref,
            checkpoint_commit,
            rollback: Some(Box::new(rollback)),
            finalize: Some(Box::new(finalize)),
            durable: false,
        }
    }

    /// Mark the relational session and checkpoint rows durable. Call this
    /// synchronously immediately after the committing store operation and
    /// before any await.
    pub fn mark_durable(&mut self) {
        self.rollback.take();
        self.durable = true;
    }

    /// Preserve an ownership marker when relational durability cannot be
    /// determined. Neither destructive rollback nor marker removal is safe;
    /// startup/operator reconciliation can inspect the retained marker.
    pub fn preserve_for_recovery(&mut self) {
        self.rollback.take();
        self.finalize.take();
        self.durable = false;
    }

    /// Release the short-lived reservation. Rollback is disarmed first so a
    /// finalization error cannot delete an already-durable session.
    pub fn finalize(mut self) -> Result<(), String> {
        self.rollback.take();
        self.durable = false;
        match self.finalize.take() {
            Some(finalize) => finalize(),
            None => Ok(()),
        }
    }

    /// Synchronously remove all artifacts proven to belong to this attempt.
    pub fn rollback(mut self) -> Result<(), String> {
        self.finalize.take();
        self.durable = false;
        match self.rollback.take() {
            Some(rollback) => rollback(),
            None => Ok(()),
        }
    }
}

impl Drop for SessionWorktreeCreation {
    fn drop(&mut self) {
        let action = if self.durable {
            self.finalize.take()
        } else {
            self.rollback.take()
        };
        let Some(action) = action else { return };
        if let Err(error) = action() {
            tracing::error!(
                %error,
                durable = self.durable,
                "failed to synchronously settle an abandoned session creation receipt"
            );
        }
    }
}

pub struct SessionWorktreeRollback {
    pub creation: SessionWorktreeCreation,
}

pub struct DeletedSessionCleanup {
    pub managed_worktree_root: PathBuf,
    pub worktree: PathBuf,
    pub repository: PathBuf,
    pub session_id: String,
    pub attachment_root: PathBuf,
    pub attachment_paths: Vec<PathBuf>,
    /// Cancelled by the durable cleanup worker as soon as it can no longer
    /// prove ownership of the cleanup claim.
    pub ownership_lost: tokio_util::sync::CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConfigMutation {
    Upsert(crate::mcp::McpServerConfig),
    SetEnabled(bool),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfigMutationRequest {
    pub path: PathBuf,
    pub name: String,
    pub mutation: McpConfigMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigMutationOutcome {
    Applied,
    NotFound,
}

const REVIEW_GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const REVIEW_GIT_CLEANUP_RESERVE: std::time::Duration = std::time::Duration::from_secs(5);
const REVIEW_GIT_DRAIN_RESERVE: std::time::Duration = std::time::Duration::from_secs(5);
const MAX_REVIEW_GIT_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_REVIEW_DIFF_FILES: usize = 250;
const MAX_REVIEW_DIFF_CHANGED_LINES: u64 = 20_000;
const MAX_REVIEW_DIFF_BYTES: usize = 16 * 1024 * 1024;

fn validate_review_repository_name(repository: &str) -> Result<[&str; 2], String> {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(format!("invalid GitHub repository name: {repository:?}"));
    }
    Ok([parts[0], parts[1]])
}

fn validate_review_commit(commit: &str) -> Result<(), String> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("invalid review commit id: {commit:?}"));
    }
    Ok(())
}

fn canonical_managed_path(root: &Path, path: &Path) -> Result<(PathBuf, PathBuf), String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("resolving managed review root {}: {error}", root.display()))?;
    let (lexical_root, relative) = if let Ok(relative) = path.strip_prefix(root) {
        (root, relative)
    } else if let Ok(relative) = path.strip_prefix(&canonical_root) {
        (canonical_root.as_path(), relative)
    } else {
        return Err(format!(
            "managed review path {} escapes {}",
            path.display(),
            canonical_root.display()
        ));
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!("invalid managed review path: {}", path.display()));
    }
    let mut inspected = lexical_root.to_path_buf();
    for component in relative.components() {
        inspected.push(component);
        if std::fs::symlink_metadata(&inspected)
            .map_err(|error| {
                format!(
                    "reading managed review path {}: {error}",
                    inspected.display()
                )
            })?
            .file_type()
            .is_symlink()
        {
            return Err(format!(
                "managed review path contains a symlink: {}",
                inspected.display()
            ));
        }
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("resolving managed review path {}: {error}", path.display()))?;
    if path == canonical_root || !path.starts_with(&canonical_root) {
        return Err(format!(
            "managed review path {} escapes {}",
            path.display(),
            canonical_root.display()
        ));
    }
    Ok((canonical_root, path))
}

fn bounded_review_git_message(bytes: &[u8]) -> String {
    let end = bytes.len().min(MAX_REVIEW_GIT_MESSAGE_BYTES);
    let mut message = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    if end < bytes.len() {
        message.push_str("\n… output truncated");
    }
    message
}

struct ReviewGitCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_review_git_pipe(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
) -> std::io::Result<ReviewGitCapture> {
    use tokio::io::AsyncReadExt as _;

    let mut bytes = Vec::with_capacity(MAX_REVIEW_GIT_MESSAGE_BYTES);
    let mut truncated = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = pipe.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_REVIEW_GIT_MESSAGE_BYTES.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(ReviewGitCapture { bytes, truncated })
}

fn configure_hermetic_review_git_environment(command: &mut tokio::process::Command) {
    // Authenticated review fetches must not inherit repository redirection,
    // helper execution, alternate object stores, proxying, or credential UI
    // from the long-lived server process. Retain only OS process basics Git
    // needs to start; every Git-specific value is set explicitly below.
    const SAFE_PROCESS_ENV: &[&str] = &[
        "PATH",
        "LANG",
        "LC_ALL",
        "TZ",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
    ];
    let retained = SAFE_PROCESS_ENV
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    command.env_clear();
    for (name, value) in retained {
        command.env(name, value);
    }
}

async fn run_review_git(
    repository_path: &Path,
    auth: &str,
    args: Vec<String>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    run_review_git_with_timeout(repository_path, auth, args, cancel, REVIEW_GIT_TIMEOUT).await
}

async fn run_review_git_with_timeout(
    repository_path: &Path,
    auth: &str,
    args: Vec<String>,
    cancel: &tokio_util::sync::CancellationToken,
    timeout: std::time::Duration,
) -> Result<String, String> {
    run_review_command_with_timeout(
        std::ffi::OsStr::new("git"),
        repository_path,
        auth,
        args,
        cancel,
        timeout,
    )
    .await
}

async fn run_review_command_with_timeout(
    executable: &std::ffi::OsStr,
    repository_path: &Path,
    auth: &str,
    args: Vec<String>,
    cancel: &tokio_util::sync::CancellationToken,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let operation_deadline = tokio::time::Instant::now() + timeout;
    let cleanup_reserve = REVIEW_GIT_CLEANUP_RESERVE.min(timeout / 3);
    let drain_reserve = REVIEW_GIT_DRAIN_RESERVE.min(timeout / 3);
    let cleanup_deadline = operation_deadline - drain_reserve;
    let leader_deadline = cleanup_deadline - cleanup_reserve;
    let mut command = tokio::process::Command::new(executable);
    configure_hermetic_review_git_environment(&mut command);
    command
        .args(&args)
        .current_dir(repository_path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_COUNT", "7")
        // Reset any repository-local extra-header list before appending the
        // one URL-scoped credential owned by this invocation.
        .env("GIT_CONFIG_KEY_0", "http.extraheader")
        .env("GIT_CONFIG_VALUE_0", "")
        .env("GIT_CONFIG_KEY_1", "http.https://github.com/.extraheader")
        .env("GIT_CONFIG_VALUE_1", format!("AUTHORIZATION: basic {auth}"))
        .env("GIT_CONFIG_KEY_2", "credential.helper")
        .env("GIT_CONFIG_VALUE_2", "")
        .env("GIT_CONFIG_KEY_3", "http.proxy")
        .env("GIT_CONFIG_VALUE_3", "")
        .env("GIT_CONFIG_KEY_4", "https.proxy")
        .env("GIT_CONFIG_VALUE_4", "")
        .env("GIT_CONFIG_KEY_5", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_5", "never")
        .env("GIT_CONFIG_KEY_6", "core.hooksPath")
        .env(
            "GIT_CONFIG_VALUE_6",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_PROTOCOL_FROM_USER", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = trouve_agents::process_env::spawn_process_tree(&mut command)
        .map_err(|error| format!("running git: {error}"))?;
    let stdout = child.take_stdout().ok_or("capturing git stdout")?;
    let stderr = child.take_stderr().ok_or("capturing git stderr")?;
    let mut stdout_task = tokio::spawn(read_review_git_pipe(stdout));
    let mut stderr_task = tokio::spawn(read_review_git_pipe(stderr));
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let terminate_deadline = cleanup_deadline
                .min(tokio::time::Instant::now() + cleanup_reserve);
            if let Err(error) = child.terminate_and_reap_until(terminate_deadline).await {
                stdout_task.abort();
                stderr_task.abort();
                return Err(format!(
                    "review repository sync cancelled; process-tree cleanup failed: {error}"
                ));
            }
            Err("review repository sync cancelled".into())
        },
        result = child.wait_and_cleanup_until(leader_deadline, cleanup_deadline) => {
            match result {
                Ok(status) => Ok(status),
                Err(wait_error) => {
                    let terminate_deadline = cleanup_deadline
                        .min(tokio::time::Instant::now() + cleanup_reserve);
                    if let Err(error) = child.terminate_and_reap_until(terminate_deadline).await {
                        stdout_task.abort();
                        stderr_task.abort();
                        return Err(format!(
                            "git {} failed or timed out ({wait_error}) and process-tree cleanup failed: {error}",
                            args.join(" "),
                        ));
                    }
                    if wait_error.kind() == std::io::ErrorKind::TimedOut {
                        Err(format!(
                            "git {} timed out after {:.1}s",
                            args.join(" "),
                            timeout.as_secs_f32(),
                        ))
                    } else {
                        Err(format!("running git or cleaning up its process tree: {wait_error}"))
                    }
                }
            }
        }
    };
    let stdout = match tokio::time::timeout_at(operation_deadline, &mut stdout_task).await {
        Ok(result) => result
            .map_err(|error| format!("git stdout task failed: {error}"))?
            .map_err(|error| format!("reading git stdout: {error}"))?,
        Err(_) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(format!("git {} timed out draining stdout", args.join(" ")));
        }
    };
    let stderr = match tokio::time::timeout_at(operation_deadline, &mut stderr_task).await {
        Ok(result) => result
            .map_err(|error| format!("git stderr task failed: {error}"))?
            .map_err(|error| format!("reading git stderr: {error}"))?,
        Err(_) => {
            stderr_task.abort();
            return Err(format!("git {} timed out draining stderr", args.join(" ")));
        }
    };
    let status = result?;
    if status.success() {
        if stdout.truncated {
            return Err(format!(
                "git {} returned more than {MAX_REVIEW_GIT_MESSAGE_BYTES} bytes",
                args.join(" ")
            ));
        }
        Ok(String::from_utf8_lossy(&stdout.bytes).trim().to_string())
    } else {
        let mut message = bounded_review_git_message(&stderr.bytes);
        if stderr.truncated && !message.ends_with("… output truncated") {
            message.push_str("\n… output truncated");
        }
        Err(message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDiffFile {
    pub path: String,
    pub diff: String,
    /// Newline-separated generated-marker tokens matched in the current
    /// snapshot. Raw patch text never populates this field, and deletions keep
    /// their full diff by leaving it absent.
    pub generated_header: Option<String>,
}

pub(crate) fn is_conventional_generated_artifact_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());
    if matches!(
        file_name,
        "Cargo.lock"
            | "Gemfile.lock"
            | "Pipfile.lock"
            | "bun.lock"
            | "bun.lockb"
            | "composer.lock"
            | "go.sum"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "poetry.lock"
            | "uv.lock"
            | "yarn.lock"
    ) {
        return false;
    }
    path.split('/').any(|component| {
        matches!(
            component,
            "generated" | "snapshots" | "__snapshots__" | "__screenshots__"
        )
    }) || file_name.ends_with(".snap")
        || file_name.ends_with(".min.js")
        || file_name.ends_with(".min.css")
        || [
            ".js.map",
            ".mjs.map",
            ".cjs.map",
            ".css.map",
            ".d.ts.map",
            ".d.mts.map",
            ".d.cts.map",
        ]
        .iter()
        .any(|suffix| file_name.ends_with(suffix))
}

/// Runs tools in-process against the local filesystem/shell, plus any MCP
/// servers configured for the workspace.
pub struct LocalToolExecutor {
    tools: Vec<Arc<dyn Tool>>,
    built_in_specs: Vec<ToolSpec>,
    mcp: crate::mcp::McpManager,
    jobs: Arc<shell::JobRegistry>,
    hashline_failures: Mutex<HashMap<String, u8>>,
    review_repository_locks: Mutex<HashMap<PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>>,
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
            Arc::new(shell::Shell::new(jobs.clone())),
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
            review_repository_locks: Mutex::new(HashMap::new()),
        }
    }

    fn review_repository_lock(&self, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.review_repository_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(path).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
        lock
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
        if !edit_strategy::benchmark_tool_allowed(ctx.edit_strategy, name) {
            return Some(ToolResult::error(format!(
                "{name} is unavailable in an enforced edit-strategy benchmark run"
            )));
        }
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
        match ctx.edit_strategy {
            EditStrategy::EnforceApplyPatch => {
                if matches!(name, "edit_file" | "hashline_edit") {
                    return Some(ToolResult::error(format!(
                        "{name} is unavailable in an enforced apply_patch benchmark run"
                    )));
                }
                if name == "write_file"
                    && let Some(path) = args.get("path").and_then(Value::as_str)
                    && ctx.resolve(path).is_ok_and(|path| path.exists())
                {
                    return Some(ToolResult::error(
                        "write_file may only create new files in an enforced apply_patch benchmark run",
                    ));
                }
            }
            EditStrategy::EnforceHashline => {
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
            }
            EditStrategy::Auto | EditStrategy::PreferApplyPatch | EditStrategy::PreferHashline => {}
        }
        None
    }
}

const MAX_ATTACHMENT_FILE_BYTES: u64 = 10 * 1024 * 1024;

fn attachment_file_name<'a>(root: &Path, path: &'a Path) -> Result<&'a std::ffi::OsStr, String> {
    if path.parent() != Some(root) {
        return Err(format!(
            "attachment path {} is not a direct child of {}",
            path.display(),
            root.display()
        ));
    }
    path.file_name()
        .ok_or_else(|| format!("attachment path {} has no file name", path.display()))
}

#[cfg(unix)]
fn attachment_c_name(root: &Path, path: &Path) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(attachment_file_name(root, path)?.as_bytes())
        .map_err(|_| "attachment file name contains NUL".to_string())
}

#[cfg(unix)]
fn open_directory_nofollow(
    path: &Path,
    create_final: bool,
) -> Result<std::os::fd::OwnedFd, String> {
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;

    // Resolve aliases in trusted ancestors (notably macOS `/var` ->
    // `/private/var`) while retaining the final component for the
    // descriptor-relative O_NOFOLLOW check below. Canonicalizing the entire
    // path would silently accept a symlink in the protected root itself.
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("attachment root has no final component: {}", path.display()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = parent.canonicalize().map_err(|error| {
        format!(
            "resolving attachment root parent {}: {error}",
            parent.display()
        )
    })?;
    let walk_path = canonical_parent.join(file_name);

    let start = if walk_path.is_absolute() { "/" } else { "." };
    let start = std::ffi::CString::new(start).expect("static path has no NUL");
    // SAFETY: `start` is NUL-terminated and the returned descriptor is
    // immediately transferred to OwnedFd.
    let start_fd = unsafe {
        libc::open(
            start.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if start_fd < 0 {
        return Err(format!(
            "opening attachment root anchor: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `start_fd` is a newly owned successful open result.
    let mut current = unsafe { OwnedFd::from_raw_fd(start_fd) };
    let components = walk_path.components().collect::<Vec<_>>();
    let mut normal_index = 0usize;
    let normal_count = components
        .iter()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    for component in components {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir | Component::CurDir) {
                continue;
            }
            return Err(format!(
                "attachment root contains unsafe component: {}",
                path.display()
            ));
        };
        normal_index += 1;
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| "attachment directory contains NUL".to_string())?;
        // SAFETY: descriptor and C string are valid; O_NOFOLLOW rejects a
        // symlink/reparse-like final component at every step in the walk.
        let mut next = unsafe {
            libc::openat(
                std::os::fd::AsRawFd::as_raw_fd(&current),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if next < 0
            && create_final
            && normal_index == normal_count
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            // SAFETY: parent descriptor and C string are valid. mkdirat does
            // not follow a nonexistent final component.
            let made = unsafe {
                libc::mkdirat(
                    std::os::fd::AsRawFd::as_raw_fd(&current),
                    name.as_ptr(),
                    0o700,
                )
            };
            if made < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(format!(
                    "creating attachment directory: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // Persist the new directory entry before any attachment row can
            // become durable in SQLite.
            if unsafe { libc::fsync(std::os::fd::AsRawFd::as_raw_fd(&current)) } < 0 {
                return Err(format!(
                    "syncing attachment directory parent: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: same validated descriptor-relative open as above.
            next = unsafe {
                libc::openat(
                    std::os::fd::AsRawFd::as_raw_fd(&current),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
        }
        if next < 0 {
            return Err(format!(
                "opening attachment directory without following links: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `next` is a newly owned successful openat result.
        current = unsafe { OwnedFd::from_raw_fd(next) };
    }
    Ok(current)
}

#[cfg(unix)]
fn prepare_attachment_nofollow(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    let root_fd = open_directory_nofollow(root, true)?;
    let name = attachment_c_name(root, path)?;
    // SAFETY: root_fd and name are valid. O_EXCL + O_NOFOLLOW gives
    // create-new semantics without following a swapped final link.
    let fd = unsafe {
        libc::openat(
            root_fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(format!(
            "creating attachment without following links: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` is a newly owned successful openat result.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    std::io::Write::write_all(&mut file, bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    if unsafe { libc::fsync(root_fd.as_raw_fd()) } < 0 {
        return Err(format!(
            "syncing attachment root: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn read_attachment_nofollow(
    root: &Path,
    path: &Path,
    expected_size: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    if expected_size > MAX_ATTACHMENT_FILE_BYTES {
        return Err(format!(
            "attachment exceeds {MAX_ATTACHMENT_FILE_BYTES} bytes"
        ));
    }
    let root_fd = open_directory_nofollow(root, false)?;
    let name = attachment_c_name(root, path)?;
    let raw = unsafe {
        libc::openat(
            root_fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(format!(
            "opening attachment without following links: {}",
            std::io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd.as_raw_fd(), metadata.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(format!(
            "attachment is not a regular file: {}",
            path.display()
        ));
    }
    if metadata.st_size < 0 || metadata.st_size as u64 != expected_size {
        return Err(format!(
            "attachment size changed: expected {expected_size}, found {}",
            metadata.st_size
        ));
    }
    let mut bytes = Vec::with_capacity(expected_size as usize);
    std::fs::File::from(fd)
        .take(expected_size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != expected_size {
        return Err(format!(
            "attachment size changed while reading: expected {expected_size}, read {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn cleanup_attachments_nofollow(
    root: &Path,
    paths: &[PathBuf],
    ownership_lost: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), String> {
    use std::os::fd::AsRawFd as _;
    ensure_cleanup_claim_owned(ownership_lost)?;
    let root_fd = match open_directory_nofollow(root, false) {
        Ok(fd) => fd,
        Err(_error) if !root.exists() => return Ok(()),
        Err(error) => return Err(error),
    };
    for path in paths {
        ensure_cleanup_claim_owned(ownership_lost)?;
        let name = attachment_c_name(root, path)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: descriptor/name/output pointer are valid. The no-follow
        // flag observes the directory entry itself.
        let inspected = unsafe {
            libc::fstatat(
                root_fd.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if inspected < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                continue;
            }
            return Err(error.to_string());
        }
        // SAFETY: fstatat succeeded and initialized metadata.
        let metadata = unsafe { metadata.assume_init() };
        let kind = metadata.st_mode & libc::S_IFMT;
        if kind != libc::S_IFREG && kind != libc::S_IFLNK {
            return Err(format!(
                "refusing to remove non-file attachment entry: {}",
                path.display()
            ));
        }
        // SAFETY: unlinkat removes only the entry below the already-open root
        // descriptor; it never follows a swapped final symlink.
        let removed = unsafe { libc::unlinkat(root_fd.as_raw_fd(), name.as_ptr(), 0) };
        if removed < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    ensure_cleanup_claim_owned(ownership_lost)?;
    if unsafe { libc::fsync(root_fd.as_raw_fd()) } < 0 {
        return Err(format!(
            "syncing attachment cleanup root: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_open_directory(path: &Path, create_final: bool) -> Result<std::fs::File, String> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if create_final && error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let directory = options.open(path).map_err(|error| error.to_string())?;
    let attributes = directory
        .metadata()
        .map_err(|error| error.to_string())?
        .file_attributes();
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(format!(
            "attachment directory is not a plain directory: {}",
            path.display()
        ));
    }
    Ok(directory)
}

#[cfg(windows)]
fn windows_final_path(file: &std::fs::File) -> Result<String, String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };
    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let needed = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if needed == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut buffer = vec![0_u16; needed as usize + 1];
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(String::from_utf16_lossy(&buffer[..written as usize])
        .trim_end_matches(['\\', '/'])
        .to_lowercase())
}

#[cfg(windows)]
fn windows_verify_direct_child(
    root_file: &std::fs::File,
    child_file: &std::fs::File,
) -> Result<(), String> {
    let root = windows_final_path(root_file)?;
    let child = windows_final_path(child_file)?;
    let parent = child
        .rfind(['\\', '/'])
        .map(|index| &child[..index])
        .ok_or_else(|| "opened attachment has no parent".to_string())?;
    if parent != root {
        return Err("opened attachment escaped its trusted root".into());
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_attachment_windows(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ,
    };
    let _ = attachment_file_name(root, path)?;
    let root_file = windows_open_directory(root, true)?;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("created attachment is not a plain regular file".into());
    }
    windows_verify_direct_child(&root_file, &file)?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    root_file.sync_all().map_err(|error| error.to_string())
}

#[cfg(windows)]
fn read_attachment_windows(
    root: &Path,
    path: &Path,
    expected_size: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    if expected_size > MAX_ATTACHMENT_FILE_BYTES {
        return Err(format!(
            "attachment exceeds {MAX_ATTACHMENT_FILE_BYTES} bytes"
        ));
    }
    let _ = attachment_file_name(root, path)?;
    let root_file = windows_open_directory(root, false)?;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("attachment is not a plain regular file".into());
    }
    if metadata.len() != expected_size {
        return Err(format!(
            "attachment size changed: expected {expected_size}, found {}",
            metadata.len()
        ));
    }
    windows_verify_direct_child(&root_file, &file)?;
    let mut bytes = Vec::with_capacity(expected_size as usize);
    file.take(expected_size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != expected_size {
        return Err(format!(
            "attachment size changed while reading: expected {expected_size}, read {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(windows)]
fn cleanup_attachments_windows(
    root: &Path,
    paths: &[PathBuf],
    ownership_lost: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), String> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo,
        SetFileInformationByHandle,
    };
    ensure_cleanup_claim_owned(ownership_lost)?;
    let root_file = match windows_open_directory(root, false) {
        Ok(file) => file,
        Err(error)
            if std::fs::symlink_metadata(root)
                .is_err_and(|missing| missing.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    for path in paths {
        ensure_cleanup_claim_owned(ownership_lost)?;
        let _ = attachment_file_name(root, path)?;
        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(GENERIC_READ | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if file
            .metadata()
            .map_err(|error| error.to_string())?
            .file_attributes()
            & FILE_ATTRIBUTE_DIRECTORY
            != 0
        {
            return Err(format!(
                "refusing to remove attachment directory: {}",
                path.display()
            ));
        }
        windows_verify_direct_child(&root_file, &file)?;
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        let removed = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
                FileDispositionInfo,
                (&raw const disposition).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if removed == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    ensure_cleanup_claim_owned(ownership_lost)?;
    root_file.sync_all().map_err(|error| error.to_string())
}

fn prepare_attachment_secure(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    return prepare_attachment_nofollow(root, path, bytes);
    #[cfg(windows)]
    return prepare_attachment_windows(root, path, bytes);
    #[cfg(not(any(unix, windows)))]
    Err("secure attachment preparation is unavailable on this platform".into())
}

fn read_attachment_secure(root: &Path, path: &Path, size: u64) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    return read_attachment_nofollow(root, path, size);
    #[cfg(windows)]
    return read_attachment_windows(root, path, size);
    #[cfg(not(any(unix, windows)))]
    Err("secure attachment reads are unavailable on this platform".into())
}

fn ensure_cleanup_claim_owned(
    ownership_lost: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), String> {
    if ownership_lost.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
        Err("artifact cleanup claim is no longer owned".into())
    } else {
        Ok(())
    }
}

fn cleanup_attachments_secure_controlled(
    root: &Path,
    paths: &[PathBuf],
    ownership_lost: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), String> {
    #[cfg(unix)]
    return cleanup_attachments_nofollow(root, paths, ownership_lost);
    #[cfg(windows)]
    return cleanup_attachments_windows(root, paths, ownership_lost);
    #[cfg(not(any(unix, windows)))]
    Err("secure attachment cleanup is unavailable on this platform".into())
}

fn cleanup_attachments_secure(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    cleanup_attachments_secure_controlled(root, paths, None)
}

fn ensure_materialized_attachment(root: &Path, path: &Path, bytes: &[u8]) -> Result<bool, String> {
    match read_attachment_secure(root, path, bytes.len() as u64) {
        Ok(existing) if existing == bytes => return Ok(false),
        Ok(_) => {
            return Err(format!(
                "materialized attachment differs: {}",
                path.display()
            ));
        }
        Err(_) => {}
    }
    match prepare_attachment_secure(root, path, bytes) {
        Ok(()) => Ok(true),
        Err(create_error) => match read_attachment_secure(root, path, bytes.len() as u64) {
            Ok(existing) if existing == bytes => Ok(false),
            Ok(_) => Err(format!(
                "materialized attachment differs: {}",
                path.display()
            )),
            Err(read_error) => Err(format!(
                "creating materialized attachment failed ({create_error}); verifying it failed ({read_error})"
            )),
        },
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
        if !edit_strategy::is_enforced_benchmark(ctx.edit_strategy) {
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
        }
        specs
    }

    async fn native_specs(&self, ctx: &ToolCtx) -> Vec<ToolSpec> {
        self.built_in_specs
            .iter()
            .cloned()
            .filter_map(|spec| edit_strategy::advertise(ctx.edit_strategy, spec))
            .collect()
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
        if let Some(denial) = self.edit_policy_denial(ctx, name, args) {
            return denial;
        }
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
        checkpoint_id: &str,
        message: &str,
    ) -> Result<String, String> {
        let worktree = worktree.to_path_buf();
        let session_id = session_id.to_string();
        let checkpoint_id = checkpoint_id.to_string();
        let message = message.to_string();
        tokio::task::spawn_blocking(move || {
            crate::git::checkpoint(&worktree, &session_id, &checkpoint_id, &message)
        })
        .await
        .map_err(|error| format!("checkpoint worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    async fn rollback_checkpoint_worktree_ref(
        &self,
        worktree: &Path,
        session_id: &str,
        checkpoint_id: &str,
        failed_commit: &str,
    ) -> Result<(), String> {
        let worktree = worktree.to_path_buf();
        let session_id = session_id.to_string();
        let checkpoint_id = checkpoint_id.to_string();
        let failed_commit = failed_commit.to_string();
        tokio::task::spawn_blocking(move || {
            crate::git::rollback_checkpoint_ref(
                &worktree,
                &session_id,
                &checkpoint_id,
                &failed_commit,
            )
        })
        .await
        .map_err(|error| format!("checkpoint rollback worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    async fn reconcile_checkpoint_worktree_refs(
        &self,
        worktree: &Path,
        session_id: &str,
        live_checkpoint_ids: &[String],
    ) -> Result<(), String> {
        let worktree = worktree.to_path_buf();
        let session_id = session_id.to_string();
        let live_checkpoint_ids = live_checkpoint_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            crate::git::reconcile_checkpoint_refs(&worktree, &session_id, &live_checkpoint_ids)
        })
        .await
        .map_err(|error| format!("checkpoint reconciliation worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    fn prepare_attachment_file(
        &self,
        root: &Path,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), String> {
        prepare_attachment_secure(root, path, bytes)
    }

    fn rollback_attachment_files(&self, root: &Path, paths: &[PathBuf]) -> Result<(), String> {
        cleanup_attachments_secure(root, paths)
    }

    async fn read_attachment_file(
        &self,
        root: &Path,
        path: &Path,
        expected_size: u64,
    ) -> Result<Vec<u8>, String> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || read_attachment_secure(&root, &path, expected_size))
            .await
            .map_err(|error| format!("attachment read worker failed: {error}"))?
    }

    async fn materialize_attachments(
        &self,
        request: &AttachmentMaterialization,
    ) -> Result<Vec<MaterializedAttachment>, String> {
        let (_, worktree) =
            canonical_managed_path(&request.managed_worktree_root, &request.worktree)?;
        let source_root = request.source_root.clone();
        let files = request.files.clone();
        let cancel = request.cancel.clone();
        tokio::task::spawn_blocking(move || {
            let trouve_root = worktree.join(".trouve");
            #[cfg(unix)]
            {
                open_directory_nofollow(&trouve_root, true)?;
            }
            #[cfg(windows)]
            {
                windows_open_directory(&trouve_root, true)?;
            }
            #[cfg(not(any(unix, windows)))]
            return Err("secure attachment materialization is unavailable on this platform".into());
            let materialized_root = trouve_root.join("attachments");
            #[cfg(unix)]
            {
                open_directory_nofollow(&materialized_root, true)?;
            }
            #[cfg(windows)]
            {
                windows_open_directory(&materialized_root, true)?;
            }

            let mut out = Vec::with_capacity(files.len());
            for file in files {
                if cancel.is_cancelled() {
                    return Err("attachment materialization cancelled".into());
                }
                let bytes =
                    read_attachment_secure(&source_root, &file.source, file.attachment.size_bytes)?;
                let ext = Path::new(&file.attachment.name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .filter(|extension| {
                        extension.len() <= 8
                            && extension
                                .chars()
                                .all(|character| character.is_ascii_alphanumeric())
                    })
                    .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
                    .unwrap_or_default();
                let filename = format!("{}{}", file.attachment.id, ext);
                if !filename
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
                {
                    return Err("attachment id cannot form an opaque materialized path".into());
                }
                let absolute_path = materialized_root.join(&filename);
                ensure_materialized_attachment(&materialized_root, &absolute_path, &bytes)?;
                out.push(MaterializedAttachment {
                    attachment: file.attachment,
                    bytes: Arc::from(bytes),
                    relative_path: Path::new(".trouve").join("attachments").join(filename),
                    absolute_path,
                });
            }
            Ok(out)
        })
        .await
        .map_err(|error| format!("attachment materialization worker failed: {error}"))?
    }

    async fn cleanup_deleted_session(&self, request: &DeletedSessionCleanup) -> Result<(), String> {
        let session_id = request.session_id.clone();
        if !session_id.starts_with("se_")
            || session_id.len() != 35
            || !session_id[3..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("deleted-session cleanup has an invalid session id".into());
        }
        let expected = request.managed_worktree_root.join(&session_id);
        if request.worktree != expected {
            return Err(format!(
                "deleted-session worktree {} is not its exact managed target {}",
                request.worktree.display(),
                expected.display()
            ));
        }
        let _managed_root = request
            .managed_worktree_root
            .canonicalize()
            .map_err(|error| format!("resolving managed worktree root: {error}"))?;
        let attachment_root = request.attachment_root.clone();
        let mut attachment_paths = Vec::new();
        for path in &request.attachment_paths {
            match attachment_file_name(&attachment_root, path) {
                Ok(_) => attachment_paths.push(path.clone()),
                Err(error) => tracing::warn!(
                    session_id,
                    path = %path.display(),
                    %error,
                    "skipping poisoned attachment path during deleted-session cleanup"
                ),
            }
        }
        match std::fs::symlink_metadata(&request.worktree) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "deleted-session worktree must not be a symlink: {}",
                    request.worktree.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure_cleanup_claim_owned(Some(&request.ownership_lost))?;
                self.evict_worktree(&request.worktree).await?;
                ensure_cleanup_claim_owned(Some(&request.ownership_lost))?;
                let ownership_lost = request.ownership_lost.clone();
                return tokio::task::spawn_blocking(move || {
                    let cleanup = cleanup_attachments_secure_controlled(
                        &attachment_root,
                        &attachment_paths,
                        Some(&ownership_lost),
                    );
                    if let Err(error) = cleanup {
                        tracing::warn!(
                            session_id,
                            %error,
                            "some deleted-session attachments could not be cleaned"
                        );
                    }
                    ensure_cleanup_claim_owned(Some(&ownership_lost))
                })
                .await
                .map_err(|error| format!("deleted session cleanup worker failed: {error}"))?;
            }
            Err(error) => {
                return Err(format!(
                    "inspecting deleted-session worktree {}: {error}",
                    request.worktree.display()
                ));
            }
        }
        let (managed_root, worktree) =
            canonical_managed_path(&request.managed_worktree_root, &request.worktree)?;
        let expected = managed_root.join(&session_id);
        if worktree != expected {
            return Err(format!(
                "deleted-session worktree {} does not resolve to {}",
                worktree.display(),
                expected.display()
            ));
        }
        let repository = request
            .repository
            .canonicalize()
            .map_err(|error| format!("resolving deleted-session repository: {error}"))?;
        let worktree_common = crate::git::common_directory(&worktree)
            .map_err(|error| format!("verifying deleted-session worktree repository: {error:#}"))?;
        let repository_common = crate::git::common_directory(&repository)
            .map_err(|error| format!("verifying deleted-session repository: {error:#}"))?;
        if worktree_common != repository_common {
            return Err(
                "deleted-session worktree is not associated with its recorded repository".into(),
            );
        }
        ensure_cleanup_claim_owned(Some(&request.ownership_lost))?;
        self.evict_worktree(&worktree).await?;
        ensure_cleanup_claim_owned(Some(&request.ownership_lost))?;
        let ownership_lost = request.ownership_lost.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if let Err(error) = cleanup_attachments_secure_controlled(
                &attachment_root,
                &attachment_paths,
                Some(&ownership_lost),
            ) {
                tracing::warn!(session_id, %error, "some deleted-session attachments could not be cleaned");
            }
            ensure_cleanup_claim_owned(Some(&ownership_lost)).map_err(anyhow::Error::msg)?;
            crate::git::delete_session_checkpoint_refs(&repository, &session_id)?;
            ensure_cleanup_claim_owned(Some(&ownership_lost)).map_err(anyhow::Error::msg)?;
            crate::git::remove_worktree(&repository, &worktree)?;
            Ok(())
        })
        .await
        .map_err(|error| format!("deleted session cleanup worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    async fn cleanup_attachment_files(
        &self,
        root: &Path,
        paths: Vec<PathBuf>,
        ownership_lost: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            cleanup_attachments_secure_controlled(&root, &paths, Some(&ownership_lost))
                .map_err(anyhow::Error::msg)?;
            Ok(())
        })
        .await
        .map_err(|error| format!("attachment cleanup worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    async fn sync_review_repository(
        &self,
        request: &ReviewRepositorySync,
    ) -> Result<PathBuf, String> {
        use base64::Engine as _;

        let [owner, repository] = validate_review_repository_name(&request.repository)?;
        validate_review_commit(&request.base_sha)?;
        validate_review_commit(&request.head_sha)?;
        std::fs::create_dir_all(&request.root).map_err(|error| error.to_string())?;
        let managed_root = request
            .root
            .canonicalize()
            .map_err(|error| format!("resolving review root: {error}"))?;
        let repository_path = managed_root.join(owner).join(repository);
        let parent = repository_path
            .parent()
            .ok_or_else(|| "invalid review repository path".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|error| format!("resolving review repository parent: {error}"))?;
        if !canonical_parent.starts_with(&managed_root) {
            return Err("review repository parent escapes the managed root".into());
        }
        if std::fs::symlink_metadata(parent)
            .map_err(|error| format!("reading review repository parent: {error}"))?
            .file_type()
            .is_symlink()
        {
            return Err("review repository parent must not be a symlink".into());
        }
        let repository_lock = self.review_repository_lock(&repository_path);
        let _repository_guard = tokio::select! {
            biased;
            _ = request.cancel.cancelled() => {
                return Err("review repository sync cancelled".into());
            }
            guard = repository_lock.lock_owned() => guard,
        };
        match std::fs::symlink_metadata(&repository_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "review repository path must not be a symlink: {}",
                    repository_path.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "{} exists but is not a directory",
                    repository_path.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&repository_path).map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
        let git_path = repository_path.join(".git");
        match std::fs::symlink_metadata(&git_path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!(
                    "review repository metadata must be a directory, not a link: {}",
                    git_path.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut contents = std::fs::read_dir(&repository_path)
                    .map_err(|error| format!("reading review repository directory: {error}"))?;
                if contents
                    .next()
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    return Err(format!(
                        "{} exists but is not an empty or initialized git repository",
                        repository_path.display()
                    ));
                }
            }
            Err(error) => return Err(error.to_string()),
        }

        let auth = base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", request.token));
        let remote_url = format!("https://github.com/{owner}/{repository}.git");
        let run = |args: Vec<String>| {
            let repository_path = repository_path.clone();
            let auth = auth.clone();
            let cancel = request.cancel.clone();
            async move { run_review_git(&repository_path, &auth, args, &cancel).await }
        };

        // `git init` is idempotent and repairs retries that previously stopped
        // after mkdir or partway through repository initialization.
        run(vec!["init".into(), "--template=".into()]).await?;
        // Reconcile every retry, including crashes after mkdir/init and
        // tampering between jobs. Fetch below uses the fixed URL directly so
        // an attacker-controlled origin can never receive the installation
        // token.
        run(vec!["remote".into(), "remove".into(), "origin".into()])
            .await
            .ok();
        run(vec![
            "remote".into(),
            "add".into(),
            "origin".into(),
            remote_url.clone(),
        ])
        .await?;

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
            remote_url,
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
        validate_review_commit(&request.base_sha)?;
        let (_, worktree) = canonical_managed_path(&request.managed_root, &request.worktree)?;
        let base_sha = request.base_sha.clone();
        let cancel = request.cancel.clone();
        tokio::task::spawn_blocking(move || {
            crate::git::session_diff_patches_cancellable(
                &worktree,
                &base_sha,
                MAX_REVIEW_DIFF_FILES,
                MAX_REVIEW_DIFF_CHANGED_LINES,
                MAX_REVIEW_DIFF_BYTES,
                &cancel,
                is_conventional_generated_artifact_path,
            )
            .map(|files| {
                files
                    .into_iter()
                    .map(|file| ReviewDiffFile {
                        path: file.path,
                        diff: file.diff,
                        generated_header: file.generated_header,
                    })
                    .collect()
            })
        })
        .await
        .map_err(|error| format!("review diff manifest task failed: {error}"))?
        .map_err(|error| error.to_string())
    }

    async fn session_diff(&self, request: &SessionRepositoryDiff) -> Result<String, String> {
        let (_, worktree) = canonical_managed_path(&request.managed_root, &request.worktree)?;
        let base_ref = request.base_ref.clone();
        let path = request.path.clone();
        let cancel = request.cancel.clone();
        tokio::task::spawn_blocking(move || match path {
            Some(path) => {
                crate::git::session_diff_path_cancellable(&worktree, &base_ref, &path, &cancel)
            }
            None => crate::git::session_diff_cancellable(&worktree, &base_ref, &cancel),
        })
        .await
        .map_err(|error| format!("session diff task failed: {error}"))?
        .map_err(|error| error.to_string())
    }

    async fn session_diff_summary(
        &self,
        request: &SessionRepositoryDiff,
    ) -> Result<Vec<crate::git::SessionDiffStat>, String> {
        let (_, worktree) = canonical_managed_path(&request.managed_root, &request.worktree)?;
        let base_ref = request.base_ref.clone();
        let cancel = request.cancel.clone();
        tokio::task::spawn_blocking(move || {
            crate::git::session_diff_summary_cancellable(&worktree, &base_ref, &cancel)
        })
        .await
        .map_err(|error| format!("session diff summary task failed: {error}"))?
        .map_err(|error| error.to_string())
    }

    async fn push_session_branch(&self, request: &SessionRepositoryPush) -> Result<String, String> {
        let (_, worktree) = canonical_managed_path(&request.managed_root, &request.worktree)?;
        let base_ref = request.base_ref.clone();
        let requested_base = request.requested_base.clone();
        let branch = request.branch.clone();
        let cancel = request.cancel.clone();
        tokio::task::spawn_blocking(move || {
            crate::git::push_session_branch_cancellable(
                &worktree,
                &base_ref,
                requested_base.as_deref(),
                &branch,
                &cancel,
            )
        })
        .await
        .map_err(|error| format!("session branch push task failed: {error}"))?
        .map_err(|error| error.to_string())
    }

    async fn create_session_worktree(
        &self,
        request: &SessionWorktreeCreate,
    ) -> Result<SessionWorktreeCreation, String> {
        let repository = request.repository.clone();
        let worktree = request.worktree.clone();
        let session_id = request.session_id.clone();
        let checkpoint_id = request.checkpoint_id.clone();
        let branch = request.branch.clone();
        let selected_base = request.base_ref.clone();
        let checkout_ref = request.checkout_ref.clone();
        let fetch_latest = request.fetch_latest;
        tokio::task::spawn_blocking(move || -> anyhow::Result<SessionWorktreeCreation> {
            let selected_base = match selected_base {
                Some(base) => base,
                None => crate::git::head_ref(&repository)?,
            };
            if worktree.exists() {
                anyhow::bail!("generated worktree path already exists: {}", worktree.display());
            }
            if crate::git::local_branch_exists(&repository, &branch)? {
                anyhow::bail!("generated session branch already exists: {branch}");
            }
            if crate::git::checkpoint_ref_exists(&repository, &session_id, &checkpoint_id)? {
                anyhow::bail!(
                    "generated session checkpoint ref already exists: {session_id}/{checkpoint_id}"
                );
            }
            let mut session_base = selected_base.clone();
            let worktree_base = if fetch_latest {
                match crate::git::fetch_upstream_base(&repository, &selected_base)? {
                    Some(fetched) => {
                        session_base = fetched.upstream_ref;
                        fetched.commit
                    }
                    None => selected_base,
                }
            } else {
                selected_base
            };
            let checkout_ref = checkout_ref.as_deref().unwrap_or(&worktree_base);
            let receipt =
                crate::git::create_worktree(&repository, &worktree, &branch, checkout_ref)?;
            let checkpoint_commit = match crate::git::checkpoint(
                &worktree,
                &session_id,
                &checkpoint_id,
                "trouve: session start",
            ) {
                Ok(commit) => commit,
                Err(error) => {
                    if let Err(rollback) =
                        crate::git::rollback_worktree_creation(&repository, &receipt, None)
                    {
                        return Err(error).context(format!(
                            "initial checkpoint failed; ownership-safe worktree rollback also failed: {rollback:#}"
                        ));
                    }
                    return Err(error).context("creating pristine session checkpoint");
                }
            };
            let receipt = std::sync::Arc::new(receipt);
            let rollback_receipt = receipt.clone();
            let rollback_repository = repository.clone();
            let rollback_session_id = session_id.clone();
            let rollback_checkpoint_id = checkpoint_id.clone();
            let rollback_commit = checkpoint_commit.clone();
            Ok(SessionWorktreeCreation::guarded(
                session_base,
                checkpoint_commit,
                move || {
                    crate::git::rollback_worktree_creation(
                        &rollback_repository,
                        &rollback_receipt,
                        Some((
                            &rollback_session_id,
                            &rollback_checkpoint_id,
                            &rollback_commit,
                        )),
                    )
                    .map_err(|error| format!("{error:#}"))
                },
                move || crate::git::finalize_worktree_creation(&receipt)
                    .map_err(|error| format!("{error:#}")),
            ))
        })
        .await
        .map_err(|error| format!("session worktree creation worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
    }

    async fn finalize_session_worktree(
        &self,
        creation: SessionWorktreeCreation,
    ) -> Result<(), String> {
        tokio::task::spawn_blocking(move || creation.finalize())
            .await
            .map_err(|error| format!("session worktree finalization worker failed: {error}"))?
    }

    async fn rollback_session_worktree(
        &self,
        request: SessionWorktreeRollback,
    ) -> Result<(), String> {
        tokio::task::spawn_blocking(move || request.creation.rollback())
            .await
            .map_err(|error| format!("session worktree rollback worker failed: {error}"))?
    }

    async fn evict_worktree(&self, worktree: &Path) -> Result<(), String> {
        // Eviction is itself teardown and must never wait on the session
        // mutation lane held by a background job. Attempt both independent
        // resource owners even when one cannot acknowledge cleanup.
        let shell = self.jobs.kill_worktree(worktree).await.err();
        let mcp = self
            .mcp
            .evict_worktree(worktree)
            .await
            .err()
            .map(|error| format!("{error:#}"));
        match (shell, mcp) {
            (None, None) => Ok(()),
            (Some(error), None) => Err(error),
            (None, Some(error)) => Err(error),
            (Some(shell), Some(mcp)) => Err(format!("{shell}; MCP cleanup also failed: {mcp}")),
        }
    }

    async fn mutate_mcp_config(
        &self,
        request: &McpConfigMutationRequest,
    ) -> Result<McpConfigMutationOutcome, String> {
        let path = request.path.clone();
        let name = request.name.clone();
        let mutation = request.mutation.clone();
        let outcome = tokio::task::spawn_blocking(
            move || -> std::result::Result<McpConfigMutationOutcome, String> {
                match mutation {
                    McpConfigMutation::Upsert(config) => {
                        crate::mcp::upsert_server(&path, &name, &config)
                            .map_err(|error| format!("{error:#}"))?;
                        Ok(McpConfigMutationOutcome::Applied)
                    }
                    McpConfigMutation::SetEnabled(enabled) => {
                        if crate::mcp::set_server_enabled(&path, &name, enabled)
                            .map_err(|error| format!("{error:#}"))?
                        {
                            Ok(McpConfigMutationOutcome::Applied)
                        } else {
                            Ok(McpConfigMutationOutcome::NotFound)
                        }
                    }
                    McpConfigMutation::Remove => {
                        crate::mcp::remove_server(&path, &name)
                            .map_err(|error| format!("{error:#}"))?;
                        Ok(McpConfigMutationOutcome::Applied)
                    }
                }
            },
        )
        .await
        .map_err(|error| format!("MCP config mutation worker failed: {error}"))??;
        if outcome == McpConfigMutationOutcome::Applied
            && let Err(error) = self.mcp.evict_server(&request.name).await
        {
            // The manager made the old definition non-reusable before it
            // attempted cleanup. The config commit therefore succeeded;
            // retain that quarantine and report cleanup as an operational
            // warning instead of inviting a retry of the committed RMW.
            tracing::warn!(
                server = %request.name,
                error = format!("{error:#}"),
                "MCP config mutation committed; quarantined process cleanup was not acknowledged"
            );
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_artifact_paths_exclude_lockfiles_and_unrelated_map_files() {
        for lockfile in [
            "Cargo.lock",
            "Gemfile.lock",
            "Pipfile.lock",
            "bun.lock",
            "bun.lockb",
            "composer.lock",
            "go.sum",
            "package-lock.json",
            "pnpm-lock.yaml",
            "poetry.lock",
            "uv.lock",
            "yarn.lock",
        ] {
            assert!(!is_conventional_generated_artifact_path(&format!(
                "generated/{lockfile}"
            )));
        }
        for source_map in [
            "assets/app.js.map",
            "assets/app.mjs.map",
            "assets/app.cjs.map",
            "assets/app.css.map",
            "assets/app.d.ts.map",
            "assets/app.d.mts.map",
            "assets/app.d.cts.map",
        ] {
            assert!(is_conventional_generated_artifact_path(source_map));
        }
        assert!(!is_conventional_generated_artifact_path(
            "assets/regions.map"
        ));
        assert!(is_conventional_generated_artifact_path(
            "generated/client.rs"
        ));
    }

    #[test]
    fn session_creation_receipt_rolls_back_exactly_once_before_durability() {
        let rollbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = rollbacks.clone();
        let receipt = SessionWorktreeCreation::guarded(
            "main".into(),
            "deadbeef".into(),
            move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            || Ok(()),
        );
        drop(receipt);
        assert_eq!(rollbacks.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn durable_session_creation_receipt_finalizes_instead_of_rolling_back_on_drop() {
        let rollbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let finalizes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rollback_count = rollbacks.clone();
        let finalize_count = finalizes.clone();
        let mut receipt = SessionWorktreeCreation::guarded(
            "main".into(),
            "deadbeef".into(),
            move || {
                rollback_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            move || {
                finalize_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("marker removal failed".into())
            },
        );
        receipt.mark_durable();
        drop(receipt);
        assert_eq!(rollbacks.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(finalizes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    struct SpecsOnlyExecutor;

    #[async_trait::async_trait]
    impl ToolExecutor for SpecsOnlyExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: "external_discovery".into(),
                description: "would require external discovery".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }

        async fn execute(&self, _ctx: &ToolCtx, name: &str, _args: &Value) -> ToolResult {
            ToolResult::error(format!("unknown tool: {name}"))
        }
    }

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

    #[test]
    fn managed_repository_validation_rejects_root_and_escape_paths() {
        let container = tempfile::tempdir().unwrap();
        let managed = container.path().join("worktrees");
        let session = managed.join("se_test");
        let outside = container.path().join("outside");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir(&outside).unwrap();

        let (_, resolved) = canonical_managed_path(&managed, &session).unwrap();
        assert_eq!(resolved, session.canonicalize().unwrap());
        assert!(canonical_managed_path(&managed, &managed).is_err());
        assert!(canonical_managed_path(&managed, &outside).is_err());
    }

    #[tokio::test]
    async fn missing_deleted_session_worktree_cleanup_is_idempotent() {
        let container = tempfile::tempdir().unwrap();
        let managed = container.path().join("worktrees");
        let attachments = container.path().join("attachments");
        std::fs::create_dir(&managed).unwrap();
        std::fs::create_dir(&attachments).unwrap();
        let attachment = attachments.join("at_test");
        std::fs::write(&attachment, b"payload").unwrap();
        let session_id = "se_00000000000000000000000000000000";
        let worktree = managed.join(session_id);
        let untouched_repository = container.path().join("repository-must-not-be-read");
        let request = DeletedSessionCleanup {
            managed_worktree_root: managed,
            worktree,
            repository: untouched_repository.clone(),
            session_id: session_id.into(),
            attachment_root: attachments,
            attachment_paths: vec![attachment.clone()],
            ownership_lost: tokio_util::sync::CancellationToken::new(),
        };
        let executor = LocalToolExecutor::default();

        executor.cleanup_deleted_session(&request).await.unwrap();
        executor.cleanup_deleted_session(&request).await.unwrap();

        assert!(!attachment.exists());
        assert!(!untouched_repository.exists());
    }

    #[cfg(unix)]
    #[test]
    fn attachment_cleanup_allows_a_symlinked_ancestor_but_not_a_symlinked_root() {
        use std::os::unix::fs::symlink;

        let real = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let real_root = real.path().join("attachments");
        std::fs::create_dir(&real_root).unwrap();
        let alias = aliases.path().join("data");
        symlink(real.path(), &alias).unwrap();
        let alias_root = alias.join("attachments");
        let attachment = alias_root.join("at_test");
        std::fs::write(&attachment, b"payload").unwrap();

        cleanup_attachments_secure(&alias_root, std::slice::from_ref(&attachment)).unwrap();
        assert!(!attachment.exists());

        let linked_root = aliases.path().join("linked-attachments");
        symlink(&real_root, &linked_root).unwrap();
        let error = cleanup_attachments_secure(&linked_root, &[]).unwrap_err();
        assert!(error.contains("opening attachment directory"), "{error}");
    }

    #[tokio::test]
    async fn lost_cleanup_claim_fences_attachment_deletion() {
        let container = tempfile::tempdir().unwrap();
        let attachments = container.path().join("attachments");
        std::fs::create_dir(&attachments).unwrap();
        let attachment = attachments.join("at_test");
        std::fs::write(&attachment, b"payload").unwrap();
        let ownership_lost = tokio_util::sync::CancellationToken::new();
        ownership_lost.cancel();

        let error = LocalToolExecutor::default()
            .cleanup_attachment_files(&attachments, vec![attachment.clone()], ownership_lost)
            .await
            .unwrap_err();

        assert!(error.contains("no longer owned"), "{error}");
        assert!(attachment.exists());
    }

    #[test]
    fn review_repository_locks_are_keyed_and_release_stale_entries() {
        let executor = LocalToolExecutor::default();
        let first = executor.review_repository_lock(Path::new("repo-a"));
        let same = executor.review_repository_lock(Path::new("repo-a"));
        let other = executor.review_repository_lock(Path::new("repo-b"));
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));

        drop(first);
        drop(same);
        drop(other);
        let replacement = executor.review_repository_lock(Path::new("repo-c"));
        let locks = executor.review_repository_locks.lock().unwrap();
        assert_eq!(locks.len(), 1);
        assert!(locks.get(Path::new("repo-c")).is_some());
        drop(locks);
        drop(replacement);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn review_git_runner_cleans_up_descendant_held_pipes_after_leader_exit() {
        let directory = tempfile::tempdir().unwrap();
        let started = tokio::time::Instant::now();
        let output = run_review_command_with_timeout(
            std::ffi::OsStr::new("/bin/sh"),
            directory.path(),
            "unused",
            vec!["-c".into(), "sleep 60 & printf complete".into()],
            &tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();

        assert_eq!(output, "complete");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn review_git_runner_bounds_leader_runtime_and_tree_cleanup_together() {
        let directory = tempfile::tempdir().unwrap();
        let started = tokio::time::Instant::now();
        let error = run_review_command_with_timeout(
            std::ffi::OsStr::new("/bin/sh"),
            directory.path(),
            "unused",
            vec!["-c".into(), "sleep 60".into()],
            &tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(300),
        )
        .await
        .unwrap_err();

        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn review_git_runner_drains_and_rejects_oversized_output() {
        let directory = tempfile::tempdir().unwrap();
        let error = run_review_command_with_timeout(
            std::ffi::OsStr::new("/bin/sh"),
            directory.path(),
            "unused",
            vec!["-c".into(), "yes x | head -c 1048576".into()],
            &tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap_err();

        assert!(error.contains("more than"), "unexpected error: {error}");
    }

    #[cfg(unix)]
    #[test]
    fn managed_repository_validation_rejects_symlinks_and_symlink_escapes() {
        let container = tempfile::tempdir().unwrap();
        let managed = container.path().join("worktrees");
        let session = managed.join("se_test");
        let outside = container.path().join("outside");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir(&outside).unwrap();

        let final_link = managed.join("linked-session");
        std::os::unix::fs::symlink(&session, &final_link).unwrap();
        assert!(canonical_managed_path(&managed, &final_link).is_err());

        let component_link = managed.join("linked-parent");
        std::fs::create_dir(outside.join("child")).unwrap();
        std::os::unix::fs::symlink(&outside, &component_link).unwrap();
        assert!(canonical_managed_path(&managed, &component_link.join("child")).is_err());
    }

    #[test]
    fn read_resolution_allows_only_registered_canonical_roots() {
        let worktree = tempfile::tempdir().unwrap();
        let readable = tempfile::tempdir().unwrap();
        let hidden = tempfile::tempdir().unwrap();
        let allowed = readable.path().join("skill");
        std::fs::create_dir(&allowed).unwrap();
        let instruction = allowed.join("SKILL.md");
        std::fs::write(&instruction, "instructions").unwrap();
        let secret = hidden.path().join("secret");
        std::fs::write(&secret, "secret").unwrap();
        let ctx = ToolCtx {
            worktree: worktree.path().to_path_buf(),
            read_only_roots: vec![allowed.canonicalize().unwrap()].into(),
            ..Default::default()
        };

        assert_eq!(
            ctx.resolve_read(instruction.to_str().unwrap()).unwrap(),
            instruction.canonicalize().unwrap()
        );
        assert!(ctx.resolve_read(secret.to_str().unwrap()).is_err());
        // Registration never widens the mutation resolver.
        assert!(ctx.resolve(instruction.to_str().unwrap()).is_err());
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

        let allowed = dir.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::os::unix::fs::symlink(outside.path(), allowed.join("escape")).unwrap();
        let read_ctx = ToolCtx {
            worktree: dir.path().to_path_buf(),
            read_only_roots: vec![allowed.canonicalize().unwrap()].into(),
            ..Default::default()
        };
        assert!(
            read_ctx
                .resolve_read(allowed.join("escape/secret").to_str().unwrap())
                .is_err()
        );
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

    #[tokio::test]
    async fn native_specs_fail_closed_for_specs_only_custom_executors() {
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            ..Default::default()
        };

        assert_eq!(SpecsOnlyExecutor.specs(&ctx).await.len(), 1);
        assert!(SpecsOnlyExecutor.native_specs(&ctx).await.is_empty());
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
    async fn enforced_hashline_catalog_isolates_the_selected_editor() {
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            edit_strategy: EditStrategy::EnforceHashline,
            ..Default::default()
        };
        let names = spec_names(exec.specs(&ctx).await);
        assert!(names.contains(&"hashline_edit".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"delete_file".to_string()));
        assert!(!names.contains(&"apply_patch_fallback".to_string()));
        assert!(!names.contains(&"shell".to_string()));
        assert!(!names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"apply_patch".to_string()));
    }

    #[tokio::test]
    async fn enforced_apply_patch_catalog_isolates_the_selected_editor() {
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            edit_strategy: EditStrategy::EnforceApplyPatch,
            ..Default::default()
        };
        let names = spec_names(exec.specs(&ctx).await);
        assert!(names.contains(&"apply_patch".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"delete_file".to_string()));
        assert!(!names.contains(&"shell".to_string()));
        assert!(!names.contains(&"hashline_edit".to_string()));
        assert!(!names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"apply_patch_fallback".to_string()));
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
    async fn enforced_hashline_denies_fallback_even_after_repeated_failures() {
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
        let denied = exec
            .execute(
                &ctx,
                "apply_patch_fallback",
                &serde_json::json!({"input": fallback}),
            )
            .await;
        assert_eq!(denied.status, ToolStatus::Error, "{:?}", denied.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "old\n"
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

    #[tokio::test]
    async fn enforced_benchmark_denies_mcp_before_dispatch() {
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            edit_strategy: EditStrategy::EnforceApplyPatch,
            ..Default::default()
        };
        let denied = exec
            .execute(&ctx, "mcp__example__edit", &serde_json::json!({}))
            .await;
        assert_eq!(denied.status, ToolStatus::Error);
        assert!(
            denied.result["error"]
                .as_str()
                .unwrap()
                .contains("benchmark")
        );
    }
}
